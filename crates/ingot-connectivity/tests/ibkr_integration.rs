use ingot_connectivity::{
    AccountProvider, Connected, IbkrConfig, IbkrRestClient, IbkrTws, MarketDataProvider,
    OrderExecutor, StreamProvider,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

// ── Helpers ──

fn test_config() -> IbkrConfig {
    IbkrConfig {
        account_id: "DU1234567".into(),
        cp_gateway_url: "http://unused".into(),
        tws_host: "127.0.0.1".into(),
        tws_port: 7497,
        client_id: 1,
        session_keepalive_secs: 60,
    }
}

async fn mount_auth_mock(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/iserver/auth/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "authenticated": true,
            "competing": false,
            "connected": true
        })))
        .mount(server)
        .await;
}

fn search_result_json(conid: i64, symbol: &str, company: &str) -> serde_json::Value {
    serde_json::json!([{
        "conid": conid,
        "company_name": company,
        "symbol": symbol,
        "sec_type": "STK",
        "exchange": "NASDAQ",
        "currency": "USD"
    }])
}

fn contract_detail_json(conid: i64, symbol: &str, company: &str) -> serde_json::Value {
    serde_json::json!({
        "con_id": conid,
        "symbol": symbol,
        "sec_type": "STK",
        "exchange": "NASDAQ",
        "currency": "USD",
        "local_symbol": symbol,
        "trading_class": symbol,
        "company_name": company,
        "valid_exchanges": "NASDAQ,NYSE"
    })
}

// ── Trait satisfaction helpers ──

fn assert_market_data_provider<T: MarketDataProvider>() {}
fn assert_order_executor<T: OrderExecutor>() {}
fn assert_account_provider<T: AccountProvider>() {}
fn assert_stream_provider<T: StreamProvider>() {}

// ── Tests 1-4: compile-time trait checks ──

#[test]
fn test_ibkr_rest_client_satisfies_market_data_provider() {
    assert_market_data_provider::<IbkrRestClient>();
}

#[test]
fn test_ibkr_rest_client_satisfies_order_executor() {
    assert_order_executor::<IbkrRestClient>();
}

#[test]
fn test_ibkr_rest_client_satisfies_account_provider() {
    assert_account_provider::<IbkrRestClient>();
}

#[test]
fn test_ibkr_tws_connected_satisfies_stream_provider() {
    assert_stream_provider::<IbkrTws<Connected>>();
}

// ── Test 7: contract registry populated on fetch ──

#[tokio::test]
async fn test_contract_registry_populated_on_fetch_instruments() -> anyhow::Result<()> {
    use anyhow::Context;
    use ingot_primitives::Symbol;

    let server = MockServer::start().await;
    let config = test_config();
    let client = IbkrRestClient::with_base_url(config, server.uri())
        .context("failed to create client with base_url")?;

    // Mount auth mock
    mount_auth_mock(&server).await;

    // ── AAPL mocks ──
    Mock::given(method("GET"))
        .and(path("/iserver/secdef/search"))
        .and(query_param("symbol", "AAPL"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_result_json(
            265598,
            "AAPL",
            "Apple Inc",
        )))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/iserver/contract/265598/info"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(contract_detail_json(
                265598,
                "AAPL",
                "Apple Inc",
            )),
        )
        .mount(&server)
        .await;

    // ── MSFT mocks ──
    Mock::given(method("GET"))
        .and(path("/iserver/secdef/search"))
        .and(query_param("symbol", "MSFT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_result_json(
            272093,
            "MSFT",
            "Microsoft Corp",
        )))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/iserver/contract/272093/info"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(contract_detail_json(
                272093,
                "MSFT",
                "Microsoft Corp",
            )),
        )
        .mount(&server)
        .await;

    // ── Execute searches ──
    let aapl_results = client
        .search_contracts("AAPL")
        .await
        .context("AAPL search failed")?;
    assert_eq!(aapl_results.len(), 1);

    let msft_results = client
        .search_contracts("MSFT")
        .await
        .context("MSFT search failed")?;
    assert_eq!(msft_results.len(), 1);

    // ── Verify registry ──
    let registry = client.registry().read().await;
    assert_eq!(registry.len(), 2);

    let aapl_sym = Symbol::new("AAPL").context("invalid symbol")?;
    let msft_sym = Symbol::new("MSFT").context("invalid symbol")?;

    // Bidirectional lookups
    assert_eq!(registry.conid_for_symbol(&aapl_sym), Some(265598));
    assert_eq!(registry.conid_for_symbol(&msft_sym), Some(272093));
    assert_eq!(
        registry.symbol_for_conid(265598).map(Symbol::as_str),
        Some("AAPL")
    );
    assert_eq!(
        registry.symbol_for_conid(272093).map(Symbol::as_str),
        Some("MSFT")
    );

    Ok(())
}

