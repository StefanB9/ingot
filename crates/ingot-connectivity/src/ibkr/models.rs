use serde::{Deserialize, Serialize};

// ── Response models (Deserialize only) ──

/// Contract search result from GET /iserver/secdef/search
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrContractSearchResult {
    pub conid: i64,
    pub company_name: String,
    pub symbol: String,
    pub sec_type: String,
    pub exchange: Option<String>,
    pub currency: String,
}

/// Contract details from GET /iserver/contract/{conid}/info
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrContractDetail {
    pub con_id: i64,
    pub symbol: String,
    pub sec_type: String,
    pub exchange: String,
    pub currency: String,
    pub local_symbol: Option<String>,
    pub trading_class: Option<String>,
    pub multiplier: Option<String>,
    pub expiry: Option<String>,
    pub strike: Option<String>,
    pub right: Option<String>,
    pub company_name: Option<String>,
    #[serde(default)]
    pub valid_exchanges: Option<String>,
}

/// Market data snapshot from GET /iserver/marketdata/snapshot.
/// IBKR uses numeric field IDs as JSON keys.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrMarketSnapshot {
    pub conid: i64,
    #[serde(rename = "31")]
    pub last_price: Option<String>,
    #[serde(rename = "84")]
    pub bid: Option<String>,
    #[serde(rename = "86")]
    pub ask: Option<String>,
    #[serde(rename = "87")]
    pub volume: Option<String>,
    #[serde(rename = "7295")]
    pub open: Option<String>,
    #[serde(rename = "7296")]
    pub high: Option<String>,
    #[serde(rename = "7297")]
    pub low: Option<String>,
    #[serde(rename = "7291")]
    pub close: Option<String>,
}

/// Order status from GET /iserver/account/order/status/{orderId}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrOrderStatus {
    pub order_id: String,
    pub conid: i64,
    pub status: String,
    pub filled_quantity: f64,
    pub remaining_quantity: f64,
    pub avg_price: f64,
    pub last_fill_price: Option<f64>,
    pub side: String,
}

/// Account balance from GET /portfolio/{accountId}/ledger
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrAccountBalance {
    pub currency: String,
    #[serde(rename = "settledcash")]
    pub settled_cash: Option<f64>,
    #[serde(rename = "cashbalance")]
    pub cash_balance: Option<f64>,
}

/// Position from GET /portfolio/{accountId}/positions/0
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrPosition {
    pub conid: i64,
    pub currency: String,
    pub position: f64,
    #[serde(rename = "avgCost")]
    pub avg_cost: f64,
    #[serde(rename = "mktPrice")]
    pub market_price: f64,
    #[serde(rename = "mktValue")]
    pub market_value: f64,
    #[serde(rename = "unrealizedPnl")]
    pub unrealized_pnl: f64,
}

/// Nested amount field used by margin/account summary responses.
/// IBKR returns these as `{ "amount": 12345.67, "currency": "USD" }`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrAmountField {
    pub amount: f64,
    pub currency: Option<String>,
}

/// Margin info from GET /portfolio/{accountId}/summary
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrMarginInfo {
    #[serde(rename = "initmarginreq")]
    pub initial_margin: Option<IbkrAmountField>,
    #[serde(rename = "maintmarginreq")]
    pub maintenance_margin: Option<IbkrAmountField>,
    #[serde(rename = "excessliquidity")]
    pub excess_liquidity: Option<IbkrAmountField>,
    #[serde(rename = "buyingpower")]
    pub buying_power: Option<IbkrAmountField>,
    #[serde(rename = "availablefunds")]
    pub available_funds: Option<IbkrAmountField>,
    #[serde(rename = "netliquidation")]
    pub net_liquidation: Option<IbkrAmountField>,
    #[serde(rename = "sma")]
    pub sma: Option<IbkrAmountField>,
}

/// Trade execution from GET /iserver/account/trades
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrTrade {
    pub execution_id: String,
    pub conid: i64,
    pub side: String,
    pub size: f64,
    pub price: f64,
    pub commission: Option<f64>,
    pub currency: String,
    pub trade_time: String,
    #[serde(rename = "order_ref")]
    pub order_ref: Option<String>,
}

/// Historical data bar from GET /iserver/marketdata/history
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrHistoryBar {
    #[serde(rename = "t")]
    pub timestamp: i64,
    #[serde(rename = "o")]
    pub open: f64,
    #[serde(rename = "h")]
    pub high: f64,
    #[serde(rename = "l")]
    pub low: f64,
    #[serde(rename = "c")]
    pub close: f64,
    #[serde(rename = "v")]
    pub volume: f64,
}

