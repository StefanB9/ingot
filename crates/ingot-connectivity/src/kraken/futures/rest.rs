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
        FuturesAccountsResponse, FuturesCancelAllResponse, FuturesCancelOrderResponse,
        FuturesFillsResponse, FuturesHistoryResponse, FuturesInstrumentsResponse,
        FuturesOpenOrdersResponse, FuturesOrderBookResponse, FuturesPositionsResponse,
        FuturesSendOrderResponse, FuturesTickersResponse,
    },
};
use crate::{
    config::KrakenFuturesConfig,
    error::ConnectivityError,
    kraken::{auth, common},
    rate_limiter::RateLimiter,
    traits::{AccountProvider, MarketDataProvider, OrderExecutor},
};

pub struct KrakenFuturesRestClient {
    http: Client,
    config: KrakenFuturesConfig,
    rate_limiter: RateLimiter,
}

impl KrakenFuturesRestClient {
    /// Create a new client with default rate limiter (10 tokens, 0.5/sec).
    pub fn new(config: KrakenFuturesConfig) -> anyhow::Result<Self> {
        let http = Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        let rate_limiter = RateLimiter::new(10, 0.5);
        Ok(Self {
            http,
            config,
            rate_limiter,
        })
    }

    /// Send a public GET request.
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
            .context("HTTP GET request failed")?;

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

        let parsed: T = serde_json::from_str(&body)
            .map_err(ConnectivityError::Deserialization)
            .context("failed to deserialize Futures response")?;

        Ok(parsed)
    }

    /// Send an authenticated GET request with API key + HMAC signature headers.
    async fn get_authenticated<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<T> {
        self.rate_limiter
            .acquire()
            .await
            .context("rate limiter failed")?;

        let nonce = auth::generate_nonce().context("failed to generate nonce")?;

        // Build query string for signature
        let query_string = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");

        let signature =
            auth::sign_futures_request(path, &nonce, &query_string, &self.config.api_secret)
                .context("failed to sign request")?;

        let url = format!("{}{path}", self.config.rest_url);
        let response = self
            .http
            .get(&url)
            .query(params)
            .header("APIKey", &self.config.api_key)
            .header("Authent", &signature)
            .header("Nonce", &nonce)
            .send()
            .await
            .map_err(ConnectivityError::Http)
            .context("authenticated GET request failed")?;

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

        let parsed: T = serde_json::from_str(&body)
            .map_err(ConnectivityError::Deserialization)
            .context("failed to deserialize Futures response")?;

        Ok(parsed)
    }

    /// Send an authenticated POST request.
    async fn post_authenticated<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<T> {
        self.rate_limiter
            .acquire()
            .await
            .context("rate limiter failed")?;

        let nonce = auth::generate_nonce().context("failed to generate nonce")?;

        let post_data = params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");

        let signature =
            auth::sign_futures_request(path, &nonce, &post_data, &self.config.api_secret)
                .context("failed to sign request")?;

        let url = format!("{}{path}", self.config.rest_url);
        let response = self
            .http
            .post(&url)
            .header("APIKey", &self.config.api_key)
            .header("Authent", &signature)
            .header("Nonce", &nonce)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(post_data)
            .send()
            .await
            .map_err(ConnectivityError::Http)
            .context("authenticated POST request failed")?;

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

        let parsed: T = serde_json::from_str(&body)
            .map_err(ConnectivityError::Deserialization)
            .context("failed to deserialize Futures response")?;

        Ok(parsed)
    }

    /// Check the Futures response envelope for errors and return an appropriate
    /// error.
    fn check_result(result: &str, error: Option<&String>) -> anyhow::Result<()> {
        if result == "success" {
            return Ok(());
        }
        let msg = error.map_or("unknown error", String::as_str);
        if msg.contains("authenticationError") || msg.contains("apiLimitExceeded") {
            return Err(ConnectivityError::AuthenticationFailed {
                reason: msg.to_owned(),
            }
            .into());
        }
        if msg.contains("Insufficient") {
            return Err(ConnectivityError::OrderRejected {
                reason: msg.to_owned(),
            }
            .into());
        }
        Err(ConnectivityError::ApiError {
            code: "KRAKEN_FUTURES".into(),
            message: msg.to_owned(),
        }
        .into())
    }
}

