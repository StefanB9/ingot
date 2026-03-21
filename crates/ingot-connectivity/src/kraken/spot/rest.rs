use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_core::{
    Balance, Instrument, OhlcvBar, OpenOrder, OrderBookSnapshot, OrderFill, OrderId, OrderRequest,
    Position, Tick, TickerSnapshot,
};
use ingot_primitives::Symbol;
use reqwest::Client;
use serde::de::DeserializeOwned;
use tracing::instrument;

use super::{
    mapper,
    models::{
        KrakenAddOrderResult, KrakenAssetPair, KrakenCancelResult, KrakenOhlcValue,
        KrakenOpenOrder, KrakenOpenOrdersResult, KrakenOrderBook, KrakenResponse, KrakenTickerInfo,
        KrakenTradesHistoryResult, KrakenTradesValue, KrakenWsTokenResult,
    },
};
use crate::{
    config::KrakenSpotConfig,
    error::ConnectivityError,
    kraken::auth,
    rate_limiter::RateLimiter,
    traits::{AccountProvider, MarketDataProvider, OrderExecutor},
};

pub struct KrakenSpotRestClient {
    http: Client,
    config: KrakenSpotConfig,
    rate_limiter: RateLimiter,
}

impl KrakenSpotRestClient {
    /// Create a new client with default rate limiter (15 tokens, ~0.33/sec).
    pub fn new(config: KrakenSpotConfig) -> anyhow::Result<Self> {
        let http = Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        let rate_limiter = RateLimiter::new(15, 1.0 / 3.0);
        Ok(Self {
            http,
            config,
            rate_limiter,
        })
    }

    /// Create a new client with a shared rate limiter.
    #[allow(dead_code)]
    pub(crate) fn with_rate_limiter(
        config: KrakenSpotConfig,
        rate_limiter: RateLimiter,
    ) -> anyhow::Result<Self> {
        let http = Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            config,
            rate_limiter,
        })
    }

    /// Fetch a WebSocket authentication token for private subscriptions.
    #[instrument(skip(self))]
    pub(crate) async fn get_ws_token(&self) -> anyhow::Result<String> {
        let result: KrakenWsTokenResult = self.post("/0/private/GetWebSocketsToken", &[]).await?;
        Ok(result.token)
    }

    /// Send a GET request, parse the Kraken envelope, and check for errors.
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<T> {
        self.rate_limiter
            .acquire()
            .await
            .context("rate limiter failed")?;

        let url = format!("{}{path}", self.config.rest_url);
        let response = self
            .http
            .get(&url)
            .query(params)
            .send()
            .await
            .map_err(ConnectivityError::Http)
            .context("HTTP request failed")?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ConnectivityError::RateLimited {
                retry_after_ms: 5000,
            }
            .into());
        }

        let body = response
            .text()
            .await
            .map_err(ConnectivityError::Http)
            .context("failed to read response body")?;

        let envelope: KrakenResponse<T> = serde_json::from_str(&body)
            .map_err(ConnectivityError::Deserialization)
            .context("failed to deserialize Kraken response")?;

        if !envelope.error.is_empty() {
            let msg = envelope.error.join("; ");
            return Err(ConnectivityError::ApiError {
                code: "KRAKEN".into(),
                message: msg,
            }
            .into());
        }

        envelope.result.ok_or_else(|| {
            ConnectivityError::InvalidResponse(
                "Kraken response had empty error array but no result".into(),
            )
            .into()
        })
    }

    /// Send an authenticated POST request to a private Kraken endpoint.
    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<T> {
        self.rate_limiter
            .acquire()
            .await
            .context("rate limiter failed")?;

        let nonce = auth::generate_nonce().context("failed to generate nonce")?;

        // Build URL-encoded POST body: nonce=...&key1=val1&key2=val2
        let mut post_data = format!("nonce={nonce}");
        for (key, value) in params {
            post_data.push('&');
            post_data.push_str(key);
            post_data.push('=');
            post_data.push_str(value);
        }

        let signature = auth::sign_request(path, &nonce, &post_data, &self.config.api_secret)
            .context("failed to sign request")?;

        let url = format!("{}{path}", self.config.rest_url);
        let response = self
            .http
            .post(&url)
            .header("API-Key", &self.config.api_key)
            .header("API-Sign", &signature)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(post_data)
            .send()
            .await
            .map_err(ConnectivityError::Http)
            .context("HTTP POST request failed")?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ConnectivityError::RateLimited {
                retry_after_ms: 5000,
            }
            .into());
        }

        let body = response
            .text()
            .await
            .map_err(ConnectivityError::Http)
            .context("failed to read response body")?;

        let envelope: KrakenResponse<T> = serde_json::from_str(&body)
            .map_err(ConnectivityError::Deserialization)
            .context("failed to deserialize Kraken response")?;

        if !envelope.error.is_empty() {
            let msg = envelope.error.join("; ");
            // Classify Kraken errors
            if msg.contains("EAPI:") {
                return Err(ConnectivityError::AuthenticationFailed { reason: msg }.into());
            }
            if msg.contains("EOrder:Insufficient") {
                return Err(ConnectivityError::OrderRejected { reason: msg }.into());
            }
            return Err(ConnectivityError::ApiError {
                code: "KRAKEN".into(),
                message: msg,
            }
            .into());
        }

        envelope.result.ok_or_else(|| {
            ConnectivityError::InvalidResponse(
                "Kraken response had empty error array but no result".into(),
            )
            .into()
        })
    }
}

