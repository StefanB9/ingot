use ingot_primitives::{Amount, OrderSide, Price, Quantity, Symbol};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    pub side: OrderSide,
    pub quantity: Quantity,
    pub average_entry_price: Price,
    pub unrealized_pnl: Option<Amount>,
    pub liquidation_price: Option<Price>,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_position_construction() -> Result<(), Box<dyn std::error::Error>> {
        let p = Position {
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            quantity: Quantity::new(dec!(0.5))?,
            average_entry_price: Price::new(dec!(67000.0)),
            unrealized_pnl: Some(Amount::new(dec!(150.0))),
            liquidation_price: None,
        };
        assert_eq!(p.symbol.as_str(), "XXBTZUSD");
        assert_eq!(p.side, OrderSide::Buy);
        Ok(())
    }

    #[test]
    fn test_position_with_liquidation_price() -> Result<(), Box<dyn std::error::Error>> {
        let p = Position {
            symbol: Symbol::new("PF_SOLUSD")?,
            side: OrderSide::Buy,
            quantity: Quantity::new(dec!(100.0))?,
            average_entry_price: Price::new(dec!(150.0)),
            unrealized_pnl: Some(Amount::new(dec!(-50.0))),
            liquidation_price: Some(Price::new(dec!(120.0))),
        };
        assert_eq!(p.liquidation_price, Some(Price::new(dec!(120.0))));
        Ok(())
    }

    #[test]
    fn test_position_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let p = Position {
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Sell,
            quantity: Quantity::new(dec!(2.0))?,
            average_entry_price: Price::new(dec!(68000.0)),
            unrealized_pnl: None,
            liquidation_price: None,
        };
        let json = serde_json::to_string(&p)?;
        let deserialized: Position = serde_json::from_str(&json)?;
        assert_eq!(p, deserialized);
        Ok(())
    }
}
