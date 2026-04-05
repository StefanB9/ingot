use std::sync::Arc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_core::{
    Balance, Instrument, OhlcvBar, OpenOrder, OrderBookSnapshot, OrderFill, OrderId, Position,
    Tick, TickerSnapshot,
};
use ingot_primitives::Symbol;
use rust_decimal::prelude::ToPrimitive;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::RwLock;
use tracing::instrument;

use super::{
    contract_registry::IbkrContractRegistry,
    error::IbkrError,
    mapper,
    models::{
        IbkrAccountBalance, IbkrConfirmReply, IbkrContractDetail, IbkrContractSearchResult,
        IbkrHistoryResponse, IbkrLiveOrdersResponse, IbkrMarginInfo, IbkrMarketSnapshot,
        IbkrOrderRequest, IbkrOrderStatus, IbkrOrderSubmitWrapper, IbkrPosition, IbkrTrade,
    },
    session::SessionManager,
};
use crate::{
    config::IbkrConfig,
    error::ConnectivityError,
    rate_limiter::RateLimiter,
    traits::{AccountProvider, MarketDataProvider, OrderExecutor},
};

pub struct IbkrRestClient {
    session: SessionManager,
    config: IbkrConfig,
    registry: Arc<RwLock<IbkrContractRegistry>>,
    rate_limiter: RateLimiter,
}

impl IbkrRestClient {
    /// Build a new REST client for the IBKR Client Portal Gateway.
    pub fn new(config: IbkrConfig) -> anyhow::Result<Self> {
        let session =
            SessionManager::new(&config).context("failed to create IBKR session manager")?;
        let registry = Arc::new(RwLock::new(IbkrContractRegistry::new()));
        let rate_limiter = RateLimiter::new(10, 1.0);
        Ok(Self {
            session,
            config,
            registry,
            rate_limiter,
        })
    }

    /// Test-only constructor: inject a pre-built session manager.
    #[cfg(test)]
    pub fn with_session(
        session: SessionManager,
        config: IbkrConfig,
        registry: Arc<RwLock<IbkrContractRegistry>>,
    ) -> Self {
        let rate_limiter = RateLimiter::new(10, 1.0);
        Self {
            session,
            config,
            registry,
            rate_limiter,
        }
    }

    /// Integration-test constructor: build a client pointing at a custom base
    /// URL.
    #[doc(hidden)]
    pub fn with_base_url(config: IbkrConfig, base_url: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        let session = SessionManager::with_client(http, base_url);
        let registry = Arc::new(RwLock::new(IbkrContractRegistry::new()));
        let rate_limiter = RateLimiter::new(10, 1.0);
        Ok(Self {
            session,
            config,
            registry,
            rate_limiter,
        })
    }

    /// GET request with session management and 401 retry.
    #[instrument(skip(self))]
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<T> {
        self.rate_limiter
            .acquire()
            .await
            .context("rate limiter acquire failed")?;
        self.session
            .ensure_authenticated()
            .await
            .context("pre-request auth check failed")?;

        let url = format!("{}{path}", self.session.base_url());

        // First attempt
        let resp = self
            .session
            .http()
            .get(&url)
            .query(params)
            .send()
            .await
            .map_err(ConnectivityError::Http)
            .context("GET request failed")?;

        if resp.status().as_u16() == 401 {
            // Re-authenticate and retry once
            self.session
                .authenticate()
                .await
                .context("re-authentication after 401 failed")?;

            let resp = self
                .session
                .http()
                .get(&url)
                .query(params)
                .send()
                .await
                .map_err(ConnectivityError::Http)
                .context("GET retry after 401 failed")?;

            return self.parse_response(resp).await;
        }

        if resp.status().as_u16() == 429 {
            return Err(IbkrError::PacingViolation.into());
        }

        self.parse_response(resp).await
    }

    /// POST request with session management and 401 retry.
    #[instrument(skip(self, body))]
    pub async fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> anyhow::Result<T> {
        self.rate_limiter
            .acquire()
            .await
            .context("rate limiter acquire failed")?;
        self.session
            .ensure_authenticated()
            .await
            .context("pre-request auth check failed")?;

        let url = format!("{}{path}", self.session.base_url());

        // First attempt
        let resp = self
            .session
            .http()
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(ConnectivityError::Http)
            .context("POST request failed")?;

        if resp.status().as_u16() == 401 {
            // Re-authenticate and retry once
            self.session
                .authenticate()
                .await
                .context("re-authentication after 401 failed")?;

            let resp = self
                .session
                .http()
                .post(&url)
                .json(body)
                .send()
                .await
                .map_err(ConnectivityError::Http)
                .context("POST retry after 401 failed")?;

            return self.parse_response(resp).await;
        }

        if resp.status().as_u16() == 429 {
            return Err(IbkrError::PacingViolation.into());
        }

        self.parse_response(resp).await
    }

    pub fn registry(&self) -> &Arc<RwLock<IbkrContractRegistry>> {
        &self.registry
    }

    pub fn session(&self) -> &SessionManager {
        &self.session
    }

    #[allow(dead_code)]
    pub fn config(&self) -> &IbkrConfig {
        &self.config
    }