impl MarketDataProvider for KrakenSpotRestClient {
    #[instrument(skip(self))]
    async fn fetch_instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        let pairs: HashMap<String, KrakenAssetPair> = self.get("/0/public/AssetPairs", &[]).await?;

        pairs
            .iter()
            .map(|(name, pair)| mapper::map_asset_pair(name, pair))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map asset pairs")
    }

    #[instrument(skip(self))]
    async fn fetch_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<OhlcvBar>> {
        let kraken_interval = mapper::interval_to_kraken(interval).context("invalid interval")?;

        let mut params = vec![("pair", symbol.as_str()), ("interval", &kraken_interval)];
        let since_str;
        if let Some(ts) = since {
            since_str = ts.timestamp().to_string();
            params.push(("since", &since_str));
        }

        let result: HashMap<String, KrakenOhlcValue> = self.get("/0/public/OHLC", &params).await?;

        let bars = result
            .into_iter()
            .find_map(|(_, v)| match v {
                KrakenOhlcValue::Bars(bars) => Some(bars),
                KrakenOhlcValue::Last(_) => None,
            })
            .ok_or_else(|| ConnectivityError::InvalidResponse("no OHLC data in response".into()))?;

        bars.iter()
            .map(|bar| mapper::map_ohlc_bar(symbol, &kraken_interval, bar))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map OHLC bars")
    }

    #[instrument(skip(self))]
    async fn fetch_trades(
        &self,
        symbol: &Symbol,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<(Vec<Tick>, Option<DateTime<Utc>>)> {
        let mut params: Vec<(&str, &str)> = vec![("pair", symbol.as_str())];
        let since_str;
        if let Some(ts) = since {
            since_str = ts
                .timestamp_nanos_opt()
                .context("timestamp out of range")?
                .to_string();
            params.push(("since", &since_str));
        }

        let result: HashMap<String, KrakenTradesValue> =
            self.get("/0/public/Trades", &params).await?;

        let mut trades_raw = None;
        let mut last_cursor = None;

        for (_key, value) in result {
            match value {
                KrakenTradesValue::Trades(t) => trades_raw = Some(t),
                KrakenTradesValue::Last(cursor) => {
                    if let Ok(nanos) = cursor.parse::<i64>() {
                        #[allow(clippy::cast_sign_loss)]
                        let sub_nanos = (nanos % 1_000_000_000) as u32;
                        last_cursor = DateTime::from_timestamp(nanos / 1_000_000_000, sub_nanos);
                    }
                }
            }
        }

        let raw = trades_raw.ok_or_else(|| {
            ConnectivityError::InvalidResponse("no trade data in response".into())
        })?;

        let ticks = raw
            .iter()
            .map(|t| mapper::map_trade(symbol, t))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map trades")?;

        Ok((ticks, last_cursor))
    }

    #[instrument(skip(self))]
    async fn fetch_ticker(&self, symbol: &Symbol) -> anyhow::Result<TickerSnapshot> {
        let result: HashMap<String, KrakenTickerInfo> = self
            .get("/0/public/Ticker", &[("pair", symbol.as_str())])
            .await?;

        let (_name, info) = result
            .into_iter()
            .next()
            .ok_or_else(|| ConnectivityError::InvalidResponse("empty ticker response".into()))?;

        mapper::map_ticker(symbol, &info).context("failed to map ticker")
    }

    #[instrument(skip(self))]
    async fn fetch_order_book(
        &self,
        symbol: &Symbol,
        depth: u32,
    ) -> anyhow::Result<OrderBookSnapshot> {
        let depth_str = depth.to_string();
        let params = [("pair", symbol.as_str()), ("count", &depth_str)];

        let result: HashMap<String, KrakenOrderBook> = self.get("/0/public/Depth", &params).await?;

        let (_name, book) = result.into_iter().next().ok_or_else(|| {
            ConnectivityError::InvalidResponse("empty order book response".into())
        })?;

        mapper::map_order_book(symbol, &book).context("failed to map order book")
    }
}