impl MarketDataProvider for KrakenFuturesRestClient {
    #[instrument(skip(self))]
    async fn fetch_instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        let resp: FuturesInstrumentsResponse = self.get("/instruments", &[]).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        resp.instruments
            .iter()
            .map(mapper::map_futures_instrument)
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map futures instruments")
    }

    #[instrument(skip(self, symbol, interval, since))]
    async fn fetch_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<OhlcvBar>> {
        let _ = (symbol, interval, since);
        tracing::debug!("Kraken Futures has no OHLCV endpoint, returning empty");
        Ok(vec![])
    }

    #[instrument(skip(self))]
    async fn fetch_trades(
        &self,
        symbol: &Symbol,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<(Vec<Tick>, Option<DateTime<Utc>>)> {
        let mut params: Vec<(&str, &str)> = vec![("symbol", symbol.as_str())];
        let since_str;
        if let Some(ts) = since {
            since_str = ts.to_rfc3339();
            params.push(("lastTime", &since_str));
        }

        let resp: FuturesHistoryResponse = self.get("/history", &params).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        let ticks = resp
            .history
            .iter()
            .map(|t| mapper::map_futures_trade(symbol, t))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map futures trades")?;

        // Cursor: last trade's timestamp
        let cursor = ticks.last().map(|t| t.time);

        Ok((ticks, cursor))
    }

    #[instrument(skip(self))]
    async fn fetch_ticker(&self, symbol: &Symbol) -> anyhow::Result<TickerSnapshot> {
        let resp: FuturesTickersResponse = self.get("/tickers", &[]).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        let ticker = resp
            .tickers
            .iter()
            .find(|t| t.symbol == symbol.as_str())
            .ok_or_else(|| ConnectivityError::SymbolNotFound(symbol.clone()))?;

        mapper::map_futures_ticker(symbol, ticker).context("failed to map futures ticker")
    }

    #[instrument(skip(self))]
    async fn fetch_order_book(
        &self,
        symbol: &Symbol,
        depth: u32,
    ) -> anyhow::Result<OrderBookSnapshot> {
        let _ = depth;
        let resp: FuturesOrderBookResponse = self
            .get("/orderbook", &[("symbol", symbol.as_str())])
            .await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        let book = resp.order_book.ok_or_else(|| {
            ConnectivityError::InvalidResponse("missing orderBook in response".into())
        })?;

        mapper::map_futures_order_book(symbol, &book).context("failed to map futures order book")
    }
}

impl OrderExecutor for KrakenFuturesRestClient {
    #[instrument(skip(self))]
    async fn place_order(&self, request: &OrderRequest) -> anyhow::Result<OrderId> {
        let order_side = common::order_side_to_kraken(request.side);
        let order_type = common::order_type_to_kraken(request.order_type);
        let qty_str = request.quantity.value().to_string();

        let mut params: Vec<(&str, &str)> = vec![
            ("symbol", request.symbol.as_str()),
            ("side", order_side),
            ("orderType", order_type),
            ("size", &qty_str),
        ];

        let price_str;
        if let Some(price) = request.limit_price {
            price_str = price.value().to_string();
            params.push(("limitPrice", &price_str));
        }

        let stop_str;
        if let Some(stop) = request.stop_price {
            stop_str = stop.value().to_string();
            params.push(("stopPrice", &stop_str));
        }

        let resp: FuturesSendOrderResponse = self.post_authenticated("/sendorder", &params).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        let status = resp.send_status.ok_or_else(|| {
            ConnectivityError::InvalidResponse("missing sendStatus in response".into())
        })?;

        OrderId::new(&status.order_id).context("invalid order_id from Kraken Futures")
    }

