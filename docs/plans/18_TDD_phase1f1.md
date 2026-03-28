# Technical Design Document: Phase 1f.1 — IBKR Types, Config, Error, Models, ContractRegistry

## 1. Context

Phase 1f adds Interactive Brokers (IBKR) as a second real broker. Sub-phase 1f.1 lays the foundation: config struct, IBKR-specific error enum with conversion to `ConnectivityError`, Client Portal API response/request models, and a bidirectional ContractId-to-Symbol registry. No network calls — purely types and data structures.

**Design decisions (from interactive planning):**
- **Config location**: Centralized `src/config.rs` alongside `KrakenSpotConfig`/`KrakenFuturesConfig`
- **Error pattern**: Separate `IbkrError` in `ibkr/error.rs` with `impl From<IbkrError> for ConnectivityError`

## 2. Files

### New Files (4)
- `crates/ingot-connectivity/src/ibkr/mod.rs`
- `crates/ingot-connectivity/src/ibkr/error.rs`
- `crates/ingot-connectivity/src/ibkr/models.rs`
- `crates/ingot-connectivity/src/ibkr/contract_registry.rs`

### Modified Files (3)
- `crates/ingot-connectivity/src/config.rs` — add `IbkrConfig` + defaults + tests
- `crates/ingot-connectivity/src/lib.rs` — add `pub mod ibkr;`, re-export `IbkrConfig`
- `crates/ingot-connectivity/src/error.rs` — add `impl From<IbkrError> for ConnectivityError`

## 3. Type Definitions

### 3.1 IbkrConfig (in `src/config.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IbkrConfig {
    /// IBKR account ID (e.g., "DU1234567" for paper, "U1234567" for live)
    pub account_id: String,
    /// Client Portal Gateway URL (local gateway proxy)
    #[serde(default = "default_ibkr_cp_gateway_url")]
    pub cp_gateway_url: String,
    /// TWS API host
    #[serde(default = "default_ibkr_tws_host")]
    pub tws_host: String,
    /// TWS API port (7497 = paper, 7496 = live)
    #[serde(default = "default_ibkr_tws_port")]
    pub tws_port: u16,
    /// TWS client ID (must be unique per concurrent connection)
    #[serde(default = "default_ibkr_client_id")]
    pub client_id: i32,
    /// Seconds between session keepalive pings
    #[serde(default = "default_ibkr_session_keepalive_secs")]
    pub session_keepalive_secs: u64,
}

fn default_ibkr_cp_gateway_url() -> String { "https://localhost:5000".into() }
fn default_ibkr_tws_host() -> String { "127.0.0.1".into() }
fn default_ibkr_tws_port() -> u16 { 7497 }
fn default_ibkr_client_id() -> i32 { 1 }
fn default_ibkr_session_keepalive_secs() -> u64 { 60 }
```

### 3.2 IbkrError (in `ibkr/error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub(crate) enum IbkrError {
    #[error("session expired, re-authentication required")]
    SessionExpired,
    #[error("contract not found: conid {0}")]
    ContractNotFound(i64),
    #[error("pacing violation: too many requests")]
    PacingViolation,
    #[error("order rejected by IBKR: {reason}")]
    OrderRejected { reason: String },
    #[error("TWS connection error: {0}")]
    TwsConnection(String),
    #[error("TWS message decode error: {0}")]
    TwsDecode(String),
    #[error("unsupported security type: {0}")]
    UnsupportedSecType(String),
    #[error("gateway not authenticated")]
    NotAuthenticated,
    #[error("margin data unavailable")]
    MarginUnavailable,
}
```

**From conversion mapping:**

| IbkrError | ConnectivityError |
|-----------|-------------------|
| `SessionExpired` | `AuthenticationFailed { reason: "session expired..." }` |
| `NotAuthenticated` | `AuthenticationFailed { reason: "gateway not authenticated" }` |
| `PacingViolation` | `RateLimited { retry_after_ms: 1000 }` |
| `OrderRejected { reason }` | `OrderRejected { reason }` |
| `ContractNotFound(conid)` | `SymbolNotFound(Symbol::new(conid.to_string()))` |
| `TwsConnection(msg)` | `WebSocket(msg)` |
| `TwsDecode(msg)` | `InvalidResponse(msg)` |
| `UnsupportedSecType(s)` | `InvalidResponse("unsupported security type: {s}")` |
| `MarginUnavailable` | `InvalidResponse("margin data unavailable")` |

Note: `ContractNotFound` → `SymbolNotFound` requires `Symbol::new()` which is fallible. Use the conid stringified as the symbol. If `Symbol::new` fails (empty string — impossible for an i64), fall back to `InvalidResponse`.

### 3.3 Response Models (in `ibkr/models.rs`, Deserialize only)

All `pub(crate)`, all `#[derive(Debug, Deserialize)]`, all `#[allow(dead_code)]`.

