use std::collections::HashMap;

use ingot_accounting::post_fill;
use ingot_connectivity::OrderExecutor;
use ingot_core::{OrderBookSnapshot, OrderFill, OrderRequest, Position, TickerSnapshot};
use ingot_primitives::{Currency, OrderType, Symbol, TimeInForce};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinHandle,
};
use tracing::{debug, error, info, warn};

use crate::{
    config::{EngineConfig, ScheduleConfig},
    controller::PortfolioController,
    error::EngineError,
    kill_switch::KillSwitch,
    order_manager::OrderManager,
    strategy::{Strategy, StrategyContext, StrategyKind},
    traits::LedgerWriter,
    types::{OrderIntention, RiskDecision, StrategyId},
};

/// Central event-driven engine orchestrator.
///
/// Consumes market data channels, dispatches events to strategies,
/// gates intentions through risk checks, submits orders, and posts
/// fills to the ledger.
pub struct Engine<E, L> {
    strategies: Vec<StrategyKind>,
    controller: PortfolioController,
    order_manager: OrderManager,
    executor: E,
    ledger_writer: L,
    config: EngineConfig,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    latest_tickers: HashMap<Symbol, TickerSnapshot>,
    latest_order_books: HashMap<Symbol, OrderBookSnapshot>,
    symbol_currencies: HashMap<Symbol, (Currency, Currency)>,
    schedules: Vec<ScheduleConfig>,
    kill_switch: KillSwitch,
}

