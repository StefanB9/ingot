use std::collections::HashMap;

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

// ---- AddOrder ----

/// Result from `POST /0/private/AddOrder`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenAddOrderResult {
    pub txid: Vec<String>,
    pub descr: KrakenOrderDescr,
}

/// Order description returned by `AddOrder`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenOrderDescr {
    pub order: String,
}

// ---- CancelOrder / CancelAll ----

/// Result from `POST /0/private/CancelOrder` or `CancelAll`.
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenCancelResult {
    pub count: u32,
}

// ---- OpenOrders ----

/// Result from `POST /0/private/OpenOrders`.
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenOpenOrdersResult {
    pub open: HashMap<String, KrakenOpenOrder>,
}

/// A single open order from Kraken.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenOpenOrder {
    pub status: String,
    pub descr: KrakenOpenOrderDescr,
    pub vol: String,
    pub vol_exec: String,
    pub cost: String,
    pub fee: String,
    #[serde(default)]
    pub avg_price: String,
    pub opentm: f64,
}

/// Order description nested in `KrakenOpenOrder`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenOpenOrderDescr {
    pub pair: String,
    #[serde(rename = "type")]
    pub side: String,
    pub ordertype: String,
    pub price: String,
    pub price2: String,
}

// ---- TradesHistory ----

/// Result from `POST /0/private/TradesHistory`.
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenTradesHistoryResult {
    pub trades: HashMap<String, KrakenTradeHistoryEntry>,
}

/// A single trade history entry from Kraken.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenTradeHistoryEntry {
    pub ordertxid: String,
    pub pair: String,
    #[serde(rename = "type")]
    pub side: String,
    pub ordertype: String,
    pub price: String,
    pub vol: String,
    pub cost: String,
    pub fee: String,
    pub time: f64,
    #[serde(default)]
    pub trade_id: Option<i64>,
}

// ---- WebSocket v2 Messages ----

/// Top-level WS channel data message — dispatched by channel.
///
/// Kraken WS v2 sends two top-level formats:
/// 1. Channel data: `{"channel": "trade", "type": "update", "data": [...]}`
/// 2. Method responses: `{"method": "subscribe", "success": true, ...}`
///
/// We try deserializing as this enum first (tagged by `channel`),
/// then fall back to `KrakenWsMethodResponse` if that fails.
#[derive(Debug, Deserialize)]
#[serde(tag = "channel")]
pub(crate) enum KrakenWsMessage {
    #[serde(rename = "heartbeat")]
    Heartbeat,
    #[serde(rename = "status")]
    Status(KrakenWsStatus),
    #[serde(rename = "trade")]
    Trade(KrakenWsTrade),
    #[serde(rename = "ticker")]
    Ticker(KrakenWsTicker),
    #[serde(rename = "book")]
    Book(KrakenWsBook),
    #[serde(rename = "executions")]
    Executions(KrakenWsExecutions),
}

/// Status/subscription response.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsStatus {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub data: Option<Vec<serde_json::Value>>,
}

/// Method response (subscribe/unsubscribe ack).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsMethodResponse {
    pub method: String,
    pub success: bool,
    pub error: Option<String>,
    pub req_id: Option<u64>,
}

/// Trade channel update.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsTrade {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub data: Vec<KrakenWsTradeEntry>,
}

/// Single trade entry from the WS trade channel.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsTradeEntry {
    pub symbol: String,
    pub price: String,
    pub qty: String,
    pub side: String,
    pub timestamp: String,
    pub trade_id: i64,
}

/// Ticker channel update.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsTicker {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub data: Vec<KrakenWsTickerEntry>,
}

/// Single ticker entry from the WS ticker channel.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsTickerEntry {
    pub symbol: String,
    pub bid: String,
    pub ask: String,
    pub last: String,
    pub volume: String,
}

/// Book channel update (snapshot or incremental).
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenWsBook {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub data: Vec<KrakenWsBookData>,
}

/// Book data payload with bids/asks.
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenWsBookData {
    pub symbol: String,
    pub bids: Vec<KrakenWsBookLevel>,
    pub asks: Vec<KrakenWsBookLevel>,
}

/// Single book level: price + quantity.
#[derive(Debug, Deserialize)]
pub(crate) struct KrakenWsBookLevel {
    pub price: String,
    pub qty: String,
}

/// Executions channel update (private).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsExecutions {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub data: Vec<KrakenWsExecEntry>,
}

