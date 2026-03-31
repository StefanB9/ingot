use std::sync::{Arc, Mutex};

use chrono::DateTime;
use ingot_backtest::{
    BacktestConfig, BacktestError, BacktestEvent, BacktestRunner, compute_metrics, merge_events,
    ohlcv_to_events, ticks_to_events,
};
use ingot_core::{OhlcvBar, OrderFill, OrderRequest, Tick, TickerSnapshot};
use ingot_engine::{
    NoopStrategy, OrderIntention, RiskConfig, SmartOrderConfig, Strategy, StrategyContext,
    StrategyId,
};
use ingot_primitives::{
    Amount, Currency, OrderSide, OrderType, Percentage, Price, Quantity, Symbol, TimeInForce,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use smol_str::SmolStr;

// ── Helpers ─────────────────────────────────────────────────────────────

fn sample_config() -> Result<BacktestConfig, Box<dyn std::error::Error>> {
    Ok(BacktestConfig {
        initial_balances: vec![(Currency::USD, dec!(100000)), (Currency::BTC, dec!(0))],
        base_currency: Currency::USD,
        slippage_bps: dec!(0),
        maker_fee_bps: dec!(0),
        taker_fee_bps: dec!(0),
        partial_fill_probability: dec!(0),
        rng_seed: 42,
        risk: RiskConfig {
            global_stop_loss: Amount::new(dec!(10000)),
            max_currency_exposure: Percentage::new(dec!(0.50))?,
            max_asset_exposure: Percentage::new(dec!(0.90))?,
            max_order_value: Amount::new(dec!(90000)),
            margin: None,
        },
        smart_order: SmartOrderConfig {
            use_mid_price: false,
            offset_bps: dec!(0),
            fallback_timeout_ms: 30_000,
        },
    })
}

fn sample_config_with_fees() -> Result<BacktestConfig, Box<dyn std::error::Error>> {
    let mut config = sample_config()?;
    config.slippage_bps = dec!(10);
    config.taker_fee_bps = dec!(26);
    Ok(config)
}

fn make_ohlcv_bar(
    symbol: &str,
    time_str: &str,
    close: Decimal,
    volume: Decimal,
) -> Result<OhlcvBar, Box<dyn std::error::Error>> {
    Ok(OhlcvBar {
        time: DateTime::parse_from_rfc3339(time_str)?.to_utc(),
        symbol: Symbol::new(symbol)?,
        exchange: SmolStr::new("backtest"),
        interval: SmolStr::new("1h"),
        open: Price::new(close),
        high: Price::new(close),
        low: Price::new(close),
        close: Price::new(close),
        volume: Quantity::new(volume)?,
        trade_count: None,
    })
}

fn make_tick(
    symbol: &str,
    time_str: &str,
    price: Decimal,
    quantity: Decimal,
) -> Result<Tick, Box<dyn std::error::Error>> {
    Ok(Tick {
        time: DateTime::parse_from_rfc3339(time_str)?.to_utc(),
        symbol: Symbol::new(symbol)?,
        exchange: SmolStr::new("backtest"),
        price: Price::new(price),
        quantity: Quantity::new(quantity)?,
        side: None,
        trade_id: None,
    })
}

// ── TestStrategy ────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct TestStrategyState {
    ticker_count: usize,
}

struct TestStrategy {
    id: StrategyId,
    state: Arc<Mutex<TestStrategyState>>,
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
        _ctx: &StrategyContext,
    ) -> Vec<OrderIntention> {
        let mut s = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.ticker_count += 1;
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
        Vec::new()
    }

    fn on_schedule(&mut self, _ctx: &StrategyContext) -> Vec<OrderIntention> {
        Vec::new()
    }

    fn shutdown(&mut self) {}
}

fn buy_intention(
    strategy_id: &str,
    symbol: &str,
    qty: Decimal,
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
        reason: Some(SmolStr::new("test buy")),
    })
}

fn sell_intention(
    strategy_id: &str,
    symbol: &str,
    qty: Decimal,
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
        reason: Some(SmolStr::new("test sell")),
    })
}

// ── Test 1: OHLCV → runner with noop ────────────────────────────────────

