pub mod handle;
pub mod strategy;

use std::sync::Arc;

use anyhow::Result;

/// Capacity of the market-data channel between the exchange adapter and the
/// engine. At Kraken WebSocket rates (~1–5 ticks/s per pair) this provides
/// several seconds of burst buffering; back-pressure to the producer kicks in
/// if the engine falls behind.
pub const MARKET_DATA_CHANNEL_CAPACITY: usize = 64;

/// Capacity of the command channel between the UI layer and the engine.
/// Commands are human-paced (key presses, CLI input) — 32 is intentionally
/// generous to ensure the REPL never blocks waiting for the engine.
pub const COMMAND_CHANNEL_CAPACITY: usize = 32;

/// Capacity of the broadcast channel for engine events (ticks, fills) sent
/// to WebSocket clients. Slow consumers that fall behind this many messages
/// will receive a `Lagged` error and skip to the latest.
pub const EVENT_BROADCAST_CAPACITY: usize = 256;
use ingot_connectivity::Exchange;
use ingot_core::{
    accounting::{Account, AccountSide, AccountType, Ledger, QuoteBoard, Transaction},
    api::{TickerUpdate, WsMessage},
    execution::{OrderAcknowledgment, OrderRequest},
    feed::Ticker,
};
use ingot_primitives::{Amount, Currency, Money, OrderStatus, Price, Symbol};
use ingot_storage::event::StorageEvent;
use rust_decimal::{Decimal, dec};
use smallvec::SmallVec;
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tracing::{error, info, instrument, warn};

use crate::strategy::{Strategy, StrategyContext};

/// Commands sent from the UI layer or IPC clients to the engine.
///
/// `PlaceOrder` carries an optional `oneshot::Sender` so that callers
/// who need synchronous acknowledgment (e.g. the IPC command thread)
/// can receive the `OrderAcknowledgment` directly. Existing callers
/// that don't need the ack pass `None`.
pub enum EngineCommand {
    PlaceOrder(OrderRequest, Option<oneshot::Sender<OrderAcknowledgment>>),
    Subscribe(Symbol, Option<oneshot::Sender<Result<()>>>),
    Heartbeat,
    Shutdown,
}

impl std::fmt::Debug for EngineCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PlaceOrder(order, _) => f.debug_tuple("PlaceOrder").field(order).finish(),
            Self::Subscribe(symbol, _) => f.debug_tuple("Subscribe").field(symbol).finish(),
            Self::Heartbeat => write!(f, "Heartbeat"),
            Self::Shutdown => write!(f, "Shutdown"),
        }
    }
}

pub struct TradingEngine<E: Exchange> {
    ledger: Arc<RwLock<Ledger>>,
    market_data: Arc<RwLock<QuoteBoard>>,
    exchange: E,
    strategies: Vec<Box<dyn Strategy>>,
    event_tx: Option<broadcast::Sender<WsMessage>>,
    storage_tx: Option<mpsc::Sender<StorageEvent>>,
}

impl<E: Exchange> TradingEngine<E> {
    pub fn new(
        ledger: Arc<RwLock<Ledger>>,
        market_data: Arc<RwLock<QuoteBoard>>,
        exchange: E,
        strategies: Vec<Box<dyn Strategy>>,
    ) -> Self {
        Self {
            ledger,
            market_data,
            exchange,
            strategies,
            event_tx: None,
            storage_tx: None,
        }
    }

