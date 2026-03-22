use chrono::{DateTime, Utc};
use ingot_accounting::Transaction;
use ingot_core::OrderFill;
use ingot_primitives::Amount;
use serde::{Deserialize, Serialize};

/// A single point on the equity curve, recorded after each fill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquityPoint {
    pub timestamp: DateTime<Utc>,
    pub nav: Amount,
    pub cash: Amount,
    pub positions_value: Amount,
}

/// Complete results from a backtest run.
#[derive(Debug, Clone)]
pub struct BacktestResult {
    pub fills: Vec<OrderFill>,
    pub transactions: Vec<Transaction>,
    pub equity_curve: Vec<EquityPoint>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub initial_capital: Amount,
    pub final_capital: Amount,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_equity_point_construction() {
        let point = EquityPoint {
            timestamp: Utc::now(),
            nav: Amount::new(dec!(100000)),
            cash: Amount::new(dec!(50000)),
            positions_value: Amount::new(dec!(50000)),
        };
        assert_eq!(point.nav, Amount::new(dec!(100000)));
        assert_eq!(point.cash, Amount::new(dec!(50000)));
        assert_eq!(point.positions_value, Amount::new(dec!(50000)));
    }

    #[test]
    fn test_equity_point_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let point = EquityPoint {
            timestamp: DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")?.to_utc(),
            nav: Amount::new(dec!(100000)),
            cash: Amount::new(dec!(80000)),
            positions_value: Amount::new(dec!(20000)),
        };
        let json = serde_json::to_string(&point)?;
        let deserialized: EquityPoint = serde_json::from_str(&json)?;
        assert_eq!(deserialized, point);
        Ok(())
    }

    #[test]
    fn test_backtest_result_construction() {
        let now = Utc::now();
        let result = BacktestResult {
            fills: Vec::new(),
            transactions: Vec::new(),
            equity_curve: vec![EquityPoint {
                timestamp: now,
                nav: Amount::new(dec!(100000)),
                cash: Amount::new(dec!(100000)),
                positions_value: Amount::zero(),
            }],
            start_time: now,
            end_time: now,
            initial_capital: Amount::new(dec!(100000)),
            final_capital: Amount::new(dec!(100000)),
        };
        assert!(result.fills.is_empty());
        assert!(result.transactions.is_empty());
        assert_eq!(result.equity_curve.len(), 1);
        assert_eq!(result.initial_capital, result.final_capital);
    }
}