    /// Search for contracts and register them in the cache.
    /// Returns instruments found for the given query string.
    #[instrument(skip(self))]
    pub async fn search_contracts(&self, query: &str) -> anyhow::Result<Vec<Instrument>> {
        let results: Vec<IbkrContractSearchResult> = self
            .get(
                "/iserver/secdef/search",
                &[("symbol", query), ("name", "true")],
            )
            .await
            .context("contract search failed")?;

        let mut instruments = Vec::new();
        for result in &results {
            let detail: IbkrContractDetail = self
                .get(&format!("/iserver/contract/{}/info", result.conid), &[])
                .await
                .with_context(|| {
                    format!("failed to fetch contract detail for conid {}", result.conid)
                })?;

            let instrument = mapper::contract_detail_to_instrument(&detail).with_context(|| {
                format!("failed to map contract detail for conid {}", result.conid)
            })?;

            let mut reg = self.registry.write().await;
            reg.register(detail.con_id, instrument.symbol.clone(), instrument.clone());
            drop(reg);

            instruments.push(instrument);
        }

        Ok(instruments)
    }

    /// DELETE request with session management and 401 retry.
    #[instrument(skip(self))]
    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        self.rate_limiter
            .acquire()
            .await
            .context("rate limiter acquire failed")?;
        self.session
            .ensure_authenticated()
            .await
            .context("pre-request auth check failed")?;

        let url = format!("{}{path}", self.session.base_url());

        let resp = self
            .session
            .http()
            .delete(&url)
            .send()
            .await
            .map_err(ConnectivityError::Http)
            .context("DELETE request failed")?;

        if resp.status().as_u16() == 401 {
            self.session
                .authenticate()
                .await
                .context("re-authentication after 401 failed")?;

            let resp = self
                .session
                .http()
                .delete(&url)
                .send()
                .await
                .map_err(ConnectivityError::Http)
                .context("DELETE retry after 401 failed")?;

            return self.parse_response(resp).await;
        }

        if resp.status().as_u16() == 429 {
            return Err(IbkrError::PacingViolation.into());
        }

        self.parse_response(resp).await
    }

    // ── Private ──

    async fn parse_response<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> anyhow::Result<T> {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(ConnectivityError::Http)
            .context("failed to read response body")?;

        if !status.is_success() {
            return Err(ConnectivityError::ApiError {
                code: status.as_u16().to_string(),
                message: body,
            }
            .into());
        }

        serde_json::from_str(&body)
            .map_err(ConnectivityError::Deserialization)
            .context("failed to deserialize response")
    }
}

impl MarketDataProvider for IbkrRestClient {
    #[instrument(skip(self))]
    async fn fetch_instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        let reg = self.registry.read().await;
        let instruments = reg
            .all_instruments()
            .into_iter()
            .map(|arc| (**arc).clone())
            .collect();
        Ok(instruments)
    }

    #[instrument(skip(self))]
    async fn fetch_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<OhlcvBar>> {
        let reg = self.registry.read().await;
        let conid = reg
            .conid_for_symbol(symbol)
            .with_context(|| format!("symbol {} not in contract registry", symbol.as_str()))?;
        drop(reg);

        let bar = mapper::ibkr_interval(interval).context("invalid interval")?;

        // Compute period from `since` or use a reasonable default
        let period = match since {
            Some(ts) => {
                let days = (Utc::now() - ts).num_days().max(1);
                if days <= 1 {
                    "1d"
                } else if days <= 7 {
                    "1w"
                } else if days <= 30 {
                    "1m"
                } else if days <= 90 {
                    "3m"
                } else if days <= 365 {
                    "1y"
                } else {
                    "5y"
                }
            }
            None => "1m",
        };

        let conid_str = conid.to_string();
        let resp: IbkrHistoryResponse = self
            .get(
                "/iserver/marketdata/history",
                &[("conid", &conid_str), ("bar", bar), ("period", period)],
            )
            .await
            .context("failed to fetch OHLCV data")?;

        resp.data
            .iter()
            .map(|b| mapper::history_bar_to_ohlcv(symbol.clone(), interval, b))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map history bars")
    }

    #[instrument(skip(self))]
    async fn fetch_trades(
        &self,
        _symbol: &Symbol,
        _since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<(Vec<Tick>, Option<DateTime<Utc>>)> {
        anyhow::bail!("IBKR Client Portal API does not support trade-level data")
    }

    #[instrument(skip(self))]
    async fn fetch_ticker(&self, symbol: &Symbol) -> anyhow::Result<TickerSnapshot> {
        let reg = self.registry.read().await;
        let conid = reg
            .conid_for_symbol(symbol)
            .with_context(|| format!("symbol {} not in contract registry", symbol.as_str()))?;
        drop(reg);

        let conid_str = conid.to_string();
        let snapshots: Vec<IbkrMarketSnapshot> = self
            .get(
                "/iserver/marketdata/snapshot",
                &[("conids", &conid_str), ("fields", "31,84,86,87")],
            )
            .await
            .context("failed to fetch market snapshot")?;

        let snap = snapshots
            .into_iter()
            .next()
            .context("empty snapshot response")?;

        mapper::market_snapshot_to_ticker(symbol.clone(), &snap)
            .context("failed to map market snapshot to ticker")
    }

    #[instrument(skip(self))]
    async fn fetch_order_book(
        &self,
        symbol: &Symbol,
        _depth: u32,
    ) -> anyhow::Result<OrderBookSnapshot> {
        let reg = self.registry.read().await;
        let conid = reg
            .conid_for_symbol(symbol)
            .with_context(|| format!("symbol {} not in contract registry", symbol.as_str()))?;
        drop(reg);

        let conid_str = conid.to_string();
        let snapshots: Vec<IbkrMarketSnapshot> = self
            .get(
                "/iserver/marketdata/snapshot",
                &[("conids", &conid_str), ("fields", "84,86")],
            )
            .await
            .context("failed to fetch order book snapshot")?;

        let snap = snapshots
            .into_iter()
            .next()
            .context("empty snapshot response")?;

        mapper::snapshot_to_order_book(symbol.clone(), &snap)
            .context("failed to map snapshot to order book")
    }
}