```rust
/// Contract search result from GET /iserver/secdef/search
pub(crate) struct IbkrContractSearchResult {
    pub conid: i64,
    pub company_name: String,
    pub symbol: String,
    pub sec_type: String,          // "STK", "OPT", "FUT", "CASH", "BOND"
    pub exchange: Option<String>,
    pub currency: String,
}

/// Contract details from GET /iserver/contract/{conid}/info
pub(crate) struct IbkrContractDetail {
    pub con_id: i64,
    pub symbol: String,
    pub sec_type: String,
    pub exchange: String,
    pub currency: String,
    pub local_symbol: Option<String>,
    pub trading_class: Option<String>,
    pub multiplier: Option<String>,
    pub expiry: Option<String>,        // "20260320" format
    pub strike: Option<String>,
    pub right: Option<String>,         // "C" or "P"
    pub company_name: Option<String>,
    #[serde(default)]
    pub valid_exchanges: Option<String>,
}

/// Market data snapshot from GET /iserver/marketdata/snapshot
/// IBKR uses numeric field IDs as JSON keys.
pub(crate) struct IbkrMarketSnapshot {
    pub conid: i64,
    #[serde(rename = "31")]  pub last_price: Option<String>,
    #[serde(rename = "84")]  pub bid: Option<String>,
    #[serde(rename = "86")]  pub ask: Option<String>,
    #[serde(rename = "87")]  pub volume: Option<String>,
    #[serde(rename = "7295")] pub open: Option<String>,
    #[serde(rename = "7296")] pub high: Option<String>,
    #[serde(rename = "7297")] pub low: Option<String>,
    #[serde(rename = "7291")] pub close: Option<String>,
}

/// Order status from GET /iserver/account/order/status/{orderId}
pub(crate) struct IbkrOrderStatus {
    pub order_id: String,
    pub conid: i64,
    pub status: String,              // "Submitted", "Filled", "Cancelled", etc.
    pub filled_quantity: f64,
    pub remaining_quantity: f64,
    pub avg_price: f64,
    pub last_fill_price: Option<f64>,
    pub side: String,                // "BUY", "SELL"
}

/// Account balance from GET /portfolio/{accountId}/ledger
pub(crate) struct IbkrAccountBalance {
    pub currency: String,
    #[serde(rename = "settledcash")]
    pub settled_cash: Option<f64>,
    #[serde(rename = "cashbalance")]
    pub cash_balance: Option<f64>,
}

/// Position from GET /portfolio/{accountId}/positions/0
pub(crate) struct IbkrPosition {
    pub conid: i64,
    pub currency: String,
    pub position: f64,
    #[serde(rename = "avgCost")]   pub avg_cost: f64,
    #[serde(rename = "mktPrice")]  pub market_price: f64,
    #[serde(rename = "mktValue")]  pub market_value: f64,
    #[serde(rename = "unrealizedPnl")] pub unrealized_pnl: f64,
}

/// IBKR returns summary fields as { "amount": 12345.67, "currency": "USD" }
pub(crate) struct IbkrAmountField {
    pub amount: f64,
    pub currency: Option<String>,
}

/// Margin info from GET /portfolio/{accountId}/summary
pub(crate) struct IbkrMarginInfo {
    #[serde(rename = "initmarginreq")]    pub initial_margin: Option<IbkrAmountField>,
    #[serde(rename = "maintmarginreq")]   pub maintenance_margin: Option<IbkrAmountField>,
    #[serde(rename = "excessliquidity")]  pub excess_liquidity: Option<IbkrAmountField>,
    #[serde(rename = "buyingpower")]      pub buying_power: Option<IbkrAmountField>,
    #[serde(rename = "availablefunds")]   pub available_funds: Option<IbkrAmountField>,
    #[serde(rename = "netliquidation")]   pub net_liquidation: Option<IbkrAmountField>,
    #[serde(rename = "sma")]             pub sma: Option<IbkrAmountField>,
}

/// Historical data bar from GET /iserver/marketdata/history
pub(crate) struct IbkrHistoryBar {
    #[serde(rename = "t")] pub timestamp: i64,
    #[serde(rename = "o")] pub open: f64,
    #[serde(rename = "h")] pub high: f64,
    #[serde(rename = "l")] pub low: f64,
    #[serde(rename = "c")] pub close: f64,
    #[serde(rename = "v")] pub volume: f64,
}

/// Historical data response wrapper
pub(crate) struct IbkrHistoryResponse {
    pub data: Vec<IbkrHistoryBar>,
}
```

