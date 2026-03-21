use chrono::{DateTime, Utc};
use ingot_primitives::{Price, Quantity, Symbol};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickerSnapshot {
    pub symbol: Symbol,
    pub bid: Price,
    pub ask: Price,
    pub last: Price,
    pub volume_24h: Quantity,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBookLevel {
    pub price: Price,
    pub quantity: Quantity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub symbol: Symbol,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_ticker_snapshot_construction() -> Result<(), Box<dyn std::error::Error>> {
        let ts = TickerSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bid: Price::new(dec!(67000.0)),
            ask: Price::new(dec!(67010.0)),
            last: Price::new(dec!(67005.0)),
            volume_24h: Quantity::new(dec!(1234.5))?,
            timestamp: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
        };
        assert_eq!(ts.symbol.as_str(), "XXBTZUSD");
        assert_eq!(ts.bid.value(), dec!(67000.0));
        assert_eq!(ts.ask.value(), dec!(67010.0));
        Ok(())
    }

    #[test]
    fn test_ticker_snapshot_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let ts = TickerSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bid: Price::new(dec!(67000.0)),
            ask: Price::new(dec!(67010.0)),
            last: Price::new(dec!(67005.0)),
            volume_24h: Quantity::new(dec!(1234.5))?,
            timestamp: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
        };
        let json = serde_json::to_string(&ts)?;
        let deserialized: TickerSnapshot = serde_json::from_str(&json)?;
        assert_eq!(ts, deserialized);
        Ok(())
    }

    #[test]
    fn test_order_book_level_copy() -> Result<(), Box<dyn std::error::Error>> {
        let level = OrderBookLevel {
            price: Price::new(dec!(67000.0)),
            quantity: Quantity::new(dec!(1.5))?,
        };
        let copy = level;
        assert_eq!(level, copy);
        Ok(())
    }

    #[test]
    fn test_order_book_snapshot_construction() -> Result<(), Box<dyn std::error::Error>> {
        let snap = OrderBookSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bids: vec![
                OrderBookLevel {
                    price: Price::new(dec!(67000.0)),
                    quantity: Quantity::new(dec!(1.5))?,
                },
                OrderBookLevel {
                    price: Price::new(dec!(66990.0)),
                    quantity: Quantity::new(dec!(2.0))?,
                },
            ],
            asks: vec![
                OrderBookLevel {
                    price: Price::new(dec!(67010.0)),
                    quantity: Quantity::new(dec!(0.8))?,
                },
                OrderBookLevel {
                    price: Price::new(dec!(67020.0)),
                    quantity: Quantity::new(dec!(3.0))?,
                },
            ],
            timestamp: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
        };
        assert_eq!(snap.bids.len(), 2);
        assert_eq!(snap.asks.len(), 2);
        assert_eq!(snap.bids[0].price.value(), dec!(67000.0));
        assert_eq!(snap.asks[0].price.value(), dec!(67010.0));
        Ok(())
    }

    #[test]
    fn test_order_book_snapshot_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let snap = OrderBookSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bids: vec![OrderBookLevel {
                price: Price::new(dec!(67000.0)),
                quantity: Quantity::new(dec!(1.5))?,
            }],
            asks: vec![OrderBookLevel {
                price: Price::new(dec!(67010.0)),
                quantity: Quantity::new(dec!(0.8))?,
            }],
            timestamp: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
        };
        let json = serde_json::to_string(&snap)?;
        let deserialized: OrderBookSnapshot = serde_json::from_str(&json)?;
        assert_eq!(snap, deserialized);
        Ok(())
    }
}