/// Single execution report entry.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsExecEntry {
    pub exec_type: String,
    pub order_id: String,
    pub symbol: String,
    pub side: String,
    pub last_price: Option<String>,
    pub last_qty: Option<String>,
    pub fee_paid: Option<String>,
    pub fee_currency: Option<String>,
    pub timestamp: String,
    pub trade_id: Option<i64>,
}

// ---- WebSocket Token ----

/// Result from `POST /0/private/GetWebSocketsToken`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct KrakenWsTokenResult {
    pub token: String,
    pub expires: u64,
}

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

    // ---- Private endpoint model tests ----

    #[test]
    fn test_deserialize_add_order_result() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "descr": {"order": "buy 0.001 XBTUSD @ market"},
            "txid": ["OABCDE-12345-FGHIJ"]
        }"#;
        let result: KrakenAddOrderResult = serde_json::from_str(json)?;
        assert_eq!(result.txid.len(), 1);
        assert_eq!(result.txid[0], "OABCDE-12345-FGHIJ");
        assert_eq!(result.descr.order, "buy 0.001 XBTUSD @ market");
        Ok(())
    }

    #[test]
    fn test_deserialize_cancel_result() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"count": 3}"#;
        let result: KrakenCancelResult = serde_json::from_str(json)?;
        assert_eq!(result.count, 3);
        Ok(())
    }

    #[test]
    fn test_deserialize_open_orders_result() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "open": {
                "OABCDE-12345-FGHIJ": {
                    "status": "open",
                    "descr": {
                        "pair": "XXBTZUSD",
                        "type": "buy",
                        "ordertype": "limit",
                        "price": "65000.0",
                        "price2": "0"
                    },
                    "vol": "0.001",
                    "vol_exec": "0.0005",
                    "cost": "32.50",
                    "fee": "0.05",
                    "avg_price": "65000.0",
                    "opentm": 1616663594.2009
                }
            }
        }"#;
        let result: KrakenOpenOrdersResult = serde_json::from_str(json)?;
        assert_eq!(result.open.len(), 1);
        let order = result
            .open
            .get("OABCDE-12345-FGHIJ")
            .ok_or("missing order")?;
        assert_eq!(order.status, "open");
        assert_eq!(order.descr.side, "buy");
        assert_eq!(order.descr.ordertype, "limit");
        assert_eq!(order.descr.price, "65000.0");
        assert_eq!(order.vol, "0.001");
        assert_eq!(order.vol_exec, "0.0005");
        Ok(())
    }

    #[test]
    fn test_deserialize_trades_history_result() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "trades": {
                "TABC-DEF-GHIJ": {
                    "ordertxid": "OABCDE-12345-FGHIJ",
                    "pair": "XXBTZUSD",
                    "type": "buy",
                    "ordertype": "market",
                    "price": "67000.50",
                    "vol": "0.001",
                    "cost": "67.0005",
                    "fee": "0.10",
                    "time": 1616663594.2009,
                    "trade_id": 99999
                }
            }
        }"#;
        let result: KrakenTradesHistoryResult = serde_json::from_str(json)?;
        assert_eq!(result.trades.len(), 1);
        let entry = result.trades.get("TABC-DEF-GHIJ").ok_or("missing trade")?;
        assert_eq!(entry.ordertxid, "OABCDE-12345-FGHIJ");
        assert_eq!(entry.pair, "XXBTZUSD");
        assert_eq!(entry.side, "buy");
        assert_eq!(entry.price, "67000.50");
        assert_eq!(entry.trade_id, Some(99999));
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_token_result() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"token": "abc123token", "expires": 900}"#;
        let result: KrakenWsTokenResult = serde_json::from_str(json)?;
        assert_eq!(result.token, "abc123token");
        assert_eq!(result.expires, 900);
        Ok(())
    }

    // ---- WebSocket v2 message deserialization tests ----

    #[test]
    fn test_deserialize_ws_heartbeat() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"channel": "heartbeat"}"#;
        let msg: KrakenWsMessage = serde_json::from_str(json)?;
        assert!(matches!(msg, KrakenWsMessage::Heartbeat));
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_status() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "channel": "status",
            "type": "update",
            "data": [{"api_version": "v2", "connection_id": 123}]
        }"#;
        let msg: KrakenWsMessage = serde_json::from_str(json)?;
        if let KrakenWsMessage::Status(status) = msg {
            assert_eq!(status.msg_type, "update");
            assert!(status.data.is_some());
        } else {
            return Err("expected Status variant".into());
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_trade() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "channel": "trade",
            "type": "update",
            "data": [{
                "symbol": "BTC/USD",
                "price": "67000.50",
                "qty": "0.001",
                "side": "buy",
                "timestamp": "2024-01-15T10:30:00.000000Z",
                "trade_id": 12345
            }]
        }"#;
        let msg: KrakenWsMessage = serde_json::from_str(json)?;
        if let KrakenWsMessage::Trade(trade) = msg {
            assert_eq!(trade.msg_type, "update");
            assert_eq!(trade.data.len(), 1);
            assert_eq!(trade.data[0].symbol, "BTC/USD");
            assert_eq!(trade.data[0].price, "67000.50");
            assert_eq!(trade.data[0].qty, "0.001");
            assert_eq!(trade.data[0].side, "buy");
            assert_eq!(trade.data[0].trade_id, 12345);
        } else {
            return Err("expected Trade variant".into());
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_ticker() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "channel": "ticker",
            "type": "update",
            "data": [{
                "symbol": "BTC/USD",
                "bid": "67000.00",
                "ask": "67010.00",
                "last": "67005.00",
                "volume": "5000.0"
            }]
        }"#;
        let msg: KrakenWsMessage = serde_json::from_str(json)?;
        if let KrakenWsMessage::Ticker(ticker) = msg {
            assert_eq!(ticker.msg_type, "update");
            assert_eq!(ticker.data.len(), 1);
            assert_eq!(ticker.data[0].symbol, "BTC/USD");
            assert_eq!(ticker.data[0].bid, "67000.00");
            assert_eq!(ticker.data[0].ask, "67010.00");
            assert_eq!(ticker.data[0].last, "67005.00");
            assert_eq!(ticker.data[0].volume, "5000.0");
        } else {
            return Err("expected Ticker variant".into());
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_book_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "channel": "book",
            "type": "snapshot",
            "data": [{
                "symbol": "BTC/USD",
                "bids": [
                    {"price": "67000.00", "qty": "3.0"},
                    {"price": "66990.00", "qty": "1.5"}
                ],
                "asks": [
                    {"price": "67010.00", "qty": "2.0"}
                ]
            }]
        }"#;
        let msg: KrakenWsMessage = serde_json::from_str(json)?;
        if let KrakenWsMessage::Book(book) = msg {
            assert_eq!(book.msg_type, "snapshot");
            assert_eq!(book.data.len(), 1);
            assert_eq!(book.data[0].symbol, "BTC/USD");
            assert_eq!(book.data[0].bids.len(), 2);
            assert_eq!(book.data[0].asks.len(), 1);
            assert_eq!(book.data[0].bids[0].price, "67000.00");
            assert_eq!(book.data[0].bids[0].qty, "3.0");
        } else {
            return Err("expected Book variant".into());
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_executions() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "channel": "executions",
            "type": "update",
            "data": [{
                "exec_type": "filled",
                "order_id": "OABCDE-12345-FGHIJ",
                "symbol": "BTC/USD",
                "side": "buy",
                "last_price": "67000.50",
                "last_qty": "0.001",
                "fee_paid": "0.10",
                "fee_currency": "USD",
                "timestamp": "2024-01-15T10:30:00.000000Z",
                "trade_id": 99999
            }]
        }"#;
        let msg: KrakenWsMessage = serde_json::from_str(json)?;
        if let KrakenWsMessage::Executions(exec) = msg {
            assert_eq!(exec.msg_type, "update");
            assert_eq!(exec.data.len(), 1);
            assert_eq!(exec.data[0].exec_type, "filled");
            assert_eq!(exec.data[0].order_id, "OABCDE-12345-FGHIJ");
            assert_eq!(exec.data[0].symbol, "BTC/USD");
            assert_eq!(exec.data[0].side, "buy");
            assert_eq!(exec.data[0].last_price.as_deref(), Some("67000.50"));
            assert_eq!(exec.data[0].trade_id, Some(99999));
        } else {
            return Err("expected Executions variant".into());
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_method_response() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "method": "subscribe",
            "success": true,
            "req_id": 42
        }"#;
        let resp: KrakenWsMethodResponse = serde_json::from_str(json)?;
        assert_eq!(resp.method, "subscribe");
        assert!(resp.success);
        assert_eq!(resp.req_id, Some(42));
        assert!(resp.error.is_none());
        Ok(())
    }
}
