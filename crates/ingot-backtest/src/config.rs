use ingot_engine::{RiskConfig, SmartOrderConfig};
use ingot_primitives::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Configuration for a backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// Initial cash balances per currency.
    pub initial_balances: Vec<(Currency, Decimal)>,
    /// Base currency for NAV calculation.
    pub base_currency: Currency,
    /// Slippage in basis points applied to fill prices.
    pub slippage_bps: Decimal,
    /// Maker fee in basis points.
    pub maker_fee_bps: Decimal,
    /// Taker fee in basis points.
    pub taker_fee_bps: Decimal,
    /// Probability of partial fills (0.0–1.0).
    pub partial_fill_probability: Decimal,
    /// RNG seed for deterministic simulation.
    pub rng_seed: u64,
    /// Risk management parameters.
    pub risk: RiskConfig,
    /// Smart limit order configuration.
    pub smart_order: SmartOrderConfig,
}

#[cfg(test)]
mod tests {
    use ingot_primitives::{Amount, Percentage};
    use rust_decimal_macros::dec;

    use super::*;

    fn sample_config() -> Result<BacktestConfig, Box<dyn std::error::Error>> {
        Ok(BacktestConfig {
            initial_balances: vec![(Currency::USD, dec!(100000)), (Currency::BTC, dec!(0))],
            base_currency: Currency::USD,
            slippage_bps: dec!(10),
            maker_fee_bps: dec!(16),
            taker_fee_bps: dec!(26),
            partial_fill_probability: dec!(0.0),
            rng_seed: 42,
            risk: RiskConfig {
                global_stop_loss: Amount::new(dec!(10000)),
                max_currency_exposure: Percentage::new(dec!(0.50))?,
                max_asset_exposure: Percentage::new(dec!(0.30))?,
                max_order_value: Amount::new(dec!(50000)),
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

    #[test]
    fn test_backtest_config_construction() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        assert_eq!(config.initial_balances.len(), 2);
        assert_eq!(config.base_currency, Currency::USD);
        assert_eq!(config.slippage_bps, dec!(10));
        assert_eq!(config.maker_fee_bps, dec!(16));
        assert_eq!(config.taker_fee_bps, dec!(26));
        assert_eq!(config.partial_fill_probability, dec!(0.0));
        assert_eq!(config.rng_seed, 42);
        assert_eq!(config.risk.global_stop_loss, Amount::new(dec!(10000)));
        assert!(!config.smart_order.use_mid_price);
        Ok(())
    }

    #[test]
    fn test_backtest_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let json = serde_json::to_string(&config)?;
        let deserialized: BacktestConfig = serde_json::from_str(&json)?;
        assert_eq!(deserialized.base_currency, config.base_currency);
        assert_eq!(deserialized.slippage_bps, config.slippage_bps);
        assert_eq!(deserialized.maker_fee_bps, config.maker_fee_bps);
        assert_eq!(deserialized.taker_fee_bps, config.taker_fee_bps);
        assert_eq!(
            deserialized.partial_fill_probability,
            config.partial_fill_probability
        );
        assert_eq!(deserialized.rng_seed, config.rng_seed);
        assert_eq!(
            deserialized.risk.global_stop_loss,
            config.risk.global_stop_loss
        );
        assert_eq!(
            deserialized.smart_order.use_mid_price,
            config.smart_order.use_mid_price
        );
        assert_eq!(deserialized.initial_balances.len(), 2);
        Ok(())
    }
}