/// Historical data response wrapper
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrHistoryResponse {
    pub data: Vec<IbkrHistoryBar>,
}

// ── Request models (Serialize only) ──

/// Order submission body for POST /iserver/account/{id}/orders
#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub(crate) struct IbkrOrderRequest {
    #[serde(rename = "acctId")]
    pub acct_id: String,
    pub conid: i64,
    #[serde(rename = "secType")]
    pub sec_type: String,
    #[serde(rename = "orderType")]
    pub order_type: String,
    pub side: String,
    pub quantity: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
    #[serde(rename = "auxPrice", skip_serializing_if = "Option::is_none")]
    pub aux_price: Option<f64>,
    pub tif: String,
    #[serde(rename = "listingExchange", skip_serializing_if = "Option::is_none")]
    pub listing_exchange: Option<String>,
}

/// Wrapper for POST /iserver/account/{id}/orders body
#[derive(Debug, Serialize)]
pub(crate) struct IbkrOrderSubmitWrapper {
    pub orders: Vec<IbkrOrderRequest>,
}

/// Confirmation reply for POST /iserver/reply/{replyId}
#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub(crate) struct IbkrConfirmReply {
    pub confirmed: bool,
}

/// Live order from GET /iserver/account/orders (richer than IbkrOrderStatus)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrLiveOrder {
    #[serde(rename = "orderId")]
    pub order_id: String,
    pub conid: i64,
    #[serde(rename = "orderType")]
    pub order_type: String,
    pub side: String,
    pub price: Option<f64>,
    #[serde(rename = "auxPrice")]
    pub aux_price: Option<f64>,
    pub quantity: f64,
    #[serde(rename = "filledQuantity")]
    pub filled_quantity: f64,
    #[serde(rename = "remainingQuantity")]
    pub remaining_quantity: f64,
    pub status: String,
    #[serde(rename = "timeInForce")]
    pub time_in_force: Option<String>,
    pub ticker: Option<String>,
}