impl OrderExecutor for IbkrRestClient {
    #[instrument(skip(self))]
    async fn place_order(&self, request: &ingot_core::OrderRequest) -> anyhow::Result<OrderId> {
        let reg = self.registry.read().await;
        let conid = reg.conid_for_symbol(&request.symbol).with_context(|| {
            format!(
                "symbol {} not in contract registry",
                request.symbol.as_str()
            )
        })?;
        let instrument = reg
            .instrument_for_conid(conid)
            .context("instrument not found for conid")?;
        let sec_type = mapper::asset_class_to_sec_type(instrument.asset_class);
        drop(reg);

        let order_type_str = mapper::order_type_to_ibkr(request.order_type)?;
        let side_str = mapper::order_side_to_ibkr(request.side);
        let tif_str = mapper::tif_to_ibkr(request.time_in_force)?;
        let quantity_f64 = request
            .quantity
            .value()
            .to_f64()
            .context("quantity conversion to f64 failed")?;

        let price = request.limit_price.and_then(|p| p.value().to_f64());
        let aux_price = request.stop_price.and_then(|p| p.value().to_f64());

        let ibkr_order = IbkrOrderRequest {
            acct_id: self.config.account_id.clone(),
            conid,
            sec_type: sec_type.into(),
            order_type: order_type_str.into(),
            side: side_str.into(),
            quantity: quantity_f64,
            price,
            aux_price,
            tif: tif_str.into(),
            listing_exchange: None,
        };

        let wrapper = IbkrOrderSubmitWrapper {
            orders: vec![ibkr_order],
        };

        let reply: Vec<serde_json::Value> = self
            .post(
                &format!("/iserver/account/{}/orders", self.config.account_id),
                &wrapper,
            )
            .await
            .context("order placement failed")?;

        let item = reply
            .into_iter()
            .next()
            .context("empty order reply from IBKR")?;

        // Success: {"order_id": "...", "order_status": "..."}
        if let Some(order_id) = item.get("order_id").and_then(|v| v.as_str()) {
            return OrderId::new(order_id).context("invalid order ID from IBKR");
        }

        // Confirmation needed: {"id": "...", "message": [...]}
        if let Some(reply_id) = item.get("id").and_then(|v| v.as_str()) {
            let confirm = IbkrConfirmReply { confirmed: true };
            let confirmed: Vec<serde_json::Value> = self
                .post(&format!("/iserver/reply/{reply_id}"), &confirm)
                .await
                .context("order confirmation failed")?;

            let confirmed_item = confirmed
                .into_iter()
                .next()
                .context("empty confirmation reply")?;
            let order_id = confirmed_item
                .get("order_id")
                .and_then(|v| v.as_str())
                .context("no order_id in confirmation reply")?;
            return OrderId::new(order_id).context("invalid order ID from IBKR confirmation");
        }

        anyhow::bail!("unexpected order reply format from IBKR: {item:?}")
    }

    #[instrument(skip(self))]
    async fn cancel_order(&self, order_id: &OrderId) -> anyhow::Result<()> {
        let _: serde_json::Value = self
            .delete(&format!(
                "/iserver/account/{}/order/{}",
                self.config.account_id,
                order_id.as_str()
            ))
            .await
            .context("order cancellation failed")?;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn cancel_all_orders(&self) -> anyhow::Result<u32> {
        let resp: IbkrLiveOrdersResponse = self
            .get("/iserver/account/orders", &[])
            .await
            .context("failed to fetch open orders")?;

        let mut count = 0u32;
        for order in &resp.orders {
            let oid = OrderId::new(&order.order_id)
                .with_context(|| format!("invalid order id: {}", order.order_id))?;
            self.cancel_order(&oid)
                .await
                .with_context(|| format!("failed to cancel order {}", order.order_id))?;
            count += 1;
        }
        Ok(count)
    }

    #[instrument(skip(self))]
    async fn get_order_status(&self, order_id: &OrderId) -> anyhow::Result<OpenOrder> {
        let status: IbkrOrderStatus = self
            .get(
                &format!("/iserver/account/order/status/{}", order_id.as_str()),
                &[],
            )
            .await
            .context("failed to fetch order status")?;

        let reg = self.registry.read().await;
        mapper::ibkr_order_status_to_open_order(&status, &reg).context("failed to map order status")
    }

    #[instrument(skip(self))]
    async fn get_open_orders(&self) -> anyhow::Result<Vec<OpenOrder>> {
        let resp: IbkrLiveOrdersResponse = self
            .get("/iserver/account/orders", &[])
            .await
            .context("failed to fetch open orders")?;

        let reg = self.registry.read().await;
        resp.orders
            .iter()
            .map(|o| mapper::ibkr_live_order_to_open_order(o, &reg))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map open orders")
    }
}

// ── AccountProvider ──

impl AccountProvider for IbkrRestClient {
    #[instrument(skip(self))]
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        let ledger: std::collections::HashMap<String, IbkrAccountBalance> = self
            .get(
                &format!("/portfolio/{}/ledger", self.config.account_id),
                &[],
            )
            .await
            .context("failed to fetch balances")?;

