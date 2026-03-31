use std::collections::HashMap;

use ingot_core::{MarginSnapshot, OrderFill, Position, TickerSnapshot};
use ingot_primitives::{Amount, OrderSide, OrderType, Symbol};
use rust_decimal::Decimal;
use smol_str::SmolStr;

use crate::{
    config::RiskConfig,
    types::{OrderIntention, RiskDecision},
};

/// Stateful risk gatekeeper. Maintains running positions and NAV,
/// gates every `OrderIntention` against configured risk limits.
pub struct PortfolioController {
    config: RiskConfig,
    positions: HashMap<Symbol, Position>,
    current_nav: Amount,
    halted: bool,
    latest_margin: Option<MarginSnapshot>,
}

impl PortfolioController {
    pub fn new(config: RiskConfig) -> Self {
        Self {
            config,
            positions: HashMap::new(),
            current_nav: Amount::zero(),
            halted: false,
            latest_margin: None,
        }
    }

    /// Evaluate an order intention against all risk limits (fail-fast).
    ///
    /// Tickers are passed externally so the controller doesn't manage market
    /// data state. Takes `&mut self` because the NAV-below-stop-loss check
    /// auto-halts.
    pub fn check_intention(
        &mut self,
        intention: &OrderIntention,
        tickers: &HashMap<Symbol, TickerSnapshot>,
    ) -> RiskDecision {
        // 1. Halted check
        if self.halted {
            return RiskDecision::Rejected {
                reason: SmolStr::new("controller is halted"),
            };
        }

        // 2. Global stop-loss check (auto-halts)
        if self.current_nav <= self.config.global_stop_loss {
            self.halted = true;
            return RiskDecision::Rejected {
                reason: SmolStr::new(format!(
                    "NAV {} below stop-loss threshold {}",
                    self.current_nav, self.config.global_stop_loss
                )),
            };
        }

        // 2.5. Margin checks (if configured)
        if let Some(ref margin_config) = self.config.margin {
            if let Some(ref snapshot) = self.latest_margin {
                if snapshot.is_margin_call() {
                    self.halted = true;
                    return RiskDecision::Rejected {
                        reason: SmolStr::new("margin call — excess liquidity depleted"),
                    };
                }
                if let Ok(util) = snapshot.utilization() {
                    if util > margin_config.max_margin_utilization {
                        return RiskDecision::Rejected {
                            reason: SmolStr::new(format!(
                                "margin utilization {} exceeds max {}",
                                util, margin_config.max_margin_utilization
                            )),
                        };
                    }
                }
                if snapshot.excess_liquidity < margin_config.min_excess_liquidity {
                    return RiskDecision::Rejected {
                        reason: SmolStr::new(format!(
                            "excess liquidity {} below minimum {}",
                            snapshot.excess_liquidity, margin_config.min_excess_liquidity
                        )),
                    };
                }
            }
        }

        // 3. Resolve order price
        let resolved_price = match resolve_order_price(intention, tickers) {
            Ok(price) => price,
            Err(reason) => return RiskDecision::Rejected { reason },
        };

        // 4. Compute order notional
        let order_notional = resolved_price * intention.request.quantity;

        // 5. Max order value check
        if order_notional > self.config.max_order_value {
            return RiskDecision::Rejected {
                reason: SmolStr::new(format!(
                    "order value {} exceeds max {}",
                    order_notional, self.config.max_order_value
                )),
            };
        }

        // 6. Per-asset exposure check
        let current_value = self
            .positions
            .get(&intention.request.symbol)
            .map_or(Amount::zero(), |p| p.average_entry_price * p.quantity);

        let projected_value = match intention.request.side {
            OrderSide::Buy => current_value + order_notional,
            OrderSide::Sell => {
                let diff = current_value - order_notional;
                Amount::new(diff.value().abs())
            }
        };

        let ratio = projected_value.value() / self.current_nav.value();
        if ratio > self.config.max_asset_exposure.value() {
            return RiskDecision::Rejected {
                reason: SmolStr::new(format!(
                    "exposure {ratio} exceeds max {}",
                    self.config.max_asset_exposure
                )),
            };
        }

        // 7. All checks passed
        RiskDecision::Approved
    }

