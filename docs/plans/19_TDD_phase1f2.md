# Technical Design Document: Phase 1f.2 — IBKR Client Portal REST: Session Management

## 1. Context

Phase 1f.1 is complete (config, error types, models, contract registry). Phase 1f.2 adds session management for the IBKR Client Portal (CP) Gateway and scaffolds the REST client. The CP Gateway runs locally at `https://localhost:5000` with a self-signed certificate. Sessions expire after ~5 minutes of inactivity. Authentication is performed externally through the gateway's web UI — the adapter can only CHECK auth status and keep sessions alive, not initiate login.

**Design decisions (from interactive planning):**
- **SessionManager owns the Client**: The keepalive task needs HTTP access to `/tickle`. IbkrRestClient accesses it via `session.http()`.
- **`with_client` test constructor**: Production uses `danger_accept_invalid_certs(true)`. Tests use a plain client pointing at wiremock HTTP.
- **`ensure_authenticated()` caches**: Avoids hitting the auth endpoint on every request. Only calls authenticate() if state is not Authenticated.
- **Unauthenticated vs Expired**: Tracked via `was_authenticated: Arc<AtomicBool>`. If previously authed and now not → Expired. If never authed → Unauthenticated.
- **Rate limiter**: `RateLimiter::new(10, 1.0)` — 10 tokens, 1/sec refill. Conservative for CP Gateway pacing rules.

## 2. Files

### New Files (2)
- `crates/ingot-connectivity/src/ibkr/session.rs` — `SessionState`, `SessionManager`, `IbkrAuthStatus`
- `crates/ingot-connectivity/src/ibkr/rest.rs` — `IbkrRestClient` with `get<T>()`, `post<T>()`, 401 retry

### Modified Files (1)
- `crates/ingot-connectivity/src/ibkr/mod.rs` — add `pub(crate) mod session;` and `pub(crate) mod rest;`

## 3. Type Definitions

### 3.1 SessionState (session.rs)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionState {
    /// Never successfully authenticated with the gateway
    Unauthenticated,
    /// Gateway confirmed the session is authenticated
    Authenticated,
    /// Was previously authenticated but session has expired
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
```

### 3.2 IbkrAuthStatus (session.rs, internal)

```rust
/// Response from POST /iserver/auth/status
#[derive(Debug, Deserialize)]
pub(crate) struct IbkrAuthStatus {
    pub authenticated: bool,
    #[serde(default)]
    pub competing: bool,
    #[serde(default)]
    pub connected: bool,
}
```

### 3.3 SessionManager (session.rs)

```rust
pub(crate) struct SessionManager {
    http: Client,                              // reqwest::Client with danger_accept_invalid_certs(true)
    base_url: String,                          // from IbkrConfig::cp_gateway_url
    state: Arc<RwLock<SessionState>>,          // tokio::sync::RwLock
    was_authenticated: Arc<AtomicBool>,        // tracks if ever auth'd (for Expired vs Unauthenticated)
    keepalive_handle: Option<JoinHandle<()>>,
    shutdown_tx: Option<watch::Sender<bool>>,
}
```

Methods:
- `new(config: &IbkrConfig) -> anyhow::Result<Self>` — builds client with `danger_accept_invalid_certs(true)`, state = Unauthenticated
- `with_client(http: Client, base_url: String) -> Self` — `#[cfg(test)]` constructor for wiremock
- `authenticate() -> anyhow::Result<()>` — POST `/iserver/auth/status`, updates state, errors if not authenticated
- `check_status() -> anyhow::Result<SessionState>` — same endpoint, returns state without erroring
- `ensure_authenticated() -> anyhow::Result<()>` — if cached state is `Authenticated`, return `Ok(())`. Otherwise call `authenticate()`.
- `tickle() -> anyhow::Result<()>` — POST `/tickle`, expects 200
- `start_keepalive(interval: Duration)` — spawns background task calling tickle periodically
- `shutdown(&mut self)` — signals keepalive task to stop, awaits join
- `http() -> &Client` — exposes HTTP client for IbkrRestClient
- `base_url() -> &str`

Private: `refresh_auth_state() -> anyhow::Result<SessionState>` — shared by `authenticate()` and `check_status()`. Hits endpoint, updates state + `was_authenticated` flag, returns new state.

### 3.4 IbkrRestClient (rest.rs)

```rust
pub(crate) struct IbkrRestClient {
    session: SessionManager,
    config: IbkrConfig,
    registry: Arc<tokio::sync::RwLock<IbkrContractRegistry>>,
    rate_limiter: RateLimiter,
}
```

