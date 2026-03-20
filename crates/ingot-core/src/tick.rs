use chrono::{DateTime, Utc};
use ingot_primitives::{OrderSide, Price, Quantity, Symbol};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tick {
    pub time: DateTime<Utc>,
    pub symbol: Symbol,
    pub exchange: SmolStr,
    pub price: Price,
    pub quantity: Quantity,
    pub side: Option<OrderSide>,
    pub trade_id: Option<SmolStr>,
}

#[cfg(test)]
mod tests {
    use ingot_primitives::PrimitiveError;
    use rust_decimal_macros::dec;

    use super::*;

    fn sample_tick() -> Result<Tick, PrimitiveError> {
        Ok(Tick {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00.123Z")
                .map_err(|_| PrimitiveError::EmptySymbol)?
                .to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            price: Price::new(dec!(67100.50)),
            quantity: Quantity::new(dec!(0.5))?,
            side: Some(OrderSide::Buy),
            trade_id: Some(SmolStr::new("kraken-12345")),
        })
    }

    #[test]
    fn test_tick_construction() -> Result<(), PrimitiveError> {
        let tick = sample_tick()?;
        assert_eq!(tick.symbol.as_str(), "XXBTZUSD");
        assert_eq!(tick.side, Some(OrderSide::Buy));
        Ok(())
    }

    #[test]
    fn test_tick_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let tick = sample_tick()?;
        let json = serde_json::to_string(&tick)?;
        let deserialized: Tick = serde_json::from_str(&json)?;
        assert_eq!(tick, deserialized);
        Ok(())
    }

    #[test]
    fn test_tick_no_side_no_trade_id() -> Result<(), PrimitiveError> {
        let tick = Tick {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")
                .map_err(|_| PrimitiveError::EmptySymbol)?
                .to_utc(),
            symbol: Symbol::new("AAPL")?,
            exchange: SmolStr::new("ibkr"),
            price: Price::new(dec!(175.50)),
            quantity: Quantity::new(dec!(100))?,
            side: None,
            trade_id: None,
        };
        assert!(tick.side.is_none());
        assert!(tick.trade_id.is_none());
        Ok(())
    }
}