#[tokio::test]
async fn test_ohlcv_to_runner_noop() -> Result<(), Box<dyn std::error::Error>> {
    let bars = vec![
        make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500), dec!(110))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(120))?,
    ];
    let events = ohlcv_to_events(&bars)?;

    let config = sample_config()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);
    runner.register_strategy(Box::new(NoopStrategy::new(StrategyId::new("noop")?)))?;

    let result = runner.run(events).await?;

    assert!(result.fills.is_empty());
    assert_eq!(result.initial_capital, result.final_capital);
    Ok(())
}

// ── Test 2: Ticks → runner with noop ────────────────────────────────────

#[tokio::test]
async fn test_ticks_to_runner_noop() -> Result<(), Box<dyn std::error::Error>> {
    let ticks = vec![
        make_tick("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(0.5))?,
        make_tick("BTCUSD", "2025-01-01T00:00:01Z", dec!(67100), dec!(0.3))?,
    ];
    let events = ticks_to_events(&ticks)?;

    let config = sample_config()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);
    runner.register_strategy(Box::new(NoopStrategy::new(StrategyId::new("noop")?)))?;

    let result = runner.run(events).await?;

    assert!(result.fills.is_empty());
    assert_eq!(result.initial_capital, result.final_capital);
    Ok(())
}

// ── Test 3: Full pipeline buy and sell → metrics ────────────────────────

#[tokio::test]
async fn test_full_pipeline_buy_and_sell() -> Result<(), Box<dyn std::error::Error>> {
    let bars = vec![
        make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500), dec!(110))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(120))?,
    ];
    let events = ohlcv_to_events(&bars)?;

    let config = sample_config()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

    let state = Arc::new(Mutex::new(TestStrategyState::default()));
    let strategy =
        TestStrategy::new(StrategyId::new("trader")?, Arc::clone(&state)).with_actions(vec![
            (1, buy_intention("trader", "BTCUSD", dec!(1))?),
            (3, sell_intention("trader", "BTCUSD", dec!(1))?),
        ]);
    runner.register_strategy(Box::new(strategy))?;

    let result = runner.run(events).await?;

    assert_eq!(result.fills.len(), 2);
    assert_eq!(result.fills[0].side, OrderSide::Buy);
    assert_eq!(result.fills[1].side, OrderSide::Sell);

    // Bought at 67000, sold at 68000 → profit
    assert!(result.final_capital > result.initial_capital);

    let metrics = compute_metrics(&result, dec!(0));
    assert!(metrics.total_return > Decimal::ZERO);
    assert_eq!(metrics.total_trades, 1);
    assert_eq!(metrics.win_rate, dec!(1));
    Ok(())
}

// ── Test 4: Pipeline with fees and slippage ─────────────────────────────

#[tokio::test]
async fn test_full_pipeline_with_fees_and_slippage() -> Result<(), Box<dyn std::error::Error>> {
    let bars = vec![
        make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500), dec!(110))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(120))?,
    ];
    let events = ohlcv_to_events(&bars)?;

    let config = sample_config_with_fees()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

    let state = Arc::new(Mutex::new(TestStrategyState::default()));
    let strategy =
        TestStrategy::new(StrategyId::new("trader")?, Arc::clone(&state)).with_actions(vec![
            (1, buy_intention("trader", "BTCUSD", dec!(1))?),
            (3, sell_intention("trader", "BTCUSD", dec!(1))?),
        ]);
    runner.register_strategy(Box::new(strategy))?;

    let result = runner.run(events).await?;

    assert_eq!(result.fills.len(), 2);

    // Verify fees were applied
    let total_fees: Amount = result.fills.iter().fold(Amount::zero(), |a, f| a + f.fee);
    assert!(total_fees > Amount::zero(), "fees should be non-zero");

    let metrics = compute_metrics(&result, dec!(0));
    assert!(metrics.total_fees > Amount::zero());
    Ok(())
}

// ── Test 5: Multi-symbol pipeline ───────────────────────────────────────

