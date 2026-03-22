use chrono::{DateTime, Utc};
use ingot_core::{OhlcvBar, Tick, TickerSnapshot};
use ingot_primitives::Symbol;

use crate::error::BacktestError;

/// A single event in the backtest timeline.
#[derive(Debug, Clone)]
pub struct BacktestEvent {
    pub timestamp: DateTime<Utc>,
    pub symbol: Symbol,
    pub ticker: TickerSnapshot,
}

/// Convert OHLCV bars into a sorted sequence of `BacktestEvent`s.
///
/// Each bar produces a `TickerSnapshot` with bid = ask = last = close,
/// `volume_24h` = volume. Returns events sorted by timestamp.
pub fn ohlcv_to_events(bars: &[OhlcvBar]) -> Result<Vec<BacktestEvent>, BacktestError> {
    if bars.is_empty() {
        return Err(BacktestError::NoData);
    }

    let mut events: Vec<BacktestEvent> = bars
        .iter()
        .map(|bar| BacktestEvent {
            timestamp: bar.time,
            symbol: bar.symbol.clone(),
            ticker: TickerSnapshot {
                symbol: bar.symbol.clone(),
                bid: bar.close,
                ask: bar.close,
                last: bar.close,
                volume_24h: bar.volume,
                timestamp: bar.time,
            },
        })
        .collect();

    events.sort_by_key(|e| e.timestamp);
    Ok(events)
}

/// Convert raw ticks into a sorted sequence of `BacktestEvent`s.
///
/// Each tick produces a `TickerSnapshot` with bid = ask = last = price,
/// `volume_24h` = quantity. Returns events sorted by timestamp.
pub fn ticks_to_events(ticks: &[Tick]) -> Result<Vec<BacktestEvent>, BacktestError> {
    if ticks.is_empty() {
        return Err(BacktestError::NoData);
    }

    let mut events: Vec<BacktestEvent> = ticks
        .iter()
        .map(|tick| BacktestEvent {
            timestamp: tick.time,
            symbol: tick.symbol.clone(),
            ticker: TickerSnapshot {
                symbol: tick.symbol.clone(),
                bid: tick.price,
                ask: tick.price,
                last: tick.price,
                volume_24h: tick.quantity,
                timestamp: tick.time,
            },
        })
        .collect();

    events.sort_by_key(|e| e.timestamp);
    Ok(events)
}

/// Merge multiple symbol feeds into a single time-sorted event stream.
pub fn merge_events(feeds: Vec<Vec<BacktestEvent>>) -> Vec<BacktestEvent> {
    let total_len: usize = feeds.iter().map(Vec::len).sum();
    let mut merged = Vec::with_capacity(total_len);
    for feed in feeds {
        merged.extend(feed);
    }
    merged.sort_by_key(|e| e.timestamp);
    merged
}

