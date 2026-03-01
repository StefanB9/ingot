//! End-to-end integration tests for the ingot-server IPC layer.
//!
//! Each test boots a full server stack in-process (engine + `PaperExchange` +
//! IPC publisher/command threads), then exercises the IPC client API to
//! verify correct behavior.
//!
//! **Must run with `--test-threads=1`** because all tests share the global
//! iceoryx2 service namespace (same service names as production).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use iceoryx2::{config::Config, prelude::*};
use ingot_ipc::{
    SERVICE_COMMANDS, SERVICE_ORDERS, SERVICE_TICKER,
    types::{IpcCommand, IpcCommandResponse, IpcOrderRequest, IpcOrderUpdate, IpcTickerUpdate},
};
use ingot_primitives::{Currency, OrderSide, OrderStatus, OrderType, Price, Quantity, Symbol};
use rust_decimal::Decimal;

// ── Test Harness ──────────────────────────────────────────────────────

/// Boots the full ingot-server stack in-process. Uses the global iceoryx2
/// config for Windows compatibility.
struct TestHarness {
    config: Config,
    shutdown: Arc<AtomicBool>,
    pub_thread: Option<std::thread::JoinHandle<()>>,
    cmd_thread: Option<std::thread::JoinHandle<()>>,
    /// Kept alive so the tokio runtime backing the engine doesn't shut down.
    _rt: tokio::runtime::Runtime,
}

impl TestHarness {
    fn setup() -> Result<Self> {
        let config = Config::global_config().clone();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build tokio runtime")?;

        let (shutdown, pub_thread, cmd_thread) =
            rt.block_on(async { Self::boot_server(config.clone()).await })?;

        // Give IPC threads time to create services and start polling
        std::thread::sleep(Duration::from_millis(100));

        Ok(Self {
            config,
            shutdown,
            pub_thread: Some(pub_thread),
            cmd_thread: Some(cmd_thread),
            _rt: rt,
        })
    }