    #[instrument(skip(self))]
    async fn cancel_order(&self, order_id: &OrderId) -> anyhow::Result<()> {
        let resp: FuturesCancelOrderResponse = self
            .post_authenticated("/cancelorder", &[("order_id", order_id.as_str())])
            .await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn cancel_all_orders(&self) -> anyhow::Result<u32> {
        let resp: FuturesCancelAllResponse =
            self.post_authenticated("/cancelallorders", &[]).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        let count = resp.cancel_status.map_or(0, |s| {
            u32::try_from(s.cancelled_orders.len()).unwrap_or(u32::MAX)
        });
        Ok(count)
    }

    #[instrument(skip(self))]
    async fn get_order_status(&self, order_id: &OrderId) -> anyhow::Result<OpenOrder> {
        let resp: FuturesOpenOrdersResponse = self.get_authenticated("/openorders", &[]).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        let order = resp
            .open_orders
            .iter()
            .find(|o| o.order_id == order_id.as_str())
            .ok_or_else(|| {
                ConnectivityError::InvalidResponse(format!(
                    "order {order_id} not found in open orders"
                ))
            })?;

        mapper::map_futures_open_order(order).context("failed to map order status")
    }

    #[instrument(skip(self))]
    async fn get_open_orders(&self) -> anyhow::Result<Vec<OpenOrder>> {
        let resp: FuturesOpenOrdersResponse = self.get_authenticated("/openorders", &[]).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        resp.open_orders
            .iter()
            .map(mapper::map_futures_open_order)
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map open orders")
    }
}

impl AccountProvider for KrakenFuturesRestClient {
    #[instrument(skip(self))]
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        let resp: FuturesAccountsResponse = self.get_authenticated("/accounts", &[]).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        let accounts = resp.accounts.ok_or_else(|| {
            ConnectivityError::InvalidResponse("missing accounts in response".into())
        })?;

        mapper::map_futures_balances(&accounts).context("failed to map futures balances")
    }

    #[instrument(skip(self))]
    async fn get_positions(&self) -> anyhow::Result<Vec<Position>> {
        let resp: FuturesPositionsResponse = self.get_authenticated("/openpositions", &[]).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        resp.open_positions
            .iter()
            .map(mapper::map_futures_position)
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map futures positions")
    }

    #[instrument(skip(self))]
    async fn get_trade_history(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<OrderFill>> {
        let mut params: Vec<(&str, &str)> = vec![];
        let since_str;
        if let Some(ts) = since {
            since_str = ts.to_rfc3339();
            params.push(("lastFillTime", &since_str));
        }

        let resp: FuturesFillsResponse = self.get_authenticated("/fills", &params).await?;
        Self::check_result(&resp.result, resp.error.as_ref())?;

        resp.fills
            .iter()
            .map(mapper::map_futures_fill)
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map futures fills")
    }
}

#[cfg(test)]
#[allow(clippy::unreadable_literal)]
mod tests {
    use rust_decimal_macros::dec;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    use super::*;

    const TEST_API_SECRET: &str = "c3VwZXJzZWNyZXRrZXkxMjM0NTY3ODkwYWJjZGVm";

    async fn setup() -> anyhow::Result<(MockServer, KrakenFuturesRestClient)> {
        let server = MockServer::start().await;
        let config = KrakenFuturesConfig {
            api_key: "test-api-key".into(),
            api_secret: TEST_API_SECRET.into(),
            rest_url: server.uri(),
            ws_url: String::new(),
        };
        let client = KrakenFuturesRestClient::new(config)?;
        Ok((server, client))
    }