### 3.4 Request Models (in `ibkr/models.rs`, Serialize only)

```rust
/// Order submission body for POST /iserver/account/{id}/orders
#[derive(Debug, Serialize)]
pub(crate) struct IbkrOrderRequest {
    #[serde(rename = "acctId")]
    pub acct_id: String,
    pub conid: i64,
    #[serde(rename = "secType")]  pub sec_type: String,
    #[serde(rename = "orderType")] pub order_type: String,
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

/// Confirmation reply for POST /iserver/reply/{replyId}
#[derive(Debug, Serialize)]
pub(crate) struct IbkrConfirmReply {
    pub confirmed: bool,
}
```

### 3.5 ContractRegistry (in `ibkr/contract_registry.rs`)

```rust
pub(crate) struct IbkrContractRegistry {
    conid_to_symbol: HashMap<i64, Symbol>,
    symbol_to_conid: HashMap<Symbol, i64>,
    conid_to_instrument: HashMap<i64, Arc<Instrument>>,
}

impl IbkrContractRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, conid: i64, symbol: Symbol, instrument: Instrument);
    pub fn symbol_for_conid(&self, conid: i64) -> Option<&Symbol>;
    pub fn conid_for_symbol(&self, symbol: &Symbol) -> Option<i64>;
    pub fn instrument_for_conid(&self, conid: i64) -> Option<&Arc<Instrument>>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

### 3.6 Module Root (`ibkr/mod.rs`)

```rust
pub(crate) mod contract_registry;
pub(crate) mod error;
pub(crate) mod models;
```

## 4. TDD Steps

### 4.1 IbkrConfig (in `src/config.rs`, 2 tests)

| # | Test Name | Verifies |
|---|-----------|----------|
| 1 | `test_ibkr_config_serde_roundtrip` | Construct with all fields, serialize, deserialize, assert all fields match |
| 2 | `test_ibkr_config_defaults` | Minimal JSON `{"account_id":"DU123"}` → defaults: cp_gateway_url=`https://localhost:5000`, tws_host=`127.0.0.1`, tws_port=7497, client_id=1, session_keepalive_secs=60 |

### 4.2 IbkrError Display (in `ibkr/error.rs`, 9 tests)

| # | Test Name | Verifies |
|---|-----------|----------|
| 3 | `test_ibkr_error_display_session_expired` | `"session expired, re-authentication required"` |
| 4 | `test_ibkr_error_display_contract_not_found` | `"contract not found: conid 265598"` |
| 5 | `test_ibkr_error_display_pacing_violation` | `"pacing violation: too many requests"` |
| 6 | `test_ibkr_error_display_order_rejected` | `"order rejected by IBKR: margin exceeded"` |
| 7 | `test_ibkr_error_display_tws_connection` | `"TWS connection error: timeout"` |
| 8 | `test_ibkr_error_display_tws_decode` | `"TWS message decode error: invalid length"` |
| 9 | `test_ibkr_error_display_unsupported_sec_type` | `"unsupported security type: WAR"` |
| 10 | `test_ibkr_error_display_not_authenticated` | `"gateway not authenticated"` |
| 11 | `test_ibkr_error_display_margin_unavailable` | `"margin data unavailable"` |

### 4.3 IbkrError → ConnectivityError (in `ibkr/error.rs`, 7 tests)