impl AccountProvider for KrakenSpotRestClient {
    #[instrument(skip(self))]
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        let raw: HashMap<String, String> = self.post("/0/private/Balance", &[]).await?;
        mapper::map_balances(&raw).context("failed to map balances")
    }

    #[instrument(skip(self))]
    async fn get_positions(&self) -> anyhow::Result<Vec<Position>> {
        // Kraken spot has no positions concept
        Ok(vec![])
    }

    #[instrument(skip(self))]
    async fn get_trade_history(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<OrderFill>> {
        let mut params: Vec<(&str, &str)> = vec![];
        let since_str;
        if let Some(ts) = since {
            since_str = ts.timestamp().to_string();
            params.push(("start", &since_str));
        }

        let result: KrakenTradesHistoryResult =
            self.post("/0/private/TradesHistory", &params).await?;

        result
            .trades
            .iter()
            .map(|(id, entry)| mapper::map_trade_history_entry(id, entry))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map trade history")
    }
}

impl OrderExecutor for KrakenSpotRestClient {
    #[instrument(skip(self))]
    async fn place_order(&self, request: &OrderRequest) -> anyhow::Result<OrderId> {
        let side = mapper::order_side_to_kraken(request.side);
        let ordertype = mapper::order_type_to_kraken(request.order_type);
        let volume = request.quantity.value().to_string();

        let mut params: Vec<(&str, &str)> = vec![
            ("pair", request.symbol.as_str()),
            ("type", side),
            ("ordertype", ordertype),
            ("volume", &volume),
        ];

        let price_str;
        if let Some(price) = request.limit_price {
            price_str = price.value().to_string();
            params.push(("price", &price_str));
        }

        let stop_price_str;
        if let Some(stop) = request.stop_price {
            stop_price_str = stop.value().to_string();
            params.push(("price2", &stop_price_str));
        }

        let tif_str;
        if let Some(tif) = mapper::time_in_force_to_kraken(request.time_in_force)
            .context("unsupported time-in-force")?
        {
            tif_str = tif.to_owned();
            params.push(("timeinforce", &tif_str));
        }

        let result: KrakenAddOrderResult = self.post("/0/private/AddOrder", &params).await?;

        let txid = result.txid.into_iter().next().ok_or_else(|| {
            ConnectivityError::InvalidResponse("AddOrder returned no txid".into())
        })?;

        OrderId::new(&txid).context("invalid order ID from Kraken")
    }