impl<E, L> Engine<E, L>
where
    E: OrderExecutor + Send + Sync,
    L: LedgerWriter + Send + Sync,
{
    pub fn new(executor: E, ledger_writer: L, config: EngineConfig) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let controller = PortfolioController::new(config.risk.clone());
        let order_manager = OrderManager::new(config.smart_order.clone());

        Self {
            strategies: Vec::new(),
            controller,
            order_manager,
            executor,
            ledger_writer,
            config,
            shutdown_tx,
            shutdown_rx,
            latest_tickers: HashMap::new(),
            latest_order_books: HashMap::new(),
            symbol_currencies: HashMap::new(),
            schedules: Vec::new(),
            kill_switch: KillSwitch::new(),
        }
    }

    /// Register a strategy. Rejects duplicate `StrategyId`s.
    pub fn register_strategy(&mut self, strategy: StrategyKind) -> Result<(), EngineError> {
        let id = strategy.id().clone();
        if self.strategies.iter().any(|s| s.id() == &id) {
            return Err(EngineError::DuplicateStrategyId(id));
        }
        self.strategies.push(strategy);
        Ok(())
    }

    /// Register a symbol's base/quote currencies for `post_fill` accounting.
    pub fn register_symbol(&mut self, symbol: Symbol, base: Currency, quote: Currency) {
        self.symbol_currencies.insert(symbol, (base, quote));
    }

    /// Returns a clone of the shutdown sender for external shutdown signaling.
    pub fn shutdown_handle(&self) -> watch::Sender<bool> {
        self.shutdown_tx.clone()
    }

    /// Returns a reference to the kill switch.
    pub fn kill_switch(&self) -> &KillSwitch {
        &self.kill_switch
    }

    /// Returns a clonable handle to the kill switch for external activation.
    pub fn kill_switch_handle(&self) -> KillSwitch {
        self.kill_switch.clone()
    }

    /// Register a schedule for an existing strategy. The strategy must
    /// already be registered. Rejects duplicate schedules for the same
    /// strategy.
    pub fn register_schedule(&mut self, config: ScheduleConfig) -> Result<(), EngineError> {
        if !self
            .strategies
            .iter()
            .any(|s| *s.id() == config.strategy_id)
        {
            return Err(EngineError::StrategyNotFound(config.strategy_id));
        }
        if self
            .schedules
            .iter()
            .any(|s| s.strategy_id == config.strategy_id)
        {
            return Err(EngineError::DuplicateStrategyId(config.strategy_id));
        }
        self.schedules.push(config);
        Ok(())
    }

    /// Run the engine event loop until shutdown is signaled.
    pub async fn run(
        &mut self,
        mut ticker_rx: broadcast::Receiver<TickerSnapshot>,
        mut book_rx: broadcast::Receiver<OrderBookSnapshot>,
        mut fill_rx: mpsc::Receiver<OrderFill>,
    ) -> Result<(), EngineError> {
        // Init all strategies
        for strategy in &mut self.strategies {
            let ctx = build_context(
                &self.controller,
                &self.latest_tickers,
                &self.latest_order_books,
            );
            let intentions = strategy.init(&ctx);
            for intention in intentions {
                process_intention(
                    intention,
                    &mut self.controller,
                    &mut self.order_manager,
                    &self.executor,
                    &self.latest_tickers,
                    &self.latest_order_books,
                )
                .await?;
            }
        }

        // Spawn schedule timer tasks
        let (schedule_tx, mut schedule_rx) = mpsc::channel::<StrategyId>(32);
        let mut timer_handles: Vec<JoinHandle<()>> = Vec::new();
        for schedule in &self.schedules {
            let tx = schedule_tx.clone();
            let shutdown = self.shutdown_rx.clone();
            let strategy_id = schedule.strategy_id.clone();
            let interval = schedule.interval_duration();
            timer_handles.push(tokio::spawn(schedule_timer_task(
                strategy_id,
                interval,
                tx,
                shutdown,
            )));
        }
        // Drop the original sender so the channel closes when all tasks end
        drop(schedule_tx);

        let mut kill_switch_rx = self.kill_switch.subscribe();
        let mut killed = false;

        loop {
            tokio::select! {
                result = ticker_rx.recv() => {
                    if let Ok(ticker) = result {
                        self.handle_ticker(ticker).await?;
                    }
                }
                result = book_rx.recv() => {
                    if let Ok(book) = result {
                        self.handle_order_book(book).await?;
                    }
                }
                fill = fill_rx.recv() => {
                    if let Some(fill) = fill {
                        self.handle_fill(fill).await?;
                    }
                }
                Some(strategy_id) = schedule_rx.recv() => {
                    self.handle_schedule(strategy_id).await?;
                }
                _ = kill_switch_rx.changed() => {
                    if *kill_switch_rx.borrow() {
                        warn!("kill switch activated, executing emergency sequence");
                        self.execute_kill_switch_sequence().await;
                        killed = true;
                        break;
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    warn!("Ctrl+C received, activating kill switch");
                    self.kill_switch.activate();
                }
                _ = self.shutdown_rx.changed() => {
                    info!("shutdown signal received, exiting engine loop");
                    break;
                }
            }
        }

        // Abort timer tasks
        for handle in &timer_handles {
            handle.abort();
        }

        // Shutdown strategies (unless kill switch already did it)
        if !killed {
            for strategy in &mut self.strategies {
                strategy.shutdown();
            }
        }

        if killed {
            Err(EngineError::KillSwitchActivated)
        } else {
            Ok(())
        }
    }

    async fn handle_ticker(&mut self, ticker: TickerSnapshot) -> Result<(), EngineError> {
        self.latest_tickers
            .insert(ticker.symbol.clone(), ticker.clone());

        let ctx = build_context(
            &self.controller,
            &self.latest_tickers,
            &self.latest_order_books,
        );

        let mut all_intentions = Vec::new();
        for strategy in &mut self.strategies {
            let intentions = strategy.on_ticker(&ticker, &ctx);
            all_intentions.extend(intentions);
        }

        for intention in all_intentions {
            process_intention(
                intention,
                &mut self.controller,
                &mut self.order_manager,
                &self.executor,
                &self.latest_tickers,
                &self.latest_order_books,
            )
            .await?;
        }
        Ok(())
    }

    async fn handle_order_book(&mut self, book: OrderBookSnapshot) -> Result<(), EngineError> {
        self.latest_order_books
            .insert(book.symbol.clone(), book.clone());

        let ctx = build_context(
            &self.controller,
            &self.latest_tickers,
            &self.latest_order_books,
        );

        let mut all_intentions = Vec::new();
        for strategy in &mut self.strategies {
            let intentions = strategy.on_order_book(&book, &ctx);
            all_intentions.extend(intentions);
        }

        for intention in all_intentions {
            process_intention(
                intention,
                &mut self.controller,
                &mut self.order_manager,
                &self.executor,
                &self.latest_tickers,
                &self.latest_order_books,
            )
            .await?;
        }
        Ok(())
    }

    async fn handle_schedule(&mut self, strategy_id: StrategyId) -> Result<(), EngineError> {
        let ctx = build_context(
            &self.controller,
            &self.latest_tickers,
            &self.latest_order_books,
        );

        let mut all_intentions = Vec::new();
        for strategy in &mut self.strategies {
            if *strategy.id() == strategy_id {
                let intentions = strategy.on_schedule(&ctx);
                all_intentions.extend(intentions);
            }
        }

        for intention in all_intentions {
            process_intention(
                intention,
                &mut self.controller,
                &mut self.order_manager,
                &self.executor,
                &self.latest_tickers,
                &self.latest_order_books,
            )
            .await?;
        }
        Ok(())
    }

    /// Execute the emergency kill switch sequence (best-effort):
    /// 1. Halt controller (prevent new orders)
    /// 2. Shutdown all strategies
    /// 3. Cancel all open orders
    /// 4. Close all positions with opposite-side market orders
    async fn execute_kill_switch_sequence(&mut self) {
        // 1. Halt controller
        self.controller.halt();
        info!("kill switch: controller halted");

        // 2. Shutdown all strategies
        for strategy in &mut self.strategies {
            strategy.shutdown();
        }
        info!("kill switch: strategies shut down");

        // 3. Cancel all open orders
        if let Err(e) = self.order_manager.cancel_all(&self.executor).await {
            error!("kill switch: failed to cancel orders: {e}");
        } else {
            info!("kill switch: open orders cancelled");
        }

        // 4. Close all positions with opposite-side market orders
        let positions: Vec<Position> = self.controller.positions().values().cloned().collect();
        for position in &positions {
            let close_order = OrderRequest {
                symbol: position.symbol.clone(),
                side: position.side.opposite(),
                order_type: OrderType::Market,
                quantity: position.quantity,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::ImmediateOrCancel,
            };
            if let Err(e) = self.executor.place_order(&close_order).await {
                error!(
                    "kill switch: failed to close position {}: {e}",
                    position.symbol
                );
            }
        }
        if !positions.is_empty() {
            info!(
                "kill switch: submitted close orders for {} position(s)",
                positions.len()
            );
        }
    }

    async fn handle_fill(&mut self, fill: OrderFill) -> Result<(), EngineError> {
        // 1. Update order manager
        self.order_manager.on_fill(&fill);

        // 2. Update controller positions
        self.controller.on_fill(&fill);

        // 3. Post to accounting if symbol is registered
        if let Some((base, quote)) = self.symbol_currencies.get(&fill.symbol) {
            let txn = post_fill(
                &fill,
                self.config.exchange,
                &self.config.venue,
                base,
                quote,
                false,
            )?;
            self.ledger_writer
                .write_transaction(&txn)
                .await
                .map_err(EngineError::Connectivity)?;
        } else {
            warn!(
                symbol = %fill.symbol,
                "fill for unregistered symbol, skipping accounting"
            );
        }

        // 4. Dispatch to strategies
        let ctx = build_context(
            &self.controller,
            &self.latest_tickers,
            &self.latest_order_books,
        );

        let mut all_intentions = Vec::new();
        for strategy in &mut self.strategies {
            let intentions = strategy.on_fill(&fill, &ctx);
            all_intentions.extend(intentions);
        }

        for intention in all_intentions {
            process_intention(
                intention,
                &mut self.controller,
                &mut self.order_manager,
                &self.executor,
                &self.latest_tickers,
                &self.latest_order_books,
            )
            .await?;
        }
        Ok(())
    }
}