| # | Test Name | Verifies |
|---|-----------|----------|
| 12 | `test_from_session_expired` | → `ConnectivityError::AuthenticationFailed` |
| 13 | `test_from_not_authenticated` | → `ConnectivityError::AuthenticationFailed` |
| 14 | `test_from_pacing_violation` | → `ConnectivityError::RateLimited { retry_after_ms: 1000 }` |
| 15 | `test_from_order_rejected` | → `ConnectivityError::OrderRejected { reason }` |
| 16 | `test_from_contract_not_found` | → `ConnectivityError::SymbolNotFound` |
| 17 | `test_from_tws_connection` | → `ConnectivityError::WebSocket` |
| 18 | `test_from_tws_decode` | → `ConnectivityError::InvalidResponse` |

### 4.4 Response Model Deserialization (in `ibkr/models.rs`, 14 tests)

| # | Test Name | Verifies |
|---|-----------|----------|
| 19 | `test_deserialize_contract_search_result` | JSON with all fields → correct struct fields |
| 20 | `test_deserialize_contract_detail_equity` | STK with symbol, exchange, currency; optional fields None |
| 21 | `test_deserialize_contract_detail_future` | FUT with expiry `"20260320"`, multiplier `"50"` |
| 22 | `test_deserialize_contract_detail_option` | OPT with strike `"150.00"`, right `"C"`, expiry |
| 23 | `test_deserialize_market_snapshot_full` | All numeric field IDs (31, 84, 86, 87, 7295-7297, 7291) present |
| 24 | `test_deserialize_market_snapshot_partial` | Only conid + last_price; other fields → None |
| 25 | `test_deserialize_order_status` | All fields including optional last_fill_price |
| 26 | `test_deserialize_account_balance` | Renamed fields `settledcash`, `cashbalance` |
| 27 | `test_deserialize_position` | camelCase renamed fields `avgCost`, `mktPrice`, etc. |
| 28 | `test_deserialize_margin_info_full` | All nested `IbkrAmountField` objects present |
| 29 | `test_deserialize_margin_info_partial` | Some fields null → None |
| 30 | `test_deserialize_history_bar` | Single-char renamed fields t, o, h, l, c, v |
| 31 | `test_deserialize_history_response` | Wrapper with `data` array of bars |
| 32 | `test_deserialize_amount_field` | `{ "amount": 12345.67, "currency": "USD" }` |

### 4.5 Request Model Serialization (in `ibkr/models.rs`, 3 tests)

| # | Test Name | Verifies |
|---|-----------|----------|
| 33 | `test_serialize_order_request_full` | All fields including price → JSON with correct renames |
| 34 | `test_serialize_order_request_skips_none` | `price: None`, `aux_price: None` → fields omitted from JSON |
| 35 | `test_serialize_confirm_reply` | `{ "confirmed": true }` |

### 4.6 ContractRegistry (in `ibkr/contract_registry.rs`, 8 tests)

| # | Test Name | Verifies |
|---|-----------|----------|
| 36 | `test_registry_new_empty` | `len()==0`, `is_empty()==true` |
| 37 | `test_registry_register_and_lookup_by_conid` | Register conid 265598 → `symbol_for_conid` returns correct Symbol |
| 38 | `test_registry_register_and_lookup_by_symbol` | `conid_for_symbol` returns correct i64 |
| 39 | `test_registry_instrument_lookup` | `instrument_for_conid` returns correct `Arc<Instrument>` |
| 40 | `test_registry_missing_conid_returns_none` | Unknown conid → None |
| 41 | `test_registry_missing_symbol_returns_none` | Unknown symbol → None |
| 42 | `test_registry_len_tracks_registrations` | After 3 registers → len=3, is_empty=false |
| 43 | `test_registry_overwrite_replaces_entry` | Register same conid twice → latest symbol/instrument wins |

## 5. Implementation Order

1. Add `IbkrConfig` + defaults + 2 tests to `src/config.rs`
2. Create `ibkr/mod.rs` (module declarations only)
3. Create `ibkr/error.rs` — `IbkrError` enum + `From` impl + 16 tests
4. Create `ibkr/models.rs` — all response/request structs + 17 tests
5. Create `ibkr/contract_registry.rs` — registry + 8 tests
6. Update `src/lib.rs` — add `pub mod ibkr;`, add `IbkrConfig` to re-exports
7. Update `src/error.rs` — add `From<IbkrError>` import (the impl lives in `ibkr/error.rs`)

## 6. Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --workspace
cargo nextest run -p ingot-connectivity
cargo check --all-targets --workspace
cargo bench --no-run
```