    #[instrument(skip(self))]
    async fn cancel_order(&self, order_id: &OrderId) -> anyhow::Result<()> {
        let _result: KrakenCancelResult = self
            .post("/0/private/CancelOrder", &[("txid", order_id.as_str())])
            .await?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn cancel_all_orders(&self) -> anyhow::Result<u32> {
        let result: KrakenCancelResult = self.post("/0/private/CancelAll", &[]).await?;
        Ok(result.count)
    }

    #[instrument(skip(self))]
    async fn get_order_status(&self, order_id: &OrderId) -> anyhow::Result<OpenOrder> {
        let result: HashMap<String, KrakenOpenOrder> = self
            .post("/0/private/QueryOrders", &[("txid", order_id.as_str())])
            .await?;

        let (txid, order) = result.into_iter().next().ok_or_else(|| {
            ConnectivityError::InvalidResponse("QueryOrders returned no orders".into())
        })?;

        mapper::map_open_order(&txid, &order).context("failed to map order status")
    }

    #[instrument(skip(self))]
    async fn get_open_orders(&self) -> anyhow::Result<Vec<OpenOrder>> {
        let result: KrakenOpenOrdersResult = self
            .post("/0/private/OpenOrders", &[("trades", "true")])
            .await?;

        result
            .open
            .iter()
            .map(|(txid, order)| mapper::map_open_order(txid, order))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map open orders")
    }
}

#[cfg(test)]
#[allow(clippy::unreadable_literal)]
mod tests {
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    use super::*;

    /// A valid base64-encoded secret for testing (contents don't matter for
    /// wiremock).
    const TEST_API_SECRET: &str = "c3VwZXJzZWNyZXRrZXkxMjM0NTY3ODkwYWJjZGVm";

    async fn setup() -> anyhow::Result<(MockServer, KrakenSpotRestClient)> {
        let server = MockServer::start().await;
        let config = KrakenSpotConfig {
            api_key: "test-api-key".into(),
            api_secret: TEST_API_SECRET.into(),
            rest_url: server.uri(),
            ws_url: String::new(),
            ws_auth_url: String::new(),
        };
        let client = KrakenSpotRestClient::new(config)?;
        Ok((server, client))
    }

    // ---- Envelope / error handling ----

