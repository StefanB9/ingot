use ingot_primitives::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountingConfig {
    pub base_currency: Currency,
    pub reconciliation_interval_secs: u64,
    pub minor_threshold_pct: Decimal,
    pub major_threshold_pct: Decimal,
}

impl Default for AccountingConfig {
    fn default() -> Self {
        Self {
            base_currency: Currency::USD,
            reconciliation_interval_secs: 300,
            minor_threshold_pct: Decimal::new(1, 3), // 0.001 = 0.1%
            major_threshold_pct: Decimal::new(1, 2), // 0.01  = 1%
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accounting_config_default() {
        let config = AccountingConfig::default();
        assert_eq!(config.base_currency, Currency::USD);
        assert_eq!(config.reconciliation_interval_secs, 300);
        assert_eq!(config.minor_threshold_pct, Decimal::new(1, 3));
        assert_eq!(config.major_threshold_pct, Decimal::new(1, 2));
    }

    #[test]
    fn test_accounting_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = AccountingConfig::default();
        let json = serde_json::to_string(&config)?;
        let deserialized: AccountingConfig = serde_json::from_str(&json)?;
        assert_eq!(deserialized.base_currency, config.base_currency);
        assert_eq!(
            deserialized.reconciliation_interval_secs,
            config.reconciliation_interval_secs
        );
        assert_eq!(deserialized.minor_threshold_pct, config.minor_threshold_pct);
        assert_eq!(deserialized.major_threshold_pct, config.major_threshold_pct);
        Ok(())
    }
}
