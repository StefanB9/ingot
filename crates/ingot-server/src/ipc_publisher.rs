//! Bridge from the engine's `broadcast::Receiver<WsMessage>` to iceoryx2
//! pub/sub publishers.
//!
//! Runs on a dedicated OS thread because iceoryx2 has a synchronous API.
//! Exits when the broadcast channel closes or the shutdown flag is set.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};
use iceoryx2::{config::Config, prelude::*};
use ingot_core::api::WsMessage;
use ingot_ipc::{
    SERVICE_NAV, SERVICE_ORDERS, SERVICE_TICKER,
    types::{IpcNavUpdate, IpcOrderUpdate, IpcTickerUpdate},
};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// Spawn the IPC publisher thread.
///
/// Returns the `JoinHandle` so the caller can join on shutdown.
pub fn spawn(
    event_rx: broadcast::Receiver<WsMessage>,
    shutdown: Arc<AtomicBool>,
    config: Config,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("ipc-publisher".into())
        .spawn(move || {
            if let Err(e) = run(event_rx, &shutdown, &config) {
                error!(error = %e, "IPC publisher thread crashed");
            }
        })
        .context("failed to spawn IPC publisher thread")
}

fn run(
    mut event_rx: broadcast::Receiver<WsMessage>,
    shutdown: &Arc<AtomicBool>,
    config: &Config,
) -> Result<()> {
    let node = NodeBuilder::new().config(config).create::<ipc::Service>()?;

    let ticker_svc_name: ServiceName = SERVICE_TICKER
        .try_into()
        .context("invalid ticker service name")?;
    let nav_svc_name: ServiceName = SERVICE_NAV.try_into().context("invalid nav service name")?;
    let orders_svc_name: ServiceName = SERVICE_ORDERS
        .try_into()
        .context("invalid orders service name")?;

    let ticker_svc = node
        .service_builder(&ticker_svc_name)
        .publish_subscribe::<IpcTickerUpdate>()
        .open_or_create()?;
    let nav_svc = node
        .service_builder(&nav_svc_name)
        .publish_subscribe::<IpcNavUpdate>()
        .open_or_create()?;
    let orders_svc = node
        .service_builder(&orders_svc_name)
        .publish_subscribe::<IpcOrderUpdate>()
        .open_or_create()?;

    let ticker_pub = ticker_svc.publisher_builder().create()?;
    let nav_pub = nav_svc.publisher_builder().create()?;
    let orders_pub = orders_svc.publisher_builder().create()?;

    info!("IPC publisher thread started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("IPC publisher: shutdown flag set, exiting");
            break;
        }

        match event_rx.try_recv() {
            Ok(msg) => {
                if let Err(e) = publish(&ticker_pub, &nav_pub, &orders_pub, &msg) {
                    warn!(error = %e, "IPC publish failed");
                }
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                warn!(skipped = n, "IPC publisher lagged behind broadcast");
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                info!("IPC publisher: broadcast channel closed, exiting");
                break;
            }
        }
    }

    Ok(())
}

/// Publish a single `WsMessage` to the appropriate iceoryx2 service.
///
/// Uses `send_copy` for simplicity — the IPC types are small and `Copy`.
fn publish(
    ticker_pub: &iceoryx2::port::publisher::Publisher<ipc::Service, IpcTickerUpdate, ()>,
    nav_pub: &iceoryx2::port::publisher::Publisher<ipc::Service, IpcNavUpdate, ()>,
    orders_pub: &iceoryx2::port::publisher::Publisher<ipc::Service, IpcOrderUpdate, ()>,
    msg: &WsMessage,
) -> Result<()> {
    match msg {
        WsMessage::Tick(update) => {
            let ipc = IpcTickerUpdate::from(update);
            ticker_pub.send_copy(ipc)?;
            debug!(symbol = %update.symbol, "published ticker update");
        }
        WsMessage::Nav(update) => {
            let ipc = IpcNavUpdate::from(update);
            nav_pub.send_copy(ipc)?;
            debug!("published NAV update");
        }
        WsMessage::OrderUpdate(ack) => {
            let ipc = IpcOrderUpdate::from(ack);
            orders_pub.send_copy(ipc)?;
            debug!(exchange_id = %ack.exchange_id, "published order update");
        }
        WsMessage::Subscribe(_) | WsMessage::Error(_) => {
            // Subscribe is client->server only; Error is logged elsewhere
        }
    }
    Ok(())
}
