use std::collections::HashMap;

use anyhow::Context;
use ingot_core::{
    Instrument, InstrumentDetails, MarginSnapshot, OrderBookSnapshot, OrderFill, OrderRequest,
    TickerSnapshot,
};
use ingot_engine::{
    EngineEvent, MarginConfig, OrderIntention, PortfolioController, RiskConfig, RiskDecision,
    RolloverConfig, RolloverMonitor, RolloverPlan, StrategyId,
};
use ingot_primitives::{
    Amount, AssetClass, Currency, Exchange, OrderSide, OrderType, Percentage, Price, Quantity,
    SettlementType, Symbol, TimeInForce,
};
use rust_decimal_macros::dec;
use smol_str::SmolStr;

fn risk_config_with_margin(max_margin: Percentage) -> anyhow::Result<RiskConfig> {
    Ok(RiskConfig {
        global_stop_loss: Amount::new(dec!(10_000)),
        max_currency_exposure: Percentage::new(dec!(0.50)).context("invalid percentage")?,
        max_asset_exposure: Percentage::new(dec!(0.30)).context("invalid percentage")?,
        max_order_value: Amount::new(dec!(100_000)),
        margin: Some(MarginConfig {
            max_margin_utilization: max_margin,
            warn_margin_utilization: Percentage::new(dec!(0.60)).context("invalid percentage")?,
            min_excess_liquidity: Amount::new(dec!(1_000)),
        }),
        rollover: None,
    })
}

// ── Test 8: margin snapshot through portfolio controller ──

#[test]
fn test_margin_snapshot_through_portfolio_controller() -> anyhow::Result<()> {
    let max_util = Percentage::new(dec!(0.80)).context("invalid percentage")?;
    let config = risk_config_with_margin(max_util)?;
    let mut controller = PortfolioController::new(config);

    // Set NAV so global stop-loss doesn't trigger
    controller.on_nav_update(Amount::new(dec!(100_000)));

    // Margin snapshot with 85% utilization (initial_margin=85000, net_liq=100000)
    let snapshot = MarginSnapshot {
        account_id: "DU1234567".into(),
        initial_margin: Amount::new(dec!(85_000)),
        maintenance_margin: Amount::new(dec!(70_000)),
        excess_liquidity: Amount::new(dec!(15_000)),
        buying_power: Amount::new(dec!(30_000)),
        sma: None,
        available_funds: Amount::new(dec!(15_000)),
        net_liquidation: Amount::new(dec!(100_000)),
        timestamp: chrono::Utc::now(),
    };

    controller.on_margin_update(snapshot);

    // Try to place an order — should be rejected due to margin > 0.80
    let symbol = Symbol::new("AAPL").context("invalid symbol")?;
    let intention = OrderIntention {
        strategy_id: StrategyId::new("test-strategy").context("invalid strategy id")?,
        request: OrderRequest {
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(10)).context("invalid quantity")?,
            limit_price: Some(Price::new(dec!(150.00))),
            stop_price: None,
            time_in_force: TimeInForce::Day,
        },
        reason: None,
    };

    let mut tickers = HashMap::new();
    tickers.insert(
        symbol.clone(),
        ingot_core::TickerSnapshot {
            symbol,
            bid: Price::new(dec!(149.90)),
            ask: Price::new(dec!(150.10)),
            last: Price::new(dec!(150.00)),
            volume_24h: Quantity::new(dec!(1_000_000)).context("invalid quantity")?,
            timestamp: chrono::Utc::now(),
        },
    );

    let decision = controller.check_intention(&intention, &tickers);
    assert!(
        matches!(decision, RiskDecision::Rejected { .. }),
        "expected rejection due to margin utilization exceeding 80%, got: {decision}"
    );

    Ok(())
}

// ── Test 10: rollover scan to intention generation ──