    /// Update internal position state from a fill.
    pub fn on_fill(&mut self, fill: &OrderFill) {
        if let Some(position) = self.positions.get_mut(&fill.symbol) {
            if position.side == fill.side {
                // Adding to position: weighted average entry price
                let old_notional = position.average_entry_price * position.quantity;
                let fill_notional = fill.fill_price * fill.fill_quantity;
                let new_qty = position.quantity + fill.fill_quantity;

                if new_qty.value() > Decimal::ZERO {
                    let new_avg_raw =
                        (old_notional.value() + fill_notional.value()) / new_qty.value();
                    position.average_entry_price = ingot_primitives::Price::new(new_avg_raw);
                    position.quantity = new_qty;
                }
            } else {
                // Reducing position
                let new_qty_raw = position.quantity.value() - fill.fill_quantity.value();
                if new_qty_raw <= Decimal::ZERO {
                    self.positions.remove(&fill.symbol);
                    return;
                }
                // Quantity::new validates >= 0, and we just checked > 0
                if let Ok(new_qty) = ingot_primitives::Quantity::new(new_qty_raw) {
                    position.quantity = new_qty;
                }
            }
        } else {
            // New position
            self.positions.insert(
                fill.symbol.clone(),
                Position {
                    symbol: fill.symbol.clone(),
                    side: fill.side,
                    quantity: fill.fill_quantity,
                    average_entry_price: fill.fill_price,
                    unrealized_pnl: None,
                    liquidation_price: None,
                },
            );
        }
    }

    /// Update NAV. Auto-halts if NAV drops below global stop-loss.
    pub fn on_nav_update(&mut self, nav: Amount) {
        self.current_nav = nav;
        if nav <= self.config.global_stop_loss {
            self.halted = true;
        }
    }

    /// Bulk sync positions from broker, replacing all internal state.
    pub fn on_position_update(&mut self, positions: &[Position]) {
        self.positions.clear();
        for position in positions {
            self.positions
                .insert(position.symbol.clone(), position.clone());
        }
    }

    /// Update the latest margin snapshot. Auto-halts on margin call.
    pub fn on_margin_update(&mut self, snapshot: MarginSnapshot) {
        if snapshot.is_margin_call() {
            tracing::warn!("margin call detected — auto-halting controller");
            self.halted = true;
        }
        if let Some(ref margin_config) = self.config.margin {
            if let Ok(util) = snapshot.utilization() {
                if util > margin_config.warn_margin_utilization {
                    tracing::warn!(
                        "margin utilization {} exceeds warning threshold {}",
                        util,
                        margin_config.warn_margin_utilization
                    );
                }
            }
        }
        self.latest_margin = Some(snapshot);
    }

    /// Read-only access to the latest margin snapshot.
    pub fn latest_margin(&self) -> Option<&MarginSnapshot> {
        self.latest_margin.as_ref()
    }

    /// Manually halt the controller (e.g., from kill switch).
    pub fn halt(&mut self) {
        self.halted = true;
    }

    pub fn is_halted(&self) -> bool {
        self.halted
    }

    pub fn current_nav(&self) -> Amount {
        self.current_nav
    }

    pub fn positions(&self) -> &HashMap<Symbol, Position> {
        &self.positions
    }
}