/// Validate that events are sorted by timestamp.
pub fn validate_sorted(events: &[BacktestEvent]) -> Result<(), BacktestError> {
    for window in events.windows(2) {
        if window[0].timestamp > window[1].timestamp {
            return Err(BacktestError::UnsortedData);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ingot_primitives::{Price, Quantity};
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;

    fn make_ohlcv_bar(
        symbol: &str,
        time_str: &str,
        close: rust_decimal::Decimal,
        volume: rust_decimal::Decimal,
    ) -> Result<OhlcvBar, Box<dyn std::error::Error>> {
        Ok(OhlcvBar {
            time: DateTime::parse_from_rfc3339(time_str)?.to_utc(),
            symbol: Symbol::new(symbol)?,
            exchange: SmolStr::new("backtest"),
            interval: SmolStr::new("1h"),
            open: Price::new(close),
            high: Price::new(close),
            low: Price::new(close),
            close: Price::new(close),
            volume: Quantity::new(volume)?,
            trade_count: None,
        })
    }

    fn make_tick(
        symbol: &str,
        time_str: &str,
        price: rust_decimal::Decimal,
        quantity: rust_decimal::Decimal,
    ) -> Result<Tick, Box<dyn std::error::Error>> {
        Ok(Tick {
            time: DateTime::parse_from_rfc3339(time_str)?.to_utc(),
            symbol: Symbol::new(symbol)?,
            exchange: SmolStr::new("backtest"),
            price: Price::new(price),
            quantity: Quantity::new(quantity)?,
            side: None,
            trade_id: None,
        })
    }

    // --- Test 1: Single OHLCV bar ---
    #[test]
    fn test_ohlcv_to_events_single_bar() -> Result<(), Box<dyn std::error::Error>> {
        let bar = make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?;
        let events = ohlcv_to_events(&[bar])?;

        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.symbol.as_str(), "BTCUSD");
        assert_eq!(e.ticker.bid, Price::new(dec!(67000)));
        assert_eq!(e.ticker.ask, Price::new(dec!(67000)));
        assert_eq!(e.ticker.last, Price::new(dec!(67000)));
        assert_eq!(e.ticker.volume_24h, Quantity::new(dec!(100))?);
        assert_eq!(e.ticker.timestamp, e.timestamp);
        Ok(())
    }

    // --- Test 2: Multiple OHLCV bars sorted ---
    #[test]
    fn test_ohlcv_to_events_multiple_bars() -> Result<(), Box<dyn std::error::Error>> {
        let bars = vec![
            make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(120))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500), dec!(110))?,
        ];
        let events = ohlcv_to_events(&bars)?;

        assert_eq!(events.len(), 3);
        // Should be sorted by time
        assert!(events[0].timestamp <= events[1].timestamp);
        assert!(events[1].timestamp <= events[2].timestamp);
        assert_eq!(events[0].ticker.last, Price::new(dec!(67000)));
        assert_eq!(events[1].ticker.last, Price::new(dec!(67500)));
        assert_eq!(events[2].ticker.last, Price::new(dec!(68000)));
        Ok(())
    }

    // --- Test 3: Empty OHLCV bars ---
    #[test]
    fn test_ohlcv_to_events_empty() {
        let result = ohlcv_to_events(&[]);
        assert!(result.is_err());
        assert!(matches!(
            result.err().ok_or("expected error"),
            Ok(BacktestError::NoData)
        ));
    }

    // --- Test 4: Single tick ---
    #[test]
    fn test_ticks_to_events_single_tick() -> Result<(), Box<dyn std::error::Error>> {
        let tick = make_tick("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(0.5))?;
        let events = ticks_to_events(&[tick])?;

        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.symbol.as_str(), "BTCUSD");
        assert_eq!(e.ticker.bid, Price::new(dec!(67000)));
        assert_eq!(e.ticker.ask, Price::new(dec!(67000)));
        assert_eq!(e.ticker.last, Price::new(dec!(67000)));
        assert_eq!(e.ticker.volume_24h, Quantity::new(dec!(0.5))?);
        assert_eq!(e.ticker.timestamp, e.timestamp);
        Ok(())
    }

    // --- Test 5: Multiple ticks sorted ---
    #[test]
    fn test_ticks_to_events_multiple_ticks() -> Result<(), Box<dyn std::error::Error>> {
        let ticks = vec![
            make_tick("BTCUSD", "2025-01-01T00:00:02Z", dec!(67200), dec!(0.3))?,
            make_tick("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(0.5))?,
            make_tick("BTCUSD", "2025-01-01T00:00:01Z", dec!(67100), dec!(0.1))?,
        ];
        let events = ticks_to_events(&ticks)?;

        assert_eq!(events.len(), 3);
        assert!(events[0].timestamp <= events[1].timestamp);
        assert!(events[1].timestamp <= events[2].timestamp);
        assert_eq!(events[0].ticker.last, Price::new(dec!(67000)));
        assert_eq!(events[1].ticker.last, Price::new(dec!(67100)));
        assert_eq!(events[2].ticker.last, Price::new(dec!(67200)));
        Ok(())
    }

    // --- Test 6: Empty ticks ---
    #[test]
    fn test_ticks_to_events_empty() {
        let result = ticks_to_events(&[]);
        assert!(result.is_err());
        assert!(matches!(
            result.err().ok_or("expected error"),
            Ok(BacktestError::NoData)
        ));
    }

    // --- Test 7: Merge two symbol feeds ---
    #[test]
    fn test_merge_events_two_symbols() -> Result<(), Box<dyn std::error::Error>> {
        let btc_bars = vec![
            make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(120))?,
        ];
        let eth_bars = vec![
            make_ohlcv_bar("ETHUSD", "2025-01-01T01:00:00Z", dec!(3500), dec!(500))?,
            make_ohlcv_bar("ETHUSD", "2025-01-01T03:00:00Z", dec!(3600), dec!(600))?,
        ];

        let btc_events = ohlcv_to_events(&btc_bars)?;
        let eth_events = ohlcv_to_events(&eth_bars)?;
        let merged = merge_events(vec![btc_events, eth_events]);

        assert_eq!(merged.len(), 4);
        // Should be interleaved by time
        assert_eq!(merged[0].symbol.as_str(), "BTCUSD"); // 00:00
        assert_eq!(merged[1].symbol.as_str(), "ETHUSD"); // 01:00
        assert_eq!(merged[2].symbol.as_str(), "BTCUSD"); // 02:00
        assert_eq!(merged[3].symbol.as_str(), "ETHUSD"); // 03:00
        Ok(())
    }

    // --- Test 8: Merge empty input ---
    #[test]
    fn test_merge_events_empty_input() {
        let merged = merge_events(vec![]);
        assert!(merged.is_empty());
    }

    // --- Test 9: Merge single feed ---
    #[test]
    fn test_merge_events_single_feed() -> Result<(), Box<dyn std::error::Error>> {
        let bars = vec![
            make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500), dec!(110))?,
        ];
        let events = ohlcv_to_events(&bars)?;
        let original_len = events.len();

        let merged = merge_events(vec![events]);
        assert_eq!(merged.len(), original_len);
        assert!(merged[0].timestamp <= merged[1].timestamp);
        Ok(())
    }

    // --- Test 10: Validate sorted (valid) ---
    #[test]
    fn test_validate_sorted_valid() -> Result<(), Box<dyn std::error::Error>> {
        let bars = vec![
            make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500), dec!(110))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T02:00:00Z", dec!(68000), dec!(120))?,
        ];
        let events = ohlcv_to_events(&bars)?;
        assert!(validate_sorted(&events).is_ok());
        Ok(())
    }

    // --- Test 11: Validate sorted (invalid) ---
    #[test]
    fn test_validate_sorted_invalid() -> Result<(), Box<dyn std::error::Error>> {
        let bars = vec![
            make_ohlcv_bar("BTCUSD", "2025-01-01T00:00:00Z", dec!(67000), dec!(100))?,
            make_ohlcv_bar("BTCUSD", "2025-01-01T01:00:00Z", dec!(67500), dec!(110))?,
        ];
        let events = ohlcv_to_events(&bars)?;

        // Manually create an unsorted sequence
        let mut unsorted = events;
        unsorted.reverse();

        let result = validate_sorted(&unsorted);
        assert!(result.is_err());
        assert!(matches!(
            result.err().ok_or("expected error"),
            Ok(BacktestError::UnsortedData)
        ));
        Ok(())
    }

    // --- Test 12: Validate sorted (empty) ---
    #[test]
    fn test_validate_sorted_empty() {
        assert!(validate_sorted(&[]).is_ok());
    }
}