        let mut balances = Vec::new();
        for (currency_key, bal) in &ledger {
            let b = mapper::ibkr_balance_to_balance(currency_key, bal)
                .with_context(|| format!("failed to map balance for {currency_key}"))?;
            if b.total.value() != rust_decimal::Decimal::ZERO {
                balances.push(b);
            }
        }
        Ok(balances)
    }

    #[instrument(skip(self))]
    async fn get_positions(&self) -> anyhow::Result<Vec<Position>> {
        let positions: Vec<IbkrPosition> = self
            .get(
                &format!("/portfolio/{}/positions/0", self.config.account_id),
                &[],
            )
            .await
            .context("failed to fetch positions")?;

        let reg = self.registry.read().await;
        positions
            .iter()
            .map(|p| mapper::ibkr_position_to_position(p, &reg))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map positions")
    }

    #[instrument(skip(self))]
    async fn get_trade_history(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<OrderFill>> {
        let trades: Vec<IbkrTrade> = self
            .get("/iserver/account/trades", &[])
            .await
            .context("failed to fetch trade history")?;

        let reg = self.registry.read().await;
        let fills: Vec<OrderFill> = trades
            .iter()
            .map(|t| mapper::ibkr_trade_to_order_fill(t, &reg))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map trade history")?;

        match since {
            Some(cutoff) => Ok(fills
                .into_iter()
                .filter(|f| f.timestamp >= cutoff)
                .collect()),
            None => Ok(fills),
        }
    }
}

impl IbkrRestClient {
    /// Fetch margin summary for the configured account.
    #[instrument(skip(self))]
    pub async fn get_margin(&self) -> anyhow::Result<ingot_core::MarginSnapshot> {
        let info: IbkrMarginInfo = self
            .get(
                &format!("/portfolio/{}/summary", self.config.account_id),
                &[],
            )
            .await
            .context("failed to fetch margin summary")?;

        Ok(mapper::ibkr_margin_to_snapshot(
            &self.config.account_id,
            &info,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use anyhow::Context;
    use ingot_primitives::{AssetClass, Currency, Exchange, Price, Quantity};
    use reqwest::Client;
    use rust_decimal_macros::dec;
    use serde::Deserialize;
    use smol_str::SmolStr;
    use wiremock::{
        Mock, MockServer, Request, Respond, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::{
        ibkr::session::SessionManager,
        traits::{AccountProvider, MarketDataProvider, OrderExecutor},
    };

    fn test_config(base_url: &str) -> IbkrConfig {
        IbkrConfig {
            account_id: "DU_TEST".into(),
            cp_gateway_url: base_url.into(),
            tws_host: "127.0.0.1".into(),
            tws_port: 7497,
            client_id: 1,
            session_keepalive_secs: 60,
        }
    }

    async fn setup_rest_client() -> anyhow::Result<(MockServer, IbkrRestClient)> {
        let server = MockServer::start().await;
        let http = Client::builder()
            .build()
            .context("failed to build test HTTP client")?;
        let session = SessionManager::with_client(http, server.uri());
        let config = test_config(&server.uri());
        let registry = Arc::new(RwLock::new(IbkrContractRegistry::new()));
        let client = IbkrRestClient::with_session(session, config, registry);
        Ok((server, client))
    }

    fn auth_response(authenticated: bool) -> serde_json::Value {
        serde_json::json!({
            "authenticated": authenticated,
            "competing": false,
            "connected": authenticated
        })
    }

    /// Mount a mock for POST /iserver/auth/status that returns authenticated.
    async fn mount_auth_ok(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(true)))
            .mount(server)
            .await;
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct TestPayload {
        value: String,
        count: i32,
    }

    // ── Test 11: IbkrRestClient construction ──

    #[tokio::test]
    async fn test_ibkr_rest_client_construction() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        let _ = server; // keep server alive

        let registry = client.registry().read().await;
        assert!(registry.is_empty());

        let state = *client.session().state().read().await;
        assert_eq!(state, crate::ibkr::session::SessionState::Unauthenticated);
        Ok(())
    }

    // ── Test 12: GET deserializes correctly ──

    #[tokio::test]
    async fn test_ibkr_rest_client_get_deserializes() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;

        mount_auth_ok(&server).await;

        Mock::given(method("GET"))
            .and(path("/test/endpoint"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"value": "hello", "count": 42})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let result: TestPayload = client
            .get("/test/endpoint", &[])
            .await
            .context("GET should succeed")?;

        assert_eq!(
            result,
            TestPayload {
                value: "hello".into(),
                count: 42
            }
        );
        Ok(())
    }

    // ── Test 13: POST deserializes correctly ──

    #[tokio::test]
    async fn test_ibkr_rest_client_post_deserializes() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;

        mount_auth_ok(&server).await;

        Mock::given(method("POST"))
            .and(path("/test/submit"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"value": "created", "count": 1})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let body = serde_json::json!({"input": "data"});
        let result: TestPayload = client
            .post("/test/submit", &body)
            .await
            .context("POST should succeed")?;

        assert_eq!(
            result,
            TestPayload {
                value: "created".into(),
                count: 1
            }
        );
        Ok(())
    }

    // ── Test 14: 401 triggers re-auth and retry ──

    /// Custom responder: returns 401 on first call, 200 with data on second.
    struct First401ThenOk {
        counter: Arc<AtomicU32>,
    }