fn build_context(
    controller: &PortfolioController,
    latest_tickers: &HashMap<Symbol, TickerSnapshot>,
    latest_order_books: &HashMap<Symbol, OrderBookSnapshot>,
) -> StrategyContext {
    StrategyContext {
        positions: controller.positions().values().cloned().collect(),
        balances: vec![],
        latest_tickers: latest_tickers.clone(),
        latest_order_books: latest_order_books.clone(),
        timestamp: chrono::Utc::now(),
    }
}

async fn process_intention<E: OrderExecutor>(
    intention: OrderIntention,
    controller: &mut PortfolioController,
    order_manager: &mut OrderManager,
    executor: &E,
    latest_tickers: &HashMap<Symbol, TickerSnapshot>,
    latest_order_books: &HashMap<Symbol, OrderBookSnapshot>,
) -> Result<(), EngineError> {
    let decision = controller.check_intention(&intention, latest_tickers);

    match decision {
        RiskDecision::Approved => {
            let book = latest_order_books.get(&intention.request.symbol);
            order_manager
                .submit_order(intention, book, executor)
                .await?;
        }
        RiskDecision::Rejected { reason } => {
            debug!(%reason, "intention rejected by risk controller");
        }
    }
    Ok(())
}

async fn schedule_timer_task(
    strategy_id: StrategyId,
    interval: std::time::Duration,
    tx: mpsc::Sender<StrategyId>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut timer = tokio::time::interval(interval);
    timer.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            _ = timer.tick() => {
                if tx.send(strategy_id.clone()).await.is_err() {
                    break;
                }
            }
            _ = shutdown_rx.changed() => {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::Utc;
    use ingot_accounting::Transaction;
    use ingot_connectivity::OrderExecutor;
    use ingot_core::{
        OpenOrder, OrderBookLevel, OrderBookSnapshot, OrderFill, OrderId, OrderRequest,
        OrderStatus, TickerSnapshot,
    };
    use ingot_primitives::{
        Amount, Currency, Exchange, OrderSide, OrderType, Percentage, Price, Quantity, Symbol,
        TimeInForce,
    };
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;
    use tokio::sync::{broadcast, mpsc};

    use super::*;
    use crate::{
        config::{RiskConfig, ScheduleConfig, SmartOrderConfig},
        strategy::{MockStrategy, MockStrategyState, NoopStrategy},
        traits::LedgerWriter,
        types::StrategyId,
    };

    // ── Mock Executor ──────────────────────────────────────────────────

    #[derive(Debug, Clone)]
    struct MockOrderExecutor {
        placed: Arc<Mutex<Vec<OrderRequest>>>,
        cancel_all_count: Arc<Mutex<u32>>,
        next_order_id: String,
    }

    impl MockOrderExecutor {
        fn new(next_id: &str) -> Self {
            Self {
                placed: Arc::new(Mutex::new(Vec::new())),
                cancel_all_count: Arc::new(Mutex::new(0)),
                next_order_id: next_id.to_owned(),
            }
        }

        fn placed_orders(&self) -> Result<Vec<OrderRequest>, anyhow::Error> {
            let guard = self
                .placed
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            Ok(guard.clone())
        }

        #[allow(dead_code)] // Available for future tests
        fn cancel_all_call_count(&self) -> Result<u32, anyhow::Error> {
            let guard = self
                .cancel_all_count
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            Ok(*guard)
        }
    }

    impl OrderExecutor for MockOrderExecutor {
        fn place_order(
            &self,
            request: &OrderRequest,
        ) -> impl Future<Output = anyhow::Result<OrderId>> + Send {
            let placed = Arc::clone(&self.placed);
            let request = request.clone();
            let id = self.next_order_id.clone();
            async move {
                let mut guard = placed
                    .lock()
                    .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
                guard.push(request);
                OrderId::new(&id).map_err(|e| anyhow::anyhow!("{e}"))
            }
        }

        async fn cancel_order(&self, _order_id: &OrderId) -> anyhow::Result<()> {
            Ok(())
        }

        async fn cancel_all_orders(&self) -> anyhow::Result<u32> {
            let mut guard = self
                .cancel_all_count
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            *guard += 1;
            Ok(0)
        }

        fn get_order_status(
            &self,
            order_id: &OrderId,
        ) -> impl Future<Output = anyhow::Result<OpenOrder>> + Send {
            let id = order_id.clone();
            async move {
                Ok(OpenOrder {
                    order_id: id,
                    request: OrderRequest {
                        symbol: Symbol::new("XXBTZUSD").map_err(|e| anyhow::anyhow!("{e}"))?,
                        side: OrderSide::Buy,
                        order_type: OrderType::Market,
                        quantity: Quantity::new(dec!(0.1)).map_err(|e| anyhow::anyhow!("{e}"))?,
                        limit_price: None,
                        stop_price: None,
                        time_in_force: TimeInForce::GoodTilCancelled,
                    },
                    status: OrderStatus::Open,
                    filled_quantity: Quantity::zero(),
                    remaining_quantity: Quantity::new(dec!(0.1))
                        .map_err(|e| anyhow::anyhow!("{e}"))?,
                    average_fill_price: None,
                    created_at: Utc::now(),
                })
            }
        }

        async fn get_open_orders(&self) -> anyhow::Result<Vec<OpenOrder>> {
            Ok(vec![])
        }
    }

    // ── Mock Ledger Writer ─────────────────────────────────────────────

    #[derive(Debug, Clone)]
    struct MockLedgerWriter {
        transactions: Arc<Mutex<Vec<Transaction>>>,
    }

    impl MockLedgerWriter {
        fn new() -> Self {
            Self {
                transactions: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn transaction_count(&self) -> Result<usize, anyhow::Error> {
            let guard = self
                .transactions
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            Ok(guard.len())
        }
    }

    impl LedgerWriter for MockLedgerWriter {
        async fn write_transaction(&self, txn: &Transaction) -> anyhow::Result<()> {
            let mut guard = self
                .transactions
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            guard.push(txn.clone());
            Ok(())
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    fn sample_risk_config() -> Result<RiskConfig, Box<dyn std::error::Error>> {
        Ok(RiskConfig {
            global_stop_loss: Amount::new(dec!(10_000)),
            max_currency_exposure: Percentage::new(dec!(0.40))?,
            max_asset_exposure: Percentage::new(dec!(0.20))?,
            max_order_value: Amount::new(dec!(50_000)),
            margin: None,
        })
    }

    fn sample_engine_config() -> Result<EngineConfig, Box<dyn std::error::Error>> {
        Ok(EngineConfig {
            risk: sample_risk_config()?,
            base_currency: Currency::USD,
            smart_order: SmartOrderConfig {
                use_mid_price: true,
                offset_bps: dec!(0),
                fallback_timeout_ms: 30_000,
            },
            exchange: Exchange::Paper,
            venue: SmolStr::new("spot"),
        })
    }

    fn make_ticker(symbol: &Symbol) -> TickerSnapshot {
        TickerSnapshot {
            symbol: symbol.clone(),
            bid: Price::new(dec!(67_000)),
            ask: Price::new(dec!(67_010)),
            last: Price::new(dec!(67_005)),
            volume_24h: Quantity::zero(),
            timestamp: Utc::now(),
        }
    }

    fn make_fill(order_id: &str, symbol: &Symbol) -> Result<OrderFill, Box<dyn std::error::Error>> {
        Ok(OrderFill {
            order_id: OrderId::new(order_id)?,
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67_005)),
            fill_quantity: Quantity::new(dec!(0.01))?,
            fee: Amount::new(dec!(0.26)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        })
    }

    fn make_small_buy_intention() -> Result<OrderIntention, Box<dyn std::error::Error>> {
        Ok(OrderIntention {
            strategy_id: StrategyId::new("mock")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: Quantity::new(dec!(0.01))?,
                limit_price: Some(Price::new(dec!(67_005))),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: None,
        })
    }

    // ── Test 1: Engine construction ────────────────────────────────────

    #[test]
    fn test_engine_new() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let engine = Engine::new(executor, ledger, config);

        assert!(engine.strategies.is_empty());
        assert!(engine.latest_tickers.is_empty());
        assert!(engine.latest_order_books.is_empty());
        assert!(engine.symbol_currencies.is_empty());
        Ok(())
    }

    // ── Test 2: Register strategy ──────────────────────────────────────

    #[test]
    fn test_register_strategy() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let strategy = StrategyKind::Noop(NoopStrategy::new(StrategyId::new("noop-1")?));
        engine.register_strategy(strategy)?;
        assert_eq!(engine.strategies.len(), 1);
        Ok(())
    }

    // ── Test 3: Duplicate strategy rejected ────────────────────────────

    #[test]
    fn test_register_duplicate_strategy_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let id = StrategyId::new("dupe")?;
        let s1 = StrategyKind::Noop(NoopStrategy::new(id.clone()));
        let s2 = StrategyKind::Noop(NoopStrategy::new(id));
        engine.register_strategy(s1)?;

        let result = engine.register_strategy(s2);
        assert!(matches!(result, Err(EngineError::DuplicateStrategyId(_))));
        Ok(())
    }

    // ── Kill switch accessor ──────────────────────────────────────────

    #[test]
    fn test_engine_kill_switch_accessor() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let engine = Engine::new(executor, ledger, config);

        assert!(!engine.kill_switch().is_activated());
        Ok(())
    }

    // ── Test 4: Engine processes ticker ─────────────────────────────────

    #[tokio::test]
    async fn test_engine_processes_ticker() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let (ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();
        let symbol = Symbol::new("XXBTZUSD")?;

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        ticker_tx.send(make_ticker(&symbol))?;
        // Small delay to let the event loop process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = shutdown.send(true);

        handle.await??;

        let s = state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert_eq!(s.on_ticker_count, 1);
        Ok(())
    }

    // ── Test 5: Engine processes order book ──────────────────────────────

    #[tokio::test]
    async fn test_engine_processes_order_book() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();
        let symbol = Symbol::new("XXBTZUSD")?;

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        let book = OrderBookSnapshot {
            symbol,
            bids: vec![OrderBookLevel {
                price: Price::new(dec!(67_000)),
                quantity: Quantity::new(dec!(1))?,
            }],
            asks: vec![OrderBookLevel {
                price: Price::new(dec!(67_010)),
                quantity: Quantity::new(dec!(1))?,
            }],
            timestamp: Utc::now(),
        };
        book_tx.send(book)?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = shutdown.send(true);

        handle.await??;

        let s = state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert_eq!(s.on_order_book_count, 1);
        Ok(())
    }

    // ── Test 6: Engine processes fill ───────────────────────────────────

    #[tokio::test]
    async fn test_engine_processes_fill() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger.clone(), config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let symbol = Symbol::new("XXBTZUSD")?;
        engine.register_symbol(symbol.clone(), Currency::BTC, Currency::USD);
        engine.controller.on_nav_update(Amount::new(dec!(100_000)));

        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        let fill = make_fill("ORD-001", &symbol)?;
        fill_tx.send(fill).await?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = shutdown.send(true);

        handle.await??;

        let s = state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert_eq!(s.on_fill_count, 1);

        // Verify ledger write was called
        assert_eq!(ledger.transaction_count()?, 1);
        Ok(())
    }

    // ── Test 7: Intention approved and submitted ────────────────────────

    #[tokio::test]
    async fn test_engine_intention_approved_and_submitted() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-100");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor.clone(), ledger, config);

        // Set NAV so risk checks pass
        engine.controller.on_nav_update(Amount::new(dec!(100_000)));

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let intention = make_small_buy_intention()?;
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state))
            .with_ticker_intentions(vec![intention]);
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let (ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();
        let symbol = Symbol::new("XXBTZUSD")?;

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        ticker_tx.send(make_ticker(&symbol))?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = shutdown.send(true);

        handle.await??;

        // Verify executor received the order
        let placed = executor.placed_orders()?;
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].symbol, symbol);
        Ok(())
    }

    // ── Test 8: Intention rejected ──────────────────────────────────────

    #[tokio::test]
    async fn test_engine_intention_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-200");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor.clone(), ledger, config);

        // NAV = 100k, max_order_value = 50k
        engine.controller.on_nav_update(Amount::new(dec!(100_000)));

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        // Intention for 1.0 BTC at 67k = 67,000 > max_order_value 50,000
        let big_intention = OrderIntention {
            strategy_id: StrategyId::new("mock")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: Quantity::new(dec!(1.0))?,
                limit_price: Some(Price::new(dec!(67_000))),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: None,
        };
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state))
            .with_ticker_intentions(vec![big_intention]);
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let (ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();
        let symbol = Symbol::new("XXBTZUSD")?;

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        ticker_tx.send(make_ticker(&symbol))?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = shutdown.send(true);

        handle.await??;

        // Executor should NOT have been called
        let placed = executor.placed_orders()?;
        assert!(placed.is_empty());
        Ok(())
    }

    // ── Test 9: Shutdown signal ─────────────────────────────────────────

    #[tokio::test]
    async fn test_engine_shutdown_signal() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        // Send shutdown immediately
        let _ = shutdown.send(true);

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await??;
        assert!(result.is_ok());

        // Verify shutdown was called on strategy
        let s = state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert!(s.shutdown_called);
        Ok(())
    }

    // ── Test 10: End-to-end with PaperExchange ──────────────────────────

    /// End-to-end: ticker → strategy emits intention → risk approved →
    /// order submitted → fill received → ledger written → strategy notified.
    #[tokio::test]
    async fn test_engine_end_to_end_full_cycle() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-E2E");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor.clone(), ledger.clone(), config);

        engine.controller.on_nav_update(Amount::new(dec!(100_000)));

        let symbol = Symbol::new("XXBTZUSD")?;
        engine.register_symbol(symbol.clone(), Currency::BTC, Currency::USD);

        // MockStrategy emits a buy on ticker, and also records fills
        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let intention = make_small_buy_intention()?;
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state))
            .with_ticker_intentions(vec![intention]);
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let (ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        // Phase 1: ticker → strategy → intention → order placed
        ticker_tx.send(make_ticker(&symbol))?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Phase 2: fill arrives → ledger write + strategy notified
        let fill = make_fill("ORD-E2E", &symbol)?;
        fill_tx.send(fill).await?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let _ = shutdown.send(true);
        handle.await??;

        // Verify full cycle
        let s = state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert_eq!(s.on_ticker_count, 1, "strategy should have seen 1 ticker");
        assert_eq!(s.on_fill_count, 1, "strategy should have seen 1 fill");
        assert!(s.shutdown_called, "strategy should have been shut down");

        let placed = executor.placed_orders()?;
        assert_eq!(placed.len(), 1, "executor should have placed 1 order");

        assert_eq!(
            ledger.transaction_count()?,
            1,
            "ledger should have 1 transaction"
        );

        Ok(())
    }

    // ── Scheduler tests ─────────────────────────────────────────────────

    #[test]
    fn test_register_schedule_unknown_strategy() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let schedule = ScheduleConfig::new(StrategyId::new("nonexistent")?, 1000)?;
        let result = engine.register_schedule(schedule);
        assert!(matches!(result, Err(EngineError::StrategyNotFound(_))));
        Ok(())
    }

    #[test]
    fn test_register_schedule_duplicate() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let id = StrategyId::new("mock")?;
        let mock = MockStrategy::new(
            id.clone(),
            Arc::new(Mutex::new(MockStrategyState::default())),
        );
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let s1 = ScheduleConfig::new(id.clone(), 1000)?;
        let s2 = ScheduleConfig::new(id, 2000)?;
        engine.register_schedule(s1)?;

        let result = engine.register_schedule(s2);
        assert!(matches!(result, Err(EngineError::DuplicateStrategyId(_))));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn test_scheduler_fires_at_interval() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let id = StrategyId::new("sched")?;
        let mock = MockStrategy::new(id.clone(), Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let schedule = ScheduleConfig::new(id, 100)?; // 100ms interval
        engine.register_schedule(schedule)?;

        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        // With start_paused=true, sleep auto-advances time when all tasks block
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        let _ = shutdown.send(true);
        handle.await??;

        let s = state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert!(
            s.on_schedule_count >= 2,
            "expected >= 2 schedule calls, got {}",
            s.on_schedule_count
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn test_scheduler_multiple_strategies_different_intervals()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        // Strategy A: 100ms interval
        let state_a = Arc::new(Mutex::new(MockStrategyState::default()));
        let id_a = StrategyId::new("fast")?;
        let mock_a = MockStrategy::new(id_a.clone(), Arc::clone(&state_a));
        engine.register_strategy(StrategyKind::Mock(mock_a))?;
        engine.register_schedule(ScheduleConfig::new(id_a, 100)?)?;

        // Strategy B: 200ms interval
        let state_b = Arc::new(Mutex::new(MockStrategyState::default()));
        let id_b = StrategyId::new("slow")?;
        let mock_b = MockStrategy::new(id_b.clone(), Arc::clone(&state_b));
        engine.register_strategy(StrategyKind::Mock(mock_b))?;
        engine.register_schedule(ScheduleConfig::new(id_b, 200)?)?;

        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        let _ = shutdown.send(true);
        handle.await??;

        let sa = state_a
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        let sb = state_b
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        assert!(
            sa.on_schedule_count >= 2,
            "fast strategy expected >= 2, got {}",
            sa.on_schedule_count
        );
        assert!(
            sb.on_schedule_count >= 1,
            "slow strategy expected >= 1, got {}",
            sb.on_schedule_count
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn test_scheduler_stops_on_shutdown() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let id = StrategyId::new("sched")?;
        let mock = MockStrategy::new(id.clone(), Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let schedule = ScheduleConfig::new(id, 50)?; // 50ms interval
        engine.register_schedule(schedule)?;

        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let shutdown = engine.shutdown_handle();

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        // Let timers fire a couple of times
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        // Record count before shutdown
        let count_before = {
            let s = state
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            s.on_schedule_count
        };

        let _ = shutdown.send(true);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await??;
        assert!(result.is_ok());

        // Advance more time — count should NOT increase after shutdown
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let s = state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert_eq!(
            s.on_schedule_count, count_before,
            "schedule count should not increase after shutdown"
        );
        assert!(s.shutdown_called);
        Ok(())
    }

    // ── Kill switch tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_engine_kill_switch_halts_controller() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let ks = engine.kill_switch_handle();
        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ks.activate();

        let result = handle.await?;
        assert!(matches!(result, Err(EngineError::KillSwitchActivated)));
        // Controller halted is verified by the kill switch sequence running
        // (it calls controller.halt() first thing). If no panic, it worked.
        Ok(())
    }

    #[tokio::test]
    async fn test_engine_kill_switch_shuts_down_strategies()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let ks = engine.kill_switch_handle();
        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ks.activate();

        let result = handle.await?;
        assert!(matches!(result, Err(EngineError::KillSwitchActivated)));

        let s = state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert!(s.shutdown_called);
        Ok(())
    }

    #[tokio::test]
    async fn test_engine_kill_switch_cancels_all_orders() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let cancel_count = Arc::clone(&executor.cancel_all_count);
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let ks = engine.kill_switch_handle();
        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ks.activate();

        let result = handle.await?;
        assert!(matches!(result, Err(EngineError::KillSwitchActivated)));

        let count = cancel_count
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert!(*count > 0, "cancel_all_orders should have been called");
        Ok(())
    }

    #[tokio::test]
    async fn test_engine_kill_switch_closes_positions() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let placed = Arc::clone(&executor.placed);
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        // Seed a Buy position via controller
        let symbol = Symbol::new("XXBTZUSD")?;
        let fill = OrderFill {
            order_id: ingot_core::OrderId::new("SEED-001")?,
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67_000)),
            fill_quantity: Quantity::new(dec!(0.5))?,
            fee: Amount::new(dec!(0.26)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };
        engine.controller.on_fill(&fill);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let ks = engine.kill_switch_handle();
        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ks.activate();

        let result = handle.await?;
        assert!(matches!(result, Err(EngineError::KillSwitchActivated)));

        let orders = placed
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert_eq!(orders.len(), 1, "should place one close order");
        assert_eq!(orders[0].symbol, symbol);
        assert_eq!(orders[0].side, OrderSide::Sell, "close Buy with Sell");
        assert_eq!(orders[0].order_type, OrderType::Market);
        assert_eq!(orders[0].quantity, Quantity::new(dec!(0.5))?);
        assert_eq!(orders[0].time_in_force, TimeInForce::ImmediateOrCancel);
        Ok(())
    }

    #[tokio::test]
    async fn test_engine_kill_switch_closes_sell_positions()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let placed = Arc::clone(&executor.placed);
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        // Seed a Sell position
        let symbol = Symbol::new("XETHZUSD")?;
        let fill = OrderFill {
            order_id: ingot_core::OrderId::new("SEED-002")?,
            symbol: symbol.clone(),
            side: OrderSide::Sell,
            fill_price: Price::new(dec!(3_500)),
            fill_quantity: Quantity::new(dec!(2))?,
            fee: Amount::new(dec!(0.10)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };
        engine.controller.on_fill(&fill);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let ks = engine.kill_switch_handle();
        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ks.activate();

        let result = handle.await?;
        assert!(matches!(result, Err(EngineError::KillSwitchActivated)));

        let orders = placed
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, OrderSide::Buy, "close Sell with Buy");
        assert_eq!(orders[0].order_type, OrderType::Market);
        Ok(())
    }

    #[tokio::test]
    async fn test_engine_kill_switch_no_positions_no_close_orders()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let placed = Arc::clone(&executor.placed);
        let cancel_count = Arc::clone(&executor.cancel_all_count);
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let ks = engine.kill_switch_handle();
        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ks.activate();

        let result = handle.await?;
        assert!(matches!(result, Err(EngineError::KillSwitchActivated)));

        let orders = placed
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert!(orders.is_empty(), "no positions = no close orders");

        let count = cancel_count
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        assert!(*count > 0, "cancel_all should still be called");
        Ok(())
    }

    #[tokio::test]
    async fn test_engine_kill_switch_returns_error() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let executor = MockOrderExecutor::new("ORD-001");
        let ledger = MockLedgerWriter::new();
        let mut engine = Engine::new(executor, ledger, config);

        let state = Arc::new(Mutex::new(MockStrategyState::default()));
        let mock = MockStrategy::new(StrategyId::new("mock")?, Arc::clone(&state));
        engine.register_strategy(StrategyKind::Mock(mock))?;

        let ks = engine.kill_switch_handle();
        let (_ticker_tx, ticker_rx) = broadcast::channel(16);
        let (_book_tx, book_rx) = broadcast::channel(16);
        let (_fill_tx, fill_rx) = mpsc::channel(16);

        let handle = tokio::spawn(async move { engine.run(ticker_rx, book_rx, fill_rx).await });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ks.activate();

        let result = handle.await?;
        assert!(
            matches!(result, Err(EngineError::KillSwitchActivated)),
            "run() should return KillSwitchActivated error"
        );
        Ok(())
    }
}
