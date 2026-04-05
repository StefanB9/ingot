use std::{collections::HashMap, fmt};

use chrono::NaiveDate;
use ingot_core::{
    Instrument, InstrumentDetails, InstrumentRegistry, OrderFill, OrderRequest, Position,
};
use ingot_primitives::{AssetClass, OrderSide, OrderType, Quantity, Symbol, TimeInForce};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{
    error::EngineError,
    types::{OrderIntention, StrategyId},
};

/// Configuration for automatic derivative rollover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloverConfig {
    /// How many calendar days before expiry to trigger a rollover scan.
    pub days_before_expiry: u32,
    /// Maximum number of simultaneous active rollovers.
    pub max_concurrent_rollovers: usize,
    /// Use limit orders instead of market orders for rollover legs.
    pub use_limit_orders: bool,
    /// Basis-point offset for limit order pricing (only used when
    /// `use_limit_orders` is true).
    pub limit_offset_bps: Decimal,
}

impl Default for RolloverConfig {
    fn default() -> Self {
        Self {
            days_before_expiry: 14,
            max_concurrent_rollovers: 5,
            use_limit_orders: false,
            limit_offset_bps: Decimal::ZERO,
        }
    }
}

/// A planned rollover from a near-month to a far-month contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloverPlan {
    pub near_symbol: Symbol,
    pub far_symbol: Symbol,
    pub quantity: Quantity,
    pub side: OrderSide,
    pub expiry_date: NaiveDate,
    pub planned_date: NaiveDate,
}

/// State of an active rollover through the close/open lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RolloverState {
    Planned,
    ClosingNearMonth,
    NearMonthClosed,
    OpeningFarMonth,
    Complete,
    Failed,
}

impl fmt::Display for RolloverState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planned => f.write_str("planned"),
            Self::ClosingNearMonth => f.write_str("closing_near_month"),
            Self::NearMonthClosed => f.write_str("near_month_closed"),
            Self::OpeningFarMonth => f.write_str("opening_far_month"),
            Self::Complete => f.write_str("complete"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

/// Internal tracking for an in-flight rollover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveRollover {
    pub plan: RolloverPlan,
    pub state: RolloverState,
}

/// Monitors derivative positions for upcoming expiry and manages the
/// close/open rollover lifecycle.
pub struct RolloverMonitor {
    config: RolloverConfig,
    active: HashMap<Symbol, ActiveRollover>,
}

impl RolloverMonitor {
    pub fn new(config: RolloverConfig) -> Self {
        Self {
            config,
            active: HashMap::new(),
        }
    }

    /// Scan positions for instruments expiring within the rollover window.
    /// Returns plans for positions not already being rolled.
    pub fn scan_for_rollovers(
        &self,
        positions: &HashMap<Symbol, Position>,
        registry: &InstrumentRegistry,
        today: NaiveDate,
    ) -> Vec<RolloverPlan> {
        let mut plans = Vec::new();
        let window = chrono::Days::new(u64::from(self.config.days_before_expiry));

        for (symbol, position) in positions {
            // Skip if already rolling
            if self.active.contains_key(symbol) {
                continue;
            }

            let Some(instrument) = registry.get(symbol) else {
                continue;
            };

            let Some(expiry) = expiry_date(instrument) else {
                continue;
            };

            // Check if expiry is within the rollover window
            let deadline = today.checked_add_days(window);
            let Some(deadline) = deadline else { continue };

            if expiry > deadline || expiry <= today {
                continue;
            }

            // Try to find the far month contract
            if let Ok(far_symbol) = Self::find_far_month(instrument, registry) {
                plans.push(RolloverPlan {
                    near_symbol: symbol.clone(),
                    far_symbol,
                    quantity: position.quantity,
                    side: position.side,
                    expiry_date: expiry,
                    planned_date: today,
                });
            }
        }

        plans
    }

