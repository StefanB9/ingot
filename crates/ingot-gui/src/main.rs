use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use egui::Color32;
use iceoryx2::prelude::*;
use ingot_ipc::{
    SERVICE_COMMANDS, SERVICE_NAV, SERVICE_ORDERS, SERVICE_TICKER,
    types::{
        IpcCommand, IpcCommandResponse, IpcNavUpdate, IpcOrderRequest, IpcOrderUpdate,
        IpcTickerUpdate,
    },
};
use ingot_primitives::{OrderSide, OrderType, Quantity, Symbol};
use rust_decimal::Decimal;
use tracing::{error, info, warn};

/// Messages sent from the IPC subscriber thread to the GUI main thread.
enum IpcEvent {
    Ticker(IpcTickerUpdate),
    Nav(IpcNavUpdate),
    Order(IpcOrderUpdate),
}

fn main() -> Result<()> {
    let config = ingot_config::AppConfig::load();

    let (filter, env_err) = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(f) => (f, None),
        Err(e) => (
            tracing_subscriber::EnvFilter::new("ingot_gui=info"),
            Some(e),
        ),
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();
    if let Some(e) = env_err {
        warn!(error = %e, "invalid RUST_LOG filter, using default");
    }

    info!("Starting Ingot GUI (IPC client mode)");

    // Channel for IPC subscriber -> GUI thread
    let (event_tx, event_rx) = std::sync::mpsc::channel::<IpcEvent>();

    // Shared shutdown flag for the subscriber thread
    let shutdown = Arc::new(AtomicBool::new(false));

    // Spawn the IPC subscriber thread
    let sub_shutdown = Arc::clone(&shutdown);
    let sub_thread = std::thread::Builder::new()
        .name("ipc-subscriber".into())
        .spawn(move || {
            if let Err(e) = run_subscriber(&event_tx, &sub_shutdown) {
                error!(error = %e, "IPC subscriber thread crashed");
            }
        })
        .context("failed to spawn IPC subscriber thread")?;

    let server_connected = check_server_connectivity();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_title("Ingot Pro"),
        ..Default::default()
    };

    eframe::run_native(
        "Ingot Pro",
        options,
        Box::new(|_cc| {
            Ok(Box::new(IngotApp::new(
                event_rx,
                Arc::clone(&shutdown),
                config.paper,
                server_connected,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("EFrame error: {e}"))?;

    // Signal subscriber thread to stop and join
    shutdown.store(true, Ordering::Relaxed);
    let _ = sub_thread.join();

    Ok(())
}

/// Check if the server is reachable via a heartbeat.
fn check_server_connectivity() -> bool {
    let Ok(node) = NodeBuilder::new().create::<ipc::Service>() else {
        return false;
    };
    let Ok(svc_name): Result<ServiceName, _> = SERVICE_COMMANDS.try_into() else {
        return false;
    };
    let Ok(service) = node
        .service_builder(&svc_name)
        .request_response::<IpcCommand, IpcCommandResponse>()
        .open_or_create()
    else {
        return false;
    };
    let Ok(client) = service.client_builder().create() else {
        return false;
    };
    send_command(&client, IpcCommand::Heartbeat).is_ok()
}

/// Run the IPC subscriber loop on a dedicated OS thread.
fn run_subscriber(
    tx: &std::sync::mpsc::Sender<IpcEvent>,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let ticker_name: ServiceName = SERVICE_TICKER
        .try_into()
        .context("invalid ticker service name")?;
    let nav_name: ServiceName = SERVICE_NAV.try_into().context("invalid nav service name")?;
    let orders_name: ServiceName = SERVICE_ORDERS
        .try_into()
        .context("invalid orders service name")?;

    let ticker_svc = node
        .service_builder(&ticker_name)
        .publish_subscribe::<IpcTickerUpdate>()
        .open_or_create()?;
    let nav_svc = node
        .service_builder(&nav_name)
        .publish_subscribe::<IpcNavUpdate>()
        .open_or_create()?;
    let orders_svc = node
        .service_builder(&orders_name)
        .publish_subscribe::<IpcOrderUpdate>()
        .open_or_create()?;

    let ticker_sub = ticker_svc.subscriber_builder().create()?;
    let nav_sub = nav_svc.subscriber_builder().create()?;
    let orders_sub = orders_svc.subscriber_builder().create()?;

    info!("IPC subscriber thread started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let mut received_any = false;

        // Drain ticker updates
        while let Ok(Some(sample)) = ticker_sub.receive() {
            if tx.send(IpcEvent::Ticker(*sample)).is_err() {
                return Ok(()); // GUI closed
            }
            received_any = true;
        }

        // Drain NAV updates
        while let Ok(Some(sample)) = nav_sub.receive() {
            if tx.send(IpcEvent::Nav(*sample)).is_err() {
                return Ok(());
            }
            received_any = true;
        }

        // Drain order updates
        while let Ok(Some(sample)) = orders_sub.receive() {
            if tx.send(IpcEvent::Order(*sample)).is_err() {
                return Ok(());
            }
            received_any = true;
        }

        if !received_any {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    Ok(())
}

struct IngotApp {
    event_rx: std::sync::mpsc::Receiver<IpcEvent>,
    shutdown: Arc<AtomicBool>,
    paper: bool,
    server_connected: bool,

    // Local snapshot state fed by IPC subscriber
    tickers: HashMap<Symbol, IpcTickerUpdate>,
    last_nav: Option<IpcNavUpdate>,

    // Subscription management
    subscribe_symbol: String,
    subscribe_status: Option<String>,

    // Order entry state
    order_side: OrderSide,
    order_symbol: String,
    order_quantity: String,
    order_error: Option<String>,
}

impl IngotApp {
    fn new(
        event_rx: std::sync::mpsc::Receiver<IpcEvent>,
        shutdown: Arc<AtomicBool>,
        paper: bool,
        server_connected: bool,
    ) -> Self {
        Self {
            event_rx,
            shutdown,
            paper,
            server_connected,
            tickers: HashMap::new(),
            last_nav: None,
            subscribe_symbol: String::new(),
            subscribe_status: None,
            order_side: OrderSide::Buy,
            order_symbol: String::from("BTC-USD"),
            order_quantity: String::new(),
            order_error: None,
        }
    }

    /// Drain all pending IPC events to update local snapshots.
    fn drain_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                IpcEvent::Ticker(t) => {
                    self.tickers.insert(t.symbol, t);
                }
                IpcEvent::Nav(n) => self.last_nav = Some(n),
                IpcEvent::Order(_o) => {
                    // Future: could display order status updates
                }
            }
        }
    }
}

impl eframe::App for IngotApp {
    #[allow(clippy::too_many_lines)]
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(100));
        self.drain_events();

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Ingot Trader Workstation");
                ui.separator();
                if self.paper {
                    ui.label(
                        egui::RichText::new("● PAPER")
                            .color(Color32::GREEN)
                            .strong(),
                    );
                } else {
                    ui.label(egui::RichText::new("● LIVE").color(Color32::RED).strong());
                }
                ui.separator();
                if self.server_connected {
                    ui.label(egui::RichText::new("IPC: Connected").color(Color32::GREEN));
                } else {
                    ui.label(egui::RichText::new("IPC: Disconnected").color(Color32::RED));
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Portfolio Overview");
            ui.add_space(8.0);

            // NAV display from IPC snapshot
            ui.label("Net Asset Value");
            if let Some(nav) = &self.last_nav {
                ui.heading(format!("$ {:.2} {}", nav.amount, nav.currency));
            } else {
                ui.label("Awaiting server data\u{2026}");
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            // -- Subscriptions ------------------------------------------------
            ui.heading("Subscriptions");
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label("Symbol");
                ui.text_edit_singleline(&mut self.subscribe_symbol);
                if ui.button("Subscribe").clicked() {
                    let sym = Symbol::new(self.subscribe_symbol.trim());
                    match send_order(IpcCommand::Subscribe(sym)) {
                        Ok(IpcCommandResponse::SubscribeAck {
                            symbol: s,
                            success: true,
                        }) => {
                            self.subscribe_status = Some(format!("Subscribed to {s}"));
                        }
                        Ok(IpcCommandResponse::SubscribeAck { success: false, .. }) => {
                            self.subscribe_status =
                                Some(format!("Subscription failed for {}", self.subscribe_symbol));
                        }
                        Ok(IpcCommandResponse::Error { message, .. }) => {
                            self.subscribe_status = Some(format!("Error: {}", message.as_str()));
                        }
                        Ok(_) => {
                            self.subscribe_status = Some("Unexpected response".to_owned());
                        }
                        Err(e) => {
                            self.subscribe_status = Some(format!("IPC error: {e}"));
                        }
                    }
                }
            });

            if let Some(status) = &self.subscribe_status {
                ui.label(egui::RichText::new(status.as_str()).italics());
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            // -- Market Data --------------------------------------------------
            ui.heading("Market Data");
            ui.add_space(4.0);

            if self.tickers.is_empty() {
                ui.label("No subscriptions \u{2014} use the panel above to subscribe.");
            } else {
                egui::Grid::new("ticker_grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for (symbol, ticker) in &self.tickers {
                            ui.label(symbol.to_string());
                            ui.label(format!("$ {}", ticker.price));
                            ui.end_row();
                        }
                    });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            ui.heading("Order Entry");
            ui.add_space(4.0);

            egui::ComboBox::from_label("Side")
                .selected_text(self.order_side.to_string())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.order_side, OrderSide::Buy, "Buy");
                    ui.selectable_value(&mut self.order_side, OrderSide::Sell, "Sell");
                });

            ui.horizontal(|ui| {
                ui.label("Symbol  ");
                ui.text_edit_singleline(&mut self.order_symbol);
            });

            ui.horizontal(|ui| {
                ui.label("Quantity");
                ui.text_edit_singleline(&mut self.order_quantity);
            });

            ui.add_space(4.0);

            if ui.button("Submit Order").clicked() {
                match self.order_quantity.trim().parse::<Decimal>() {
                    Ok(qty) if qty > Decimal::ZERO => {
                        let ipc_req = IpcOrderRequest {
                            symbol: Symbol::new(&self.order_symbol),
                            side: self.order_side,
                            order_type: OrderType::Market,
                            quantity: Quantity::from(qty),
                            has_price: false,
                            price: ingot_primitives::Price::from(Decimal::ZERO),
                            validate_only: false,
                        };
                        // Send order via IPC (fire-and-forget from GUI thread)
                        match send_order(IpcCommand::PlaceOrder(ipc_req)) {
                            Ok(IpcCommandResponse::OrderAck(ack)) => {
                                info!(
                                    exchange_id = %ack.exchange_id,
                                    status = %ack.status,
                                    "Order acknowledged"
                                );
                                self.order_error = None;
                            }
                            Ok(IpcCommandResponse::Error { message, .. }) => {
                                self.order_error =
                                    Some(format!("Server error: {}", message.as_str()));
                            }
                            Ok(_) => {
                                self.order_error = Some("Unexpected response".to_owned());
                            }
                            Err(e) => {
                                self.order_error = Some(format!("IPC error: {e}"));
                            }
                        }
                    }
                    Ok(_) => {
                        self.order_error = Some("Quantity must be positive".to_owned());
                    }
                    Err(_) => {
                        self.order_error = Some(
                            "Invalid quantity \u{2014} enter a number (e.g. 0.001)".to_owned(),
                        );
                    }
                }
            }

            if let Some(err) = &self.order_error {
                ui.label(egui::RichText::new(err.as_str()).color(Color32::RED));
            }

            ui.add_space(8.0);
            ui.separator();

            if ui.button("Emergency Shutdown").clicked() {
                match send_order(IpcCommand::Shutdown) {
                    Ok(IpcCommandResponse::ShutdownAck { success: true }) => {
                        info!("Server acknowledged shutdown");
                    }
                    Ok(resp) => {
                        warn!(?resp, "Unexpected shutdown response");
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to send shutdown command");
                    }
                }
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Send an IPC command and wait for a response (up to 5 seconds).
///
/// Creates a fresh IPC client for each command to avoid lifetime issues
/// with egui's render loop. The overhead is acceptable for user-initiated
/// commands (not hot-path).
fn send_order(cmd: IpcCommand) -> Result<IpcCommandResponse> {
    let node = NodeBuilder::new()
        .create::<ipc::Service>()
        .context("failed to create iceoryx2 node")?;

    let svc_name: ServiceName = SERVICE_COMMANDS
        .try_into()
        .context("invalid command service name")?;

    let service = node
        .service_builder(&svc_name)
        .request_response::<IpcCommand, IpcCommandResponse>()
        .open_or_create()
        .context("failed to open command service")?;

    let client = service
        .client_builder()
        .create()
        .context("failed to create IPC client")?;

    send_command(&client, cmd)
}

/// Send a command via an existing IPC client and poll for a response.
fn send_command(
    client: &iceoryx2::port::client::Client<ipc::Service, IpcCommand, (), IpcCommandResponse, ()>,
    cmd: IpcCommand,
) -> Result<IpcCommandResponse> {
    let pending = client
        .send_copy(cmd)
        .map_err(|e| anyhow::anyhow!("failed to send IPC command: {e:?}"))?;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match pending.receive() {
            Ok(Some(response)) => return Ok(*response),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    anyhow::bail!("IPC command timed out (no response within 5s)");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => anyhow::bail!("IPC receive error: {e:?}"),
        }
    }
}