    #[tokio::test]
    async fn test_fetch_instruments_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/instruments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "instruments": [{
                    "symbol": "PF_XBTUSD",
                    "type": "perpetual",
                    "tickSize": "0.5",
                    "contractSize": "1",
                    "maxPositionSize": "500000",
                    "initialMarginRate": "0.02",
                    "maintenanceMarginRate": "0.01",
                    "pair": "xbt:usd"
                }]
            })))
            .mount(&server)
            .await;

        let instruments = client.fetch_instruments().await?;
        assert_eq!(instruments.len(), 1);
        assert_eq!(instruments[0].symbol.as_str(), "PF_XBTUSD");
        assert_eq!(
            instruments[0].exchange,
            ingot_primitives::Exchange::KrakenFutures
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_instruments_api_error() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/instruments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "error",
                "error": "server unavailable"
            })))
            .mount(&server)
            .await;

        let result = client.fetch_instruments().await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_ohlcv_returns_empty() -> anyhow::Result<()> {
        let (_server, client) = setup().await?;
        let symbol = Symbol::new("PF_XBTUSD")?;
        let bars = client.fetch_ohlcv(&symbol, "1m", None).await?;
        assert!(bars.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_trades_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;
        let symbol = Symbol::new("PF_XBTUSD")?;

        Mock::given(method("GET"))
            .and(path("/history"))
            .and(query_param("symbol", "PF_XBTUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "history": [{
                    "uid": "t1",
                    "side": "buy",
                    "symbol": "PF_XBTUSD",
                    "price": "67000.5",
                    "size": "0.01",
                    "time": "2024-01-15T10:30:00Z"
                }]
            })))
            .mount(&server)
            .await;

        let (ticks, cursor) = client.fetch_trades(&symbol, None).await?;
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].price.value(), dec!(67000.5));
        assert!(cursor.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_trades_with_since() -> anyhow::Result<()> {
        let (server, client) = setup().await?;
        let symbol = Symbol::new("PF_XBTUSD")?;

        Mock::given(method("GET"))
            .and(path("/history"))
            .and(query_param("symbol", "PF_XBTUSD"))
            .and(query_param("lastTime", "2024-01-15T10:00:00+00:00"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "history": []
            })))
            .mount(&server)
            .await;

        let since = "2024-01-15T10:00:00Z".parse::<DateTime<Utc>>()?;
        let (ticks, _) = client.fetch_trades(&symbol, Some(since)).await?;
        assert!(ticks.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_ticker_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;
        let symbol = Symbol::new("PF_XBTUSD")?;

        Mock::given(method("GET"))
            .and(path("/tickers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "tickers": [{
                    "symbol": "PF_XBTUSD",
                    "bid": "67000.0",
                    "ask": "67010.0",
                    "last": "67005.0",
                    "vol24h": "15000.5"
                }]
            })))
            .mount(&server)
            .await;

        let snap = client.fetch_ticker(&symbol).await?;
        assert_eq!(snap.bid.value(), dec!(67000.0));
        assert_eq!(snap.ask.value(), dec!(67010.0));
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_order_book_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;
        let symbol = Symbol::new("PF_XBTUSD")?;

        Mock::given(method("GET"))
            .and(path("/orderbook"))
            .and(query_param("symbol", "PF_XBTUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "orderBook": {
                    "bids": [[67000.0, 3.5], [66990.0, 1.2]],
                    "asks": [[67010.0, 2.0]]
                }
            })))
            .mount(&server)
            .await;

        let snap = client.fetch_order_book(&symbol, 10).await?;
        assert_eq!(snap.bids.len(), 2);
        assert_eq!(snap.asks.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_place_order_market() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/sendorder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "sendStatus": {
                    "order_id": "ord-12345",
                    "status": "placed"
                }
            })))
            .mount(&server)
            .await;

        let request = OrderRequest {
            symbol: Symbol::new("PF_XBTUSD")?,
            side: ingot_primitives::OrderSide::Buy,
            order_type: ingot_primitives::OrderType::Market,
            quantity: ingot_primitives::Quantity::new(dec!(1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::GoodTilCancelled,
        };

        let order_id = client.place_order(&request).await?;
        assert_eq!(order_id.as_str(), "ord-12345");
        Ok(())
    }

    #[tokio::test]
    async fn test_place_order_limit() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/sendorder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "sendStatus": {
                    "order_id": "ord-limit-1",
                    "status": "placed"
                }
            })))
            .mount(&server)
            .await;

        let request = OrderRequest {
            symbol: Symbol::new("PF_XBTUSD")?,
            side: ingot_primitives::OrderSide::Buy,
            order_type: ingot_primitives::OrderType::Limit,
            quantity: ingot_primitives::Quantity::new(dec!(1))?,
            limit_price: Some(ingot_primitives::Price::new(dec!(65000.0))),
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::GoodTilCancelled,
        };

        let order_id = client.place_order(&request).await?;
        assert_eq!(order_id.as_str(), "ord-limit-1");
        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_order_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/cancelorder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "cancelStatus": {"status": "cancelled"}
            })))
            .mount(&server)
            .await;

        let order_id = OrderId::new("ord-123")?;
        client.cancel_order(&order_id).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_all_orders_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("POST"))
            .and(path("/cancelallorders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "cancelStatus": {
                    "cancelledOrders": [
                        {"order_id": "ord-1"},
                        {"order_id": "ord-2"}
                    ]
                }
            })))
            .mount(&server)
            .await;

        let count = client.cancel_all_orders().await?;
        assert_eq!(count, 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_get_open_orders_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/openorders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "openOrders": [{
                    "order_id": "ord-999",
                    "symbol": "PF_XBTUSD",
                    "side": "buy",
                    "orderType": "lmt",
                    "quantity": "10",
                    "filledQuantity": "0",
                    "limitPrice": "65000.0",
                    "status": "untouched",
                    "receivedTime": "2024-01-15T10:30:00Z"
                }]
            })))
            .mount(&server)
            .await;

        let orders = client.get_open_orders().await?;
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].order_id.as_str(), "ord-999");
        Ok(())
    }

    #[tokio::test]
    async fn test_get_order_status_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/openorders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
                    "receivedTime": "2024-01-15T10:30:00Z"
                }]
            })))
            .mount(&server)
            .await;

        let order_id = OrderId::new("ord-999")?;
        let order = client.get_order_status(&order_id).await?;
        assert_eq!(order.status, ingot_core::OrderStatus::PartiallyFilled);
        assert_eq!(order.filled_quantity.value(), dec!(5));
        Ok(())
    }

    #[tokio::test]
    async fn test_get_balances_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
            })))
            .mount(&server)
            .await;

        let balances = client.get_balances().await?;
        assert_eq!(balances.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_get_positions_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/openpositions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "openPositions": [{
                    "symbol": "PF_XBTUSD",
                    "side": "long",
                    "size": "100",
                    "price": "67000.0",
                    "unrealizedFunding": "12.50"
                }]
            })))
            .mount(&server)
            .await;

        let positions = client.get_positions().await?;
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].side, ingot_primitives::OrderSide::Buy);
        Ok(())
    }

    #[tokio::test]
    async fn test_get_trade_history_success() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/fills"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "fills": [{
                    "fill_id": "fill-1",
                    "order_id": "ord-1",
                    "symbol": "PF_XBTUSD",
                    "side": "buy",
                    "price": "67000.0",
                    "size": "0.01",
                    "fee": "0.05",
                    "fillTime": "2024-01-15T10:30:00Z"
                }]
            })))
            .mount(&server)
            .await;

        let fills = client.get_trade_history(None).await?;
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].fill_price.value(), dec!(67000.0));
        Ok(())
    }

    #[tokio::test]
    async fn test_authenticated_request_headers() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/openorders"))
            .and(wiremock::matchers::header("APIKey", "test-api-key"))
            .and(wiremock::matchers::header_exists("Authent"))
            .and(wiremock::matchers::header_exists("Nonce"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "success",
                "openOrders": []
            })))
            .mount(&server)
            .await;

        let orders = client.get_open_orders().await?;
        assert!(orders.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_api_error_handling() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/tickers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": "error",
                "error": "authenticationError: invalid API key"
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("PF_XBTUSD")?;
        let result = client.fetch_ticker(&symbol).await;
        assert!(result.is_err());
        let err = result.err().context("expected error")?;
        let err_str = format!("{err:#}");
        assert!(err_str.contains("authentication"), "got: {err_str}");
        Ok(())
    }
}
