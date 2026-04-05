use std::collections::HashMap;

use ingot_accounting::post_fill;
use ingot_core::TickerSnapshot;
use ingot_engine::{
    LedgerWriter, OrderManager, PortfolioController, RiskDecision, Strategy, StrategyContext,
};
use ingot_primitives::{Amount, Currency, Exchange, Symbol};
use rust_decimal::Decimal;

use crate::{
    config::BacktestConfig,
    error::BacktestError,
    exchange::BacktestExchange,
    feed::{BacktestEvent, validate_sorted},
    ledger::InMemoryLedger,
    result::{BacktestResult, EquityPoint},
};

/// Synchronous, event-driven backtest runner.
///
/// Iterates through `BacktestEvent`s sequentially, dispatching to strategies,
/// checking risk, placing orders, and collecting results.
pub struct BacktestRunner {
    config: BacktestConfig,
    strategies: Vec<Box<dyn Strategy>>,
    symbol_currencies: HashMap<Symbol, (Currency, Currency)>,
}

impl BacktestRunner {
    pub fn new(config: BacktestConfig) -> Self {
        Self {
            config,
            strategies: Vec::new(),
            symbol_currencies: HashMap::new(),
        }
    }

    /// Register a strategy. Rejects duplicate strategy IDs.
    pub fn register_strategy(&mut self, strategy: Box<dyn Strategy>) -> Result<(), BacktestError> {
        let id = strategy.id().clone();
        if self.strategies.iter().any(|s| *s.id() == id) {
            return Err(BacktestError::DuplicateStrategy(id));
        }
        self.strategies.push(strategy);
        Ok(())
    }

    /// Register a symbol's base and quote currencies.
    pub fn register_symbol(&mut self, symbol: Symbol, base: Currency, quote: Currency) {
        self.symbol_currencies.insert(symbol, (base, quote));
    }

    /// Run the backtest over a sequence of events.
    #[allow(clippy::too_many_lines)]
    pub async fn run(
        &mut self,
        events: Vec<BacktestEvent>,
    ) -> Result<BacktestResult, BacktestError> {
        // Validate preconditions
        if self.strategies.is_empty() {
            return Err(BacktestError::NoStrategies);
        }
        if events.is_empty() {
            return Err(BacktestError::NoData);
        }
        validate_sorted(&events)?;

        // Create exchange
        let exchange = BacktestExchange::new(&self.config)?;
        for (symbol, (base, quote)) in &self.symbol_currencies {
            exchange.register_symbol(symbol.clone(), base.clone(), quote.clone());
        }

        // Create components
        let mut controller = PortfolioController::new(self.config.risk.clone());
        let mut order_manager = OrderManager::new(self.config.smart_order.clone());
        let ledger = InMemoryLedger::new();

        // Set initial NAV
        let initial_cash = self
            .config
            .initial_balances
            .iter()
            .find(|(c, _)| *c == self.config.base_currency)
            .map_or(Decimal::ZERO, |(_, amt)| *amt);
        let initial_capital = Amount::new(initial_cash);
        controller.on_nav_update(initial_capital);

        // Track tickers and equity curve
        let mut latest_tickers: HashMap<Symbol, TickerSnapshot> = HashMap::new();
        let mut equity_curve: Vec<EquityPoint> = Vec::new();

        let start_time = events[0].timestamp;
        let end_time = events[events.len() - 1].timestamp;

        // Record initial equity point
        equity_curve.push(compute_equity_point(&exchange, &self.config, start_time));

        // Init strategies
        let init_ctx = build_context(&exchange, &latest_tickers, start_time);
        for strategy in &mut self.strategies {
            let _init_intentions = strategy.init(&init_ctx);
            // Init intentions are ignored (consistent with Engine behavior)
        }

        // Main event loop
        for event in &events {
            let timestamp = event.timestamp;
            let symbol = &event.symbol;

            // a. Advance price on exchange → limit order fills
            let limit_fills = exchange.on_price_update(symbol, event.ticker.last, timestamp);

            // Update latest tickers
            latest_tickers.insert(symbol.clone(), event.ticker.clone());

            // b. Process limit fills
            for fill in &limit_fills {
                controller.on_fill(fill);
                order_manager.on_fill(fill);

                // Post accounting
                if let Some((base, quote)) = self.symbol_currencies.get(&fill.symbol) {
                    let txn = post_fill(fill, Exchange::Paper, "backtest", base, quote, false)?;
                    ledger.write_transaction(&txn).await.map_err(|e| {
                        BacktestError::Exchange(format!("ledger write failed: {e}"))
                    })?;
                }

                // Dispatch fill to strategies
                let ctx = build_context(&exchange, &latest_tickers, timestamp);
                for strategy in &mut self.strategies {
                    let fill_intentions = strategy.on_fill(fill, &ctx);
                    process_intentions(
                        fill_intentions,
                        &mut controller,
                        &mut order_manager,
                        &exchange,
                        &latest_tickers,
                        &self.symbol_currencies,
                        &ledger,
                        timestamp,
                        &mut equity_curve,
                        &self.config,
                    )
                    .await?;
                }

                // Update NAV after fill
                let eq = compute_equity_point(&exchange, &self.config, timestamp);
                controller.on_nav_update(eq.nav);
                equity_curve.push(eq);
            }

            // c. Dispatch ticker to strategies
            let ctx = build_context(&exchange, &latest_tickers, timestamp);
            let mut all_intentions = Vec::new();
            for strategy in &mut self.strategies {
                let intentions = strategy.on_ticker(&event.ticker, &ctx);
                all_intentions.extend(intentions);
            }

            // d. Process strategy intentions
            process_intentions(
                all_intentions,
                &mut controller,
                &mut order_manager,
                &exchange,
                &latest_tickers,
                &self.symbol_currencies,
                &ledger,
                timestamp,
                &mut equity_curve,
                &self.config,
            )
            .await?;
        }

        // Shutdown strategies
        for strategy in &mut self.strategies {
            strategy.shutdown();
        }

        // Compute final equity
        let final_eq = compute_equity_point(&exchange, &self.config, end_time);
        let final_capital = final_eq.nav;

        Ok(BacktestResult {
            fills: exchange.fills(),
            transactions: ledger.into_transactions(),
            equity_curve,
            start_time,
            end_time,
            initial_capital,
            final_capital,
        })
    }
}

