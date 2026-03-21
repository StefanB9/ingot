use chrono::{DateTime, Utc};
use ingot_primitives::{Amount, Currency, Price};
use serde::{Deserialize, Serialize};

/// Point-in-time NAV calculation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavSnapshot {
    pub timestamp: DateTime<Utc>,
    pub base_currency: Currency,
    pub total_nav: Amount,
    pub breakdown: Vec<NavBreakdownEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavBreakdownEntry {
    pub currency: Currency,
    pub native_balance: Amount,
    pub fx_rate: Price,
    pub base_currency_value: Amount,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_nav_snapshot_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = NavSnapshot {
            timestamp: Utc::now(),
            base_currency: Currency::USD,
            total_nav: Amount::new(dec!(100000)),
            breakdown: vec![NavBreakdownEntry {
                currency: Currency::BTC,
                native_balance: Amount::new(dec!(1)),
                fx_rate: Price::new(dec!(67000)),
                base_currency_value: Amount::new(dec!(67000)),
            }],
        };
        let json = serde_json::to_string(&snapshot)?;
        let deserialized: NavSnapshot = serde_json::from_str(&json)?;
        assert_eq!(snapshot, deserialized);
        Ok(())
    }

    #[test]
    fn test_nav_breakdown_entry_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let entry = NavBreakdownEntry {
            currency: Currency::ETH,
            native_balance: Amount::new(dec!(10)),
            fx_rate: Price::new(dec!(3500)),
            base_currency_value: Amount::new(dec!(35000)),
        };
        let json = serde_json::to_string(&entry)?;
        let deserialized: NavBreakdownEntry = serde_json::from_str(&json)?;
        assert_eq!(entry, deserialized);
        Ok(())
    }
}
