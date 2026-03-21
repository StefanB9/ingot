use ingot_primitives::{Amount, Currency};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub currency: Currency,
    pub total: Amount,
    pub available: Amount,
    pub held: Amount,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_balance_construction() {
        let b = Balance {
            currency: Currency::USD,
            total: Amount::new(dec!(10000.0)),
            available: Amount::new(dec!(8000.0)),
            held: Amount::new(dec!(2000.0)),
        };
        assert_eq!(b.currency, Currency::USD);
        assert_eq!(b.total.value(), dec!(10000.0));
        assert_eq!(b.available.value(), dec!(8000.0));
        assert_eq!(b.held.value(), dec!(2000.0));
    }

    #[test]
    fn test_balance_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let b = Balance {
            currency: Currency::BTC,
            total: Amount::new(dec!(1.5)),
            available: Amount::new(dec!(1.0)),
            held: Amount::new(dec!(0.5)),
        };
        let json = serde_json::to_string(&b)?;
        let deserialized: Balance = serde_json::from_str(&json)?;
        assert_eq!(b, deserialized);
        Ok(())
    }
}
