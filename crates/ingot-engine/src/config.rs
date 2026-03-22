use ingot_primitives::{Amount, Currency, Percentage};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Top-level engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub risk: RiskConfig,
    pub base_currency: Currency,
    pub smart_order: SmartOrderConfig,
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

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

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
}