#[test]
fn test_rollover_scan_to_intention_generation() -> anyhow::Result<()> {
    let config = RolloverConfig {
        days_before_expiry: 5,
        max_concurrent_rollovers: 3,
        use_limit_orders: false,
        limit_offset_bps: dec!(0),
    };
    let monitor = RolloverMonitor::new(config);

    let near_symbol = Symbol::new("ESM26").context("invalid symbol")?;
    let far_symbol = Symbol::new("ESU26").context("invalid symbol")?;

    // Near month: expiring in 3 days (within the 5-day window)
    let today = chrono::NaiveDate::from_ymd_opt(2026, 6, 17).context("invalid date")?;
    let near_expiry = chrono::NaiveDate::from_ymd_opt(2026, 6, 20).context("invalid date")?;
    let far_expiry = chrono::NaiveDate::from_ymd_opt(2026, 9, 18).context("invalid date")?;

    let near_instrument = Instrument {
        symbol: near_symbol.clone(),
        asset_class: AssetClass::Future,
        exchange: Exchange::IBKR,
        base_currency: Currency::USD,
        quote_currency: Currency::USD,
        tick_size: Price::new(dec!(0.25)),
        display_name: SmolStr::new("E-mini S&P 500 Jun 2026"),
        details: InstrumentDetails::Future {
            underlying: Some(Symbol::new("ES").context("invalid symbol")?),
            expiry: near_expiry,
            multiplier: dec!(50),
            settlement: SettlementType::Cash,
        },
    };

    let far_instrument = Instrument {
        symbol: far_symbol.clone(),
        asset_class: AssetClass::Future,
        exchange: Exchange::IBKR,
        base_currency: Currency::USD,
        quote_currency: Currency::USD,
        tick_size: Price::new(dec!(0.25)),
        display_name: SmolStr::new("E-mini S&P 500 Sep 2026"),
        details: InstrumentDetails::Future {
            underlying: Some(Symbol::new("ES").context("invalid symbol")?),
            expiry: far_expiry,
            multiplier: dec!(50),
            settlement: SettlementType::Cash,
        },
    };

    let registry = ingot_core::InstrumentRegistry::new(vec![near_instrument, far_instrument]);

    let mut positions = HashMap::new();
    positions.insert(
        near_symbol.clone(),
        ingot_core::Position {
            symbol: near_symbol.clone(),
            side: OrderSide::Buy,
            quantity: Quantity::new(dec!(2)).context("invalid quantity")?,
            average_entry_price: Price::new(dec!(5000.00)),
            unrealized_pnl: None,
            liquidation_price: None,
        },
    );

    let plans = monitor.scan_for_rollovers(&positions, &registry, today);

    assert_eq!(plans.len(), 1);
    let plan = &plans[0];
    assert_eq!(plan.near_symbol, near_symbol);
    assert_eq!(plan.far_symbol, far_symbol);
    assert_eq!(
        plan.quantity,
        Quantity::new(dec!(2)).context("invalid quantity")?
    );
    assert_eq!(plan.side, OrderSide::Buy);
    assert_eq!(plan.expiry_date, near_expiry);

    Ok(())
}

// ── Test 11: exhaustive engine event variants ──

#[test]
fn test_engine_event_variants_exhaustive() -> anyhow::Result<()> {
    use ingot_core::OrderId;

    let symbol = Symbol::new("AAPL").context("invalid symbol")?;
    let now = chrono::Utc::now();

    let events: Vec<EngineEvent> = vec![
        EngineEvent::Ticker(TickerSnapshot {
            symbol: symbol.clone(),
            bid: Price::new(dec!(150.00)),
            ask: Price::new(dec!(150.10)),
            last: Price::new(dec!(150.05)),
            volume_24h: Quantity::new(dec!(1_000_000)).context("invalid quantity")?,
            timestamp: now,
        }),
        EngineEvent::OrderBook(OrderBookSnapshot {
            symbol: symbol.clone(),
            bids: vec![],
            asks: vec![],
            timestamp: now,
        }),
        EngineEvent::Fill(OrderFill {
            order_id: OrderId::new("fill-1").context("invalid order id")?,
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(150.00)),
            fill_quantity: Quantity::new(dec!(10)).context("invalid quantity")?,
            fee: Amount::new(dec!(1.50)),
            fee_currency: Currency::USD,
            timestamp: now,
            trade_id: None,
        }),
        EngineEvent::MarginUpdate(MarginSnapshot {
            account_id: "DU1234567".into(),
            initial_margin: Amount::new(dec!(50_000)),
            maintenance_margin: Amount::new(dec!(40_000)),
            excess_liquidity: Amount::new(dec!(60_000)),
            buying_power: Amount::new(dec!(200_000)),
            sma: None,
            available_funds: Amount::new(dec!(60_000)),
            net_liquidation: Amount::new(dec!(100_000)),
            timestamp: now,
        }),
        EngineEvent::ScheduleTrigger(StrategyId::new("test-strat").context("invalid strategy id")?),
        EngineEvent::RolloverTriggered(RolloverPlan {
            near_symbol: Symbol::new("ESM26").context("invalid symbol")?,
            far_symbol: Symbol::new("ESU26").context("invalid symbol")?,
            quantity: Quantity::new(dec!(1)).context("invalid quantity")?,
            side: OrderSide::Buy,
            expiry_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 20).context("invalid date")?,
            planned_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 17).context("invalid date")?,
        }),
        EngineEvent::RolloverCompleted(symbol.clone()),
        EngineEvent::RolloverFailed {
            near_symbol: symbol.clone(),
            reason: SmolStr::new("test failure"),
        },
        EngineEvent::KillSwitch,
        EngineEvent::Shutdown,
    ];

    // Exhaustive match — no wildcard `_`. Adding a variant forces a compile error.
    for event in &events {
        let _label = match event {
            EngineEvent::Ticker(_) => "ticker",
            EngineEvent::OrderBook(_) => "orderbook",
            EngineEvent::Fill(_) => "fill",
            EngineEvent::MarginUpdate(_) => "margin",
            EngineEvent::ScheduleTrigger(_) => "schedule",
            EngineEvent::RolloverTriggered(_) => "rollover_triggered",
            EngineEvent::RolloverCompleted(_) => "rollover_completed",
            EngineEvent::RolloverFailed { .. } => "rollover_failed",
            EngineEvent::KillSwitch => "killswitch",
            EngineEvent::Shutdown => "shutdown",
        };
    }

    assert_eq!(events.len(), 10);

    Ok(())
}