    #[tokio::test]
    async fn test_get_handles_api_error() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Ticker"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": ["EGeneral:Invalid arguments"],
                "result": null
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("BAD")?;
        let result = client.fetch_ticker(&symbol).await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("EGeneral:Invalid arguments"),
            "unexpected error: {err_msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_get_handles_429_rate_limit() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Ticker"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let result = client.fetch_ticker(&symbol).await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("rate limited"),
            "unexpected error: {err_msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_get_handles_invalid_json() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Ticker"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not valid json"))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let result = client.fetch_ticker(&symbol).await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("deserialize"),
            "unexpected error: {err_msg}"
        );
        Ok(())
    }

    // ---- fetch_instruments ----

    #[tokio::test]
    async fn test_fetch_instruments_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/AssetPairs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": {
                        "base": "XXBT",
                        "quote": "ZUSD",
                        "wsname": "XBT/USD",
                        "altname": "XBTUSD",
                        "pair_decimals": 1,
                        "lot_decimals": 8,
                        "ordermin": "0.0001",
                        "costmin": "0.5",
                        "tick_size": "0.1",
                        "leverage_buy": [2, 3],
                        "leverage_sell": [2, 3]
                    },
                    "XETHZUSD": {
                        "base": "XETH",
                        "quote": "ZUSD",
                        "wsname": "ETH/USD",
                        "altname": "ETHUSD",
                        "pair_decimals": 2,
                        "lot_decimals": 8,
                        "ordermin": "0.01",
                        "costmin": "0.5",
                        "tick_size": "0.01",
                        "leverage_buy": [],
                        "leverage_sell": []
                    }
                }
            })))
            .mount(&server)
            .await;

        let instruments = client.fetch_instruments().await?;
        assert_eq!(instruments.len(), 2);

        let btc = instruments
            .iter()
            .find(|i| i.symbol.as_str() == "XXBTZUSD")
            .context("missing XXBTZUSD")?;
        assert_eq!(btc.base_currency, ingot_primitives::Currency::BTC);
        assert_eq!(btc.quote_currency, ingot_primitives::Currency::USD);
        assert_eq!(btc.tick_size.value(), dec!(0.1));
        assert_eq!(btc.display_name.as_str(), "XBT/USD");

        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_instruments_api_error() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/AssetPairs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": ["EGeneral:Unknown method"],
                "result": null
            })))
            .mount(&server)
            .await;

        let result = client.fetch_instruments().await;
        assert!(result.is_err());
        Ok(())
    }

    // ---- fetch_ohlcv ----

    #[tokio::test]
    async fn test_fetch_ohlcv_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/OHLC"))
            .and(query_param("pair", "XXBTZUSD"))
            .and(query_param("interval", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": [
                        [1616663400, "56200.0", "56300.0", "56100.0", "56250.0", "56225.5", "12.345", 847],
                        [1616663460, "56250.0", "56350.0", "56200.0", "56300.0", "56275.0", "8.5", 520]
                    ],
                    "last": 1616663460
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let bars = client.fetch_ohlcv(&symbol, "1m", None).await?;

        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].open.value(), dec!(56200.0));
        assert_eq!(bars[0].interval.as_str(), "1m");
        assert_eq!(bars[1].close.value(), dec!(56300.0));
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_ohlcv_with_since() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/OHLC"))
            .and(query_param("since", "1616663400"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": [
                        [1616663460, "56250.0", "56350.0", "56200.0", "56300.0", "56275.0", "8.5", 520]
                    ],
                    "last": 1616663460
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let since = DateTime::from_timestamp(1_616_663_400, 0).context("bad ts")?;
        let bars = client.fetch_ohlcv(&symbol, "1m", Some(since)).await?;

        assert_eq!(bars.len(), 1);
        Ok(())
    }

    // ---- fetch_trades ----

    #[tokio::test]
    async fn test_fetch_trades_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Trades"))
            .and(query_param("pair", "XXBTZUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": [
                        ["56200.10000", "0.00100000", 1616663594.2009, "b", "m", "", 12345],
                        ["56210.00000", "0.50000000", 1616663595.0, "s", "l", "", 12346]
                    ],
                    "last": "1616663595000000000"
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let (ticks, cursor) = client.fetch_trades(&symbol, None).await?;

        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[0].side, Some(ingot_primitives::OrderSide::Buy));
        assert_eq!(ticks[1].side, Some(ingot_primitives::OrderSide::Sell));
        assert!(cursor.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_trades_buy_sell_mapping() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Trades"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": [
                        ["56200.0", "1.0", 1616663594.0, "b", "m", "", 1],
                        ["56210.0", "2.0", 1616663595.0, "s", "l", "", 2]
                    ],
                    "last": "1616663595000000000"
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let (ticks, _) = client.fetch_trades(&symbol, None).await?;

        assert_eq!(ticks[0].side, Some(ingot_primitives::OrderSide::Buy));
        assert_eq!(ticks[1].side, Some(ingot_primitives::OrderSide::Sell));
        Ok(())
    }

    // ---- fetch_ticker ----

    #[tokio::test]
    async fn test_fetch_ticker_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Ticker"))
            .and(query_param("pair", "XXBTZUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": {
                        "a": ["67010.00000", "1", "1.000"],
                        "b": ["67000.00000", "2", "2.000"],
                        "c": ["67005.00000", "0.001"],
                        "v": ["1000.0", "5000.0"],
                        "p": ["67005.0", "67000.0"],
                        "t": [100, 500],
                        "l": ["66900.0", "66800.0"],
                        "h": ["67100.0", "67200.0"],
                        "o": "66950.0"
                    }
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let ticker = client.fetch_ticker(&symbol).await?;

        assert_eq!(ticker.bid.value(), dec!(67000.00000));
        assert_eq!(ticker.ask.value(), dec!(67010.00000));
        assert_eq!(ticker.last.value(), dec!(67005.00000));
        assert_eq!(ticker.volume_24h.value(), dec!(5000.0));
        Ok(())
    }

    // ---- fetch_order_book ----

    #[tokio::test]
    async fn test_fetch_order_book_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Depth"))
            .and(query_param("pair", "XXBTZUSD"))
            .and(query_param("count", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": {
                        "asks": [
                            ["67010.0", "1.5", 1616663400],
                            ["67020.0", "2.0", 1616663401]
                        ],
                        "bids": [
                            ["67000.0", "3.0", 1616663400],
                            ["66990.0", "1.0", 1616663399]
                        ]
                    }
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let book = client.fetch_order_book(&symbol, 2).await?;

        assert_eq!(book.asks.len(), 2);
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.asks[0].price.value(), dec!(67010.0));
        assert_eq!(book.bids[0].price.value(), dec!(67000.0));
        assert_eq!(book.bids[0].quantity.value(), dec!(3.0));
        Ok(())
    }

    // ---- POST helper / auth ----

    #[tokio::test]
    async fn test_post_sends_auth_headers() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/Balance"))
            .and(wiremock::matchers::header_exists("API-Key"))
            .and(wiremock::matchers::header_exists("API-Sign"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {}
            })))
            .mount(&server)
            .await;

        let balances = client.get_balances().await?;
        assert!(balances.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_post_auth_error_classification() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/Balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": ["EAPI:Invalid nonce"],
                "result": null
            })))
            .mount(&server)
            .await;

        let result = client.get_balances().await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("authentication failed"),
            "expected auth error, got: {err_msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_post_handles_429() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/Balance"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let result = client.get_balances().await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("rate limited"),
            "expected rate limit error, got: {err_msg}"
        );
        Ok(())
    }

    // ---- get_ws_token ----

    #[tokio::test]
    async fn test_get_ws_token_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/GetWebSocketsToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "token": "ws-auth-token-123",
                    "expires": 900
                }
            })))
            .mount(&server)
            .await;

        let token = client.get_ws_token().await?;
        assert_eq!(token, "ws-auth-token-123");
        Ok(())
    }

    // ---- get_balances ----

    #[tokio::test]
    async fn test_get_balances_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/Balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBT": "1.5000000000",
                    "ZUSD": "10000.0000",
                    "XETH": "0.0000000000"
                }
            })))
            .mount(&server)
            .await;

        let balances = client.get_balances().await?;
        // Zero balances should be skipped
        assert_eq!(balances.len(), 2);

        let btc = balances
            .iter()
            .find(|b| b.currency == ingot_primitives::Currency::BTC)
            .context("missing BTC")?;
        assert_eq!(btc.total.value(), dec!(1.5));
        assert_eq!(btc.available.value(), dec!(1.5));
        assert_eq!(btc.held.value(), Decimal::ZERO);
        Ok(())
    }

    #[tokio::test]
    async fn test_get_balances_empty() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/Balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {}
            })))
            .mount(&server)
            .await;

        let balances = client.get_balances().await?;
        assert!(balances.is_empty());
        Ok(())
    }

    // ---- get_positions ----

    #[tokio::test]
    async fn test_get_positions_returns_empty() -> anyhow::Result<()> {
        let (_server, client) = setup().await?;
        let positions = client.get_positions().await?;
        assert!(positions.is_empty());
        Ok(())
    }

    // ---- place_order ----

    #[tokio::test]
    async fn test_place_order_market_buy() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/AddOrder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "descr": {"order": "buy 0.001 XBTUSD @ market"},
                    "txid": ["OABCDE-12345-FGHIJ"]
                }
            })))
            .mount(&server)
            .await;

        let request = OrderRequest {
            symbol: Symbol::new("XXBTZUSD")?,
            side: ingot_primitives::OrderSide::Buy,
            order_type: ingot_primitives::OrderType::Market,
            quantity: ingot_primitives::Quantity::new(dec!(0.001))?,
            limit_price: None,
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::GoodTilCancelled,
        };
        let order_id = client.place_order(&request).await?;
        assert_eq!(order_id.as_str(), "OABCDE-12345-FGHIJ");
        Ok(())
    }

    #[tokio::test]
    async fn test_place_order_limit_sell() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/AddOrder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "descr": {"order": "sell 0.5 XBTUSD @ limit 70000"},
                    "txid": ["OSELL-LIMIT-00001"]
                }
            })))
            .mount(&server)
            .await;

        let request = OrderRequest {
            symbol: Symbol::new("XXBTZUSD")?,
            side: ingot_primitives::OrderSide::Sell,
            order_type: ingot_primitives::OrderType::Limit,
            quantity: ingot_primitives::Quantity::new(dec!(0.5))?,
            limit_price: Some(ingot_primitives::Price::new(dec!(70000.0))),
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::GoodTilCancelled,
        };
        let order_id = client.place_order(&request).await?;
        assert_eq!(order_id.as_str(), "OSELL-LIMIT-00001");
        Ok(())
    }

    #[tokio::test]
    async fn test_place_order_insufficient_funds() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/AddOrder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": ["EOrder:Insufficient funds"],
                "result": null
            })))
            .mount(&server)
            .await;

        let request = OrderRequest {
            symbol: Symbol::new("XXBTZUSD")?,
            side: ingot_primitives::OrderSide::Buy,
            order_type: ingot_primitives::OrderType::Market,
            quantity: ingot_primitives::Quantity::new(dec!(100.0))?,
            limit_price: None,
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::GoodTilCancelled,
        };
        let result = client.place_order(&request).await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("order rejected"),
            "expected order rejected, got: {err_msg}"
        );
        Ok(())
    }

    // ---- cancel_order ----

    #[tokio::test]
    async fn test_cancel_order_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/CancelOrder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {"count": 1}
            })))
            .mount(&server)
            .await;

        let order_id = OrderId::new("OABCDE-12345-FGHIJ")?;
        client.cancel_order(&order_id).await?;
        Ok(())
    }

    // ---- cancel_all_orders ----

    #[tokio::test]
    async fn test_cancel_all_orders_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/CancelAll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {"count": 5}
            })))
            .mount(&server)
            .await;

        let count = client.cancel_all_orders().await?;
        assert_eq!(count, 5);
        Ok(())
    }

    // ---- get_order_status ----

    #[tokio::test]
    async fn test_get_order_status_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/QueryOrders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
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
                        "vol_exec": "0.0",
                        "cost": "0.0",
                        "fee": "0.0",
                        "avg_price": "0",
                        "opentm": 1616663594.0
                    }
                }
            })))
            .mount(&server)
            .await;

        let order_id = OrderId::new("OABCDE-12345-FGHIJ")?;
        let order = client.get_order_status(&order_id).await?;
        assert_eq!(order.order_id.as_str(), "OABCDE-12345-FGHIJ");
        assert_eq!(order.status, ingot_core::OrderStatus::Open);
        assert_eq!(order.request.side, ingot_primitives::OrderSide::Buy);
        Ok(())
    }

    // ---- get_open_orders ----

    #[tokio::test]
    async fn test_get_open_orders_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/OpenOrders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
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
                            "opentm": 1616663594.0
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let orders = client.get_open_orders().await?;
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].order_id.as_str(), "OABCDE-12345-FGHIJ");
        assert_eq!(orders[0].status, ingot_core::OrderStatus::PartiallyFilled);
        Ok(())
    }

    #[tokio::test]
    async fn test_get_open_orders_empty() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/OpenOrders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "open": {}
                }
            })))
            .mount(&server)
            .await;

        let orders = client.get_open_orders().await?;
        assert!(orders.is_empty());
        Ok(())
    }

    // ---- get_trade_history ----

    #[tokio::test]
    async fn test_get_trade_history_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/TradesHistory"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "trades": {
                        "TABC-DEF-001": {
                            "ordertxid": "OABCDE-12345-FGHIJ",
                            "pair": "XXBTZUSD",
                            "type": "buy",
                            "ordertype": "market",
                            "price": "67000.50",
                            "vol": "0.001",
                            "cost": "67.0005",
                            "fee": "0.10",
                            "time": 1616663594.0
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let fills = client.get_trade_history(None).await?;
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].order_id.as_str(), "OABCDE-12345-FGHIJ");
        assert_eq!(fills[0].fill_price.value(), dec!(67000.50));
        assert_eq!(fills[0].fee_currency, ingot_primitives::Currency::USD);
        Ok(())
    }

    #[tokio::test]
    async fn test_get_trade_history_empty() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/0/private/TradesHistory"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "trades": {}
                }
            })))
            .mount(&server)
            .await;

        let fills = client.get_trade_history(None).await?;
        assert!(fills.is_empty());
        Ok(())
    }
}
