use serde::Deserialize;

/// Generic Kraken API response envelope.
/// All endpoints return `{"error": [...], "result": ...}`.
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenResponse<T> {
    pub error: Vec<String>,
    pub result: Option<T>,
}

// ---- AssetPairs ----

/// Value type from `GET /0/public/AssetPairs`.
/// Result is `HashMap<String, KrakenAssetPair>`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenAssetPair {
    pub base: String,
    pub quote: String,
    pub wsname: Option<String>,
    pub altname: String,
    pub pair_decimals: u8,
    pub lot_decimals: u8,
    #[serde(default)]
    pub ordermin: Option<String>,
    #[serde(default)]
    pub costmin: Option<String>,
    #[serde(default)]
    pub tick_size: Option<String>,
    #[serde(default)]
    pub leverage_buy: Vec<u8>,
    #[serde(default)]
    pub leverage_sell: Vec<u8>,
}

// ---- OHLC ----

/// OHLC result value: either a bar array or the `"last"` cursor.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
pub(crate) enum KrakenOhlcValue {
    Bars(Vec<KrakenOhlcTuple>),
    Last(i64),
}

/// Single OHLC bar: `[time, open, high, low, close, vwap, volume, count]`
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenOhlcTuple(
    pub i64,    // timestamp
    pub String, // open
    pub String, // high
    pub String, // low
    pub String, // close
    pub String, // vwap
    pub String, // volume
    pub i64,    // count
);

// ---- Trades ----

/// Trades result value: either a trade array or the `"last"` cursor.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum KrakenTradesValue {
    Trades(Vec<KrakenTradeTuple>),
    Last(String),
}

/// Single trade: `[price, volume, time, buy_sell, market_limit, misc,
/// trade_id]`
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenTradeTuple(
    pub String, // price
    pub String, // volume
    pub f64,    // time (Unix with fractional seconds)
    pub String, // "b" or "s"
    pub String, // "m" or "l"
    pub String, // misc
    pub i64,    // trade_id
);

// ---- Ticker ----

/// Ticker info from `GET /0/public/Ticker`.
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenTickerInfo {
    pub a: Vec<String>, // [ask_price, whole_lot_volume, lot_volume]
    pub b: Vec<String>, // [bid_price, whole_lot_volume, lot_volume]
    pub c: Vec<String>, // [last_price, lot_volume]
    pub v: Vec<String>, // [volume_today, volume_24h]
}

// ---- Depth (Order Book) ----

/// Order book from `GET /0/public/Depth`.
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenOrderBook {
    pub asks: Vec<KrakenBookLevel>,
    pub bids: Vec<KrakenBookLevel>,
}

/// Single book level: `[price, volume, timestamp]`
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenBookLevel(
    pub String, // price
    pub String, // volume
    pub i64,    // timestamp
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_kraken_response_success() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"error": [], "result": {"key": "value"}}"#;
        let resp: KrakenResponse<std::collections::HashMap<String, String>> =
            serde_json::from_str(json)?;
        assert!(resp.error.is_empty());
        assert!(resp.result.is_some());
        Ok(())
    }

    #[test]
    fn test_deserialize_kraken_response_error() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"error": ["EGeneral:Invalid arguments"], "result": null}"#;
        let resp: KrakenResponse<serde_json::Value> = serde_json::from_str(json)?;
        assert_eq!(resp.error.len(), 1);
        assert!(resp.result.is_none());
        Ok(())
    }

    #[test]
    fn test_deserialize_asset_pair() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "base": "XXBT",
            "quote": "ZUSD",
            "wsname": "XBT/USD",
            "altname": "XBTUSD",
            "pair_decimals": 1,
            "lot_decimals": 8,
            "ordermin": "0.0001",
            "costmin": "0.5",
            "tick_size": "0.1",
            "leverage_buy": [2, 3, 4, 5],
            "leverage_sell": [2, 3, 4, 5]
        }"#;
        let pair: KrakenAssetPair = serde_json::from_str(json)?;
        assert_eq!(pair.base, "XXBT");
        assert_eq!(pair.quote, "ZUSD");
        assert_eq!(pair.wsname.as_deref(), Some("XBT/USD"));
        assert_eq!(pair.pair_decimals, 1);
        assert_eq!(pair.lot_decimals, 8);
        assert_eq!(pair.leverage_buy.len(), 4);
        Ok(())
    }

    #[test]
    fn test_deserialize_ohlc_tuple() -> Result<(), Box<dyn std::error::Error>> {
        let json =
            r#"[1616663400, "56200.0", "56300.0", "56100.0", "56250.0", "56225.5", "12.345", 847]"#;
        let bar: KrakenOhlcTuple = serde_json::from_str(json)?;
        assert_eq!(bar.0, 1_616_663_400);
        assert_eq!(bar.1, "56200.0");
        assert_eq!(bar.6, "12.345");
        assert_eq!(bar.7, 847);
        Ok(())
    }

    #[test]
    fn test_deserialize_trade_tuple() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"["56200.10000", "0.00100000", 1616663594.2009, "b", "m", "", 12345]"#;
        let trade: KrakenTradeTuple = serde_json::from_str(json)?;
        assert_eq!(trade.0, "56200.10000");
        assert_eq!(trade.1, "0.00100000");
        assert!((trade.2 - 1_616_663_594.200_9).abs() < 0.001);
        assert_eq!(trade.3, "b");
        assert_eq!(trade.6, 12345);
        Ok(())
    }

    #[test]
    fn test_deserialize_ticker_info() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "a": ["67010.00000", "1", "1.000"],
            "b": ["67000.00000", "2", "2.000"],
            "c": ["67005.00000", "0.001"],
            "v": ["1000.0", "5000.0"]
        }"#;
        let ticker: KrakenTickerInfo = serde_json::from_str(json)?;
        assert_eq!(ticker.a[0], "67010.00000");
        assert_eq!(ticker.b[0], "67000.00000");
        assert_eq!(ticker.c[0], "67005.00000");
        assert_eq!(ticker.v[1], "5000.0");
        Ok(())
    }

    #[test]
    fn test_deserialize_order_book() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "asks": [["67010.0", "1.5", 1616663400], ["67020.0", "2.0", 1616663401]],
            "bids": [["67000.0", "3.0", 1616663400]]
        }"#;
        let book: KrakenOrderBook = serde_json::from_str(json)?;
        assert_eq!(book.asks.len(), 2);
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.asks[0].0, "67010.0");
        assert_eq!(book.bids[0].0, "67000.0");
        Ok(())
    }
}