// ── Test 6: session recovery on 401 ──

#[tokio::test]
async fn test_session_recovery_on_401() -> anyhow::Result<()> {
    use anyhow::Context;

    let server = MockServer::start().await;
    let config = test_config();
    let client =
        IbkrRestClient::with_base_url(config, server.uri()).context("failed to create client")?;

    // Auth mock — always authenticated
    mount_auth_mock(&server).await;

    // First call to /test/data → 401 (expires after 1 use, higher priority)
    Mock::given(method("GET"))
        .and(path("/test/data"))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    // Second call to /test/data → 200 with JSON (lower priority, always available)
    Mock::given(method("GET"))
        .and(path("/test/data"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": 42})))
        .with_priority(2)
        .mount(&server)
        .await;

    // The client should get 401, re-authenticate, and retry successfully
    let result: serde_json::Value = client
        .get("/test/data", &[])
        .await
        .context("GET with 401 retry should succeed")?;

    assert_eq!(result["value"], 42);

    Ok(())
}

// ── Test 5: full order lifecycle ──

#[tokio::test]
async fn test_full_order_lifecycle() -> anyhow::Result<()> {
    use anyhow::Context;
    use ingot_core::{OrderRequest, OrderStatus};
    use ingot_primitives::{OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce};
    use rust_decimal_macros::dec;

    let server = MockServer::start().await;
    let config = test_config();
    let client =
        IbkrRestClient::with_base_url(config, server.uri()).context("failed to create client")?;

    mount_auth_mock(&server).await;

    // ── Register AAPL via search ──
    Mock::given(method("GET"))
        .and(path("/iserver/secdef/search"))
        .and(query_param("symbol", "AAPL"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_result_json(
            265598,
            "AAPL",
            "Apple Inc",
        )))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/iserver/contract/265598/info"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(contract_detail_json(
                265598,
                "AAPL",
                "Apple Inc",
            )),
        )
        .mount(&server)
        .await;

    client
        .search_contracts("AAPL")
        .await
        .context("AAPL search failed")?;

    // ── Place order mock (direct accept, no confirmation) ──
    Mock::given(method("POST"))
        .and(path("/iserver/account/DU1234567/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"order_id": "12345", "order_status": "Submitted"}
        ])))
        .mount(&server)
        .await;

    let symbol = Symbol::new("AAPL").context("invalid symbol")?;
    let order_request = OrderRequest {
        symbol,
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        quantity: Quantity::new(dec!(10)).context("invalid quantity")?,
        limit_price: Some(Price::new(dec!(150.00))),
        stop_price: None,
        time_in_force: TimeInForce::Day,
    };

    let order_id = client
        .place_order(&order_request)
        .await
        .context("place_order failed")?;
    assert_eq!(order_id.as_str(), "12345");

    // ── Get order status mock ──
    Mock::given(method("GET"))
        .and(path("/iserver/account/order/status/12345"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "order_id": "12345",
            "conid": 265598,
            "status": "Filled",
            "filled_quantity": 10.0,
            "remaining_quantity": 0.0,
            "avg_price": 149.50,
            "last_fill_price": 149.50,
            "side": "BUY"
        })))
        .mount(&server)
        .await;

    let open_order = client
        .get_order_status(&order_id)
        .await
        .context("get_order_status failed")?;
    assert_eq!(open_order.status, OrderStatus::Filled);
    assert_eq!(open_order.order_id.as_str(), "12345");

    // ── Cancel order mock ──
    Mock::given(method("DELETE"))
        .and(path("/iserver/account/DU1234567/order/12345"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"msg": "cancelled"})),
        )
        .mount(&server)
        .await;

    client
        .cancel_order(&order_id)
        .await
        .context("cancel_order failed")?;

    Ok(())
}

// ── Test 12: concurrent requests through rate limiter ──

#[tokio::test]
async fn test_ibkr_rest_concurrent_requests() -> anyhow::Result<()> {
    use std::sync::Arc;

    use anyhow::Context;

    let server = MockServer::start().await;
    let config = test_config();
    let client = Arc::new(
        IbkrRestClient::with_base_url(config, server.uri()).context("failed to create client")?,
    );

    mount_auth_mock(&server).await;

    Mock::given(method("GET"))
        .and(path("/test/concurrent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;

    let mut handles = Vec::new();
    for _ in 0..20 {
        let c = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            let _: serde_json::Value = c.get("/test/concurrent", &[]).await?;
            Ok::<(), anyhow::Error>(())
        }));
    }

    let results = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        futures_util::future::join_all(handles),
    )
    .await
    .context("concurrent requests timed out after 30s")?;

    for (i, result) in results.into_iter().enumerate() {
        result
            .with_context(|| format!("task {i} panicked"))?
            .with_context(|| format!("task {i} returned error"))?;
    }

    Ok(())
}
