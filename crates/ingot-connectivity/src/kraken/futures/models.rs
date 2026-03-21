// Many fields are deserialized but only accessed by tests or for future use.
#![allow(dead_code)]
use serde::Deserialize;

// ---- REST Response Structs ----

/// Instruments endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesInstrumentsResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(default)]
    pub instruments: Vec<FuturesInstrument>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesInstrument {
    pub symbol: String,
    #[serde(rename = "type")]
    pub instrument_type: String,
    pub underlying: Option<String>,
    #[serde(rename = "tickSize")]
    pub tick_size: Option<String>,
    #[serde(rename = "contractSize")]
    pub contract_size: Option<String>,
    #[serde(rename = "maxPositionSize")]
    pub max_position_size: Option<String>,
    #[serde(rename = "initialMarginRate")]
    pub initial_margin_rate: Option<String>,
    #[serde(rename = "maintenanceMarginRate")]
    pub maintenance_margin_rate: Option<String>,
    #[serde(rename = "lastTradingTime")]
    pub last_trading_time: Option<String>,
    pub pair: Option<String>,
}

/// Tickers endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesTickersResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(default)]
    pub tickers: Vec<FuturesTicker>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesTicker {
    pub symbol: String,
    pub bid: Option<String>,
    pub ask: Option<String>,
    pub last: Option<String>,
    pub vol24h: Option<String>,
    #[serde(rename = "markPrice")]
    pub mark_price: Option<String>,
    #[serde(rename = "openInterest")]
    pub open_interest: Option<String>,
}