#[tokio::test]
async fn test_multi_symbol_pipeline() -> Result<(), Box<dyn std::error::Error>> {
    let btc_bars = vec![
        make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(120))?,
    ];
    let eth_bars = vec![
        make_ohlcv_bar("ETHUSD", "2025-01-01T01:00:00Z", dec!(3500), dec!(500))?,
        make_ohlcv_bar("ETHUSD", "2025-01-01T03:00:00Z", dec!(3600), dec!(600))?,
    ];

    let btc_events = ohlcv_to_events(&btc_bars)?;
    let eth_events = ohlcv_to_events(&eth_bars)?;
    let events = merge_events(vec![btc_events, eth_events]);

    assert_eq!(events.len(), 4);

    let config = sample_config()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);
    runner.register_symbol(Symbol::new("ETHUSD")?, Currency::ETH, Currency::USD);

    let state = Arc::new(Mutex::new(TestStrategyState::default()));
    // Buy BTC on tick 1, buy ETH on tick 2
    let strategy =
        TestStrategy::new(StrategyId::new("multi")?, Arc::clone(&state)).with_actions(vec![
            (1, buy_intention("multi", "BTCUSD", dec!(1))?),
            (2, buy_intention("multi", "ETHUSD", dec!(1))?),
        ]);
    runner.register_strategy(Box::new(strategy))?;

    let result = runner.run(events).await?;

    assert_eq!(result.fills.len(), 2);
    assert_eq!(result.fills[0].symbol.as_str(), "BTCUSD");
    assert_eq!(result.fills[1].symbol.as_str(), "ETHUSD");
    Ok(())
}

// ── Test 6: Drawdown from runner ────────────────────────────────────────

#[tokio::test]
async fn test_metrics_drawdown_from_runner() -> Result<(), Box<dyn std::error::Error>> {
    // Buy at high, sell at low (captures the loss in equity curve),
    // then buy again and sell higher to create recovery.
    // Equity curve records points on fills, so we need multiple fills to see
    // drawdown.
    let bars = vec![
        make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(60000), dec!(100))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(65000), dec!(100))?,
        make_ohlcv_bar("BTCUSD", "2025-01-01T03:00:00Z", dec!(70000), dec!(100))?,
    ];
    let events = ohlcv_to_events(&bars)?;

    let config = sample_config()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

    let state = Arc::new(Mutex::new(TestStrategyState::default()));
    // Buy at 67000, sell at 60000 (loss → drawdown), then buy at 65000, sell at
    // 70000
    let strategy =
        TestStrategy::new(StrategyId::new("dd")?, Arc::clone(&state)).with_actions(vec![
            (1, buy_intention("dd", "BTCUSD", dec!(1))?),
            (2, sell_intention("dd", "BTCUSD", dec!(1))?),
            (3, buy_intention("dd", "BTCUSD", dec!(1))?),
            (4, sell_intention("dd", "BTCUSD", dec!(1))?),
        ]);
    runner.register_strategy(Box::new(strategy))?;

    let result = runner.run(events).await?;

    assert_eq!(result.fills.len(), 4);

    let metrics = compute_metrics(&result, dec!(0));
    // After selling at 60000 (bought at 67000), NAV dropped → drawdown should be
    // negative
    assert!(
        metrics.max_drawdown < Decimal::ZERO,
        "max_drawdown should be negative, got {}",
        metrics.max_drawdown
    );
    Ok(())
}

// ── Test 7: Multiple round trips metrics ────────────────────────────────

#[tokio::test]
async fn test_metrics_multiple_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let bars = vec![
        make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?, // Buy 1
        make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(68000), dec!(100))?, // Sell 1 (win)
        make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(100))?, // Buy 2
        make_ohlcv_bar("BTCUSD", "2025-01-01T03:00:00Z", dec!(69000), dec!(100))?, // Sell 2 (win)
        make_ohlcv_bar("BTCUSD", "2025-01-01T04:00:00Z", dec!(69000), dec!(100))?, // Buy 3
        make_ohlcv_bar("BTCUSD", "2025-01-01T05:00:00Z", dec!(67000), dec!(100))?, // Sell 3 (loss)
    ];
    let events = ohlcv_to_events(&bars)?;

    let config = sample_config()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

    let state = Arc::new(Mutex::new(TestStrategyState::default()));
    let strategy =
        TestStrategy::new(StrategyId::new("trader")?, Arc::clone(&state)).with_actions(vec![
            (1, buy_intention("trader", "BTCUSD", dec!(1))?),
            (2, sell_intention("trader", "BTCUSD", dec!(1))?),
            (3, buy_intention("trader", "BTCUSD", dec!(1))?),
            (4, sell_intention("trader", "BTCUSD", dec!(1))?),
            (5, buy_intention("trader", "BTCUSD", dec!(1))?),
            (6, sell_intention("trader", "BTCUSD", dec!(1))?),
        ]);
    runner.register_strategy(Box::new(strategy))?;

    let result = runner.run(events).await?;

    assert_eq!(result.fills.len(), 6);

    let metrics = compute_metrics(&result, dec!(0));
    assert_eq!(metrics.total_trades, 3);
    assert_eq!(metrics.winning_trades, 2);
    assert_eq!(metrics.losing_trades, 1);

    // Win rate = 2/3
    let expected_wr = Decimal::from(2) / Decimal::from(3);
    assert_eq!(metrics.win_rate, expected_wr);

    // Profit factor = gross_profit / gross_loss = (1000 + 1000) / 2000 = 1
    assert_eq!(metrics.profit_factor, dec!(1));
    Ok(())
}

