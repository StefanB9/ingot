use std::fmt;

use ingot_core::{MarginSnapshot, OrderBookSnapshot, OrderFill, OrderRequest, TickerSnapshot};
use ingot_primitives::Symbol;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::error::EngineError;

/// Unique identifier for a registered strategy.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StrategyId(SmolStr);

impl StrategyId {
    pub fn new(id: &str) -> Result<Self, EngineError> {
        if id.is_empty() {
            return Err(EngineError::EmptyStrategyId);
        }
        Ok(Self(SmolStr::new(id)))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for StrategyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

/// A strategy's request to trade — the controller evaluates this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderIntention {
    pub strategy_id: StrategyId,
    pub request: OrderRequest,
    pub reason: Option<SmolStr>,
}

/// Result of a risk check on an `OrderIntention`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskDecision {
    Approved,
    Rejected { reason: SmolStr },
}

impl fmt::Display for RiskDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Approved => f.write_str("approved"),
            Self::Rejected { reason } => write!(f, "rejected: {reason}"),
        }
    }
}

/// Events flowing through the engine event loop.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Ticker(TickerSnapshot),
    OrderBook(OrderBookSnapshot),
    Fill(OrderFill),
    MarginUpdate(MarginSnapshot),
    ScheduleTrigger(StrategyId),
    RolloverTriggered(crate::rollover::RolloverPlan),
    RolloverCompleted(Symbol),
    RolloverFailed {
        near_symbol: Symbol,
        reason: SmolStr,
    },
    KillSwitch,
    Shutdown,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_core::{MarginSnapshot, OrderId};
    use ingot_primitives::{
        Amount, Currency, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce,
    };
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;

    // ── StrategyId ──────────────────────────────────────────────────────

    #[test]
    fn test_strategy_id_valid() -> Result<(), EngineError> {
        let id = StrategyId::new("my-strat")?;
        assert_eq!(id.as_str(), "my-strat");
        Ok(())
    }

    #[test]
    fn test_strategy_id_empty_rejected() {
        let result = StrategyId::new("");
        assert!(matches!(result, Err(EngineError::EmptyStrategyId)));
    }

    #[test]
    fn test_strategy_id_display() -> Result<(), EngineError> {
        let id = StrategyId::new("alpha-v2")?;
        assert_eq!(id.to_string(), "alpha-v2");
        Ok(())
    }

    #[test]
    fn test_strategy_id_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("rebalancer")?;
        let json = serde_json::to_string(&id)?;
        let deserialized: StrategyId = serde_json::from_str(&json)?;
        assert_eq!(id, deserialized);
        Ok(())
    }

    // ── OrderIntention ──────────────────────────────────────────────────

    #[test]
    fn test_order_intention_construction() -> Result<(), Box<dyn std::error::Error>> {
        let intention = OrderIntention {
            strategy_id: StrategyId::new("momentum")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: Quantity::new(dec!(0.5))?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::ImmediateOrCancel,
            },
            reason: Some(SmolStr::new("signal triggered")),
        };
        assert_eq!(intention.strategy_id.as_str(), "momentum");
        assert_eq!(intention.request.side, OrderSide::Buy);
        assert_eq!(intention.reason.as_deref(), Some("signal triggered"));
        Ok(())
    }

    // ── RiskDecision ────────────────────────────────────────────────────

    #[test]
    fn test_risk_decision_approved_display() {
        let decision = RiskDecision::Approved;
        assert_eq!(decision.to_string(), "approved");
    }

    #[test]
    fn test_risk_decision_rejected_display() {
        let decision = RiskDecision::Rejected {
            reason: SmolStr::new("exposure limit exceeded"),
        };
        assert_eq!(decision.to_string(), "rejected: exposure limit exceeded");
    }

    // ── EngineEvent ─────────────────────────────────────────────────────

    #[test]
    fn test_engine_event_ticker_variant() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = TickerSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bid: Price::new(dec!(67000)),
            ask: Price::new(dec!(67010)),
            last: Price::new(dec!(67005)),
            volume_24h: Quantity::new(dec!(1234))?,
            timestamp: Utc::now(),
        };
        let event = EngineEvent::Ticker(snapshot);
        assert!(matches!(event, EngineEvent::Ticker(_)));
        Ok(())
    }

    #[test]
    fn test_engine_event_fill_variant() -> Result<(), Box<dyn std::error::Error>> {
        let fill = OrderFill {
            order_id: OrderId::new("ORD-001")?,
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67000)),
            fill_quantity: Quantity::new(dec!(1))?,
            fee: Amount::new(dec!(0.26)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };
        let event = EngineEvent::Fill(fill);
        assert!(matches!(event, EngineEvent::Fill(_)));
        Ok(())
    }

    #[test]
    fn test_engine_event_kill_switch_variant() {
        let event = EngineEvent::KillSwitch;
        assert!(matches!(event, EngineEvent::KillSwitch));
    }

    #[test]
    fn test_engine_event_shutdown_variant() {
        let event = EngineEvent::Shutdown;
        assert!(matches!(event, EngineEvent::Shutdown));
    }

    #[test]
    fn test_engine_event_schedule_trigger_variant() -> Result<(), EngineError> {
        let id = StrategyId::new("rebalancer")?;
        let event = EngineEvent::ScheduleTrigger(id);
        assert!(matches!(event, EngineEvent::ScheduleTrigger(_)));
        Ok(())
    }

    #[test]
    fn test_engine_event_margin_update_variant() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = MarginSnapshot {
            account_id: "U1234567".to_string(),
            initial_margin: Amount::new(dec!(50000)),
            maintenance_margin: Amount::new(dec!(30000)),
            excess_liquidity: Amount::new(dec!(50000)),
            buying_power: Amount::new(dec!(200000)),
            sma: None,
            available_funds: Amount::new(dec!(70000)),
            net_liquidation: Amount::new(dec!(100000)),
            timestamp: Utc::now(),
        };
        let event = EngineEvent::MarginUpdate(snapshot);
        assert!(matches!(event, EngineEvent::MarginUpdate(_)));
        Ok(())
    }

    #[test]
    fn test_engine_event_order_book_variant() -> Result<(), Box<dyn std::error::Error>> {
        let book = OrderBookSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bids: vec![],
            asks: vec![],
            timestamp: Utc::now(),
        };
        let event = EngineEvent::OrderBook(book);
        assert!(matches!(event, EngineEvent::OrderBook(_)));
        Ok(())
    }
}