fn build_context(
    exchange: &BacktestExchange,
    latest_tickers: &HashMap<Symbol, TickerSnapshot>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> StrategyContext {
    let positions: Vec<_> = exchange.positions().into_values().collect();
    let balances: Vec<_> = exchange.balances().into_values().collect();

    StrategyContext {
        positions,
        balances,
        latest_tickers: latest_tickers.clone(),
        latest_order_books: HashMap::new(),
        timestamp,
    }
}

fn compute_equity_point(
    exchange: &BacktestExchange,
    config: &BacktestConfig,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> EquityPoint {
    let balances = exchange.balances();
    let positions = exchange.positions();
    let last_prices = exchange.last_prices();

    let cash = balances
        .get(&config.base_currency)
        .map_or(Amount::zero(), |b| b.total);

    let positions_value = positions
        .values()
        .map(|p| {
            let price = last_prices
                .get(&p.symbol)
                .copied()
                .unwrap_or(p.average_entry_price);
            Amount::new(price.value() * p.quantity.value())
        })
        .fold(Amount::zero(), |a, b| a + b);

    EquityPoint {
        timestamp,
        nav: cash + positions_value,
        cash,
        positions_value,
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_intentions(
    intentions: Vec<ingot_engine::OrderIntention>,
    controller: &mut PortfolioController,
    order_manager: &mut OrderManager,
    exchange: &BacktestExchange,
    latest_tickers: &HashMap<Symbol, TickerSnapshot>,
    symbol_currencies: &HashMap<Symbol, (Currency, Currency)>,
    ledger: &InMemoryLedger,
    timestamp: chrono::DateTime<chrono::Utc>,
    equity_curve: &mut Vec<EquityPoint>,
    config: &BacktestConfig,
) -> Result<(), BacktestError> {
    for intention in intentions {
        let decision = controller.check_intention(&intention, latest_tickers);
        if let RiskDecision::Rejected { .. } = decision {
            continue;
        }

        let fills_before = exchange.fills().len();

        // No order book in backtest — pass None so smart limit conversion is skipped
        let _order_id = order_manager
            .submit_order(intention, None, exchange)
            .await?;

        // Check for immediate fills (market orders)
        let fills_after = exchange.fills();
        if fills_after.len() > fills_before {
            for fill in &fills_after[fills_before..] {
                controller.on_fill(fill);
                order_manager.on_fill(fill);

                if let Some((base, quote)) = symbol_currencies.get(&fill.symbol) {
                    let txn = post_fill(fill, Exchange::Paper, "backtest", base, quote, false)?;
                    ledger.write_transaction(&txn).await.map_err(|e| {
                        BacktestError::Exchange(format!("ledger write failed: {e}"))
                    })?;
                }

                let eq = compute_equity_point(exchange, config, timestamp);
                controller.on_nav_update(eq.nav);
                equity_curve.push(eq);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::DateTime;
    use ingot_core::{OrderFill, OrderRequest, TickerSnapshot};
    use ingot_engine::{
        NoopStrategy, OrderIntention, RiskConfig, SmartOrderConfig, Strategy, StrategyContext,
        StrategyId,
    };
    use ingot_primitives::{
        Amount, Currency, OrderSide, OrderType, Percentage, Price, Quantity, Symbol, TimeInForce,
    };
    use rust_decimal_macros::dec;

    use super::*;
    use crate::{config::BacktestConfig, feed::BacktestEvent};

    // ── Helpers ─────────────────────────────────────────────────────────

    fn sample_config() -> Result<BacktestConfig, Box<dyn std::error::Error>> {
        Ok(BacktestConfig {
            initial_balances: vec![(Currency::USD, dec!(100000)), (Currency::BTC, dec!(0))],
            base_currency: Currency::USD,
            slippage_bps: dec!(0),
            maker_fee_bps: dec!(0),
            taker_fee_bps: dec!(26),
            partial_fill_probability: dec!(0),
            rng_seed: 42,
            risk: RiskConfig {
                global_stop_loss: Amount::new(dec!(10000)),
                max_currency_exposure: Percentage::new(dec!(0.50))?,
                max_asset_exposure: Percentage::new(dec!(0.90))?,
                max_order_value: Amount::new(dec!(90000)),
                margin: None,
                rollover: None,
            },
            smart_order: SmartOrderConfig {
                use_mid_price: false,
                offset_bps: dec!(0),
                fallback_timeout_ms: 30_000,
            },
        })
    }

    fn make_event(
        symbol: &str,
        time_str: &str,
        price: rust_decimal::Decimal,
    ) -> Result<BacktestEvent, Box<dyn std::error::Error>> {
        let sym = Symbol::new(symbol)?;
        let ts = DateTime::parse_from_rfc3339(time_str)?.to_utc();
        Ok(BacktestEvent {
            timestamp: ts,
            symbol: sym.clone(),
            ticker: TickerSnapshot {
                symbol: sym,
                bid: Price::new(price),
                ask: Price::new(price),
                last: Price::new(price),
                volume_24h: Quantity::new(dec!(100))?,
                timestamp: ts,
            },
        })
    }

    // ── TestStrategy ────────────────────────────────────────────────────

    /// Shared state for test strategy introspection.
    #[derive(Debug, Default)]
    struct TestStrategyState {
        ticker_count: usize,
        fill_count: usize,
        last_ctx_timestamp: Option<DateTime<chrono::Utc>>,
        shutdown_called: bool,
    }

    /// A test strategy that emits configured intentions at specific ticker
    /// indices.
    struct TestStrategy {
        id: StrategyId,
        state: Arc<Mutex<TestStrategyState>>,
        /// (`ticker_index`, intention) — emits intention when `ticker_count`
        /// matches index.
        actions: Vec<(usize, OrderIntention)>,
    }

    impl TestStrategy {
        fn new(id: StrategyId, state: Arc<Mutex<TestStrategyState>>) -> Self {
            Self {
                id,
                state,
                actions: Vec::new(),
            }
        }

        fn with_actions(mut self, actions: Vec<(usize, OrderIntention)>) -> Self {
            self.actions = actions;
            self
        }
    }

    impl Strategy for TestStrategy {
        fn id(&self) -> &StrategyId {
            &self.id
        }

        fn init(&mut self, _ctx: &StrategyContext) -> Vec<OrderIntention> {
            Vec::new()
        }

        fn on_ticker(
            &mut self,
            _ticker: &TickerSnapshot,
            ctx: &StrategyContext,
        ) -> Vec<OrderIntention> {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            s.ticker_count += 1;
            s.last_ctx_timestamp = Some(ctx.timestamp);
            let count = s.ticker_count;
            drop(s);

            self.actions
                .iter()
                .filter(|(idx, _)| *idx == count)
                .map(|(_, intention)| intention.clone())
                .collect()
        }

        fn on_order_book(
            &mut self,
            _book: &ingot_core::OrderBookSnapshot,
            _ctx: &StrategyContext,
        ) -> Vec<OrderIntention> {
            Vec::new()
        }

        fn on_fill(&mut self, _fill: &OrderFill, _ctx: &StrategyContext) -> Vec<OrderIntention> {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            s.fill_count += 1;
            Vec::new()
        }

        fn on_schedule(&mut self, _ctx: &StrategyContext) -> Vec<OrderIntention> {
            Vec::new()
        }

        fn shutdown(&mut self) {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            s.shutdown_called = true;
        }
    }

    fn buy_intention(
        strategy_id: &str,
        symbol: &str,
        qty: rust_decimal::Decimal,
    ) -> Result<OrderIntention, Box<dyn std::error::Error>> {
        Ok(OrderIntention {
            strategy_id: StrategyId::new(strategy_id)?,
            request: OrderRequest {
                symbol: Symbol::new(symbol)?,
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: Quantity::new(qty)?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: Some(smol_str::SmolStr::new("test buy")),
        })
    }

    fn sell_intention(
        strategy_id: &str,
        symbol: &str,
        qty: rust_decimal::Decimal,
    ) -> Result<OrderIntention, Box<dyn std::error::Error>> {
        Ok(OrderIntention {
            strategy_id: StrategyId::new(strategy_id)?,
            request: OrderRequest {
                symbol: Symbol::new(symbol)?,
                side: OrderSide::Sell,
                order_type: OrderType::Market,
                quantity: Quantity::new(qty)?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: Some(smol_str::SmolStr::new("test sell")),
        })
    }

    fn limit_buy_intention(
        strategy_id: &str,
        symbol: &str,
        qty: rust_decimal::Decimal,
        price: rust_decimal::Decimal,
    ) -> Result<OrderIntention, Box<dyn std::error::Error>> {
        Ok(OrderIntention {
            strategy_id: StrategyId::new(strategy_id)?,
            request: OrderRequest {
                symbol: Symbol::new(symbol)?,
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: Quantity::new(qty)?,
                limit_price: Some(Price::new(price)),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: Some(smol_str::SmolStr::new("test limit buy")),
        })
    }

    // ── Test 1: Runner construction ─────────────────────────────────────

    #[test]
    fn test_runner_new() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let runner = BacktestRunner::new(config);
        assert!(runner.strategies.is_empty());
        assert!(runner.symbol_currencies.is_empty());
        Ok(())
    }

    // ── Test 2: Register strategy ───────────────────────────────────────

    #[test]
    fn test_runner_register_strategy() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        let id = StrategyId::new("noop-1")?;
        runner.register_strategy(Box::new(NoopStrategy::new(id)))?;
        assert_eq!(runner.strategies.len(), 1);
        Ok(())
    }

    // ── Test 3: Duplicate strategy ID rejected ──────────────────────────

    #[test]
    fn test_runner_register_duplicate_strategy_rejected() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        let id = StrategyId::new("dup")?;
        runner.register_strategy(Box::new(NoopStrategy::new(id.clone())))?;
        let result = runner.register_strategy(Box::new(NoopStrategy::new(id)));
        assert!(matches!(result, Err(BacktestError::DuplicateStrategy(_))));
        Ok(())
    }

    // ── Test 4: No strategies error ─────────────────────────────────────

    #[tokio::test]
    async fn test_runner_run_no_strategies_error() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        let events = vec![make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?];
        let result = runner.run(events).await;
        assert!(matches!(result, Err(BacktestError::NoStrategies)));
        Ok(())
    }

    // ── Test 5: No data error ───────────────────────────────────────────

    #[tokio::test]
    async fn test_runner_run_no_data_error() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        let id = StrategyId::new("noop")?;
        runner.register_strategy(Box::new(NoopStrategy::new(id)))?;
        let result = runner.run(vec![]).await;
        assert!(matches!(result, Err(BacktestError::NoData)));
        Ok(())
    }

    // ── Test 6: Noop strategy → zero fills ──────────────────────────────

    #[tokio::test]
    async fn test_runner_run_noop_strategy() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);
        let id = StrategyId::new("noop")?;
        runner.register_strategy(Box::new(NoopStrategy::new(id)))?;

        let events = vec![
            make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?,
            make_event("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500))?,
        ];
        let result = runner.run(events).await?;

        assert!(result.fills.is_empty());
        assert!(result.transactions.is_empty());
        assert_eq!(result.initial_capital, result.final_capital);
        Ok(())
    }

    // ── Test 7: Single fill from market buy ─────────────────────────────

    #[tokio::test]
    async fn test_runner_run_single_fill() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let id = StrategyId::new("buyer")?;
        let strategy = TestStrategy::new(id, Arc::clone(&state))
            .with_actions(vec![(1, buy_intention("buyer", "BTCUSD", dec!(1))?)]);
        runner.register_strategy(Box::new(strategy))?;

        let events = vec![
            make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?,
            make_event("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500))?,
        ];
        let result = runner.run(events).await?;

        assert_eq!(result.fills.len(), 1);
        assert_eq!(result.fills[0].side, OrderSide::Buy);
        assert_eq!(result.transactions.len(), 1);
        Ok(())
    }

    // ── Test 8: Equity curve recorded on fill ───────────────────────────

    #[tokio::test]
    async fn test_runner_equity_curve_recorded_on_fill() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let id = StrategyId::new("buyer")?;
        let strategy = TestStrategy::new(id, Arc::clone(&state))
            .with_actions(vec![(1, buy_intention("buyer", "BTCUSD", dec!(1))?)]);
        runner.register_strategy(Box::new(strategy))?;

        let events = vec![
            make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?,
            make_event("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500))?,
        ];
        let result = runner.run(events).await?;

        // Initial equity point + one from the fill
        assert!(result.equity_curve.len() >= 2);
        // First point is the initial capital
        assert_eq!(result.equity_curve[0].nav, Amount::new(dec!(100000)));
        Ok(())
    }

    // ── Test 9: Context uses event timestamp ────────────────────────────

    #[tokio::test]
    async fn test_runner_context_uses_event_timestamp() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let id = StrategyId::new("timestamp-check")?;
        let strategy = TestStrategy::new(id, Arc::clone(&state));
        runner.register_strategy(Box::new(strategy))?;

        let event_time = "2025-06-15T12:00:00Z";
        let events = vec![make_event("BTCUSD", event_time, dec!(67000))?];
        let _result = runner.run(events).await?;

        let s = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expected_ts = DateTime::parse_from_rfc3339(event_time)?.to_utc();
        assert_eq!(s.last_ctx_timestamp, Some(expected_ts));
        Ok(())
    }

    // ── Test 10: Risk rejection → no fill ───────────────────────────────

    #[tokio::test]
    async fn test_runner_risk_rejection_no_fill() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = sample_config()?;
        // Set max_order_value very low so the buy gets rejected
        config.risk.max_order_value = Amount::new(dec!(100));
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let id = StrategyId::new("big-buyer")?;
        let strategy = TestStrategy::new(id, Arc::clone(&state))
            .with_actions(vec![(1, buy_intention("big-buyer", "BTCUSD", dec!(1))?)]);
        runner.register_strategy(Box::new(strategy))?;

        let events = vec![make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?];
        let result = runner.run(events).await?;

        assert!(result.fills.is_empty());
        Ok(())
    }

    // ── Test 11: Multiple symbols ───────────────────────────────────────

    #[tokio::test]
    async fn test_runner_multiple_symbols() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);
        runner.register_symbol(Symbol::new("ETHUSD")?, Currency::ETH, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let id = StrategyId::new("multi")?;
        let strategy = TestStrategy::new(id, Arc::clone(&state));
        runner.register_strategy(Box::new(strategy))?;

        let events = vec![
            make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?,
            make_event("ETHUSD", "2025-01-01T01:00:00Z", dec!(3500))?,
            make_event("BTCUSD", "2025-01-01T02:00:00Z", dec!(67500))?,
        ];
        let result = runner.run(events).await?;

        // Strategy receives 3 tickers total
        let s = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(s.ticker_count, 3);
        assert!(result.fills.is_empty());
        Ok(())
    }

    // ── Test 12: Limit order fills on price cross ───────────────────────

    #[tokio::test]
    async fn test_runner_limit_order_fills_on_cross() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let id = StrategyId::new("limit-buyer")?;
        // Place limit buy at 66000 on first tick (price is 67000)
        let strategy = TestStrategy::new(id, Arc::clone(&state)).with_actions(vec![(
            1,
            limit_buy_intention("limit-buyer", "BTCUSD", dec!(1), dec!(66000))?,
        )]);
        runner.register_strategy(Box::new(strategy))?;

        let events = vec![
            make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?,
            // Price drops to 65000, crossing the limit
            make_event("BTCUSD", "2025-01-01T01:00:00Z", dec!(65000))?,
        ];
        let result = runner.run(events).await?;

        assert_eq!(result.fills.len(), 1);
        assert_eq!(result.fills[0].side, OrderSide::Buy);
        Ok(())
    }

    // ── Test 13: Deterministic with same seed ───────────────────────────

    #[tokio::test]
    async fn test_runner_deterministic_same_seed() -> Result<(), Box<dyn std::error::Error>> {
        async fn run_once() -> Result<BacktestResult, Box<dyn std::error::Error>> {
            let config = sample_config()?;
            let mut runner = BacktestRunner::new(config);
            runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

            let state = Arc::new(Mutex::new(TestStrategyState::default()));
            let id = StrategyId::new("buyer")?;
            let strategy = TestStrategy::new(id, Arc::clone(&state))
                .with_actions(vec![(1, buy_intention("buyer", "BTCUSD", dec!(1))?)]);
            runner.register_strategy(Box::new(strategy))?;

            let events = vec![
                make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?,
                make_event("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500))?,
            ];
            Ok(runner.run(events).await?)
        }

        let result1 = run_once().await?;
        let result2 = run_once().await?;

        assert_eq!(result1.fills.len(), result2.fills.len());
        for (f1, f2) in result1.fills.iter().zip(result2.fills.iter()) {
            assert_eq!(f1.fill_price, f2.fill_price);
            assert_eq!(f1.fill_quantity, f2.fill_quantity);
            assert_eq!(f1.fee, f2.fee);
        }
        assert_eq!(result1.final_capital, result2.final_capital);
        Ok(())
    }

    // ── Test 14: Accounting integration ─────────────────────────────────

    #[tokio::test]
    async fn test_runner_accounting_integration() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let id = StrategyId::new("acct")?;
        let strategy = TestStrategy::new(id, Arc::clone(&state))
            .with_actions(vec![(1, buy_intention("acct", "BTCUSD", dec!(1))?)]);
        runner.register_strategy(Box::new(strategy))?;

        let events = vec![
            make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?,
            make_event("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500))?,
        ];
        let result = runner.run(events).await?;

        assert_eq!(result.transactions.len(), 1);
        // Transaction should have journal entries
        assert!(!result.transactions[0].entries.is_empty());
        Ok(())
    }

    // ── Test 15: End-to-end buy and sell ─────────────────────────────────

    #[tokio::test]
    async fn test_runner_end_to_end_buy_and_sell() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let id = StrategyId::new("round-trip")?;
        let strategy = TestStrategy::new(id, Arc::clone(&state)).with_actions(vec![
            (1, buy_intention("round-trip", "BTCUSD", dec!(1))?),
            (3, sell_intention("round-trip", "BTCUSD", dec!(1))?),
        ]);
        runner.register_strategy(Box::new(strategy))?;

        let events = vec![
            make_event("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000))?,
            make_event("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500))?,
            make_event("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000))?,
        ];
        let result = runner.run(events).await?;

        assert_eq!(result.fills.len(), 2);
        assert_eq!(result.fills[0].side, OrderSide::Buy);
        assert_eq!(result.fills[1].side, OrderSide::Sell);
        assert_eq!(result.transactions.len(), 2);

        // Bought at 67000, sold at 68000 → profit (minus fees)
        assert!(result.final_capital > result.initial_capital - Amount::new(dec!(100)));
        Ok(())
    }
}