// ── Test 8: Unsorted events rejected ────────────────────────────────────

#[tokio::test]
async fn test_unsorted_events_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let config = sample_config()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);
    runner.register_strategy(Box::new(NoopStrategy::new(StrategyId::new("noop")?)))?;

    // Manually create unsorted events
    let sym = Symbol::new("BTCUSD")?;
    let events = vec![
        BacktestEvent {
            timestamp: DateTime::parse_from_rfc3339("2025-01-01T02:00:00Z")?.to_utc(),
            symbol: sym.clone(),
            ticker: TickerSnapshot {
                symbol: sym.clone(),
                bid: Price::new(dec!(68000)),
                ask: Price::new(dec!(68000)),
                last: Price::new(dec!(68000)),
                volume_24h: Quantity::new(dec!(100))?,
                timestamp: DateTime::parse_from_rfc3339("2025-01-01T02:00:00Z")?.to_utc(),
            },
        },
        BacktestEvent {
            timestamp: DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")?.to_utc(),
            symbol: sym.clone(),
            ticker: TickerSnapshot {
                symbol: sym,
                bid: Price::new(dec!(67000)),
                ask: Price::new(dec!(67000)),
                last: Price::new(dec!(67000)),
                volume_24h: Quantity::new(dec!(100))?,
                timestamp: DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")?.to_utc(),
            },
        },
    ];

    let result = runner.run(events).await;
    assert!(matches!(result, Err(BacktestError::UnsortedData)));
    Ok(())
}

// ── Test 9: Empty data rejected ─────────────────────────────────────────

#[tokio::test]
async fn test_empty_data_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let config = sample_config()?;
    let mut runner = BacktestRunner::new(config);
    runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);
    runner.register_strategy(Box::new(NoopStrategy::new(StrategyId::new("noop")?)))?;

    let result = runner.run(vec![]).await;
    assert!(matches!(result, Err(BacktestError::NoData)));
    Ok(())
}

// ── Test 10: Deterministic across runs ──────────────────────────────────

#[tokio::test]
async fn test_deterministic_across_runs() -> Result<(), Box<dyn std::error::Error>> {
    async fn run_once() -> Result<
        (
            ingot_backtest::BacktestResult,
            ingot_backtest::PerformanceMetrics,
        ),
        Box<dyn std::error::Error>,
    > {
        let bars = vec![
            make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500), dec!(110))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(120))?,
        ];
        let events = ohlcv_to_events(&bars)?;

        let config = sample_config_with_fees()?;
        let mut runner = BacktestRunner::new(config);
        runner.register_symbol(Symbol::new("BTCUSD")?, Currency::BTC, Currency::USD);

        let state = Arc::new(Mutex::new(TestStrategyState::default()));
        let strategy = TestStrategy::new(StrategyId::new("trader")?, Arc::clone(&state))
            .with_actions(vec![
                (1, buy_intention("trader", "BTCUSD", dec!(1))?),
                (3, sell_intention("trader", "BTCUSD", dec!(1))?),
            ]);
        runner.register_strategy(Box::new(strategy))?;

        let result = runner.run(events).await?;
        let metrics = compute_metrics(&result, dec!(0.05));
        Ok((result, metrics))
    }

    let (r1, m1) = run_once().await?;
    let (r2, m2) = run_once().await?;

    assert_eq!(r1.fills.len(), r2.fills.len());
    for (f1, f2) in r1.fills.iter().zip(r2.fills.iter()) {
        assert_eq!(f1.fill_price, f2.fill_price);
        assert_eq!(f1.fill_quantity, f2.fill_quantity);
        assert_eq!(f1.fee, f2.fee);
    }
    assert_eq!(r1.final_capital, r2.final_capital);
    assert_eq!(m1.total_return, m2.total_return);
    assert_eq!(m1.sharpe_ratio, m2.sharpe_ratio);
    assert_eq!(m1.max_drawdown, m2.max_drawdown);
    Ok(())
}