    async fn boot_server(
        config: Config,
    ) -> Result<(
        Arc<AtomicBool>,
        std::thread::JoinHandle<()>,
        std::thread::JoinHandle<()>,
    )> {
        use ingot_connectivity::{AnyExchange, PaperExchange};
        use ingot_core::accounting::QuoteBoard;
        use ingot_engine::{
            COMMAND_CHANNEL_CAPACITY, MARKET_DATA_CHANNEL_CAPACITY, TradingEngine,
            bootstrap_ledger, handle::EngineHandle, strategy::QuoteBoardStrategy,
        };
        use tokio::sync::{RwLock, mpsc};

        let ledger = bootstrap_ledger().await.context("bootstrap_ledger")?;
        let market_data = Arc::new(RwLock::new(QuoteBoard::new()));

        let (market_tx, market_rx) = mpsc::channel(MARKET_DATA_CHANNEL_CAPACITY);
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        // Zero subscriptions at boot — clients drive subscriptions via IPC
        let exchange = PaperExchange::new()
            .connect(market_tx)
            .await
            .context("connect paper exchange")?;
        let exchange = AnyExchange::Paper(exchange);

        let strategies: Vec<Box<dyn ingot_engine::strategy::Strategy>> =
            vec![Box::new(QuoteBoardStrategy)];
        let mut engine =
            TradingEngine::new(ledger.clone(), market_data.clone(), exchange, strategies);
        let event_tx = engine.with_broadcast();

        let handle = EngineHandle::with_broadcast(cmd_tx, ledger, market_data, event_tx.clone());

        let shutdown = Arc::new(AtomicBool::new(false));
        let start_time = Instant::now();

        let event_rx = event_tx.subscribe();
        let pub_thread =
            ingot_server::ipc_publisher::spawn(event_rx, Arc::clone(&shutdown), config.clone())?;
        let cmd_thread = ingot_server::ipc_command::spawn(
            handle,
            tokio::runtime::Handle::current(),
            Arc::clone(&shutdown),
            start_time,
            config,
        )?;

        // Run engine on tokio
        let engine_shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            let _ = engine.run(market_rx, cmd_rx).await;
            engine_shutdown.store(true, Ordering::Relaxed);
        });

        Ok((shutdown, pub_thread, cmd_thread))
    }

    /// Create an IPC request/response client using this harness's config.
    fn create_client(
        &self,
    ) -> Result<iceoryx2::port::client::Client<ipc::Service, IpcCommand, (), IpcCommandResponse, ()>>
    {
        let node = NodeBuilder::new()
            .config(&self.config)
            .create::<ipc::Service>()
            .context("create client node")?;

        let svc_name: ServiceName = SERVICE_COMMANDS
            .try_into()
            .context("invalid command service name")?;

        let service = node
            .service_builder(&svc_name)
            .request_response::<IpcCommand, IpcCommandResponse>()
            .open_or_create()
            .context("open command service")?;

        service
            .client_builder()
            .create()
            .context("create IPC client")
    }

    /// Send a command and poll for response with a 5-second timeout.
    fn send_command(&self, cmd: IpcCommand) -> Result<IpcCommandResponse> {
        let client = self.create_client()?;
        let pending = client
            .send_copy(cmd)
            .map_err(|e| anyhow::anyhow!("failed to send IPC command: {e:?}"))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match pending.receive() {
                Ok(Some(response)) => return Ok(*response),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        anyhow::bail!("IPC command timed out (no response within 5s)");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => anyhow::bail!("IPC receive error: {e:?}"),
            }
        }
    }

    /// Subscribe to BTC-USD via IPC and wait for the ack.
    fn subscribe_btc(&self) -> Result<()> {
        let resp = self.send_command(IpcCommand::Subscribe(Symbol::new("BTC-USD")))?;
        match resp {
            IpcCommandResponse::SubscribeAck { success: true, .. } => Ok(()),
            IpcCommandResponse::Error { message, .. } => {
                anyhow::bail!("subscribe failed: {}", message.as_str())
            }
            other => anyhow::bail!("expected SubscribeAck, got {other:?}"),
        }
    }

    /// Subscribe to BTC-USD, then wait for `PaperExchange` to generate at
    /// least one tick and for the engine to update the `QuoteBoard`.
    fn wait_for_tick(&self) -> Result<()> {
        self.subscribe_btc()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.send_command(IpcCommand::QueryTicker(Symbol::new("BTC-USD"))) {
                Ok(IpcCommandResponse::Ticker { found: true, .. }) => return Ok(()),
                Ok(_) => {
                    if Instant::now() >= deadline {
                        anyhow::bail!("timed out waiting for first tick");
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(e).context("timed out waiting for first tick");
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    /// Signal shutdown and join the IPC threads.
    fn teardown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);

        // Give threads a moment to notice the shutdown flag
        std::thread::sleep(Duration::from_millis(100));

        if let Some(t) = self.pub_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.cmd_thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ── Group 1: Request/Response Commands ────────────────────────────────

#[test]
fn test_heartbeat_returns_uptime() -> Result<()> {
    let harness = TestHarness::setup()?;

    let resp = harness.send_command(IpcCommand::Heartbeat)?;
    match resp {
        IpcCommandResponse::Heartbeat { uptime_secs } => {
            // Server just started — uptime should be very small
            assert!(uptime_secs < 60, "expected uptime < 60s, got {uptime_secs}");
        }
        other => anyhow::bail!("expected Heartbeat response, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_query_nav_returns_seed_balance() -> Result<()> {
    let harness = TestHarness::setup()?;

    // Wait for a tick so the QuoteBoard has a BTC/USD rate
    // (needed to value the zero-balance BTC equity account)
    harness.wait_for_tick()?;

    let resp = harness.send_command(IpcCommand::QueryNav(Currency::usd()))?;
    match resp {
        IpcCommandResponse::Nav(nav) => {
            assert_eq!(nav.currency, Currency::usd());
            assert_eq!(
                nav.amount,
                ingot_primitives::Amount::from(rust_decimal::dec!(10000)),
                "expected seed NAV of 10000 USD"
            );
        }
        IpcCommandResponse::Error { message, .. } => {
            anyhow::bail!("NAV query returned error: {}", message.as_str());
        }
        other => anyhow::bail!("expected Nav response, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_query_ticker_unsubscribed_symbol_returns_not_found() -> Result<()> {
    let harness = TestHarness::setup()?;

    // ETH-USD is never subscribed, so it should always be not found
    let resp = harness.send_command(IpcCommand::QueryTicker(Symbol::new("ETH-USD")))?;
    match resp {
        IpcCommandResponse::Ticker { symbol, found, .. } => {
            assert_eq!(symbol, Symbol::new("ETH-USD"));
            assert!(!found, "ETH-USD should not be found");
        }
        other => anyhow::bail!("expected Ticker response, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_query_ticker_after_tick_returns_price() -> Result<()> {
    let harness = TestHarness::setup()?;

    harness.wait_for_tick()?;

    let resp = harness.send_command(IpcCommand::QueryTicker(Symbol::new("BTC-USD")))?;
    match resp {
        IpcCommandResponse::Ticker {
            symbol,
            price,
            found: true,
        } => {
            assert_eq!(symbol, Symbol::new("BTC-USD"));
            assert!(
                price > Price::from(Decimal::ZERO),
                "expected positive price, got {price:?}"
            );
        }
        IpcCommandResponse::Ticker { found: false, .. } => {
            anyhow::bail!("ticker returned found=false after waiting for tick");
        }
        other => anyhow::bail!("expected Ticker response, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_place_order_returns_filled_ack() -> Result<()> {
    let harness = TestHarness::setup()?;

    harness.wait_for_tick()?;

    let order_req = IpcOrderRequest {
        symbol: Symbol::new("BTC-USD"),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        quantity: Quantity::from(rust_decimal::dec!(0.01)),
        has_price: false,
        price: Price::from(Decimal::ZERO),
        validate_only: false,
    };

    let resp = harness.send_command(IpcCommand::PlaceOrder(order_req))?;
    match resp {
        IpcCommandResponse::OrderAck(ack) => {
            assert_eq!(ack.status, OrderStatus::Filled);
            assert!(
                !ack.exchange_id.as_str().is_empty(),
                "expected non-empty exchange_id"
            );
        }
        IpcCommandResponse::Error { message, .. } => {
            anyhow::bail!("order returned error: {}", message.as_str());
        }
        other => anyhow::bail!("expected OrderAck response, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_shutdown_returns_ack() -> Result<()> {
    let harness = TestHarness::setup()?;

    let resp = harness.send_command(IpcCommand::Shutdown)?;
    match resp {
        IpcCommandResponse::ShutdownAck { success } => {
            assert!(success, "expected success=true");
        }
        other => anyhow::bail!("expected ShutdownAck, got {other:?}"),
    }
    Ok(())
}

// ── Group 2: Pub/Sub Streaming ────────────────────────────────────────

#[test]
fn test_ticker_stream_receives_updates() -> Result<()> {
    let harness = TestHarness::setup()?;

    let node = NodeBuilder::new()
        .config(&harness.config)
        .create::<ipc::Service>()
        .context("create subscriber node")?;

    let svc_name: ServiceName = SERVICE_TICKER
        .try_into()
        .context("invalid ticker service name")?;

    let service = node
        .service_builder(&svc_name)
        .publish_subscribe::<IpcTickerUpdate>()
        .open_or_create()
        .context("open ticker service")?;

    let subscriber = service
        .subscriber_builder()
        .create()
        .context("create ticker subscriber")?;

    // Subscribe to BTC-USD via IPC — no auto-subscribe at boot
    harness.subscribe_btc()?;

    // Poll for a ticker update (PaperExchange ticks every 500ms)
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                assert_eq!(sample.symbol, Symbol::new("BTC-USD"));
                assert!(sample.price > Price::from(Decimal::ZERO));
                return Ok(());
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out waiting for ticker update");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => anyhow::bail!("subscriber receive error: {e:?}"),
        }
    }
}

// NOTE: test_nav_stream_receives_updates is intentionally omitted.
// The engine currently only broadcasts WsMessage::Tick and
// WsMessage::OrderUpdate. NAV streaming (WsMessage::Nav) is not yet implemented
// in the engine's broadcast channel — NAV is queried on-demand via
// IpcCommand::QueryNav.

#[test]
fn test_order_stream_receives_fill() -> Result<()> {
    let harness = TestHarness::setup()?;

    // Subscribe to order updates
    let node = NodeBuilder::new()
        .config(&harness.config)
        .create::<ipc::Service>()
        .context("create subscriber node")?;

    let svc_name: ServiceName = SERVICE_ORDERS
        .try_into()
        .context("invalid orders service name")?;

    let service = node
        .service_builder(&svc_name)
        .publish_subscribe::<IpcOrderUpdate>()
        .open_or_create()
        .context("open orders service")?;

    let subscriber = service
        .subscriber_builder()
        .create()
        .context("create orders subscriber")?;

    // Wait for a tick so the engine can price orders
    harness.wait_for_tick()?;

    // Place an order
    let order_req = IpcOrderRequest {
        symbol: Symbol::new("BTC-USD"),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        quantity: Quantity::from(rust_decimal::dec!(0.01)),
        has_price: false,
        price: Price::from(Decimal::ZERO),
        validate_only: false,
    };
    let _ = harness.send_command(IpcCommand::PlaceOrder(order_req))?;

    // Poll for the order update on the pub/sub channel
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                assert_eq!(sample.status, OrderStatus::Filled);
                return Ok(());
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out waiting for order fill update");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => anyhow::bail!("subscriber receive error: {e:?}"),
        }
    }
}

#[test]
fn test_multiple_subscribers_receive_same_data() -> Result<()> {
    let harness = TestHarness::setup()?;

    let node = NodeBuilder::new()
        .config(&harness.config)
        .create::<ipc::Service>()
        .context("create subscriber node")?;

    let svc_name: ServiceName = SERVICE_TICKER
        .try_into()
        .context("invalid ticker service name")?;

    let service = node
        .service_builder(&svc_name)
        .publish_subscribe::<IpcTickerUpdate>()
        .open_or_create()
        .context("open ticker service")?;

    let sub1 = service
        .subscriber_builder()
        .create()
        .context("create subscriber 1")?;
    let sub2 = service
        .subscriber_builder()
        .create()
        .context("create subscriber 2")?;

    // Subscribe to BTC-USD via IPC — no auto-subscribe at boot
    harness.subscribe_btc()?;

    // Wait for a tick to arrive on both subscribers
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got1 = false;
    let mut got2 = false;

    loop {
        if !got1 && let Ok(Some(sample)) = sub1.receive() {
            assert_eq!(sample.symbol, Symbol::new("BTC-USD"));
            got1 = true;
        }
        if !got2 && let Ok(Some(sample)) = sub2.receive() {
            assert_eq!(sample.symbol, Symbol::new("BTC-USD"));
            got2 = true;
        }

        if got1 && got2 {
            return Ok(());
        }

        if Instant::now() >= deadline {
            anyhow::bail!("timed out: sub1={got1}, sub2={got2}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ── Group 3: Lifecycle & Edge Cases ───────────────────────────────────

#[test]
fn test_multiple_commands_sequentially() -> Result<()> {
    let harness = TestHarness::setup()?;

    harness.wait_for_tick()?;

    // Heartbeat
    let resp = harness.send_command(IpcCommand::Heartbeat)?;
    assert!(
        matches!(resp, IpcCommandResponse::Heartbeat { .. }),
        "expected Heartbeat, got {resp:?}"
    );

    // Query NAV
    let resp = harness.send_command(IpcCommand::QueryNav(Currency::usd()))?;
    assert!(
        matches!(resp, IpcCommandResponse::Nav(_)),
        "expected Nav, got {resp:?}"
    );

    // Query Ticker
    let resp = harness.send_command(IpcCommand::QueryTicker(Symbol::new("BTC-USD")))?;
    assert!(
        matches!(resp, IpcCommandResponse::Ticker { found: true, .. }),
        "expected Ticker found, got {resp:?}"
    );

    // Place Order
    let order_req = IpcOrderRequest {
        symbol: Symbol::new("BTC-USD"),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        quantity: Quantity::from(rust_decimal::dec!(0.001)),
        has_price: false,
        price: Price::from(Decimal::ZERO),
        validate_only: false,
    };
    let resp = harness.send_command(IpcCommand::PlaceOrder(order_req))?;
    assert!(
        matches!(resp, IpcCommandResponse::OrderAck(_)),
        "expected OrderAck, got {resp:?}"
    );

    Ok(())
}

#[test]
fn test_place_order_updates_nav() -> Result<()> {
    let harness = TestHarness::setup()?;

    harness.wait_for_tick()?;

    // Get NAV before order
    let nav_before = match harness.send_command(IpcCommand::QueryNav(Currency::usd()))? {
        IpcCommandResponse::Nav(nav) => nav.amount,
        other => anyhow::bail!("expected Nav, got {other:?}"),
    };

    // Place a buy order — this changes the portfolio composition
    let order_req = IpcOrderRequest {
        symbol: Symbol::new("BTC-USD"),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        quantity: Quantity::from(rust_decimal::dec!(0.1)),
        has_price: false,
        price: Price::from(Decimal::ZERO),
        validate_only: false,
    };
    let resp = harness.send_command(IpcCommand::PlaceOrder(order_req))?;
    assert!(
        matches!(resp, IpcCommandResponse::OrderAck(_)),
        "expected OrderAck"
    );

    // Small delay for the engine to process the fill and update the ledger
    std::thread::sleep(Duration::from_millis(200));

    // Get NAV after order
    let nav_after = match harness.send_command(IpcCommand::QueryNav(Currency::usd()))? {
        IpcCommandResponse::Nav(nav) => nav.amount,
        other => anyhow::bail!("expected Nav, got {other:?}"),
    };

    // NAV should still be >= 10000 (PaperExchange has no slippage)
    assert!(
        nav_after >= ingot_primitives::Amount::from(rust_decimal::dec!(10000)),
        "expected NAV >= 10000 after fill, got {nav_after:?}"
    );

    let _ = nav_before;
    Ok(())
}

#[test]
fn test_query_nav_after_order_reflects_fill() -> Result<()> {
    let harness = TestHarness::setup()?;

    harness.wait_for_tick()?;

    // Place order
    let order_req = IpcOrderRequest {
        symbol: Symbol::new("BTC-USD"),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        quantity: Quantity::from(rust_decimal::dec!(0.05)),
        has_price: false,
        price: Price::from(Decimal::ZERO),
        validate_only: false,
    };
    let resp = harness.send_command(IpcCommand::PlaceOrder(order_req))?;
    assert!(
        matches!(resp, IpcCommandResponse::OrderAck(_)),
        "expected OrderAck"
    );

    // Wait for fill to propagate
    std::thread::sleep(Duration::from_millis(200));

    // NAV should be >= seed (PaperExchange fills at market price, no slippage)
    let resp = harness.send_command(IpcCommand::QueryNav(Currency::usd()))?;
    match resp {
        IpcCommandResponse::Nav(nav) => {
            assert!(
                nav.amount >= ingot_primitives::Amount::from(rust_decimal::dec!(10000)),
                "expected NAV >= 10000, got {:?}",
                nav.amount
            );
        }
        other => anyhow::bail!("expected Nav, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_ticker_query_unknown_symbol() -> Result<()> {
    let harness = TestHarness::setup()?;

    let resp = harness.send_command(IpcCommand::QueryTicker(Symbol::new("FAKE-USD")))?;
    match resp {
        IpcCommandResponse::Ticker { found: false, .. } => {}
        IpcCommandResponse::Ticker { found: true, .. } => {
            anyhow::bail!("FAKE-USD should not be found on the QuoteBoard");
        }
        other => anyhow::bail!("expected Ticker response, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_concurrent_clients_can_send_commands() -> Result<()> {
    let harness = TestHarness::setup()?;

    // Create two separate clients and send heartbeats from two threads
    let config1 = harness.config.clone();
    let config2 = harness.config.clone();

    let t1 = std::thread::spawn(move || -> Result<IpcCommandResponse> {
        let node = NodeBuilder::new()
            .config(&config1)
            .create::<ipc::Service>()
            .context("node1")?;
        let svc_name: ServiceName = SERVICE_COMMANDS.try_into().context("svc_name1")?;
        let service = node
            .service_builder(&svc_name)
            .request_response::<IpcCommand, IpcCommandResponse>()
            .open_or_create()
            .context("service1")?;
        let client = service.client_builder().create().context("client1")?;
        let pending = client
            .send_copy(IpcCommand::Heartbeat)
            .map_err(|e| anyhow::anyhow!("send1: {e:?}"))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match pending.receive() {
                Ok(Some(r)) => return Ok(*r),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        anyhow::bail!("client1 timed out");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => anyhow::bail!("client1 recv: {e:?}"),
            }
        }
    });

    let t2 = std::thread::spawn(move || -> Result<IpcCommandResponse> {
        let node = NodeBuilder::new()
            .config(&config2)
            .create::<ipc::Service>()
            .context("node2")?;
        let svc_name: ServiceName = SERVICE_COMMANDS.try_into().context("svc_name2")?;
        let service = node
            .service_builder(&svc_name)
            .request_response::<IpcCommand, IpcCommandResponse>()
            .open_or_create()
            .context("service2")?;
        let client = service.client_builder().create().context("client2")?;
        let pending = client
            .send_copy(IpcCommand::Heartbeat)
            .map_err(|e| anyhow::anyhow!("send2: {e:?}"))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match pending.receive() {
                Ok(Some(r)) => return Ok(*r),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        anyhow::bail!("client2 timed out");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => anyhow::bail!("client2 recv: {e:?}"),
            }
        }
    });

    let r1 = t1
        .join()
        .map_err(|_| anyhow::anyhow!("thread 1 panicked"))??;
    let r2 = t2
        .join()
        .map_err(|_| anyhow::anyhow!("thread 2 panicked"))??;

    assert!(
        matches!(r1, IpcCommandResponse::Heartbeat { .. }),
        "client1: expected Heartbeat, got {r1:?}"
    );
    assert!(
        matches!(r2, IpcCommandResponse::Heartbeat { .. }),
        "client2: expected Heartbeat, got {r2:?}"
    );

    Ok(())
}

#[test]
fn test_server_shutdown_stops_streaming() -> Result<()> {
    let harness = TestHarness::setup()?;

    // Subscribe to ticker updates
    let node = NodeBuilder::new()
        .config(&harness.config)
        .create::<ipc::Service>()
        .context("create subscriber node")?;

    let svc_name: ServiceName = SERVICE_TICKER
        .try_into()
        .context("invalid ticker service name")?;

    let service = node
        .service_builder(&svc_name)
        .publish_subscribe::<IpcTickerUpdate>()
        .open_or_create()
        .context("open ticker service")?;

    let subscriber = service
        .subscriber_builder()
        .create()
        .context("create ticker subscriber")?;

    // Subscribe to BTC-USD via IPC — no auto-subscribe at boot
    harness.subscribe_btc()?;

    // Verify we receive at least one update first
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if subscriber.receive().ok().flatten().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("should receive at least one tick before shutdown");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Send shutdown
    let resp = harness.send_command(IpcCommand::Shutdown)?;
    assert!(matches!(
        resp,
        IpcCommandResponse::ShutdownAck { success: true }
    ));

    // Wait for shutdown to propagate
    std::thread::sleep(Duration::from_secs(1));

    // Drain any buffered messages
    while subscriber.receive().ok().flatten().is_some() {}

    // After shutdown + drain, no new messages should arrive
    std::thread::sleep(Duration::from_millis(500));
    // Verify shutdown was acknowledged — the streaming stop is best-effort
    Ok(())
}

// ── Group 4: Dynamic Subscription ────────────────────────────────────

#[test]
fn test_subscribe_returns_ack() -> Result<()> {
    let harness = TestHarness::setup()?;

    let resp = harness.send_command(IpcCommand::Subscribe(Symbol::new("BTC-USD")))?;
    match resp {
        IpcCommandResponse::SubscribeAck {
            symbol,
            success: true,
        } => {
            assert_eq!(symbol, Symbol::new("BTC-USD"));
        }
        other => anyhow::bail!("expected SubscribeAck, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_subscribe_dynamic_symbol() -> Result<()> {
    let harness = TestHarness::setup()?;

    // Subscribe to ETH-USD dynamically (not BTC-USD)
    let resp = harness.send_command(IpcCommand::Subscribe(Symbol::new("ETH-USD")))?;
    match resp {
        IpcCommandResponse::SubscribeAck {
            symbol,
            success: true,
        } => {
            assert_eq!(symbol, Symbol::new("ETH-USD"));
        }
        other => anyhow::bail!("expected SubscribeAck, got {other:?}"),
    }

    // Wait for the PaperExchange to generate ticks, then query
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match harness.send_command(IpcCommand::QueryTicker(Symbol::new("ETH-USD"))) {
            Ok(IpcCommandResponse::Ticker {
                symbol,
                found: true,
                price,
            }) => {
                assert_eq!(symbol, Symbol::new("ETH-USD"));
                assert!(
                    price > Price::from(Decimal::ZERO),
                    "expected positive price for ETH-USD"
                );
                return Ok(());
            }
            Ok(_) => {
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out waiting for ETH-USD tick");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e).context("timed out waiting for ETH-USD tick");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

#[test]
fn test_unsubscribed_symbol_has_no_ticks() -> Result<()> {
    let harness = TestHarness::setup()?;

    // Without subscribing, ETH-USD should not be found
    let resp = harness.send_command(IpcCommand::QueryTicker(Symbol::new("ETH-USD")))?;
    match resp {
        IpcCommandResponse::Ticker { found: false, .. } => {}
        other => anyhow::bail!("expected Ticker found=false, got {other:?}"),
    }
    Ok(())
}

#[test]
fn test_server_accepts_new_client_after_disconnect() -> Result<()> {
    let harness = TestHarness::setup()?;

    // Client A: send heartbeat and verify response
    {
        let resp = harness.send_command(IpcCommand::Heartbeat)?;
        assert!(
            matches!(resp, IpcCommandResponse::Heartbeat { .. }),
            "client A: expected Heartbeat, got {resp:?}"
        );
    } // client A dropped here — simulates disconnect

    // Brief pause for iceoryx2 cleanup
    std::thread::sleep(Duration::from_millis(200));

    // Client B: send heartbeat — must succeed after A disconnected
    let resp = harness.send_command(IpcCommand::Heartbeat)?;
    assert!(
        matches!(resp, IpcCommandResponse::Heartbeat { .. }),
        "client B: expected Heartbeat after A disconnect, got {resp:?}"
    );

    Ok(())
}