/// Resolve the effective price for an order intention.
/// Limit orders use their `limit_price`; market orders use the ticker.
fn resolve_order_price(
    intention: &OrderIntention,
    tickers: &HashMap<Symbol, TickerSnapshot>,
) -> Result<ingot_primitives::Price, SmolStr> {
    if let Some(limit_price) = intention.request.limit_price {
        return Ok(limit_price);
    }

    match intention.request.order_type {
        OrderType::Market => {
            let ticker = tickers.get(&intention.request.symbol).ok_or_else(|| {
                SmolStr::new(format!(
                    "no ticker available for {}",
                    intention.request.symbol
                ))
            })?;
            match intention.request.side {
                OrderSide::Buy => Ok(ticker.ask),
                OrderSide::Sell => Ok(ticker.bid),
            }
        }
        // For non-market orders without a limit_price, reject
        _ => Err(SmolStr::new(format!(
            "no price available for {} order on {}",
            intention.request.order_type, intention.request.symbol
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use ingot_core::{MarginSnapshot, OrderFill, OrderId, OrderRequest, Position, TickerSnapshot};
    use ingot_primitives::{
        Amount, Currency, OrderSide, OrderType, Percentage, Price, Quantity, Symbol, TimeInForce,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::types::StrategyId;

    fn sample_risk_config() -> Result<RiskConfig, Box<dyn std::error::Error>> {
        Ok(RiskConfig {
            global_stop_loss: Amount::new(dec!(10_000)),
            max_currency_exposure: Percentage::new(dec!(0.40))?,
            max_asset_exposure: Percentage::new(dec!(0.20))?,
            max_order_value: Amount::new(dec!(50_000)),
            margin: None,
            rollover: None,
        })
    }

    fn make_small_limit_buy() -> Result<OrderIntention, Box<dyn std::error::Error>> {
        Ok(OrderIntention {
            strategy_id: StrategyId::new("test")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: Quantity::new(dec!(0.01))?,
                limit_price: Some(Price::new(dec!(67_000))),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: None,
        })
    }

    fn make_ticker(symbol: &Symbol) -> TickerSnapshot {
        TickerSnapshot {
            symbol: symbol.clone(),
            bid: Price::new(dec!(66_990)),
            ask: Price::new(dec!(67_010)),
            last: Price::new(dec!(67_000)),
            volume_24h: Quantity::zero(),
            timestamp: Utc::now(),
        }
    }

    // ── Construction ───────────────────────────────────────────────────

    #[test]
    fn test_controller_new_default_state() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?;
        let controller = PortfolioController::new(config);
        assert_eq!(controller.current_nav(), Amount::zero());
        assert!(!controller.is_halted());
        Ok(())
    }

    // ── Approved ───────────────────────────────────────────────────────

    #[test]
    fn test_check_intention_approved_within_limits() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?;
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));

        let symbol = Symbol::new("XXBTZUSD")?;
        let intention = OrderIntention {
            strategy_id: StrategyId::new("test")?,
            request: OrderRequest {
                symbol: symbol.clone(),
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: Quantity::new(dec!(0.1))?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: None,
        };

        let mut tickers = HashMap::new();
        tickers.insert(symbol, make_ticker(&Symbol::new("XXBTZUSD")?));

        let decision = controller.check_intention(&intention, &tickers);
        assert_eq!(decision, RiskDecision::Approved);
        Ok(())
    }

    // ── Rejections ─────────────────────────────────────────────────────

    #[test]
    fn test_check_intention_rejected_halted() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?;
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));
        controller.halt();

        let intention = make_small_limit_buy()?;
        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);
        assert!(
            matches!(decision, RiskDecision::Rejected { ref reason } if reason.contains("halted"))
        );
        Ok(())
    }

    #[test]
    fn test_check_intention_rejected_nav_below_stop_loss() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = sample_risk_config()?; // global_stop_loss = 10_000
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(5_000)));

        let intention = make_small_limit_buy()?;
        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);

        assert!(matches!(decision, RiskDecision::Rejected { .. }));
        assert!(controller.is_halted());
        Ok(())
    }

    #[test]
    fn test_check_intention_rejected_exposure_limit() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?; // max_asset_exposure = 0.20
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));

        // 0.4 BTC at 67k = 26,800 → 26.8% > 20%
        let intention = OrderIntention {
            strategy_id: StrategyId::new("test")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: Quantity::new(dec!(0.4))?,
                limit_price: Some(Price::new(dec!(67_000))),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: None,
        };

        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);
        assert!(
            matches!(decision, RiskDecision::Rejected { ref reason } if reason.contains("exposure"))
        );
        Ok(())
    }

    #[test]
    fn test_check_intention_rejected_max_order_value() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?; // max_order_value = 50_000
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(1_000_000)));

        // 1.0 BTC at 67k = 67,000 > 50,000
        let intention = OrderIntention {
            strategy_id: StrategyId::new("test")?,
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

        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);
        assert!(
            matches!(decision, RiskDecision::Rejected { ref reason } if reason.contains("order value"))
        );
        Ok(())
    }

    // ── State mutations ────────────────────────────────────────────────

    #[test]
    fn test_on_fill_updates_positions() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?;
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));

        // Fill: buy 0.2 BTC at 67k → position value = 13,400
        let fill = OrderFill {
            order_id: OrderId::new("ORD-001")?,
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67_000)),
            fill_quantity: Quantity::new(dec!(0.2))?,
            fee: Amount::new(dec!(1)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };
        controller.on_fill(&fill);

        // Now try adding 0.15 BTC → cumulative 0.35 * 67k = 23,450 → 23.45% > 20%
        let intention = OrderIntention {
            strategy_id: StrategyId::new("test")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: Quantity::new(dec!(0.15))?,
                limit_price: Some(Price::new(dec!(67_000))),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: None,
        };
        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);
        assert!(matches!(decision, RiskDecision::Rejected { .. }));
        Ok(())
    }

    #[test]
    fn test_on_nav_update() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?; // global_stop_loss = 10_000
        let mut controller = PortfolioController::new(config);

        controller.on_nav_update(Amount::new(dec!(50_000)));
        assert!(!controller.is_halted());
        assert_eq!(controller.current_nav(), Amount::new(dec!(50_000)));

        controller.on_nav_update(Amount::new(dec!(9_000)));
        assert!(controller.is_halted());
        assert_eq!(controller.current_nav(), Amount::new(dec!(9_000)));
        Ok(())
    }

    #[test]
    fn test_on_position_update() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?; // max_asset_exposure = 0.20
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));

        // Sync 0.25 BTC position from broker
        let positions = vec![Position {
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            quantity: Quantity::new(dec!(0.25))?,
            average_entry_price: Price::new(dec!(67_000)),
            unrealized_pnl: None,
            liquidation_price: None,
        }];
        controller.on_position_update(&positions);

        // Adding 0.1 BTC → cumulative 0.35 * 67k = 23,450 → 23.45% > 20%
        let intention = OrderIntention {
            strategy_id: StrategyId::new("test")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: Quantity::new(dec!(0.1))?,
                limit_price: Some(Price::new(dec!(67_000))),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            reason: None,
        };
        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);
        assert!(matches!(decision, RiskDecision::Rejected { .. }));
        Ok(())
    }

    #[test]
    fn test_halt_and_is_halted() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?;
        let mut controller = PortfolioController::new(config);
        assert!(!controller.is_halted());

        controller.halt();
        assert!(controller.is_halted());
        Ok(())
    }

    // ── Margin tests ───────────────────────────────────────────────────

    fn sample_margin_risk_config() -> Result<RiskConfig, Box<dyn std::error::Error>> {
        Ok(RiskConfig {
            global_stop_loss: Amount::new(dec!(10_000)),
            max_currency_exposure: Percentage::new(dec!(0.40))?,
            max_asset_exposure: Percentage::new(dec!(0.20))?,
            max_order_value: Amount::new(dec!(50_000)),
            margin: Some(crate::config::MarginConfig {
                max_margin_utilization: Percentage::new(dec!(0.80))?,
                warn_margin_utilization: Percentage::new(dec!(0.60))?,
                min_excess_liquidity: Amount::new(dec!(10_000)),
            }),
            rollover: None,
        })
    }

    fn sample_margin_snapshot(
        initial_margin: Decimal,
        net_liquidation: Decimal,
        excess_liquidity: Decimal,
    ) -> MarginSnapshot {
        MarginSnapshot {
            account_id: "U1234567".to_string(),
            initial_margin: Amount::new(initial_margin),
            maintenance_margin: Amount::new(dec!(30_000)),
            excess_liquidity: Amount::new(excess_liquidity),
            buying_power: Amount::new(dec!(200_000)),
            sma: None,
            available_funds: Amount::new(dec!(70_000)),
            net_liquidation: Amount::new(net_liquidation),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_on_margin_update_stores_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_margin_risk_config()?;
        let mut controller = PortfolioController::new(config);
        assert!(controller.latest_margin().is_none());

        let snapshot = sample_margin_snapshot(dec!(50_000), dec!(100_000), dec!(50_000));
        controller.on_margin_update(snapshot.clone());

        assert!(controller.latest_margin().is_some());
        assert_eq!(controller.latest_margin(), Some(&snapshot));
        Ok(())
    }

    #[test]
    fn test_check_intention_approved_with_margin_headroom() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = sample_margin_risk_config()?;
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));

        // 50% utilization, well within 80% max; 50k excess > 10k min
        let snapshot = sample_margin_snapshot(dec!(50_000), dec!(100_000), dec!(50_000));
        controller.on_margin_update(snapshot);

        let intention = make_small_limit_buy()?;
        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);
        assert_eq!(decision, RiskDecision::Approved);
        Ok(())
    }

    #[test]
    fn test_check_intention_rejected_margin_utilization() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = sample_margin_risk_config()?; // max = 80%
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));

        // 85% utilization > 80% max
        let snapshot = sample_margin_snapshot(dec!(85_000), dec!(100_000), dec!(15_000));
        controller.on_margin_update(snapshot);

        let intention = make_small_limit_buy()?;
        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);
        assert!(matches!(
            decision,
            RiskDecision::Rejected { ref reason } if reason.contains("margin utilization")
        ));
        Ok(())
    }

    #[test]
    fn test_check_intention_rejected_low_excess_liquidity() -> Result<(), Box<dyn std::error::Error>>
    {
        let config = sample_margin_risk_config()?; // min excess = 10_000
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));

        // 50% utilization (within limit), but only 5k excess < 10k min
        let snapshot = sample_margin_snapshot(dec!(50_000), dec!(100_000), dec!(5_000));
        controller.on_margin_update(snapshot);

        let intention = make_small_limit_buy()?;
        let tickers = HashMap::new();
        let decision = controller.check_intention(&intention, &tickers);
        assert!(matches!(
            decision,
            RiskDecision::Rejected { ref reason } if reason.contains("excess liquidity")
        ));
        Ok(())
    }

    #[test]
    fn test_margin_update_auto_halts_on_margin_call() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_margin_risk_config()?;
        let mut controller = PortfolioController::new(config);
        assert!(!controller.is_halted());

        // Excess liquidity <= 0 → margin call
        let snapshot = sample_margin_snapshot(dec!(95_000), dec!(100_000), dec!(-500));
        controller.on_margin_update(snapshot);

        assert!(controller.is_halted());
        Ok(())
    }

    #[test]
    fn test_no_margin_config_skips_margin_check() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?; // margin: None
        let mut controller = PortfolioController::new(config);
        controller.on_nav_update(Amount::new(dec!(100_000)));

        // Feed a margin snapshot with terrible numbers
        let snapshot = sample_margin_snapshot(dec!(95_000), dec!(100_000), dec!(-500));
        controller.on_margin_update(snapshot);

        // Note: on_margin_update auto-halts on margin call regardless of config.
        // But check_intention's margin block is skipped when config.margin is None.
        // Since on_margin_update halts, we need a non-margin-call snapshot to test
        // skip.
        let mut controller2 = PortfolioController::new(RiskConfig {
            global_stop_loss: Amount::new(dec!(10_000)),
            max_currency_exposure: Percentage::new(dec!(0.40))?,
            max_asset_exposure: Percentage::new(dec!(0.20))?,
            max_order_value: Amount::new(dec!(50_000)),
            margin: None,
            rollover: None,
        });
        controller2.on_nav_update(Amount::new(dec!(100_000)));

        // 95% utilization — would be rejected with margin config, but config.margin is
        // None
        let snapshot2 = sample_margin_snapshot(dec!(95_000), dec!(100_000), dec!(5_000));
        controller2.on_margin_update(snapshot2);

        let intention = make_small_limit_buy()?;
        let tickers = HashMap::new();
        let decision = controller2.check_intention(&intention, &tickers);
        assert_eq!(decision, RiskDecision::Approved);
        Ok(())
    }

    // ── Property-based tests ───────────────────────────────────────────

    mod proptests {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(1000))]

            #[test]
            fn prop_test_approved_orders_within_limits(
                nav_raw in 50_000i64..=500_000,
                qty_milli in 1i64..=50,
            ) {
                let config = sample_risk_config()
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
                let mut controller = PortfolioController::new(config);
                let nav = Amount::new(Decimal::from(nav_raw));
                controller.on_nav_update(nav);

                let price = dec!(67_000);
                let qty = Decimal::from(qty_milli) / dec!(1000);
                let notional = price * qty;

                // Guard: only test cases genuinely within limits
                prop_assume!(notional < dec!(50_000));
                prop_assume!(notional / Decimal::from(nav_raw) < dec!(0.20));

                let intention = OrderIntention {
                    strategy_id: StrategyId::new("prop")
                        .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                    request: OrderRequest {
                        symbol: Symbol::new("XXBTZUSD")
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                        side: OrderSide::Buy,
                        order_type: OrderType::Limit,
                        quantity: Quantity::new(qty)
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                        limit_price: Some(Price::new(price)),
                        stop_price: None,
                        time_in_force: TimeInForce::GoodTilCancelled,
                    },
                    reason: None,
                };

                let tickers = HashMap::new();
                let decision = controller.check_intention(&intention, &tickers);
                prop_assert_eq!(decision, RiskDecision::Approved);
            }

            #[test]
            fn prop_test_exposure_never_exceeds_limit(
                nav_raw in 100_000i64..=500_000,
                qty_millis in proptest::collection::vec(1i64..=30, 1..=10),
            ) {
                let config = sample_risk_config()
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
                let mut controller = PortfolioController::new(config);
                let nav = Amount::new(Decimal::from(nav_raw));
                controller.on_nav_update(nav);

                let price = dec!(67_000);
                let symbol = Symbol::new("XXBTZUSD")
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;

                for (i, &qty_m) in qty_millis.iter().enumerate() {
                    let qty = Decimal::from(qty_m) / dec!(1000);

                    let intention = OrderIntention {
                        strategy_id: StrategyId::new("prop")
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                        request: OrderRequest {
                            symbol: symbol.clone(),
                            side: OrderSide::Buy,
                            order_type: OrderType::Limit,
                            quantity: Quantity::new(qty)
                                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                            limit_price: Some(Price::new(price)),
                            stop_price: None,
                            time_in_force: TimeInForce::GoodTilCancelled,
                        },
                        reason: None,
                    };

                    let tickers = HashMap::new();
                    let decision = controller.check_intention(&intention, &tickers);

                    if decision == RiskDecision::Approved {
                        let fill = OrderFill {
                            order_id: OrderId::new(&format!("ORD-{i}"))
                                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                            symbol: symbol.clone(),
                            side: OrderSide::Buy,
                            fill_price: Price::new(price),
                            fill_quantity: Quantity::new(qty)
                                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                            fee: Amount::zero(),
                            fee_currency: Currency::USD,
                            timestamp: Utc::now(),
                            trade_id: None,
                        };
                        controller.on_fill(&fill);
                    }
                }

                // After all fills, an order at the full exposure limit should be rejected
                // since existing fills already consumed some exposure
                let max_exposure_qty = Decimal::from(nav_raw) * dec!(0.20) / price + dec!(0.001);
                if let Ok(q) = Quantity::new(max_exposure_qty) {
                    let big_intention = OrderIntention {
                        strategy_id: StrategyId::new("prop")
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                        request: OrderRequest {
                            symbol: symbol.clone(),
                            side: OrderSide::Buy,
                            order_type: OrderType::Limit,
                            quantity: q,
                            limit_price: Some(Price::new(price)),
                            stop_price: None,
                            time_in_force: TimeInForce::GoodTilCancelled,
                        },
                        reason: None,
                    };
                    let tickers = HashMap::new();
                    let decision = controller.check_intention(&big_intention, &tickers);
                    // With any accumulated fills, this must be rejected
                    if !qty_millis.is_empty() {
                        let is_rejected = matches!(decision, RiskDecision::Rejected { .. });
                        prop_assert!(is_rejected);
                    }
                }
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(1000))]

            #[test]
            fn prop_test_margin_check_never_approves_above_limit(
                utilization_pct in 81i64..=100,
                net_liq in 100_000i64..=500_000,
            ) {
                let config = sample_margin_risk_config()
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
                let mut controller = PortfolioController::new(config);
                let nav = Amount::new(Decimal::from(net_liq));
                controller.on_nav_update(nav);

                let net = Decimal::from(net_liq);
                let init = net * Decimal::from(utilization_pct) / dec!(100);
                let excess = net - init;

                let snapshot = sample_margin_snapshot(init, net, excess);
                controller.on_margin_update(snapshot);

                let intention = OrderIntention {
                    strategy_id: StrategyId::new("prop")
                        .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                    request: OrderRequest {
                        symbol: Symbol::new("XXBTZUSD")
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                        side: OrderSide::Buy,
                        order_type: OrderType::Limit,
                        quantity: Quantity::new(dec!(0.001))
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                        limit_price: Some(Price::new(dec!(67_000))),
                        stop_price: None,
                        time_in_force: TimeInForce::GoodTilCancelled,
                    },
                    reason: None,
                };

                let tickers = HashMap::new();
                let decision = controller.check_intention(&intention, &tickers);
                prop_assert!(
                    matches!(decision, RiskDecision::Rejected { .. }),
                    "expected rejection for utilization {}%, got {:?}",
                    utilization_pct,
                    decision
                );
            }
        }
    }
}