/// Order book endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesOrderBookResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(rename = "orderBook")]
    pub order_book: Option<FuturesOrderBook>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesOrderBook {
    pub bids: Vec<FuturesBookLevel>,
    pub asks: Vec<FuturesBookLevel>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesBookLevel(pub f64, pub f64);

/// Public trades (history) endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesHistoryResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(default)]
    pub history: Vec<FuturesTrade>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesTrade {
    pub uid: Option<String>,
    pub side: String,
    pub symbol: String,
    pub price: String,
    pub size: String,
    pub time: String,
}

/// Send order endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesSendOrderResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(rename = "sendStatus")]
    pub send_status: Option<FuturesSendStatus>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesSendStatus {
    pub order_id: String,
    pub status: String,
}

/// Cancel order endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesCancelOrderResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(rename = "cancelStatus")]
    pub cancel_status: Option<FuturesCancelStatus>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesCancelStatus {
    pub status: String,
}

/// Cancel all orders endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesCancelAllResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(rename = "cancelStatus")]
    pub cancel_status: Option<FuturesCancelAllStatus>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesCancelAllStatus {
    #[serde(rename = "cancelledOrders")]
    pub cancelled_orders: Vec<FuturesCancelledOrder>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesCancelledOrder {
    pub order_id: String,
}

/// Open orders endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesOpenOrdersResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(rename = "openOrders", default)]
    pub open_orders: Vec<FuturesOpenOrder>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesOpenOrder {
    pub order_id: String,
    pub symbol: String,
    pub side: String,
    #[serde(rename = "orderType")]
    pub order_type: String,
    pub quantity: String,
    #[serde(rename = "filledQuantity")]
    pub filled_quantity: String,
    #[serde(rename = "limitPrice")]
    pub limit_price: Option<String>,
    pub status: String,
    #[serde(rename = "receivedTime")]
    pub received_time: Option<String>,
}

/// Accounts endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesAccountsResponse {
    pub result: String,
    pub error: Option<String>,
    pub accounts: Option<FuturesAccounts>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesAccounts {
    #[serde(rename = "flex")]
    pub flex: Option<FuturesFlexAccount>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesFlexAccount {
    #[serde(rename = "availableMargin")]
    pub available_margin: Option<String>,
    #[serde(rename = "portfolioValue")]
    pub portfolio_value: Option<String>,
    pub balances: Option<std::collections::HashMap<String, String>>,
}

/// Open positions endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesPositionsResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(rename = "openPositions", default)]
    pub open_positions: Vec<FuturesPosition>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesPosition {
    pub symbol: String,
    pub side: String,
    pub size: String,
    pub price: String,
    #[serde(rename = "unrealizedFunding")]
    pub unrealized_funding: Option<String>,
}

/// Fills endpoint response.
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesFillsResponse {
    pub result: String,
    pub error: Option<String>,
    #[serde(default)]
    pub fills: Vec<FuturesFill>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesFill {
    pub fill_id: String,
    pub order_id: String,
    pub symbol: String,
    pub side: String,
    pub price: String,
    pub size: String,
    pub fee: Option<String>,
    #[serde(rename = "fillTime")]
    pub fill_time: String,
}

// ---- WebSocket Message Structs ----

/// Control messages from the Futures WS (have an "event" field).
#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsControlMessage {
    pub event: String,
    pub feed: Option<String>,
    pub message: Option<String>,
    pub version: Option<i64>,
    #[serde(rename = "product_ids")]
    pub product_ids: Option<Vec<String>>,
}

/// Data messages from the Futures WS (tagged by "feed" field).
#[derive(Debug, Deserialize)]
#[serde(tag = "feed")]
pub(crate) enum FuturesWsFeed {
    #[serde(rename = "ticker")]
    Ticker(FuturesWsTickerEntry),
    #[serde(rename = "ticker_lite")]
    TickerLite(FuturesWsTickerEntry),
    #[serde(rename = "trade")]
    Trade(FuturesWsTradeData),
    #[serde(rename = "trade_snapshot")]
    TradeSnapshot(FuturesWsTradeData),
    #[serde(rename = "book")]
    Book(FuturesWsBookEntry),
    #[serde(rename = "book_snapshot")]
    BookSnapshot(FuturesWsBookEntry),
    #[serde(rename = "fills")]
    Fills(FuturesWsFillData),
    #[serde(rename = "open_orders")]
    OpenOrders(FuturesWsOpenOrdersData),
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsTickerEntry {
    pub product_id: String,
    pub bid: Option<String>,
    pub ask: Option<String>,
    pub last: Option<String>,
    pub volume: Option<String>,
    #[serde(rename = "markPrice")]
    pub mark_price: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsTradeData {
    pub product_id: Option<String>,
    #[serde(default)]
    pub trades: Vec<FuturesWsTradeEntry>,
    // Single trade messages (non-snapshot) embed fields directly
    pub side: Option<String>,
    pub price: Option<String>,
    pub qty: Option<String>,
    pub time: Option<i64>,
    pub uid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsTradeEntry {
    pub side: String,
    pub price: String,
    pub qty: String,
    pub time: i64,
    pub uid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsBookEntry {
    pub product_id: String,
    pub bids: Vec<FuturesWsBookLevel>,
    pub asks: Vec<FuturesWsBookLevel>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsBookLevel {
    pub price: String,
    pub qty: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsFillData {
    #[serde(default)]
    pub fills: Vec<FuturesWsFillEntry>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsFillEntry {
    pub instrument: String,
    pub side: String,
    pub price: String,
    pub qty: String,
    pub order_id: String,
    pub fill_id: String,
    pub fee_paid: Option<String>,
    pub fee_currency: Option<String>,
    pub time: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FuturesWsOpenOrdersData {
    #[serde(default)]
    pub orders: Vec<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn test_deserialize_instruments_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "instruments": [
                {
                    "symbol": "PF_XBTUSD",
                    "type": "perpetual",
                    "underlying": "rr_xbtusd",
                    "tickSize": "0.5",
                    "contractSize": "1",
                    "maxPositionSize": "500000",
                    "initialMarginRate": "0.02",
                    "maintenanceMarginRate": "0.01",
                    "pair": "xbt:usd"
                }
            ]
        }"#;
        let resp: FuturesInstrumentsResponse = serde_json::from_str(json)?;
        assert_eq!(resp.result, "success");
        assert_eq!(resp.instruments.len(), 1);
        assert_eq!(resp.instruments[0].symbol, "PF_XBTUSD");
        assert_eq!(resp.instruments[0].instrument_type, "perpetual");
        assert_eq!(resp.instruments[0].pair.as_deref(), Some("xbt:usd"));
        Ok(())
    }

    #[test]
    fn test_deserialize_instruments_error() -> anyhow::Result<()> {
        let json = r#"{"result": "error", "error": "some error"}"#;
        let resp: FuturesInstrumentsResponse = serde_json::from_str(json)?;
        assert_eq!(resp.result, "error");
        assert_eq!(resp.error.as_deref(), Some("some error"));
        assert!(resp.instruments.is_empty());
        Ok(())
    }

    #[test]
    fn test_deserialize_tickers_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "tickers": [{
                "symbol": "PF_XBTUSD",
                "bid": "67000.0",
                "ask": "67010.0",
                "last": "67005.0",
                "vol24h": "15000.5",
                "markPrice": "67002.0",
                "openInterest": "5000000"
            }]
        }"#;
        let resp: FuturesTickersResponse = serde_json::from_str(json)?;
        assert_eq!(resp.tickers.len(), 1);
        assert_eq!(resp.tickers[0].symbol, "PF_XBTUSD");
        assert_eq!(resp.tickers[0].bid.as_deref(), Some("67000.0"));
        assert_eq!(resp.tickers[0].vol24h.as_deref(), Some("15000.5"));
        Ok(())
    }

    #[test]
    fn test_deserialize_order_book_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "orderBook": {
                "bids": [[67000.0, 3.5], [66990.0, 1.2]],
                "asks": [[67010.0, 2.0]]
            }
        }"#;
        let resp: FuturesOrderBookResponse = serde_json::from_str(json)?;
        let book = resp.order_book.context("missing orderBook")?;
        assert_eq!(book.bids.len(), 2);
        assert!((book.bids[0].0 - 67000.0).abs() < f64::EPSILON);
        assert!((book.bids[0].1 - 3.5).abs() < f64::EPSILON);
        assert_eq!(book.asks.len(), 1);
        Ok(())
    }

    #[test]
    fn test_deserialize_history_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "history": [{
                "uid": "abc123",
                "side": "buy",
                "symbol": "PF_XBTUSD",
                "price": "67000.5",
                "size": "0.01",
                "time": "2024-01-15T10:30:00.000Z"
            }]
        }"#;
        let resp: FuturesHistoryResponse = serde_json::from_str(json)?;
        assert_eq!(resp.history.len(), 1);
        assert_eq!(resp.history[0].side, "buy");
        assert_eq!(resp.history[0].price, "67000.5");
        Ok(())
    }

    #[test]
    fn test_deserialize_send_order_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "sendStatus": {
                "order_id": "ord-12345",
                "status": "placed"
            }
        }"#;
        let resp: FuturesSendOrderResponse = serde_json::from_str(json)?;
        let status = resp.send_status.context("missing sendStatus")?;
        assert_eq!(status.order_id, "ord-12345");
        assert_eq!(status.status, "placed");
        Ok(())
    }

    #[test]
    fn test_deserialize_cancel_order_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "cancelStatus": {
                "status": "cancelled"
            }
        }"#;
        let resp: FuturesCancelOrderResponse = serde_json::from_str(json)?;
        let status = resp.cancel_status.context("missing cancelStatus")?;
        assert_eq!(status.status, "cancelled");
        Ok(())
    }

    #[test]
    fn test_deserialize_cancel_all_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "cancelStatus": {
                "cancelledOrders": [
                    {"order_id": "ord-1"},
                    {"order_id": "ord-2"}
                ]
            }
        }"#;
        let resp: FuturesCancelAllResponse = serde_json::from_str(json)?;
        let status = resp.cancel_status.context("missing cancelStatus")?;
        assert_eq!(status.cancelled_orders.len(), 2);
        assert_eq!(status.cancelled_orders[0].order_id, "ord-1");
        Ok(())
    }

    #[test]
    fn test_deserialize_open_orders_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "openOrders": [{
                "order_id": "ord-999",
                "symbol": "PF_XBTUSD",
                "side": "buy",
                "orderType": "lmt",
                "quantity": "10",
                "filledQuantity": "5",
                "limitPrice": "65000.0",
                "status": "partiallyFilled",
                "receivedTime": "2024-01-15T10:30:00.000Z"
            }]
        }"#;
        let resp: FuturesOpenOrdersResponse = serde_json::from_str(json)?;
        assert_eq!(resp.open_orders.len(), 1);
        assert_eq!(resp.open_orders[0].order_id, "ord-999");
        assert_eq!(resp.open_orders[0].filled_quantity, "5");
        Ok(())
    }

    #[test]
    fn test_deserialize_accounts_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "accounts": {
                "flex": {
                    "availableMargin": "5000.00",
                    "portfolioValue": "15000.00",
                    "balances": {
                        "xbt": "0.5",
                        "usd": "10000.00"
                    }
                }
            }
        }"#;
        let resp: FuturesAccountsResponse = serde_json::from_str(json)?;
        let flex = resp
            .accounts
            .context("missing accounts")?
            .flex
            .context("missing flex")?;
        assert_eq!(flex.available_margin.as_deref(), Some("5000.00"));
        assert_eq!(flex.balances.as_ref().context("missing balances")?.len(), 2);
        Ok(())
    }

    #[test]
    fn test_deserialize_positions_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "openPositions": [{
                "symbol": "PF_XBTUSD",
                "side": "long",
                "size": "100",
                "price": "67000.0",
                "unrealizedFunding": "12.50"
            }]
        }"#;
        let resp: FuturesPositionsResponse = serde_json::from_str(json)?;
        assert_eq!(resp.open_positions.len(), 1);
        assert_eq!(resp.open_positions[0].symbol, "PF_XBTUSD");
        assert_eq!(resp.open_positions[0].side, "long");
        Ok(())
    }

    #[test]
    fn test_deserialize_fills_response() -> anyhow::Result<()> {
        let json = r#"{
            "result": "success",
            "fills": [{
                "fill_id": "fill-1",
                "order_id": "ord-1",
                "symbol": "PF_XBTUSD",
                "side": "buy",
                "price": "67000.0",
                "size": "0.01",
                "fee": "0.05",
                "fillTime": "2024-01-15T10:30:00.000Z"
            }]
        }"#;
        let resp: FuturesFillsResponse = serde_json::from_str(json)?;
        assert_eq!(resp.fills.len(), 1);
        assert_eq!(resp.fills[0].fill_id, "fill-1");
        assert_eq!(resp.fills[0].fee.as_deref(), Some("0.05"));
        Ok(())
    }

    // ---- WebSocket message tests ----

    #[test]
    fn test_deserialize_ws_control_message() -> anyhow::Result<()> {
        let json = r#"{
            "event": "subscribed",
            "feed": "ticker",
            "product_ids": ["PF_XBTUSD"]
        }"#;
        let msg: FuturesWsControlMessage = serde_json::from_str(json)?;
        assert_eq!(msg.event, "subscribed");
        assert_eq!(msg.feed.as_deref(), Some("ticker"));
        assert_eq!(
            msg.product_ids
                .as_ref()
                .context("missing product_ids")?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_ticker_feed() -> anyhow::Result<()> {
        let json = r#"{
            "feed": "ticker",
            "product_id": "PF_XBTUSD",
            "bid": "67000.0",
            "ask": "67010.0",
            "last": "67005.0",
            "volume": "15000.5",
            "markPrice": "67002.0"
        }"#;
        let feed: FuturesWsFeed = serde_json::from_str(json)?;
        if let FuturesWsFeed::Ticker(entry) = feed {
            assert_eq!(entry.product_id, "PF_XBTUSD");
            assert_eq!(entry.bid.as_deref(), Some("67000.0"));
        } else {
            return Err(anyhow::anyhow!("expected Ticker variant"));
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_trade_feed() -> anyhow::Result<()> {
        let json = r#"{
            "feed": "trade",
            "product_id": "PF_XBTUSD",
            "side": "buy",
            "price": "67000.5",
            "qty": "0.01",
            "time": 1705312200000
        }"#;
        let feed: FuturesWsFeed = serde_json::from_str(json)?;
        if let FuturesWsFeed::Trade(data) = feed {
            assert_eq!(data.product_id.as_deref(), Some("PF_XBTUSD"));
            assert_eq!(data.side.as_deref(), Some("buy"));
        } else {
            return Err(anyhow::anyhow!("expected Trade variant"));
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_book_snapshot() -> anyhow::Result<()> {
        let json = r#"{
            "feed": "book_snapshot",
            "product_id": "PF_XBTUSD",
            "bids": [{"price": "67000.0", "qty": "3.5"}],
            "asks": [{"price": "67010.0", "qty": "2.0"}]
        }"#;
        let feed: FuturesWsFeed = serde_json::from_str(json)?;
        if let FuturesWsFeed::BookSnapshot(entry) = feed {
            assert_eq!(entry.product_id, "PF_XBTUSD");
            assert_eq!(entry.bids.len(), 1);
            assert_eq!(entry.asks.len(), 1);
            assert_eq!(entry.bids[0].price, "67000.0");
        } else {
            return Err(anyhow::anyhow!("expected BookSnapshot variant"));
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_ws_fills() -> anyhow::Result<()> {
        let json = r#"{
            "feed": "fills",
            "fills": [{
                "instrument": "PF_XBTUSD",
                "side": "buy",
                "price": "67000.0",
                "qty": "0.01",
                "order_id": "ord-1",
                "fill_id": "fill-1",
                "fee_paid": "0.05",
                "fee_currency": "USD",
                "time": 1705312200000
            }]
        }"#;
        let feed: FuturesWsFeed = serde_json::from_str(json)?;
        if let FuturesWsFeed::Fills(data) = feed {
            assert_eq!(data.fills.len(), 1);
            assert_eq!(data.fills[0].instrument, "PF_XBTUSD");
            assert_eq!(data.fills[0].fill_id, "fill-1");
        } else {
            return Err(anyhow::anyhow!("expected Fills variant"));
        }
        Ok(())
    }
}