    /// Attach a broadcast channel for engine events (ticks, fills).
    ///
    /// Returns the `broadcast::Sender` which can be passed to
    /// [`EngineHandle`](crate::handle::EngineHandle) for WS forwarding.
    pub fn with_broadcast(&mut self) -> broadcast::Sender<WsMessage> {
        let (tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        self.event_tx = Some(tx.clone());
        tx
    }

    /// Attach a storage event channel for persistence.
    pub fn with_storage(&mut self, storage_tx: mpsc::Sender<StorageEvent>) {
        self.storage_tx = Some(storage_tx);
    }

    /// The main event loop.
    pub async fn run(
        &mut self,
        mut market_data_rx: mpsc::Receiver<Ticker>,
        mut command_rx: mpsc::Receiver<EngineCommand>,
    ) -> Result<()> {
        info!("Engine Online: Waiting for events");

        loop {
            tokio::select! {
                Some(tick) = market_data_rx.recv() => {
                    self.handle_tick(tick).await;
                }

                Some(command) = command_rx.recv() => {
                    match command {
                        EngineCommand::PlaceOrder(order, ack_tx) => {
                            self.handle_order_request(&order, ack_tx).await;
                        }
                        EngineCommand::Subscribe(symbol, ack_tx) => {
                            self.handle_subscribe(symbol, ack_tx).await;
                        }
                        EngineCommand::Heartbeat => {
                            info!("System Heartbeat: Lub-dub. (Engine is responsive)");
                        }
                        EngineCommand::Shutdown => {
                            warn!("Shutdown command received. Stopping engine.");
                            break;
                        }
                    }
                }

                else => {
                    warn!("All event channels closed. Exiting loop.");
                    break;
                }
            }
        }

        Ok(())
    }

    #[instrument(name = "subscribe", skip(self, ack_tx), fields(symbol = %symbol))]
    async fn handle_subscribe(
        &mut self,
        symbol: Symbol,
        ack_tx: Option<oneshot::Sender<Result<()>>>,
    ) {
        self.ensure_accounts_for_symbol(symbol).await;
        let result = self.exchange.subscribe_ticker(symbol).await;
        match &result {
            Ok(()) => info!("subscribed to ticker"),
            Err(e) => error!(error = %e, "subscription failed"),
        }
        if let Some(tx) = ack_tx {
            let _ = tx.send(result);
        }
    }

    /// Ensures the ledger has asset + equity accounts for both currencies
    /// in the given symbol pair. Creates them if they don't exist.
    #[instrument(name = "ensure_accounts", skip(self), fields(symbol = %symbol))]
    async fn ensure_accounts_for_symbol(&self, symbol: Symbol) {
        let Some((base_code, quote_code)) = symbol.parts() else {
            warn!(symbol = %symbol, "cannot provision accounts: invalid symbol format");
            return;
        };

        let base = Currency::from_code(base_code);
        let quote = Currency::from_code(quote_code);

        let mut created: SmallVec<Account, 4> = SmallVec::new();
        {
            let mut ledger = self.ledger.write().await;
            for (currency, label) in [(base, base_code.as_str()), (quote, quote_code.as_str())] {
                if !ledger.has_account(AccountType::Asset, currency) {
                    let account =
                        Account::new(format!("{label} (Kraken)"), AccountType::Asset, currency);
                    ledger.add_account(account.clone());
                    created.push(account);
                    info!(currency = label, "provisioned asset account");
                }
                if !ledger.has_account(AccountType::Equity, currency) {
                    let account =
                        Account::new(format!("{label} Capital"), AccountType::Equity, currency);
                    ledger.add_account(account.clone());
                    created.push(account);
                    info!(currency = label, "provisioned equity account");
                }
            }
        } // write lock released

        if let Some(ref storage_tx) = self.storage_tx {
            for account in created {
                let _ = storage_tx.try_send(StorageEvent::AccountCreated { account });
            }
        }
    }

    #[instrument(name = "ingest", level = "trace", skip(self, tick), fields(symbol = %tick.symbol, price = %tick.price))]
    async fn handle_tick(&mut self, tick: Ticker) {
        let mut orders: SmallVec<OrderRequest, 2> = SmallVec::new();
        {
            let mut board = self.market_data.write().await;
            let mut ctx = StrategyContext {
                market_data: &mut board,
                orders: &mut orders,
            };
            for strategy in &mut self.strategies {
                strategy.on_tick(&tick, &mut ctx);
            }
        } // write lock released

        // Broadcast tick event to WS subscribers (if any)
        if let Some(tx) = &self.event_tx {
            let update = WsMessage::Tick(TickerUpdate {
                symbol: tick.symbol,
                price: Price::from(tick.price),
            });
            // send() fails only when there are zero receivers — not an error
            let _ = tx.send(update);
        }

        // Best-effort storage — drop ticks if channel is full
        if let Some(ref storage_tx) = self.storage_tx {
            let _ = storage_tx.try_send(StorageEvent::Tick { ticker: tick });
        }

        for order in orders {
            self.handle_order_request(&order, None).await;
        }
    }

    #[instrument(name = "execution", skip(self, ack_tx), fields(
        side = %order.side,
        symbol = %order.symbol,
        quantity = %order.quantity,
        price = ?order.price
    ))]
    async fn handle_order_request(
        &self,
        order: &OrderRequest,
        ack_tx: Option<oneshot::Sender<OrderAcknowledgment>>,
    ) {
        info!("Placing order");

        match self.exchange.place_order(order).await {
            Ok(ack) => {
                info!(exchange_id = %ack.exchange_id, "Order Acknowledged");
                if ack.status == OrderStatus::Filled {
                    self.record_fill(order).await;
                }
                // Best-effort storage of order placement
                if let Some(ref storage_tx) = self.storage_tx {
                    let _ = storage_tx.try_send(StorageEvent::OrderPlaced {
                        request: *order,
                        acknowledgment: ack.clone(),
                    });
                }
                // Send ack to the requester if they provided a oneshot channel
                if let Some(tx) = ack_tx {
                    // Receiver may have been dropped — not an error
                    let _ = tx.send(ack.clone());
                }
                // Broadcast order update to WS subscribers
                if let Some(tx) = &self.event_tx {
                    let _ = tx.send(WsMessage::OrderUpdate(ack));
                }
            }
            Err(e) => {
                error!(error = %e, "Order Placement Failed");
            }
        }
    }

    #[instrument(name = "record_fill", level = "debug", skip(self, order), fields(
        side = %order.side,
        symbol = %order.symbol,
        qty = %order.quantity
    ))]
    async fn record_fill(&self, order: &OrderRequest) {
        let Some((base_code, quote_code)) = order.symbol.parts() else {
            warn!(symbol = %order.symbol, "fill not recorded: invalid symbol format");
            return;
        };

        let base = Currency::from_code(base_code);
        let quote = Currency::from_code(quote_code);

        let fill_price = if let Some(p) = order.price {
            p
        } else {
            let one_base = Money::new(Amount::from(dec!(1)), base);
            let result = {
                let board = self.market_data.read().await;
                board.convert(&one_base, &quote)
            };
            match result {
                Ok(m) => Price::from(Decimal::from(m.amount)),
                Err(e) => {
                    error!(symbol = %order.symbol, error = %e, "no market price for fill; ledger not updated");
                    return;
                }
            }
        };

        let base_qty = Amount::from(order.quantity);
        let fill_result = {
            let mut ledger = self.ledger.write().await;
            ledger.post_order_fill(order.side, base, quote, base_qty, fill_price)
        }; // write lock released

        match fill_result {
            Ok(transaction) => {
                if let Some(ref storage_tx) = self.storage_tx
                    && let Err(e) = storage_tx
                        .send(StorageEvent::TransactionPosted { transaction })
                        .await
                {
                    error!(error = %e, "storage channel closed");
                }
            }
            Err(e) => {
                error!(error = %e, "failed to record fill in ledger");
            }
        }
    }
}