/// Response wrapper for GET /iserver/account/orders
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrLiveOrdersResponse {
    pub orders: Vec<IbkrLiveOrder>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Deserialization tests ──

    #[test]
    fn test_deserialize_contract_search_result() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "conid": 265598,
            "company_name": "Apple Inc",
            "symbol": "AAPL",
            "sec_type": "STK",
            "exchange": "NASDAQ",
            "currency": "USD"
        }"#;
        let r: IbkrContractSearchResult = serde_json::from_str(json)?;
        assert_eq!(r.conid, 265598);
        assert_eq!(r.symbol, "AAPL");
        assert_eq!(r.sec_type, "STK");
        assert_eq!(r.exchange.as_deref(), Some("NASDAQ"));
        assert_eq!(r.currency, "USD");
        Ok(())
    }

    #[test]
    fn test_deserialize_contract_detail_equity() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "con_id": 265598,
            "symbol": "AAPL",
            "sec_type": "STK",
            "exchange": "SMART",
            "currency": "USD",
            "company_name": "Apple Inc"
        }"#;
        let d: IbkrContractDetail = serde_json::from_str(json)?;
        assert_eq!(d.con_id, 265598);
        assert_eq!(d.sec_type, "STK");
        assert!(d.expiry.is_none());
        assert!(d.strike.is_none());
        assert!(d.right.is_none());
        assert!(d.multiplier.is_none());
        Ok(())
    }

    #[test]
    fn test_deserialize_contract_detail_future() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "con_id": 495512551,
            "symbol": "ES",
            "sec_type": "FUT",
            "exchange": "CME",
            "currency": "USD",
            "multiplier": "50",
            "expiry": "20260320",
            "trading_class": "ES"
        }"#;
        let d: IbkrContractDetail = serde_json::from_str(json)?;
        assert_eq!(d.sec_type, "FUT");
        assert_eq!(d.multiplier.as_deref(), Some("50"));
        assert_eq!(d.expiry.as_deref(), Some("20260320"));
        Ok(())
    }

    #[test]
    fn test_deserialize_contract_detail_option() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "con_id": 620518337,
            "symbol": "AAPL",
            "sec_type": "OPT",
            "exchange": "SMART",
            "currency": "USD",
            "multiplier": "100",
            "expiry": "20260320",
            "strike": "150.00",
            "right": "C"
        }"#;
        let d: IbkrContractDetail = serde_json::from_str(json)?;
        assert_eq!(d.sec_type, "OPT");
        assert_eq!(d.strike.as_deref(), Some("150.00"));
        assert_eq!(d.right.as_deref(), Some("C"));
        assert_eq!(d.multiplier.as_deref(), Some("100"));
        Ok(())
    }

    #[test]
    fn test_deserialize_market_snapshot_full() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "conid": 265598,
            "31": "178.50",
            "84": "178.49",
            "86": "178.51",
            "87": "45230100",
            "7295": "177.00",
            "7296": "179.20",
            "7297": "176.80",
            "7291": "178.10"
        }"#;
        let s: IbkrMarketSnapshot = serde_json::from_str(json)?;
        assert_eq!(s.conid, 265598);
        assert_eq!(s.last_price.as_deref(), Some("178.50"));
        assert_eq!(s.bid.as_deref(), Some("178.49"));
        assert_eq!(s.ask.as_deref(), Some("178.51"));
        assert_eq!(s.volume.as_deref(), Some("45230100"));
        assert_eq!(s.open.as_deref(), Some("177.00"));
        assert_eq!(s.high.as_deref(), Some("179.20"));
        assert_eq!(s.low.as_deref(), Some("176.80"));
        assert_eq!(s.close.as_deref(), Some("178.10"));
        Ok(())
    }

    #[test]
    fn test_deserialize_market_snapshot_partial() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"conid": 265598, "31": "178.50"}"#;
        let s: IbkrMarketSnapshot = serde_json::from_str(json)?;
        assert_eq!(s.conid, 265598);
        assert_eq!(s.last_price.as_deref(), Some("178.50"));
        assert!(s.bid.is_none());
        assert!(s.ask.is_none());
        assert!(s.volume.is_none());
        Ok(())
    }

    #[test]
    fn test_deserialize_order_status() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "order_id": "12345",
            "conid": 265598,
            "status": "Filled",
            "filled_quantity": 100.0,
            "remaining_quantity": 0.0,
            "avg_price": 178.50,
            "last_fill_price": 178.52,
            "side": "BUY"
        }"#;
        let s: IbkrOrderStatus = serde_json::from_str(json)?;
        assert_eq!(s.order_id, "12345");
        assert_eq!(s.status, "Filled");
        assert!((s.filled_quantity - 100.0).abs() < f64::EPSILON);
        assert!((s.remaining_quantity).abs() < f64::EPSILON);
        assert_eq!(s.last_fill_price, Some(178.52));
        assert_eq!(s.side, "BUY");
        Ok(())
    }

    #[test]
    fn test_deserialize_account_balance() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "currency": "USD",
            "settledcash": 50000.0,
            "cashbalance": 52000.0
        }"#;
        let b: IbkrAccountBalance = serde_json::from_str(json)?;
        assert_eq!(b.currency, "USD");
        assert_eq!(b.settled_cash, Some(50000.0));
        assert_eq!(b.cash_balance, Some(52000.0));
        Ok(())
    }

    #[test]
    fn test_deserialize_position() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "conid": 265598,
            "currency": "USD",
            "position": 100.0,
            "avgCost": 175.50,
            "mktPrice": 178.50,
            "mktValue": 17850.0,
            "unrealizedPnl": 300.0
        }"#;
        let p: IbkrPosition = serde_json::from_str(json)?;
        assert_eq!(p.conid, 265598);
        assert!((p.position - 100.0).abs() < f64::EPSILON);
        assert!((p.avg_cost - 175.50).abs() < f64::EPSILON);
        assert!((p.market_price - 178.50).abs() < f64::EPSILON);
        assert!((p.unrealized_pnl - 300.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn test_deserialize_margin_info_full() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "initmarginreq": {"amount": 25000.0, "currency": "USD"},
            "maintmarginreq": {"amount": 20000.0, "currency": "USD"},
            "excessliquidity": {"amount": 75000.0, "currency": "USD"},
            "buyingpower": {"amount": 150000.0, "currency": "USD"},
            "availablefunds": {"amount": 80000.0, "currency": "USD"},
            "netliquidation": {"amount": 100000.0, "currency": "USD"},
            "sma": {"amount": 90000.0, "currency": "USD"}
        }"#;
        let m: IbkrMarginInfo = serde_json::from_str(json)?;
        let im = m.initial_margin.as_ref().ok_or("missing initial_margin")?;
        assert!((im.amount - 25000.0).abs() < f64::EPSILON);
        assert_eq!(im.currency.as_deref(), Some("USD"));
        let nl = m
            .net_liquidation
            .as_ref()
            .ok_or("missing net_liquidation")?;
        assert!((nl.amount - 100000.0).abs() < f64::EPSILON);
        assert!(m.sma.is_some());
        Ok(())
    }

    #[test]
    fn test_deserialize_margin_info_partial() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "initmarginreq": {"amount": 25000.0, "currency": "USD"},
            "maintmarginreq": {"amount": 20000.0}
        }"#;
        let m: IbkrMarginInfo = serde_json::from_str(json)?;
        assert!(m.initial_margin.is_some());
        let mm = m
            .maintenance_margin
            .as_ref()
            .ok_or("missing maintenance_margin")?;
        assert!(mm.currency.is_none());
        assert!(m.excess_liquidity.is_none());
        assert!(m.buying_power.is_none());
        Ok(())
    }

    #[test]
    fn test_deserialize_history_bar() -> Result<(), Box<dyn std::error::Error>> {
        let json =
            r#"{"t": 1711900800, "o": 177.0, "h": 179.2, "l": 176.8, "c": 178.5, "v": 45230100.0}"#;
        let b: IbkrHistoryBar = serde_json::from_str(json)?;
        assert_eq!(b.timestamp, 1_711_900_800);
        assert!((b.open - 177.0).abs() < f64::EPSILON);
        assert!((b.close - 178.5).abs() < f64::EPSILON);
        assert!((b.volume - 45_230_100.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn test_deserialize_history_response() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"data": [
            {"t": 1711900800, "o": 177.0, "h": 179.2, "l": 176.8, "c": 178.5, "v": 45230100.0},
            {"t": 1711987200, "o": 178.5, "h": 180.0, "l": 178.0, "c": 179.5, "v": 38100000.0}
        ]}"#;
        let r: IbkrHistoryResponse = serde_json::from_str(json)?;
        assert_eq!(r.data.len(), 2);
        assert_eq!(r.data[0].timestamp, 1_711_900_800);
        assert_eq!(r.data[1].timestamp, 1_711_987_200);
        Ok(())
    }

    #[test]
    fn test_deserialize_amount_field() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"amount": 12345.67, "currency": "USD"}"#;
        let f: IbkrAmountField = serde_json::from_str(json)?;
        assert!((f.amount - 12345.67).abs() < f64::EPSILON);
        assert_eq!(f.currency.as_deref(), Some("USD"));
        Ok(())
    }

    // ── Serialization tests ──

    #[test]
    fn test_serialize_order_request_full() -> Result<(), Box<dyn std::error::Error>> {
        let req = IbkrOrderRequest {
            acct_id: "DU123".into(),
            conid: 265598,
            sec_type: "STK".into(),
            order_type: "LMT".into(),
            side: "BUY".into(),
            quantity: 100.0,
            price: Some(178.50),
            aux_price: None,
            tif: "GTC".into(),
            listing_exchange: Some("SMART".into()),
        };
        let json = serde_json::to_string(&req)?;
        assert!(json.contains(r#""acctId":"DU123""#));
        assert!(json.contains(r#""orderType":"LMT""#));
        assert!(json.contains(r#""secType":"STK""#));
        assert!(json.contains(r#""price":178.5"#));
        assert!(json.contains(r#""listingExchange":"SMART""#));
        // auxPrice should be omitted
        assert!(!json.contains("auxPrice"));
        Ok(())
    }

    #[test]
    fn test_serialize_order_request_skips_none() -> Result<(), Box<dyn std::error::Error>> {
        let req = IbkrOrderRequest {
            acct_id: "DU123".into(),
            conid: 265598,
            sec_type: "STK".into(),
            order_type: "MKT".into(),
            side: "SELL".into(),
            quantity: 50.0,
            price: None,
            aux_price: None,
            tif: "DAY".into(),
            listing_exchange: None,
        };
        let json = serde_json::to_string(&req)?;
        assert!(!json.contains("price"));
        assert!(!json.contains("auxPrice"));
        assert!(!json.contains("listingExchange"));
        Ok(())
    }

    #[test]
    fn test_serialize_confirm_reply() -> Result<(), Box<dyn std::error::Error>> {
        let reply = IbkrConfirmReply { confirmed: true };
        let json = serde_json::to_string(&reply)?;
        assert_eq!(json, r#"{"confirmed":true}"#);
        Ok(())
    }
}
