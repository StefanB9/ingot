use std::sync::Arc;

use anyhow::Context;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::RwLock;
use tracing::instrument;

use super::{contract_registry::IbkrContractRegistry, error::IbkrError, session::SessionManager};
use crate::{config::IbkrConfig, error::ConnectivityError, rate_limiter::RateLimiter};

pub(crate) struct IbkrRestClient {
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use anyhow::Context;
    use reqwest::Client;
    use serde::Deserialize;
    use wiremock::{
        Mock, MockServer, Request, Respond, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::ibkr::session::SessionManager;

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
}