    /// Find the far month contract by searching the registry for instruments
    /// with the same underlying (or same base/quote for crypto futures),
    /// same asset class, and the nearest expiry strictly after the near
    /// contract's expiry.
    pub fn find_far_month(
        near: &Instrument,
        registry: &InstrumentRegistry,
    ) -> Result<Symbol, EngineError> {
        let near_expiry =
            expiry_date(near).ok_or_else(|| EngineError::RolloverFarMonthNotFound {
                near_symbol: near.symbol.clone(),
            })?;

        let candidates = registry.list_by_asset_class(near.asset_class);
        let mut best: Option<(Symbol, NaiveDate)> = None;

        for sym in candidates {
            if *sym == near.symbol {
                continue;
            }

            let Some(candidate) = registry.get(sym) else {
                continue;
            };

            let Some(candidate_expiry) = expiry_date(candidate) else {
                continue;
            };

            if candidate_expiry <= near_expiry {
                continue;
            }

            // Match criteria depends on asset class
            if !is_same_product(near, candidate) {
                continue;
            }

            match &best {
                Some((_, best_expiry)) if candidate_expiry < *best_expiry => {
                    best = Some((sym.clone(), candidate_expiry));
                }
                None => {
                    best = Some((sym.clone(), candidate_expiry));
                }
                _ => {}
            }
        }

        best.map(|(sym, _)| sym)
            .ok_or_else(|| EngineError::RolloverFarMonthNotFound {
                near_symbol: near.symbol.clone(),
            })
    }

    /// Generate a close intention for the near month position.
    pub fn close_intention(
        plan: &RolloverPlan,
        strategy_id: &StrategyId,
        config: &RolloverConfig,
    ) -> OrderIntention {
        let close_side = plan.side.opposite();
        OrderIntention {
            strategy_id: strategy_id.clone(),
            request: OrderRequest {
                symbol: plan.near_symbol.clone(),
                side: close_side,
                order_type: if config.use_limit_orders {
                    OrderType::Limit
                } else {
                    OrderType::Market
                },
                quantity: plan.quantity,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::Day,
            },
            reason: Some(SmolStr::new(format!(
                "rollover close near month {} expiring {}",
                plan.near_symbol, plan.expiry_date
            ))),
        }
    }

    /// Generate an open intention for the far month position.
    pub fn open_intention(
        plan: &RolloverPlan,
        strategy_id: &StrategyId,
        config: &RolloverConfig,
    ) -> OrderIntention {
        OrderIntention {
            strategy_id: strategy_id.clone(),
            request: OrderRequest {
                symbol: plan.far_symbol.clone(),
                side: plan.side,
                order_type: if config.use_limit_orders {
                    OrderType::Limit
                } else {
                    OrderType::Market
                },
                quantity: plan.quantity,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::Day,
            },
            reason: Some(SmolStr::new(format!(
                "rollover open far month {} replacing {}",
                plan.far_symbol, plan.near_symbol
            ))),
        }
    }

    /// Begin tracking a rollover. Moves state to `ClosingNearMonth`.
    pub fn begin_rollover(&mut self, plan: RolloverPlan) -> Result<(), EngineError> {
        if self.active.len() >= self.config.max_concurrent_rollovers {
            return Err(EngineError::RolloverLimitExceeded {
                active: self.active.len(),
                max: self.config.max_concurrent_rollovers,
            });
        }
        let symbol = plan.near_symbol.clone();
        self.active.insert(
            symbol,
            ActiveRollover {
                plan,
                state: RolloverState::ClosingNearMonth,
            },
        );
        Ok(())
    }