    impl Respond for First401ThenOk {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let n = self.counter.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                ResponseTemplate::new(401)
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"value": "retried", "count": 99}))
            }
        }
    }

    #[tokio::test]
    async fn test_ibkr_rest_client_handles_401_retry() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;

        // Manually set state to Authenticated so ensure_authenticated() skips the auth
        // call
        *client.session().state().write().await = crate::ibkr::session::SessionState::Authenticated;

        // Auth endpoint: returns authenticated (called during 401 retry)
        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(true)))
            .expect(1)
            .named("re-auth after 401")
            .mount(&server)
            .await;

        // Data endpoint: first call returns 401, second returns 200
        let counter = Arc::new(AtomicU32::new(0));
        Mock::given(method("GET"))
            .and(path("/test/data"))
            .respond_with(First401ThenOk {
                counter: Arc::clone(&counter),
            })
            .expect(2)
            .named("GET with 401 then 200")
            .mount(&server)
            .await;

        let result: TestPayload = client
            .get("/test/data", &[])
            .await
            .context("GET with 401 retry should succeed")?;

        assert_eq!(
            result,
            TestPayload {
                value: "retried".into(),
                count: 99
            }
        );
        // Verify the endpoint was hit twice (401 + retry)
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        Ok(())
    }

    // ── Helper: pre-register a symbol in the registry ──

    async fn register_aapl(client: &IbkrRestClient) -> anyhow::Result<()> {
        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let instrument = Instrument {
            symbol: symbol.clone(),
            asset_class: AssetClass::Equity,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.01)),
            display_name: SmolStr::new("Apple Inc"),
            details: ingot_core::InstrumentDetails::Equity {
                isin: None,
                lot_size: Quantity::new(dec!(1))?,
                fractional: false,
            },
        };
        let mut reg = client.registry().write().await;
        reg.register(265598, symbol, instrument);
        Ok(())
    }

    // ── Test 20: search_contracts ──

    #[tokio::test]
    async fn test_search_contracts() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        // Mock search endpoint
        Mock::given(method("GET"))
            .and(path("/iserver/secdef/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "conid": 265598,
                    "company_name": "Apple Inc",
                    "symbol": "AAPL",
                    "sec_type": "STK",
                    "exchange": "NASDAQ",
                    "currency": "USD"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        // Mock contract detail endpoint
        Mock::given(method("GET"))
            .and(path("/iserver/contract/265598/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "con_id": 265598,
                "symbol": "AAPL",
                "sec_type": "STK",
                "exchange": "SMART",
                "currency": "USD",
                "company_name": "Apple Inc"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let instruments = client
            .search_contracts("AAPL")
            .await
            .context("search_contracts should succeed")?;

        assert_eq!(instruments.len(), 1);
        assert_eq!(instruments[0].symbol.as_str(), "AAPL");
        assert_eq!(instruments[0].asset_class, AssetClass::Equity);

        // Verify registered in cache
        let reg = client.registry().read().await;
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.conid_for_symbol(&instruments[0].symbol), Some(265598));
        Ok(())
    }

    // ── Test 21: fetch_ohlcv ──

    #[tokio::test]
    async fn test_fetch_ohlcv() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        Mock::given(method("GET"))
            .and(path("/iserver/marketdata/history"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"t": 1711900800, "o": 177.0, "h": 179.2, "l": 176.8, "c": 178.5, "v": 45230100.0},
                    {"t": 1711987200, "o": 178.5, "h": 180.0, "l": 178.0, "c": 179.5, "v": 38100000.0}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let bars = client
            .fetch_ohlcv(&symbol, "1d", None)
            .await
            .context("fetch_ohlcv should succeed")?;

        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].time.timestamp(), 1_711_900_800);
        assert_eq!(bars[0].interval.as_str(), "1d");
        assert_eq!(bars[1].time.timestamp(), 1_711_987_200);
        Ok(())
    }

    // ── Test 22: fetch_ticker ──

    #[tokio::test]
    async fn test_fetch_ticker() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        Mock::given(method("GET"))
            .and(path("/iserver/marketdata/snapshot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "conid": 265598,
                    "31": "178.50",
                    "84": "178.45",
                    "86": "178.55",
                    "87": "45000000"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let ticker = client
            .fetch_ticker(&symbol)
            .await
            .context("fetch_ticker should succeed")?;

        assert_eq!(ticker.symbol.as_str(), "AAPL");
        assert_eq!(ticker.last, Price::new(dec!(178.50)));
        assert_eq!(ticker.bid, Price::new(dec!(178.45)));
        assert_eq!(ticker.ask, Price::new(dec!(178.55)));
        Ok(())
    }

    // ── Test 23: fetch_order_book ──

    #[tokio::test]
    async fn test_fetch_order_book() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        Mock::given(method("GET"))
            .and(path("/iserver/marketdata/snapshot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "conid": 265598,
                    "84": "178.45",
                    "86": "178.55"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let book = client
            .fetch_order_book(&symbol, 10)
            .await
            .context("fetch_order_book should succeed")?;

        assert_eq!(book.symbol.as_str(), "AAPL");
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.asks.len(), 1);
        assert_eq!(book.bids[0].price, Price::new(dec!(178.45)));
        assert_eq!(book.asks[0].price, Price::new(dec!(178.55)));
        Ok(())
    }

    // ── Test 24: fetch_trades_unsupported ──

    #[tokio::test]
    async fn test_fetch_trades_unsupported() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        let _ = server;

        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let result = client.fetch_trades(&symbol, None).await;

        assert!(result.is_err());
        let err_msg = format!("{}", result.err().context("expected error")?);
        assert!(err_msg.contains("does not support trade-level data"));
        Ok(())
    }

    // ── Test 7: place_order_market ──

    #[tokio::test]
    async fn test_place_order_market() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        Mock::given(method("POST"))
            .and(path("/iserver/account/DU_TEST/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"order_id": "12345", "order_status": "Submitted"}
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let request = ingot_core::OrderRequest {
            symbol: ingot_primitives::Symbol::new("AAPL")?,
            side: ingot_primitives::OrderSide::Buy,
            order_type: ingot_primitives::OrderType::Market,
            quantity: Quantity::new(dec!(100))?,
            limit_price: None,
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::Day,
        };

        let order_id = client
            .place_order(&request)
            .await
            .context("place_order should succeed")?;

        assert_eq!(order_id.as_str(), "12345");
        Ok(())
    }

    // ── Test 8: place_order_limit ──

    #[tokio::test]
    async fn test_place_order_limit() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        Mock::given(method("POST"))
            .and(path("/iserver/account/DU_TEST/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"order_id": "12346", "order_status": "PreSubmitted"}
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let request = ingot_core::OrderRequest {
            symbol: ingot_primitives::Symbol::new("AAPL")?,
            side: ingot_primitives::OrderSide::Buy,
            order_type: ingot_primitives::OrderType::Limit,
            quantity: Quantity::new(dec!(50))?,
            limit_price: Some(Price::new(dec!(178.50))),
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::GoodTilCancelled,
        };

        let order_id = client
            .place_order(&request)
            .await
            .context("place_order limit should succeed")?;

        assert_eq!(order_id.as_str(), "12346");
        Ok(())
    }

    // ── Test 9: place_order_with_confirmation ──

    #[tokio::test]
    async fn test_place_order_with_confirmation() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        // First POST returns confirmation prompt
        Mock::given(method("POST"))
            .and(path("/iserver/account/DU_TEST/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "reply-abc", "message": ["Are you sure you want to submit this order?"]}
            ])))
            .expect(1)
            .mount(&server)
            .await;

        // Confirmation POST returns success
        Mock::given(method("POST"))
            .and(path("/iserver/reply/reply-abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"order_id": "12347", "order_status": "Submitted"}
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let request = ingot_core::OrderRequest {
            symbol: ingot_primitives::Symbol::new("AAPL")?,
            side: ingot_primitives::OrderSide::Sell,
            order_type: ingot_primitives::OrderType::Market,
            quantity: Quantity::new(dec!(100))?,
            limit_price: None,
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::Day,
        };

        let order_id = client
            .place_order(&request)
            .await
            .context("place_order with confirmation should succeed")?;

        assert_eq!(order_id.as_str(), "12347");
        Ok(())
    }

    // ── Test 10: place_order_rejected ──

    #[tokio::test]
    async fn test_place_order_rejected() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        Mock::given(method("POST"))
            .and(path("/iserver/account/DU_TEST/orders"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_string(r#"{"error": "Order rejected: insufficient margin"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;

        let request = ingot_core::OrderRequest {
            symbol: ingot_primitives::Symbol::new("AAPL")?,
            side: ingot_primitives::OrderSide::Buy,
            order_type: ingot_primitives::OrderType::Market,
            quantity: Quantity::new(dec!(100))?,
            limit_price: None,
            stop_price: None,
            time_in_force: ingot_primitives::TimeInForce::Day,
        };

        let result = client.place_order(&request).await;
        assert!(result.is_err());
        Ok(())
    }

    // ── Test 11: cancel_order ──

    #[tokio::test]
    async fn test_cancel_order() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        Mock::given(method("DELETE"))
            .and(path("/iserver/account/DU_TEST/order/12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "order_id": "12345",
                "msg": "Order 12345 has been cancelled",
                "conid": 265598
            })))
            .expect(1)
            .mount(&server)
            .await;

        let order_id = ingot_core::OrderId::new("12345")?;
        client
            .cancel_order(&order_id)
            .await
            .context("cancel_order should succeed")?;
        Ok(())
    }

    // ── Test 12: cancel_order_not_found ──

    #[tokio::test]
    async fn test_cancel_order_not_found() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        Mock::given(method("DELETE"))
            .and(path("/iserver/account/DU_TEST/order/99999"))
            .respond_with(
                ResponseTemplate::new(404).set_body_string(r#"{"error": "order not found"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;

        let order_id = ingot_core::OrderId::new("99999")?;
        let result = client.cancel_order(&order_id).await;
        assert!(result.is_err());
        Ok(())
    }

    // ── Test 13: cancel_all_orders ──

    #[tokio::test]
    async fn test_cancel_all_orders() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        Mock::given(method("GET"))
            .and(path("/iserver/account/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "orders": [
                    {
                        "orderId": "111",
                        "conid": 265598,
                        "orderType": "LMT",
                        "side": "BUY",
                        "quantity": 100.0,
                        "filledQuantity": 0.0,
                        "remainingQuantity": 100.0,
                        "status": "Submitted",
                        "timeInForce": "GTC",
                        "price": 170.0
                    },
                    {
                        "orderId": "222",
                        "conid": 265598,
                        "orderType": "LMT",
                        "side": "SELL",
                        "quantity": 50.0,
                        "filledQuantity": 0.0,
                        "remainingQuantity": 50.0,
                        "status": "Submitted",
                        "timeInForce": "DAY",
                        "price": 180.0
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("DELETE"))
            .and(path("/iserver/account/DU_TEST/order/111"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "order_id": "111", "msg": "cancelled"
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("DELETE"))
            .and(path("/iserver/account/DU_TEST/order/222"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "order_id": "222", "msg": "cancelled"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let count = client
            .cancel_all_orders()
            .await
            .context("cancel_all_orders should succeed")?;

        assert_eq!(count, 2);
        Ok(())
    }

    // ── Test 14: get_order_status ──

    #[tokio::test]
    async fn test_get_order_status() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        Mock::given(method("GET"))
            .and(path("/iserver/account/order/status/12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "order_id": "12345",
                "conid": 265598,
                "status": "Filled",
                "filled_quantity": 100.0,
                "remaining_quantity": 0.0,
                "avg_price": 178.50,
                "side": "BUY"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let order_id = ingot_core::OrderId::new("12345")?;
        let open = client
            .get_order_status(&order_id)
            .await
            .context("get_order_status should succeed")?;

        assert_eq!(open.order_id.as_str(), "12345");
        assert_eq!(open.status, ingot_core::OrderStatus::Filled);
        assert_eq!(open.request.symbol.as_str(), "AAPL");
        assert_eq!(open.request.side, ingot_primitives::OrderSide::Buy);
        // Sparse endpoint defaults
        assert_eq!(open.request.order_type, ingot_primitives::OrderType::Market);
        assert_eq!(
            open.request.time_in_force,
            ingot_primitives::TimeInForce::Day
        );
        assert_eq!(open.average_fill_price, Some(Price::new(dec!(178.50))));
        Ok(())
    }

    // ── Test 15: get_open_orders ──

    #[tokio::test]
    async fn test_get_open_orders() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;
        register_aapl(&client).await?;

        Mock::given(method("GET"))
            .and(path("/iserver/account/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "orders": [
                    {
                        "orderId": "111",
                        "conid": 265598,
                        "orderType": "LMT",
                        "side": "BUY",
                        "price": 170.0,
                        "quantity": 100.0,
                        "filledQuantity": 0.0,
                        "remainingQuantity": 100.0,
                        "status": "Submitted",
                        "timeInForce": "GTC"
                    },
                    {
                        "orderId": "222",
                        "conid": 265598,
                        "orderType": "MKT",
                        "side": "SELL",
                        "quantity": 50.0,
                        "filledQuantity": 25.0,
                        "remainingQuantity": 25.0,
                        "status": "Submitted",
                        "timeInForce": "DAY"
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let orders = client
            .get_open_orders()
            .await
            .context("get_open_orders should succeed")?;

        assert_eq!(orders.len(), 2);

        assert_eq!(orders[0].order_id.as_str(), "111");
        assert_eq!(
            orders[0].request.order_type,
            ingot_primitives::OrderType::Limit
        );
        assert_eq!(orders[0].request.side, ingot_primitives::OrderSide::Buy);
        assert_eq!(orders[0].request.limit_price, Some(Price::new(dec!(170.0))));
        assert_eq!(
            orders[0].request.time_in_force,
            ingot_primitives::TimeInForce::GoodTilCancelled
        );

        assert_eq!(orders[1].order_id.as_str(), "222");
        assert_eq!(
            orders[1].request.order_type,
            ingot_primitives::OrderType::Market
        );
        assert_eq!(orders[1].request.side, ingot_primitives::OrderSide::Sell);
        assert_eq!(orders[1].filled_quantity, Quantity::new(dec!(25))?);
        Ok(())
    }

    // ── Test 16: get_open_orders_empty ──

    #[tokio::test]
    async fn test_get_open_orders_empty() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        Mock::given(method("GET"))
            .and(path("/iserver/account/orders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "orders": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let orders = client
            .get_open_orders()
            .await
            .context("get_open_orders_empty should succeed")?;

        assert!(orders.is_empty());
        Ok(())
    }

    // ── Test 25: get_balances ──

    #[tokio::test]
    async fn test_get_balances() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        Mock::given(method("GET"))
            .and(path("/portfolio/DU_TEST/ledger"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "USD": {
                    "currency": "USD",
                    "cashbalance": 10000.0,
                    "settledcash": 8000.0
                },
                "EUR": {
                    "currency": "EUR",
                    "cashbalance": 0.0,
                    "settledcash": 0.0
                },
                "GBP": {
                    "currency": "GBP",
                    "cashbalance": 5000.0,
                    "settledcash": 5000.0
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let balances = client.get_balances().await.context("get_balances failed")?;

        // EUR should be skipped (zero balance)
        assert_eq!(balances.len(), 2);

        let usd = balances
            .iter()
            .find(|b| b.currency == ingot_primitives::Currency::USD)
            .context("missing USD")?;
        assert_eq!(usd.total.value(), dec!(10000));
        assert_eq!(usd.available.value(), dec!(8000));
        assert_eq!(usd.held.value(), dec!(2000));

        let gbp = balances
            .iter()
            .find(|b| b.currency == ingot_primitives::Currency::GBP)
            .context("missing GBP")?;
        assert_eq!(gbp.total.value(), dec!(5000));
        assert_eq!(gbp.available.value(), dec!(5000));
        assert_eq!(gbp.held.value(), dec!(0));

        Ok(())
    }

    // ── Test 26: get_positions ──

    #[tokio::test]
    async fn test_get_positions() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        // Register AAPL in the contract registry
        {
            let mut reg = client.registry.write().await;
            let symbol = ingot_primitives::Symbol::new("AAPL")?;
            let instrument = ingot_core::Instrument {
                symbol: symbol.clone(),
                asset_class: AssetClass::Equity,
                exchange: Exchange::IBKR,
                base_currency: Currency::USD,
                quote_currency: Currency::USD,
                tick_size: Price::new(dec!(0.01)),
                display_name: SmolStr::new("Apple Inc"),
                details: ingot_core::InstrumentDetails::Equity {
                    isin: None,
                    lot_size: Quantity::new(dec!(1))?,
                    fractional: false,
                },
            };
            reg.register(265598, symbol, instrument);
        }

        Mock::given(method("GET"))
            .and(path("/portfolio/DU_TEST/positions/0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "conid": 265598,
                    "currency": "USD",
                    "position": 100.0,
                    "avgCost": 175.50,
                    "mktPrice": 180.0,
                    "mktValue": 18000.0,
                    "unrealizedPnl": 450.0
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let positions = client
            .get_positions()
            .await
            .context("get_positions failed")?;
        assert_eq!(positions.len(), 1);

        let pos = &positions[0];
        assert_eq!(pos.symbol.as_str(), "AAPL");
        assert_eq!(pos.side, ingot_primitives::OrderSide::Buy);
        assert_eq!(pos.quantity, Quantity::new(dec!(100))?);
        assert_eq!(pos.average_entry_price, Price::new(dec!(175.5)));
        assert_eq!(
            pos.unrealized_pnl,
            Some(ingot_primitives::Amount::new(dec!(450)))
        );
        assert!(pos.liquidation_price.is_none());

        Ok(())
    }

    // ── Test 27: get_positions_empty ──

    #[tokio::test]
    async fn test_get_positions_empty() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        Mock::given(method("GET"))
            .and(path("/portfolio/DU_TEST/positions/0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let positions = client
            .get_positions()
            .await
            .context("get_positions_empty should succeed")?;
        assert!(positions.is_empty());
        Ok(())
    }

    // ── Test 28: get_trade_history ──

    #[tokio::test]
    async fn test_get_trade_history() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        // Register AAPL
        {
            let mut reg = client.registry.write().await;
            let symbol = ingot_primitives::Symbol::new("AAPL")?;
            let instrument = ingot_core::Instrument {
                symbol: symbol.clone(),
                asset_class: AssetClass::Equity,
                exchange: Exchange::IBKR,
                base_currency: Currency::USD,
                quote_currency: Currency::USD,
                tick_size: Price::new(dec!(0.01)),
                display_name: SmolStr::new("Apple Inc"),
                details: ingot_core::InstrumentDetails::Equity {
                    isin: None,
                    lot_size: Quantity::new(dec!(1))?,
                    fractional: false,
                },
            };
            reg.register(265598, symbol, instrument);
        }

        Mock::given(method("GET"))
            .and(path("/iserver/account/trades"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "execution_id": "EXEC001",
                    "conid": 265598,
                    "side": "BUY",
                    "size": 50.0,
                    "price": 178.25,
                    "commission": 1.50,
                    "currency": "USD",
                    "trade_time": "20260329-14:30:00",
                    "order_ref": "ORD123"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let fills = client
            .get_trade_history(None)
            .await
            .context("get_trade_history failed")?;
        assert_eq!(fills.len(), 1);

        let fill = &fills[0];
        assert_eq!(fill.order_id.as_str(), "ORD123");
        assert_eq!(fill.symbol.as_str(), "AAPL");
        assert_eq!(fill.side, ingot_primitives::OrderSide::Buy);
        assert_eq!(fill.fill_price, Price::new(dec!(178.25)));
        assert_eq!(fill.fill_quantity, Quantity::new(dec!(50))?);
        assert_eq!(fill.fee.value(), dec!(1.5));
        assert_eq!(fill.fee_currency, Currency::USD);
        assert_eq!(fill.trade_id.as_deref(), Some("EXEC001"));

        Ok(())
    }

    // ── Test 29: get_margin ──

    #[tokio::test]
    async fn test_get_margin() -> anyhow::Result<()> {
        let (server, client) = setup_rest_client().await?;
        mount_auth_ok(&server).await;

        Mock::given(method("GET"))
            .and(path("/portfolio/DU_TEST/summary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "initmarginreq": { "amount": 50000.0, "currency": "USD" },
                "maintmarginreq": { "amount": 30000.0, "currency": "USD" },
                "excessliquidity": { "amount": 70000.0, "currency": "USD" },
                "buyingpower": { "amount": 200000.0, "currency": "USD" },
                "availablefunds": { "amount": 50000.0, "currency": "USD" },
                "netliquidation": { "amount": 100000.0, "currency": "USD" },
                "sma": { "amount": 80000.0, "currency": "USD" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let snap = client.get_margin().await.context("get_margin failed")?;

        assert_eq!(snap.account_id, "DU_TEST");
        assert_eq!(snap.initial_margin.value(), dec!(50000));
        assert_eq!(snap.maintenance_margin.value(), dec!(30000));
        assert_eq!(snap.excess_liquidity.value(), dec!(70000));
        assert_eq!(snap.buying_power.value(), dec!(200000));
        assert_eq!(snap.available_funds.value(), dec!(50000));
        assert_eq!(snap.net_liquidation.value(), dec!(100000));
        assert_eq!(snap.sma.map(|s| s.value()), Some(dec!(80000)));

        // Verify computed methods
        let util = snap.utilization()?;
        assert_eq!(util.value(), dec!(0.5));
        assert!(!snap.is_margin_call());
        assert_eq!(snap.available_margin().value(), dec!(70000));

        Ok(())
    }
}
