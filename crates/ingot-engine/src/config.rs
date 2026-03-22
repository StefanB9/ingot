use std::time::Duration;

use ingot_primitives::{Amount, Currency, Exchange, Percentage};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{error::EngineError, types::StrategyId};

/// Top-level engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub risk: RiskConfig,
    pub base_currency: Currency,
    pub smart_order: SmartOrderConfig,
    pub exchange: Exchange,
    pub venue: SmolStr,
}

/// Risk management parameters for the portfolio controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Halt all trading if NAV drops below this amount.
    pub global_stop_loss: Amount,
    /// Maximum exposure per currency as a fraction of NAV (0.0–1.0).
    pub max_currency_exposure: Percentage,
    /// Maximum exposure per single asset as a fraction of NAV.
    pub max_asset_exposure: Percentage,
    /// Maximum single order size (quote currency value).
    pub max_order_value: Amount,
}

/// Configuration for smart limit order pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartOrderConfig {
    /// Use mid-price from order book for limit orders.
    pub use_mid_price: bool,
    /// Basis points offset from mid-price (positive = more aggressive).
    pub offset_bps: Decimal,
    /// Fallback to market order after this many milliseconds.
    pub fallback_timeout_ms: u64,
}

/// Per-strategy schedule configuration for interval-based timers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    pub strategy_id: StrategyId,
    /// Timer interval in milliseconds.
    pub interval_ms: u64,
}

impl ScheduleConfig {
    /// Create a new schedule config. Returns error if interval is zero.
    pub fn new(strategy_id: StrategyId, interval_ms: u64) -> Result<Self, EngineError> {
        if interval_ms == 0 {
            return Err(EngineError::InvalidScheduleInterval);
        }
        Ok(Self {
            strategy_id,
            interval_ms,
        })
    }

    /// Convert the interval to a `std::time::Duration`.
    pub fn interval_duration(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ingot_primitives::Exchange;
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;
    use crate::types::StrategyId;

    fn sample_risk_config() -> Result<RiskConfig, Box<dyn std::error::Error>> {
        Ok(RiskConfig {
            global_stop_loss: Amount::new(dec!(10000)),
            max_currency_exposure: Percentage::new(dec!(0.40))?,
            max_asset_exposure: Percentage::new(dec!(0.20))?,
            max_order_value: Amount::new(dec!(50000)),
        })
    }

    fn sample_smart_order_config() -> SmartOrderConfig {
        SmartOrderConfig {
            use_mid_price: true,
            offset_bps: dec!(5),
            fallback_timeout_ms: 30_000,
        }
    }

    fn sample_engine_config() -> Result<EngineConfig, Box<dyn std::error::Error>> {
        Ok(EngineConfig {
            risk: sample_risk_config()?,
            base_currency: Currency::USD,
            smart_order: sample_smart_order_config(),
            exchange: Exchange::Paper,
            venue: SmolStr::new("spot"),
        })
    }

    #[test]
    fn test_engine_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_engine_config()?;
        let json = serde_json::to_string(&config)?;
        let deserialized: EngineConfig = serde_json::from_str(&json)?;
        assert_eq!(deserialized.base_currency, Currency::USD);
        assert_eq!(deserialized.risk.global_stop_loss, Amount::new(dec!(10000)));
        assert!(deserialized.smart_order.use_mid_price);
        assert_eq!(deserialized.smart_order.fallback_timeout_ms, 30_000);
        assert_eq!(deserialized.exchange, Exchange::Paper);
        assert_eq!(deserialized.venue, SmolStr::new("spot"));
        Ok(())
    }

    #[test]
    fn test_risk_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_risk_config()?;
        let json = serde_json::to_string(&config)?;
        let deserialized: RiskConfig = serde_json::from_str(&json)?;
        assert_eq!(deserialized.global_stop_loss, Amount::new(dec!(10000)));
        assert_eq!(
            deserialized.max_currency_exposure,
            Percentage::new(dec!(0.40))?
        );
        assert_eq!(
            deserialized.max_asset_exposure,
            Percentage::new(dec!(0.20))?
        );
        assert_eq!(deserialized.max_order_value, Amount::new(dec!(50000)));
        Ok(())
    }

    #[test]
    fn test_smart_order_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_smart_order_config();
        let json = serde_json::to_string(&config)?;
        let deserialized: SmartOrderConfig = serde_json::from_str(&json)?;
        assert!(deserialized.use_mid_price);
        assert_eq!(deserialized.offset_bps, dec!(5));
        assert_eq!(deserialized.fallback_timeout_ms, 30_000);
        Ok(())
    }

    // ── ScheduleConfig ─────────────────────────────────────────────────

    #[test]
    fn test_schedule_config_construction() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("rebalancer")?;
        let config = ScheduleConfig::new(id.clone(), 5000)?;
        assert_eq!(config.strategy_id, id);
        assert_eq!(config.interval_ms, 5000);
        assert_eq!(config.interval_duration(), Duration::from_secs(5));

        // Zero interval rejected
        let id2 = StrategyId::new("zero")?;
        let result = ScheduleConfig::new(id2, 0);
        assert!(matches!(
            result,
            Err(crate::error::EngineError::InvalidScheduleInterval)
        ));
        Ok(())
    }

    #[test]
    fn test_schedule_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("momentum")?;
        let config = ScheduleConfig::new(id, 10_000)?;
        let json = serde_json::to_string(&config)?;
        let deserialized: ScheduleConfig = serde_json::from_str(&json)?;
        assert_eq!(deserialized.strategy_id, config.strategy_id);
        assert_eq!(deserialized.interval_ms, 10_000);
        Ok(())
    }
}
