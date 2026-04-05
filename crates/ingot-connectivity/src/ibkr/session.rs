use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Context;
use reqwest::Client;
use serde::Deserialize;
use tokio::{
    sync::{RwLock, watch},
    task::JoinHandle,
    time::Duration,
};
use tracing::{debug, instrument, warn};

use super::error::IbkrError;
use crate::config::IbkrConfig;

// ── Types ──

/// Session state for the IBKR Client Portal Gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionState {
    /// Never successfully authenticated with the gateway.
    Unauthenticated,
    /// Gateway confirmed the session is authenticated.
    Authenticated,
    /// Was previously authenticated but session has expired.
    Expired,
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthenticated => write!(f, "Unauthenticated"),
            Self::Authenticated => write!(f, "Authenticated"),
            Self::Expired => write!(f, "Expired"),
        }
    }
}

/// Response from `POST /iserver/auth/status`.
#[derive(Debug, Deserialize)]
pub(crate) struct IbkrAuthStatus {
    pub authenticated: bool,
    #[serde(default)]
    pub competing: bool,
    #[serde(default)]
    pub connected: bool,
}

// ── SessionManager ──

pub(crate) struct SessionManager {
    http: Client,
    base_url: String,
    state: Arc<RwLock<SessionState>>,
    was_authenticated: Arc<AtomicBool>,
    keepalive_handle: Option<JoinHandle<()>>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl SessionManager {
    /// Build a session manager for production use (self-signed cert accepted).
    pub fn new(config: &IbkrConfig) -> anyhow::Result<Self> {
        let http = Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .context("failed to build IBKR HTTP client")?;
        Ok(Self {
            http,
            base_url: config.cp_gateway_url.clone(),
            state: Arc::new(RwLock::new(SessionState::Unauthenticated)),
            was_authenticated: Arc::new(AtomicBool::new(false)),
            keepalive_handle: None,
            shutdown_tx: None,
        })
    }

    /// Test-only constructor: inject a plain HTTP client pointing at wiremock.
    pub fn with_client(http: Client, base_url: String) -> Self {
        Self {
            http,
            base_url,
            state: Arc::new(RwLock::new(SessionState::Unauthenticated)),
            was_authenticated: Arc::new(AtomicBool::new(false)),
            keepalive_handle: None,
            shutdown_tx: None,
        }
    }

    /// Check auth with the gateway and return error if not authenticated.
    #[instrument(skip(self))]
    pub async fn authenticate(&self) -> anyhow::Result<()> {
        let state = self
            .refresh_auth_state()
            .await
            .context("failed to refresh auth state")?;
        if state == SessionState::Authenticated {
            Ok(())
        } else {
            Err(IbkrError::NotAuthenticated.into())
        }
    }

    /// Check auth with the gateway and return the current state (never errors
    /// on unauthenticated).
    #[instrument(skip(self))]
    pub async fn check_status(&self) -> anyhow::Result<SessionState> {
        self.refresh_auth_state()
            .await
            .context("failed to check auth status")
    }

    /// If cached state is `Authenticated`, return immediately. Otherwise call
    /// `authenticate()`.
    #[instrument(skip(self))]
    pub async fn ensure_authenticated(&self) -> anyhow::Result<()> {
        let current = *self.state.read().await;
        if current == SessionState::Authenticated {
            return Ok(());
        }
        self.authenticate().await
    }

    /// Keep the session alive by hitting the `/tickle` endpoint.
    #[instrument(skip(self))]
    pub async fn tickle(&self) -> anyhow::Result<()> {
        let url = format!("{}/tickle", self.base_url);
        let resp = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| crate::error::ConnectivityError::Http(e))
            .context("tickle request failed")?;

        if !resp.status().is_success() {
            anyhow::bail!("tickle returned HTTP {}", resp.status());
        }

        debug!("session tickle successful");
        Ok(())
    }