Methods:
- `new(config: IbkrConfig) -> anyhow::Result<Self>` — builds SessionManager, rate limiter (10 tokens, 1.0/sec)
- `with_session(session, config, registry) -> Self` — `#[cfg(test)]` constructor
- `get<T: DeserializeOwned>(path: &str, params: &[(&str, &str)]) -> anyhow::Result<T>` — rate limit → ensure_authenticated → GET → 401 retry → deserialize
- `post<T: DeserializeOwned, B: Serialize>(path: &str, body: &B) -> anyhow::Result<T>` — same with POST + JSON body
- `registry() -> &Arc<tokio::sync::RwLock<IbkrContractRegistry>>`
- `session() -> &SessionManager`

## 4. TDD Steps (14 tests)

| # | Test Name | File | Verifies |
|---|-----------|------|----------|
| 1 | `test_session_state_display` | session.rs | Display for Unauthenticated, Authenticated, Expired |
| 2 | `test_auth_status_deserialize` | session.rs | IbkrAuthStatus from full JSON + minimal JSON (missing optional fields) |
| 3 | `test_session_manager_initial_state_unauthenticated` | session.rs | `with_client()` → state is Unauthenticated |
| 4 | `test_session_authenticate_success` | session.rs | wiremock: `POST /iserver/auth/status` → `{"authenticated":true,...}` → Ok(()), state = Authenticated |
| 5 | `test_session_authenticate_failure` | session.rs | wiremock: `{"authenticated":false,...}` → Err(NotAuthenticated), state = Unauthenticated |
| 6 | `test_session_check_status_authenticated` | session.rs | wiremock: authenticated → Ok(Authenticated) |
| 7 | `test_session_check_status_expired` | session.rs | First authenticate (mocked authenticated), then check_status (mocked not authenticated) → Ok(Expired) |
| 8 | `test_session_tickle_success` | session.rs | wiremock: `POST /tickle` returns 200 → Ok(()) |
| 9 | `test_session_ensure_authenticated_when_already_authed` | session.rs | Set state to Authenticated manually, call ensure_authenticated() → Ok(()) without HTTP call (mock expects 0 hits) |
| 10 | `test_session_ensure_authenticated_re_auths_when_expired` | session.rs | Set state to Expired, mock auth status returns authenticated → Ok(()), state = Authenticated |
| 11 | `test_ibkr_rest_client_construction` | rest.rs | new() succeeds, registry is empty, session state = Unauthenticated |
| 12 | `test_ibkr_rest_client_get_deserializes` | rest.rs | wiremock: auth status OK + GET /test returns JSON → correctly deserialized |
| 13 | `test_ibkr_rest_client_post_deserializes` | rest.rs | wiremock: auth status OK + POST /test returns JSON → correctly deserialized |
| 14 | `test_ibkr_rest_client_handles_401_retry` | rest.rs | wiremock: first GET → 401, auth status → authenticated, second GET → 200 → Ok(data) |

## 5. Implementation Order

### Step 1: SessionState + Display (test 1)
Write test, implement enum + Display.

### Step 2: IbkrAuthStatus (test 2)
Write deserialization test, implement struct.

### Step 3: SessionManager::new (test 3)
Write test, implement struct + new() + with_client().

### Step 4: authenticate() success + failure (tests 4, 5)
Write wiremock tests, implement refresh_auth_state() + authenticate().

### Step 5: check_status() + Expired distinction (tests 6, 7)
Write tests, implement check_status() with Unauthenticated/Expired distinction via `was_authenticated` flag.

### Step 6: tickle() (test 8)
Write wiremock test, implement tickle().

### Step 7: ensure_authenticated() (tests 9, 10)
Write tests, implement ensure_authenticated() with cached-state shortcut.

### Step 8: start_keepalive + shutdown (no dedicated test)
Implement keepalive background task. Tested implicitly through tickle + shutdown methods.

### Step 9: IbkrRestClient construction (test 11)
Write test, implement struct + new() + with_session().

### Step 10: get<T> + post<T> (tests 12, 13)
Write wiremock tests, implement get/post with rate limiting + ensure_authenticated.

### Step 11: 401 retry (test 14)
Write wiremock test with sequenced responses. Implement retry logic.

### Step 12: Module wiring
Add `pub(crate) mod session;` and `pub(crate) mod rest;` to ibkr/mod.rs.

### Step 13: Verify
```bash
SQLX_OFFLINE=true cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --all-targets --workspace
SQLX_OFFLINE=true cargo nextest run -p ingot-connectivity
SQLX_OFFLINE=true cargo check --all-targets --workspace
SQLX_OFFLINE=true cargo bench --no-run
```

## 6. Key Files (reference)
- `crates/ingot-connectivity/src/ibkr/error.rs` — IbkrError::NotAuthenticated, SessionExpired
- `crates/ingot-connectivity/src/config.rs` — IbkrConfig with cp_gateway_url, session_keepalive_secs
- `crates/ingot-connectivity/src/rate_limiter.rs` — RateLimiter::new(capacity, refill_rate)
- `crates/ingot-connectivity/src/kraken/spot/rest.rs` — reference pattern for REST client structure
