//! Headless ingot trading daemon.
//!
//! Bootstraps the trading engine, connects to an exchange, and exposes
//! IPC services via iceoryx2 shared memory for thin CLI/GUI clients.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result};
use iceoryx2::config::Config;
use ingot_config::ServerConfig;
use ingot_connectivity::{AnyExchange, PaperExchange, kraken::KrakenExchange};
use ingot_core::accounting::QuoteBoard;
use ingot_engine::{
    COMMAND_CHANNEL_CAPACITY, MARKET_DATA_CHANNEL_CAPACITY, TradingEngine, bootstrap_ledger,
    handle::EngineHandle,
    strategy::{QuoteBoardStrategy, Strategy},
};
use ingot_server::{ipc_command, ipc_publisher};
use tokio::sync::{RwLock, mpsc};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let config = ServerConfig::load();

    let (filter, env_err) = match EnvFilter::try_from_default_env() {
        Ok(f) => (f, None),
        Err(e) => {
            let level = if config.verbose { "debug" } else { "info" };
            let default = EnvFilter::new(format!(
                "ingot_server={level},ingot_engine={level},ingot_connectivity=debug,\
                 ingot_core=info"
            ));
            (default, Some(e))
        }
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();
    if let Some(e) = env_err {
        warn!(error = %e, "invalid RUST_LOG filter, using default");
    }

    info!(paper = config.paper, "Starting ingot-server");

    let ledger = bootstrap_ledger()
        .await
        .context("failed to bootstrap ledger")?;

    let market_data = Arc::new(RwLock::new(QuoteBoard::new()));

    let (market_tx, market_rx) = mpsc::channel(MARKET_DATA_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

    // Connect exchange — paper or live Kraken
    let exchange: AnyExchange = if config.paper {
        let paper = PaperExchange::new()
            .connect(market_tx)
            .await
            .context("failed to connect paper exchange")?;
        info!("Paper exchange connected (zero subscriptions — use CLI/GUI to subscribe)");
        AnyExchange::Paper(paper)
    } else {
        let mut kraken =
            KrakenExchange::from_env().context("failed to create Kraken exchange from env")?;
        kraken
            .connect(market_tx)
            .await
            .context("failed to connect to Kraken WebSocket")?;
        info!("Kraken exchange connected (zero subscriptions — use CLI/GUI to subscribe)");
        AnyExchange::Kraken(kraken)
    };

    let strategies: Vec<Box<dyn Strategy>> = vec![Box::new(QuoteBoardStrategy)];
    let mut engine = TradingEngine::new(ledger.clone(), market_data.clone(), exchange, strategies);
    let event_tx = engine.with_broadcast();

    let handle = EngineHandle::with_broadcast(cmd_tx, ledger, market_data, event_tx.clone());

    // Shared shutdown flag
    let shutdown = Arc::new(AtomicBool::new(false));
    let start_time = Instant::now();

    // Spawn IPC threads
    let ipc_config = Config::global_config().clone();
    let event_rx = event_tx.subscribe();
    let pub_thread = ipc_publisher::spawn(event_rx, Arc::clone(&shutdown), ipc_config.clone())?;
    let cmd_thread = ipc_command::spawn(
        handle.clone(),
        tokio::runtime::Handle::current(),
        Arc::clone(&shutdown),
        start_time,
        ipc_config,
    )?;

    // Spawn engine on tokio runtime
    let engine_done = tokio::spawn(async move {
        if let Err(e) = engine.run(market_rx, cmd_rx).await {
            error!(error = %e, "engine crashed");
        }
    });

    info!("ingot-server running. Press Ctrl+C to stop.");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for ctrl-c")?;

    warn!("Ctrl+C received, shutting down...");
    shutdown.store(true, Ordering::Relaxed);

    // Send shutdown command to the engine
    if let Err(e) = handle.shutdown().await {
        warn!(error = %e, "failed to send shutdown to engine");
    }

    // Wait for engine to finish
    if let Err(e) = engine_done.await {
        error!(error = %e, "engine task panicked");
    }

    // Join IPC threads (they check the shutdown flag)
    if let Err(e) = pub_thread.join() {
        error!("IPC publisher thread panicked: {e:?}");
    }
    if let Err(e) = cmd_thread.join() {
        error!("IPC command thread panicked: {e:?}");
    }

    info!("ingot-server stopped");
    Ok(())
}