    /// Advance the rollover state machine when a fill arrives.
    /// Returns the near symbol and new state if a tracked rollover advanced.
    pub fn on_fill(&mut self, fill: &OrderFill) -> Option<(Symbol, RolloverState)> {
        // Check if fill is for a near-month close
        if let Some(active) = self.active.get_mut(&fill.symbol) {
            if active.state == RolloverState::ClosingNearMonth {
                active.state = RolloverState::NearMonthClosed;
                return Some((fill.symbol.clone(), RolloverState::NearMonthClosed));
            }
        }

        // Check if fill is for a far-month open
        let near_symbol = self
            .active
            .iter()
            .find(|(_, ar)| {
                ar.plan.far_symbol == fill.symbol && ar.state == RolloverState::OpeningFarMonth
            })
            .map(|(sym, _)| sym.clone());

        if let Some(near) = near_symbol {
            self.active.remove(&near);
            return Some((near, RolloverState::Complete));
        }

        None
    }

    /// Mark a rollover as failed and remove from active tracking.
    pub fn fail_rollover(&mut self, near_symbol: &Symbol, _reason: &str) {
        self.active.remove(near_symbol);
    }

    /// Advance a rollover from `NearMonthClosed` to `OpeningFarMonth`.
    pub fn begin_far_month(&mut self, near_symbol: &Symbol) -> bool {
        if let Some(active) = self.active.get_mut(near_symbol) {
            if active.state == RolloverState::NearMonthClosed {
                active.state = RolloverState::OpeningFarMonth;
                return true;
            }
        }
        false
    }

    /// Read-only access to active rollovers.
    pub(crate) fn active_rollovers(&self) -> &HashMap<Symbol, ActiveRollover> {
        &self.active
    }

    /// Check if a symbol is currently being rolled.
    pub fn is_rolling(&self, symbol: &Symbol) -> bool {
        self.active.contains_key(symbol)
    }
}

/// Extract expiry date from an instrument's details. Returns `None` for
/// equities, forex, crypto spot, perpetual crypto futures, and bonds.
fn expiry_date(instrument: &Instrument) -> Option<NaiveDate> {
    match &instrument.details {
        InstrumentDetails::Future { expiry, .. } => Some(*expiry),
        InstrumentDetails::Option { expiry, .. } => Some(*expiry),
        InstrumentDetails::CryptoFuture {
            expiry: Some(dt), ..
        } => Some(dt.date_naive()),
        _ => None,
    }
}

