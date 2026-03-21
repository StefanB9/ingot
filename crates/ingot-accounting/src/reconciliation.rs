use std::fmt;

use chrono::{DateTime, Utc};
use ingot_primitives::{Amount, Currency, Exchange};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Discrepancy between ledger and broker balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discrepancy {
    pub currency: Currency,
    pub ledger_balance: Amount,
    pub broker_balance: Amount,
    pub difference: Amount,
    pub severity: DiscrepancySeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiscrepancySeverity {
    None,
    Minor,
    Major,
    Critical,
}

impl fmt::Display for DiscrepancySeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Minor => f.write_str("minor"),
            Self::Major => f.write_str("major"),
            Self::Critical => f.write_str("critical"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReconciliationStatus {
    Pass,
    Fail,
}

impl fmt::Display for ReconciliationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => f.write_str("pass"),
            Self::Fail => f.write_str("fail"),
        }
    }
}

/// Result of a reconciliation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub id: Uuid,
    pub exchange: Exchange,
    pub timestamp: DateTime<Utc>,
    pub discrepancies: Vec<Discrepancy>,
    pub status: ReconciliationStatus,
}

#[cfg(test)]
mod tests {
    use ingot_primitives::Amount;
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_discrepancy_severity_display() {
        assert_eq!(DiscrepancySeverity::None.to_string(), "none");
        assert_eq!(DiscrepancySeverity::Minor.to_string(), "minor");
        assert_eq!(DiscrepancySeverity::Major.to_string(), "major");
        assert_eq!(DiscrepancySeverity::Critical.to_string(), "critical");
    }

    #[test]
    fn test_reconciliation_status_display() {
        assert_eq!(ReconciliationStatus::Pass.to_string(), "pass");
        assert_eq!(ReconciliationStatus::Fail.to_string(), "fail");
    }

    #[test]
    fn test_reconciliation_result_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let result = ReconciliationResult {
            id: Uuid::nil(),
            exchange: Exchange::Kraken,
            timestamp: Utc::now(),
            discrepancies: vec![],
            status: ReconciliationStatus::Pass,
        };
        let json = serde_json::to_string(&result)?;
        let deserialized: ReconciliationResult = serde_json::from_str(&json)?;
        assert_eq!(result, deserialized);
        Ok(())
    }

    #[test]
    fn test_discrepancy_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let disc = Discrepancy {
            currency: Currency::USD,
            ledger_balance: Amount::new(dec!(1000)),
            broker_balance: Amount::new(dec!(999)),
            difference: Amount::new(dec!(1)),
            severity: DiscrepancySeverity::Minor,
        };
        let json = serde_json::to_string(&disc)?;
        let deserialized: Discrepancy = serde_json::from_str(&json)?;
        assert_eq!(disc, deserialized);
        Ok(())
    }
}