#[instrument]
pub async fn bootstrap_ledger() -> Result<Arc<RwLock<Ledger>>> {
    let ledger = Ledger::new();
    let ledger = Arc::new(RwLock::new(ledger));

    {
        let mut w = ledger.write().await;

        let usd = Currency::usd();
        let btc = Currency::btc();

        let cash_acc = Account::new("Cash (Kraken)".into(), AccountType::Asset, usd);
        let crypto_acc = Account::new("BTC (Kraken)".into(), AccountType::Asset, btc);
        let equity_acc = Account::new("Capital".into(), AccountType::Equity, Currency::usd());
        let btc_equity_acc = Account::new("BTC Capital".into(), AccountType::Equity, btc);

        w.add_account(cash_acc.clone());
        w.add_account(crypto_acc.clone());
        w.add_account(equity_acc.clone());
        w.add_account(btc_equity_acc);

        let mut tx = Transaction::new("Seed Funding".into());
        tx.add_entry(&cash_acc, Amount::from(dec!(10000)), AccountSide::Debit)?;
        tx.add_entry(&equity_acc, Amount::from(dec!(10000)), AccountSide::Credit)?;
        let vtx = w.prepare_transaction(tx)?;
        w.post_transaction(vtx);
    }

    info!(nav = 10000, "Ledger Bootstrapped");

    Ok(ledger)
}
