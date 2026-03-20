use chrono::{DateTime, Utc};
use ingot_primitives::{Price, Quantity, Symbol};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OhlcvBar {
    pub time: DateTime<Utc>,
    pub symbol: Symbol,
    pub exchange: SmolStr,
    pub interval: SmolStr,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub volume: Quantity,
    pub trade_count: Option<i32>,
}

#[cfg(test)]
mod tests {
    use ingot_primitives::PrimitiveError;
    use rust_decimal_macros::dec;

    use super::*;

    fn sample_bar() -> Result<OhlcvBar, PrimitiveError> {
        Ok(OhlcvBar {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")
                .map_err(|_| PrimitiveError::EmptySymbol)?
                .to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            interval: SmolStr::new("1m"),
            open: Price::new(dec!(67000.0)),
            high: Price::new(dec!(67150.5)),
            low: Price::new(dec!(66980.0)),
            close: Price::new(dec!(67100.0)),
            volume: Quantity::new(dec!(12.345))?,
            trade_count: Some(847),
        })
    }

    #[test]
    fn test_ohlcv_construction() -> Result<(), PrimitiveError> {
        let bar = sample_bar()?;
        assert_eq!(bar.symbol.as_str(), "XXBTZUSD");
        assert_eq!(bar.interval.as_str(), "1m");
        assert_eq!(bar.trade_count, Some(847));
        Ok(())
    }

    #[test]
    fn test_ohlcv_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let bar = sample_bar()?;
        let json = serde_json::to_string(&bar)?;
        let deserialized: OhlcvBar = serde_json::from_str(&json)?;
        assert_eq!(bar, deserialized);
        Ok(())
    }

    #[test]
    fn test_ohlcv_no_trade_count() -> Result<(), PrimitiveError> {
        let mut bar = sample_bar()?;
        bar.trade_count = None;
        assert!(bar.trade_count.is_none());
        Ok(())
    }
}