    /// Spawn a background task that periodically calls `/tickle`.
    pub fn start_keepalive(&mut self, interval: Duration) {
        let (tx, mut rx) = watch::channel(false);
        let http = self.http.clone();
        let base_url = self.base_url.clone();

        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first immediate tick
            ticker.tick().await;

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let url = format!("{base_url}/tickle");
                        match http.post(&url).send().await {
                            Ok(resp) if resp.status().is_success() => {
                                debug!("keepalive tickle OK");
                            }
                            Ok(resp) => {
                                warn!("keepalive tickle returned HTTP {}", resp.status());
                            }
                            Err(e) => {
                                warn!("keepalive tickle failed: {e}");
                            }
                        }
                    }
                    _ = rx.changed() => {
                        debug!("keepalive shutdown signal received");
                        break;
                    }
                }
            }
        });

        self.keepalive_handle = Some(handle);
        self.shutdown_tx = Some(tx);
    }

    /// Signal the keepalive task to stop and wait for it to finish.
    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(handle) = self.keepalive_handle.take() {
            let _ = handle.await;
        }
    }

    /// Get a reference to the underlying HTTP client.
    pub fn http(&self) -> &Client {
        &self.http
    }

    /// Get the base URL for the CP Gateway.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get a clone of the session state handle (for reading from other
    /// contexts).
    pub fn state(&self) -> Arc<RwLock<SessionState>> {
        Arc::clone(&self.state)
    }

    // ── Private ──

    /// Hit `/iserver/auth/status`, update internal state, return the new state.
    async fn refresh_auth_state(&self) -> anyhow::Result<SessionState> {
        let url = format!("{}/iserver/auth/status", self.base_url);
        let resp = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| crate::error::ConnectivityError::Http(e))
            .context("auth status request failed")?;

        let body = resp
            .text()
            .await
            .map_err(|e| crate::error::ConnectivityError::Http(e))
            .context("failed to read auth status response body")?;

        let status: IbkrAuthStatus = serde_json::from_str(&body)
            .map_err(crate::error::ConnectivityError::Deserialization)
            .context("failed to deserialize auth status")?;

        let new_state = if status.authenticated {
            self.was_authenticated.store(true, Ordering::Relaxed);
            SessionState::Authenticated
        } else if self.was_authenticated.load(Ordering::Relaxed) {
            SessionState::Expired
        } else {
            SessionState::Unauthenticated
        };

        *self.state.write().await = new_state;
        Ok(new_state)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;

    async fn setup_session() -> anyhow::Result<(MockServer, SessionManager)> {
        let server = MockServer::start().await;
        let http = Client::builder()
            .build()
            .context("failed to build test HTTP client")?;
        let session = SessionManager::with_client(http, server.uri());
        Ok((server, session))
    }

    fn auth_response(authenticated: bool) -> serde_json::Value {
        serde_json::json!({
            "authenticated": authenticated,
            "competing": false,
            "connected": authenticated
        })
    }

    // ── Test 1: SessionState Display ──

    #[test]
    fn test_session_state_display() {
        assert_eq!(SessionState::Unauthenticated.to_string(), "Unauthenticated");
        assert_eq!(SessionState::Authenticated.to_string(), "Authenticated");
        assert_eq!(SessionState::Expired.to_string(), "Expired");
    }

    // ── Test 2: IbkrAuthStatus deserialization ──

    #[test]
    fn test_auth_status_deserialize() -> anyhow::Result<()> {
        // Full JSON
        let full: IbkrAuthStatus =
            serde_json::from_str(r#"{"authenticated":true,"competing":false,"connected":true}"#)
                .context("full deser")?;
        assert!(full.authenticated);
        assert!(!full.competing);
        assert!(full.connected);

        // Minimal JSON (only required field)
        let minimal: IbkrAuthStatus =
            serde_json::from_str(r#"{"authenticated":false}"#).context("minimal deser")?;
        assert!(!minimal.authenticated);
        assert!(!minimal.competing); // default
        assert!(!minimal.connected); // default

        Ok(())
    }

    // ── Test 3: SessionManager initial state ──

    #[tokio::test]
    async fn test_session_manager_initial_state_unauthenticated() -> anyhow::Result<()> {
        let (_server, session) = setup_session().await?;
        let state = *session.state.read().await;
        assert_eq!(state, SessionState::Unauthenticated);
        Ok(())
    }

    // ── Test 4: authenticate success ──

    #[tokio::test]
    async fn test_session_authenticate_success() -> anyhow::Result<()> {
        let (server, session) = setup_session().await?;

        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(true)))
            .mount(&server)
            .await;

        session
            .authenticate()
            .await
            .context("authenticate should succeed")?;

        let state = *session.state.read().await;
        assert_eq!(state, SessionState::Authenticated);
        Ok(())
    }

    // ── Test 5: authenticate failure ──

    #[tokio::test]
    async fn test_session_authenticate_failure() -> anyhow::Result<()> {
        let (server, session) = setup_session().await?;

        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(false)))
            .mount(&server)
            .await;

        let result = session.authenticate().await;
        assert!(result.is_err());

        let state = *session.state.read().await;
        assert_eq!(state, SessionState::Unauthenticated);
        Ok(())
    }

    // ── Test 6: check_status authenticated ──

    #[tokio::test]
    async fn test_session_check_status_authenticated() -> anyhow::Result<()> {
        let (server, session) = setup_session().await?;

        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(true)))
            .mount(&server)
            .await;

        let state = session
            .check_status()
            .await
            .context("check_status should succeed")?;
        assert_eq!(state, SessionState::Authenticated);
        Ok(())
    }

    // ── Test 7: check_status expired (was authenticated, now not) ──

    #[tokio::test]
    async fn test_session_check_status_expired() -> anyhow::Result<()> {
        let (server, session) = setup_session().await?;

        // First: authenticate successfully
        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(true)))
            .expect(1)
            .mount(&server)
            .await;

        session
            .authenticate()
            .await
            .context("first authenticate should succeed")?;

        // Clear mocks and set up "not authenticated" response
        server.reset().await;

        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(false)))
            .expect(1)
            .mount(&server)
            .await;

        // Now check_status should return Expired (was authenticated, now not)
        let state = session
            .check_status()
            .await
            .context("check_status should succeed")?;
        assert_eq!(state, SessionState::Expired);
        Ok(())
    }

    // ── Test 8: tickle success ──

    #[tokio::test]
    async fn test_session_tickle_success() -> anyhow::Result<()> {
        let (server, session) = setup_session().await?;

        Mock::given(method("POST"))
            .and(path("/tickle"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "session": "abc123",
                "ssoExpires": 1234567890_i64
            })))
            .expect(1)
            .mount(&server)
            .await;

        session.tickle().await.context("tickle should succeed")?;
        Ok(())
    }

    // ── Test 9: ensure_authenticated when already authed (no HTTP call) ──

    #[tokio::test]
    async fn test_session_ensure_authenticated_when_already_authed() -> anyhow::Result<()> {
        let (server, session) = setup_session().await?;

        // Manually set state to Authenticated
        *session.state.write().await = SessionState::Authenticated;

        // Mount a mock that expects 0 hits — ensure_authenticated should not call the
        // endpoint
        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(true)))
            .expect(0)
            .mount(&server)
            .await;

        session
            .ensure_authenticated()
            .await
            .context("ensure_authenticated should succeed without HTTP")?;

        let state = *session.state.read().await;
        assert_eq!(state, SessionState::Authenticated);
        Ok(())
    }

    // ── Test 10: ensure_authenticated re-authenticates when expired ──

    #[tokio::test]
    async fn test_session_ensure_authenticated_re_auths_when_expired() -> anyhow::Result<()> {
        let (server, session) = setup_session().await?;

        // Manually set state to Expired
        *session.state.write().await = SessionState::Expired;

        Mock::given(method("POST"))
            .and(path("/iserver/auth/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_response(true)))
            .expect(1)
            .mount(&server)
            .await;

        session
            .ensure_authenticated()
            .await
            .context("ensure_authenticated should re-auth")?;

        let state = *session.state.read().await;
        assert_eq!(state, SessionState::Authenticated);
        Ok(())
    }
}