/// Check whether two instruments represent the same product line
/// (same underlying for futures/options, same currency pair for crypto
/// futures).
fn is_same_product(a: &Instrument, b: &Instrument) -> bool {
    if a.asset_class != b.asset_class {
        return false;
    }
    match (&a.details, &b.details) {
        (
            InstrumentDetails::Future { underlying: ua, .. },
            InstrumentDetails::Future { underlying: ub, .. },
        ) => ua == ub,
        (
            InstrumentDetails::Option { underlying: ua, .. },
            InstrumentDetails::Option { underlying: ub, .. },
        ) => ua == ub,
        (InstrumentDetails::CryptoFuture { .. }, InstrumentDetails::CryptoFuture { .. }) => {
            a.exchange == b.exchange
                && a.base_currency == b.base_currency
                && a.quote_currency == b.quote_currency
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{NaiveDate, Utc};
    use ingot_core::{
        Instrument, InstrumentDetails, InstrumentRegistry, OrderFill, OrderId, Position,
    };
    use ingot_primitives::{
        Amount, AssetClass, Currency, Exchange, OptionRight, OptionStyle, OrderSide, OrderType,
        Price, Quantity, SettlementType, Symbol,
    };
    use proptest::prelude::*;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;

    // ── Test helpers ──────────────────────────────────────────────────

    fn sample_future(
        symbol_str: &str,
        expiry: NaiveDate,
        underlying: &str,
    ) -> Result<Instrument, Box<dyn std::error::Error>> {
        Ok(Instrument {
            symbol: Symbol::new(symbol_str)?,
            asset_class: AssetClass::Future,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.25)),
            display_name: SmolStr::new(symbol_str),
            details: InstrumentDetails::Future {
                underlying: Some(Symbol::new(underlying)?),
                expiry,
                multiplier: dec!(50),
                settlement: SettlementType::Cash,
            },
        })
    }

    fn sample_option(
        symbol_str: &str,
        expiry: NaiveDate,
        underlying: &str,
    ) -> Result<Instrument, Box<dyn std::error::Error>> {
        Ok(Instrument {
            symbol: Symbol::new(symbol_str)?,
            asset_class: AssetClass::Option,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.01)),
            display_name: SmolStr::new(symbol_str),
            details: InstrumentDetails::Option {
                underlying: Symbol::new(underlying)?,
                strike: Price::new(dec!(5000)),
                right: OptionRight::Call,
                expiry,
                multiplier: dec!(100),
                style: OptionStyle::American,
            },
        })
    }

    fn sample_equity(symbol_str: &str) -> Result<Instrument, Box<dyn std::error::Error>> {
        Ok(Instrument {
            symbol: Symbol::new(symbol_str)?,
            asset_class: AssetClass::Equity,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.01)),
            display_name: SmolStr::new(symbol_str),
            details: InstrumentDetails::Equity {
                isin: None,
                lot_size: Quantity::new(dec!(1))?,
                fractional: false,
            },
        })
    }

    fn sample_position(
        symbol_str: &str,
        side: OrderSide,
        qty: Decimal,
    ) -> Result<(Symbol, Position), Box<dyn std::error::Error>> {
        let sym = Symbol::new(symbol_str)?;
        let pos = Position {
            symbol: sym.clone(),
            side,
            quantity: Quantity::new(qty)?,
            average_entry_price: Price::new(dec!(5000)),
            unrealized_pnl: None,
            liquidation_price: None,
        };
        Ok((sym, pos))
    }

    fn sample_fill(
        symbol_str: &str,
        side: OrderSide,
    ) -> Result<OrderFill, Box<dyn std::error::Error>> {
        Ok(OrderFill {
            order_id: OrderId::new("ORD-ROLL-001")?,
            symbol: Symbol::new(symbol_str)?,
            side,
            fill_price: Price::new(dec!(5000)),
            fill_quantity: Quantity::new(dec!(10))?,
            fee: Amount::new(dec!(2.50)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        })
    }

    // ── Cycle 0: RolloverConfig ──────────────────────────────────────

    #[test]
    fn test_rollover_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = RolloverConfig {
            days_before_expiry: 10,
            max_concurrent_rollovers: 3,
            use_limit_orders: true,
            limit_offset_bps: dec!(5),
        };
        let json = serde_json::to_string(&config)?;
        let deserialized: RolloverConfig = serde_json::from_str(&json)?;
        assert_eq!(config, deserialized);
        Ok(())
    }

    #[test]
    fn test_rollover_config_defaults_sensible() {
        let config = RolloverConfig::default();
        assert_eq!(config.days_before_expiry, 14);
        assert_eq!(config.max_concurrent_rollovers, 5);
        assert!(!config.use_limit_orders);
        assert_eq!(config.limit_offset_bps, Decimal::ZERO);
    }

    // ── Cycle 1: RolloverState + RolloverPlan ────────────────────────

    #[test]
    fn test_rollover_state_display_all_variants() {
        assert_eq!(RolloverState::Planned.to_string(), "planned");
        assert_eq!(
            RolloverState::ClosingNearMonth.to_string(),
            "closing_near_month"
        );
        assert_eq!(
            RolloverState::NearMonthClosed.to_string(),
            "near_month_closed"
        );
        assert_eq!(
            RolloverState::OpeningFarMonth.to_string(),
            "opening_far_month"
        );
        assert_eq!(RolloverState::Complete.to_string(), "complete");
        assert_eq!(RolloverState::Failed.to_string(), "failed");
    }

    #[test]
    fn test_rollover_plan_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(10))?,
            side: OrderSide::Buy,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };
        let json = serde_json::to_string(&plan)?;
        let deserialized: RolloverPlan = serde_json::from_str(&json)?;
        assert_eq!(plan, deserialized);
        Ok(())
    }

    // ── Cycle 2: EngineEvent rollover variants ───────────────────────

    #[test]
    fn test_engine_event_rollover_variants() -> Result<(), Box<dyn std::error::Error>> {
        use crate::types::EngineEvent;

        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(5))?,
            side: OrderSide::Buy,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };

        let triggered = EngineEvent::RolloverTriggered(plan);
        assert!(matches!(triggered, EngineEvent::RolloverTriggered(_)));

        let completed = EngineEvent::RolloverCompleted(Symbol::new("ESM26")?);
        assert!(matches!(completed, EngineEvent::RolloverCompleted(_)));

        let failed = EngineEvent::RolloverFailed {
            near_symbol: Symbol::new("ESM26")?,
            reason: SmolStr::new("no far month found"),
        };
        assert!(matches!(failed, EngineEvent::RolloverFailed { .. }));

        Ok(())
    }

    // ── Cycle 4: scan_for_rollovers ──────────────────────────────────

    #[test]
    fn test_scan_no_positions() {
        let monitor = RolloverMonitor::new(RolloverConfig::default());
        let positions = HashMap::new();
        let registry = InstrumentRegistry::new(vec![]);
        let today = NaiveDate::from_ymd_opt(2026, 6, 1).ok_or("bad date").ok();
        let plans = monitor.scan_for_rollovers(&positions, &registry, today.unwrap_or_default());
        assert!(plans.is_empty());
    }

    #[test]
    fn test_scan_no_expiring() -> Result<(), Box<dyn std::error::Error>> {
        let equity = sample_equity("AAPL")?;
        let registry = InstrumentRegistry::new(vec![equity]);
        let (sym, pos) = sample_position("AAPL", OrderSide::Buy, dec!(100))?;
        let mut positions = HashMap::new();
        positions.insert(sym, pos);

        let monitor = RolloverMonitor::new(RolloverConfig::default());
        let today = NaiveDate::from_ymd_opt(2026, 6, 1).ok_or("bad date")?;
        let plans = monitor.scan_for_rollovers(&positions, &registry, today);
        assert!(plans.is_empty());
        Ok(())
    }

    #[test]
    fn test_scan_future_within_window() -> Result<(), Box<dyn std::error::Error>> {
        let today = NaiveDate::from_ymd_opt(2026, 6, 9).ok_or("bad date")?;
        let near_expiry = NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?;
        let far_expiry = NaiveDate::from_ymd_opt(2026, 9, 18).ok_or("bad date")?;

        let near = sample_future("ESM26", near_expiry, "ES")?;
        let far = sample_future("ESU26", far_expiry, "ES")?;
        let registry = InstrumentRegistry::new(vec![near, far]);

        let (sym, pos) = sample_position("ESM26", OrderSide::Buy, dec!(10))?;
        let mut positions = HashMap::new();
        positions.insert(sym, pos);

        let monitor = RolloverMonitor::new(RolloverConfig::default());
        let plans = monitor.scan_for_rollovers(&positions, &registry, today);

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].near_symbol.as_str(), "ESM26");
        assert_eq!(plans[0].far_symbol.as_str(), "ESU26");
        assert_eq!(plans[0].quantity, Quantity::new(dec!(10))?);
        assert_eq!(plans[0].side, OrderSide::Buy);
        assert_eq!(plans[0].expiry_date, near_expiry);
        Ok(())
    }

    #[test]
    fn test_scan_option_within_window() -> Result<(), Box<dyn std::error::Error>> {
        let today = NaiveDate::from_ymd_opt(2026, 6, 12).ok_or("bad date")?;
        let near_expiry = NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?;
        let far_expiry = NaiveDate::from_ymd_opt(2026, 7, 17).ok_or("bad date")?;

        let near = sample_option("SPX260619C5000", near_expiry, "SPX")?;
        let far = sample_option("SPX260717C5000", far_expiry, "SPX")?;
        let registry = InstrumentRegistry::new(vec![near, far]);

        let (sym, pos) = sample_position("SPX260619C5000", OrderSide::Buy, dec!(5))?;
        let mut positions = HashMap::new();
        positions.insert(sym, pos);

        let monitor = RolloverMonitor::new(RolloverConfig::default());
        let plans = monitor.scan_for_rollovers(&positions, &registry, today);

        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].far_symbol.as_str(), "SPX260717C5000");
        Ok(())
    }

    #[test]
    fn test_scan_equity_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let today = NaiveDate::from_ymd_opt(2026, 6, 1).ok_or("bad date")?;
        let near_expiry = NaiveDate::from_ymd_opt(2026, 6, 10).ok_or("bad date")?;

        let equity = sample_equity("AAPL")?;
        let future = sample_future("ESM26", near_expiry, "ES")?;
        let registry = InstrumentRegistry::new(vec![equity, future]);

        let (sym_eq, pos_eq) = sample_position("AAPL", OrderSide::Buy, dec!(100))?;
        let mut positions = HashMap::new();
        positions.insert(sym_eq, pos_eq);

        let monitor = RolloverMonitor::new(RolloverConfig::default());
        let plans = monitor.scan_for_rollovers(&positions, &registry, today);
        assert!(plans.is_empty());
        Ok(())
    }

    #[test]
    fn test_scan_already_rolling() -> Result<(), Box<dyn std::error::Error>> {
        let today = NaiveDate::from_ymd_opt(2026, 6, 9).ok_or("bad date")?;
        let near_expiry = NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?;
        let far_expiry = NaiveDate::from_ymd_opt(2026, 9, 18).ok_or("bad date")?;

        let near = sample_future("ESM26", near_expiry, "ES")?;
        let far = sample_future("ESU26", far_expiry, "ES")?;
        let registry = InstrumentRegistry::new(vec![near, far]);

        let (sym, pos) = sample_position("ESM26", OrderSide::Buy, dec!(10))?;
        let mut positions = HashMap::new();
        positions.insert(sym, pos);

        let mut monitor = RolloverMonitor::new(RolloverConfig::default());
        // Pre-populate as already rolling
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(10))?,
            side: OrderSide::Buy,
            expiry_date: near_expiry,
            planned_date: today,
        };
        monitor.begin_rollover(plan)?;

        let plans = monitor.scan_for_rollovers(&positions, &registry, today);
        assert!(plans.is_empty());
        Ok(())
    }

    // ── Cycle 5: find_far_month ──────────────────────────────────────

    #[test]
    fn test_find_far_month_futures() -> Result<(), Box<dyn std::error::Error>> {
        let jun = NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?;
        let sep = NaiveDate::from_ymd_opt(2026, 9, 18).ok_or("bad date")?;
        let dec = NaiveDate::from_ymd_opt(2026, 12, 18).ok_or("bad date")?;

        let near = sample_future("ESM26", jun, "ES")?;
        let mid = sample_future("ESU26", sep, "ES")?;
        let far = sample_future("ESZ26", dec, "ES")?;
        let registry = InstrumentRegistry::new(vec![near.clone(), mid, far]);

        let result = RolloverMonitor::find_far_month(&near, &registry)?;
        // Should find nearest after Jun → Sep
        assert_eq!(result.as_str(), "ESU26");
        Ok(())
    }

    #[test]
    fn test_find_far_month_not_found() -> Result<(), Box<dyn std::error::Error>> {
        let jun = NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?;
        let near = sample_future("ESM26", jun, "ES")?;
        // Registry has only the near contract
        let registry = InstrumentRegistry::new(vec![near.clone()]);

        let result = RolloverMonitor::find_far_month(&near, &registry);
        assert!(matches!(
            result,
            Err(EngineError::RolloverFarMonthNotFound { .. })
        ));
        Ok(())
    }

    // ── Cycle 6: Intention generation ────────────────────────────────

    #[test]
    fn test_close_intention_sell_for_long() -> Result<(), Box<dyn std::error::Error>> {
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(10))?,
            side: OrderSide::Buy,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };
        let strat = StrategyId::new("rollover")?;
        let config = RolloverConfig::default();

        let intention = RolloverMonitor::close_intention(&plan, &strat, &config);
        assert_eq!(intention.request.symbol.as_str(), "ESM26");
        assert_eq!(intention.request.side, OrderSide::Sell);
        assert_eq!(intention.request.order_type, OrderType::Market);
        assert_eq!(intention.request.quantity, Quantity::new(dec!(10))?);
        assert!(intention.reason.is_some());
        Ok(())
    }

    #[test]
    fn test_close_intention_buy_for_short() -> Result<(), Box<dyn std::error::Error>> {
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(5))?,
            side: OrderSide::Sell,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };
        let strat = StrategyId::new("rollover")?;
        let config = RolloverConfig::default();

        let intention = RolloverMonitor::close_intention(&plan, &strat, &config);
        assert_eq!(intention.request.side, OrderSide::Buy);
        Ok(())
    }

    #[test]
    fn test_open_intention_buy_for_long() -> Result<(), Box<dyn std::error::Error>> {
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(10))?,
            side: OrderSide::Buy,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };
        let strat = StrategyId::new("rollover")?;
        let config = RolloverConfig::default();

        let intention = RolloverMonitor::open_intention(&plan, &strat, &config);
        assert_eq!(intention.request.symbol.as_str(), "ESU26");
        assert_eq!(intention.request.side, OrderSide::Buy);
        assert_eq!(intention.request.order_type, OrderType::Market);
        assert_eq!(intention.request.quantity, Quantity::new(dec!(10))?);
        Ok(())
    }

    #[test]
    fn test_open_intention_sell_for_short() -> Result<(), Box<dyn std::error::Error>> {
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(5))?,
            side: OrderSide::Sell,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };
        let strat = StrategyId::new("rollover")?;
        let config = RolloverConfig::default();

        let intention = RolloverMonitor::open_intention(&plan, &strat, &config);
        assert_eq!(intention.request.side, OrderSide::Sell);
        Ok(())
    }

    // ── Cycle 7: on_fill state machine ───────────────────────────────

    #[test]
    fn test_on_fill_advances_close_to_near_month_closed() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut monitor = RolloverMonitor::new(RolloverConfig::default());
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(10))?,
            side: OrderSide::Buy,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };
        monitor.begin_rollover(plan)?;

        let fill = sample_fill("ESM26", OrderSide::Sell)?;
        let result = monitor.on_fill(&fill);
        assert_eq!(
            result,
            Some((Symbol::new("ESM26")?, RolloverState::NearMonthClosed))
        );

        // Verify state advanced
        let active = monitor.active_rollovers();
        assert_eq!(
            active.get(&Symbol::new("ESM26")?).map(|a| a.state),
            Some(RolloverState::NearMonthClosed)
        );
        Ok(())
    }

    #[test]
    fn test_on_fill_advances_open_to_complete() -> Result<(), Box<dyn std::error::Error>> {
        let mut monitor = RolloverMonitor::new(RolloverConfig::default());
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(10))?,
            side: OrderSide::Buy,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };
        monitor.begin_rollover(plan)?;

        // Advance to NearMonthClosed
        let close_fill = sample_fill("ESM26", OrderSide::Sell)?;
        monitor.on_fill(&close_fill);
        // Advance to OpeningFarMonth
        monitor.begin_far_month(&Symbol::new("ESM26")?);

        // Fill on far month
        let open_fill = sample_fill("ESU26", OrderSide::Buy)?;
        let result = monitor.on_fill(&open_fill);
        assert_eq!(
            result,
            Some((Symbol::new("ESM26")?, RolloverState::Complete))
        );

        // Verify removed from active
        assert!(monitor.active_rollovers().is_empty());
        Ok(())
    }

    #[test]
    fn test_on_fill_unrelated_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let mut monitor = RolloverMonitor::new(RolloverConfig::default());
        let plan = RolloverPlan {
            near_symbol: Symbol::new("ESM26")?,
            far_symbol: Symbol::new("ESU26")?,
            quantity: Quantity::new(dec!(10))?,
            side: OrderSide::Buy,
            expiry_date: NaiveDate::from_ymd_opt(2026, 6, 19).ok_or("bad date")?,
            planned_date: NaiveDate::from_ymd_opt(2026, 6, 5).ok_or("bad date")?,
        };
        monitor.begin_rollover(plan)?;

        // Unrelated fill
        let fill = sample_fill("AAPL", OrderSide::Buy)?;
        let result = monitor.on_fill(&fill);
        assert!(result.is_none());
        Ok(())
    }

    // ── Cycle 8: Proptest ────────────────────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_test_scan_only_finds_within_window(
            today_offset in 0u32..365u32,
            expiry_offset in 0u32..60u32,
            days_before in 1u32..30u32,
        ) {
            let base = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap_or_default();
            let today = base.checked_add_days(chrono::Days::new(u64::from(today_offset)))
                .unwrap_or(base);
            let expiry = today.checked_add_days(chrono::Days::new(u64::from(expiry_offset)))
                .unwrap_or(today);
            let far_expiry = expiry.checked_add_days(chrono::Days::new(90))
                .unwrap_or(expiry);

            // Build instruments
            let near_sym = Symbol::new("ESM26")
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
            let far_sym = Symbol::new("ESU26")
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;

            let near = Instrument {
                symbol: near_sym.clone(),
                asset_class: AssetClass::Future,
                exchange: Exchange::IBKR,
                base_currency: Currency::USD,
                quote_currency: Currency::USD,
                tick_size: Price::new(dec!(0.25)),
                display_name: SmolStr::new("ESM26"),
                details: InstrumentDetails::Future {
                    underlying: Some(Symbol::new("ES")
                        .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?),
                    expiry,
                    multiplier: dec!(50),
                    settlement: SettlementType::Cash,
                },
            };
            let far = Instrument {
                symbol: far_sym.clone(),
                asset_class: AssetClass::Future,
                exchange: Exchange::IBKR,
                base_currency: Currency::USD,
                quote_currency: Currency::USD,
                tick_size: Price::new(dec!(0.25)),
                display_name: SmolStr::new("ESU26"),
                details: InstrumentDetails::Future {
                    underlying: Some(Symbol::new("ES")
                        .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?),
                    expiry: far_expiry,
                    multiplier: dec!(50),
                    settlement: SettlementType::Cash,
                },
            };

            let registry = InstrumentRegistry::new(vec![near, far]);
            let qty = Quantity::new(dec!(10))
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
            let pos = Position {
                symbol: near_sym.clone(),
                side: OrderSide::Buy,
                quantity: qty,
                average_entry_price: Price::new(dec!(5000)),
                unrealized_pnl: None,
                liquidation_price: None,
            };
            let mut positions = HashMap::new();
            positions.insert(near_sym, pos);

            let config = RolloverConfig {
                days_before_expiry: days_before,
                ..RolloverConfig::default()
            };
            let monitor = RolloverMonitor::new(config);
            let plans = monitor.scan_for_rollovers(&positions, &registry, today);

            // If any plans found, verify expiry is within [today+1, today+days_before]
            for plan in &plans {
                let days_to_expiry = (plan.expiry_date - today).num_days();
                prop_assert!(days_to_expiry > 0, "expiry must be after today");
                prop_assert!(
                    days_to_expiry <= i64::from(days_before),
                    "expiry {} is {} days away, window is {}",
                    plan.expiry_date, days_to_expiry, days_before
                );
            }
        }
    }
}
