use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_primitives::{Amount, Percentage};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarginSnapshot {
    pub account_id: String,
    pub initial_margin: Amount,
    pub maintenance_margin: Amount,
    pub excess_liquidity: Amount,
    pub buying_power: Amount,
    pub sma: Option<Amount>,
    pub available_funds: Amount,
    pub net_liquidation: Amount,
    pub timestamp: DateTime<Utc>,
}

impl MarginSnapshot {
    /// Margin utilization ratio: initial_margin / net_liquidation.
    /// Returns 0% if net_liquidation is zero.
    pub fn utilization(&self) -> anyhow::Result<Percentage> {
        let net_liq = self.net_liquidation.value();
        if net_liq == Decimal::ZERO {
            return Percentage::new(Decimal::ZERO).context("failed to create zero percentage");
        }
        let ratio = self.initial_margin.value() / net_liq;
        Percentage::new(ratio).context("margin utilization out of range")
    }

    /// True when excess_liquidity <= 0 (margin call territory).
    pub fn is_margin_call(&self) -> bool {
        self.excess_liquidity.value() <= Decimal::ZERO
    }

    /// Available margin = excess_liquidity.
    pub fn available_margin(&self) -> Amount {
        self.excess_liquidity
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn sample_snapshot(
        initial_margin: Decimal,
        net_liquidation: Decimal,
        excess_liquidity: Decimal,
    ) -> MarginSnapshot {
        MarginSnapshot {
            account_id: "U1234567".to_string(),
            initial_margin: Amount::new(initial_margin),
            maintenance_margin: Amount::new(dec!(30000)),
            excess_liquidity: Amount::new(excess_liquidity),
            buying_power: Amount::new(dec!(200000)),
            sma: Some(Amount::new(dec!(80000))),
            available_funds: Amount::new(dec!(70000)),
            net_liquidation: Amount::new(net_liquidation),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_margin_utilization() -> anyhow::Result<()> {
        let snap = sample_snapshot(dec!(50000), dec!(100000), dec!(50000));
        let util = snap.utilization()?;
        assert_eq!(util.value(), dec!(0.5));
        Ok(())
    }

    #[test]
    fn test_margin_is_margin_call_true() -> anyhow::Result<()> {
        let snap = sample_snapshot(dec!(50000), dec!(100000), dec!(-100));
        assert!(snap.is_margin_call());
        Ok(())
    }

    #[test]
    fn test_margin_is_margin_call_false() -> anyhow::Result<()> {
        let snap = sample_snapshot(dec!(50000), dec!(100000), dec!(5000));
        assert!(!snap.is_margin_call());
        Ok(())
    }

    #[test]
    fn test_margin_available_margin() -> anyhow::Result<()> {
        let snap = sample_snapshot(dec!(50000), dec!(100000), dec!(25000));
        assert_eq!(snap.available_margin().value(), dec!(25000));
        Ok(())
    }

    #[test]
    fn test_margin_serde_roundtrip() -> anyhow::Result<()> {
        let snap = sample_snapshot(dec!(50000), dec!(100000), dec!(50000));
        let json = serde_json::to_string(&snap).context("serialize failed")?;
        let deserialized: MarginSnapshot =
            serde_json::from_str(&json).context("deserialize failed")?;
        assert_eq!(snap, deserialized);
        Ok(())
    }
}
