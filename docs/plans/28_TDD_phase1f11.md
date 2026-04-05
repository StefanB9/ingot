# Phase 1f.11: Integration Tests + Public API — TDD Plan

## Context

Phase 1f.11 is the final sub-phase of the IBKR integration. Phases 1f.1–1f.10 are implemented and committed on `feature/phase-1f-ibkr`. This phase has two goals:

1. **Public API** — Re-export key IBKR types from `ingot-connectivity` so downstream crates (`ingot-engine`, future UI bridge) can use them directly.
2. **Integration Tests** — 12 end-to-end tests proving the IBKR components work together across crate boundaries.

---

## Part A: Public API Changes

### A1. Module visibility in `ibkr/mod.rs`

| Module | Current | Target | Why |
|--------|---------|--------|-----|
| `contract_registry` | `pub(crate)` | **`pub`** | Downstream needs `IbkrContractRegistry` for conid lookups |
| `error` | `pub(crate)` | **`pub`** | Downstream needs `IbkrError` for matching |
| `rest` | `pub(crate)` | **`pub`** | `IbkrRestClient` is the primary consumer type |
| `tws` | `pub(crate)` | **`pub`** | `IbkrTws<Connected/Disconnected>` needed by consumers |
| `mapper` | `pub(crate)` | `pub(crate)` | Internal conversion logic |
| `margin` | `pub(crate)` | `pub(crate)` | Re-exports from `ingot_core`; consumers get it there |
| `models` | `pub(crate)` | `pub(crate)` | Wire-format DTOs, not public API |
| `session` | `pub(crate)` | `pub(crate)` | Implementation detail behind `IbkrRestClient` |
| `tws_codec` | `pub(crate)` | `pub(crate)` | Internal codec |
| `tws_models` | `pub(crate)` | `pub(crate)` | Internal TWS message types |

### A2. Struct visibility changes

| File | Change |
|------|--------|
| `ibkr/rest.rs:33` | `pub(crate) struct IbkrRestClient` → `pub struct IbkrRestClient` |
| `ibkr/tws.rs:124` | `pub(crate) struct IbkrTws` → `pub struct IbkrTws` |
| `ibkr/contract_registry.rs:6` | `pub(crate) struct IbkrContractRegistry` → `pub struct IbkrContractRegistry` |
| `ibkr/error.rs:6` | `pub(crate) enum IbkrError` → `pub enum IbkrError` |

`Connected` and `Disconnected` are already `pub struct` in `tws.rs:47,50`.

### A3. Re-exports in `lib.rs`

Add after existing re-exports (following the Kraken pattern):

```rust
pub use ibkr::{
    contract_registry::IbkrContractRegistry,
    error::IbkrError,
    rest::IbkrRestClient,
    tws::{Connected, Disconnected, IbkrTws},
};
```

### A4. Test constructor for integration tests

`IbkrRestClient::with_session` (rest.rs:56) and `SessionManager::with_client` (session.rs:81) are `#[cfg(test)]`, invisible to integration tests in `tests/` (compiled as external crates).

**Approach:** Add `#[doc(hidden)] pub fn with_base_url(config, base_url)` on `IbkrRestClient` that internally constructs a `SessionManager` via `with_client`. Keeps `SessionManager` unexposed.

Changes:
1. Remove `#[cfg(test)]` from `SessionManager::with_client` (session.rs:81) — stays internal since `session` module is `pub(crate)`
2. Keep `IbkrRestClient::with_session` as-is for unit tests (remove `#[cfg(test)]` so `with_base_url` can call it, or have `with_base_url` construct directly)
3. Add `#[doc(hidden)] pub fn with_base_url(...)` on `IbkrRestClient`:

```rust
#[doc(hidden)]
pub fn with_base_url(config: IbkrConfig, base_url: String) -> anyhow::Result<Self> {
    let http = reqwest::Client::builder().build().context("failed to build HTTP client")?;
    let session = SessionManager::with_client(http, base_url);
    let registry = Arc::new(RwLock::new(IbkrContractRegistry::new()));
    let rate_limiter = RateLimiter::new(10, 1.0);
    Ok(Self { session, config, registry, rate_limiter })
}
```

---

## Part B: Test Placement

`ingot-connectivity` does **not** depend on `ingot-engine` or `ingot-accounting`. Tests needing those types live in their owning crate.

| File | Tests |
|------|-------|
| `crates/ingot-connectivity/tests/ibkr_integration.rs` | 1–7, 12 (8 tests) |
| `crates/ingot-engine/tests/ibkr_integration.rs` | 8, 10, 11 (3 tests) |
| `crates/ingot-accounting/tests/ibkr_integration.rs` | 9 (1 test) |

---

## Part C: 12 Integration Tests — Detailed Specs

### Tests 1–4: Trait Satisfaction (compile-time checks)

Pattern: `fn assert_impl<T: Trait>() {}` — compiles = passes.

| # | Test | Trait |
|---|------|-------|
| 1 | `test_ibkr_rest_client_satisfies_market_data_provider` | `MarketDataProvider` |
| 2 | `test_ibkr_rest_client_satisfies_order_executor` | `OrderExecutor` |
| 3 | `test_ibkr_rest_client_satisfies_account_provider` | `AccountProvider` |
| 4 | `test_ibkr_tws_connected_satisfies_stream_provider` | `StreamProvider` |

No mocking needed. Just type-level assertions.

### Test 5: `test_full_order_lifecycle`

**What:** Place order → check status → cancel. Full REST lifecycle through wiremock.

**Setup:** MockServer with mocks for:
- `POST /iserver/auth/status` → authenticated
- `GET /iserver/secdef/search` → AAPL contract
- `GET /iserver/contract/{conid}/info` → contract detail
- `POST /iserver/account/{id}/orders` → order accepted (direct, no confirmation prompt)
- `GET /iserver/account/order/status/{orderId}` → Filled
- `DELETE /iserver/account/{id}/order/{orderId}` → cancelled

**Flow:** `search_contracts("AAPL")` → `place_order(request)` → `get_order_status(id)` → `cancel_order(id)`

**Assertions:** Each call returns `Ok`, order ID matches, status transitions are correct.

### Test 6: `test_session_recovery_on_401`

**What:** REST client gets 401 on first request, re-authenticates, retries successfully.

**Setup:** MockServer with:
- `/iserver/auth/status` → authenticated
- `/test/endpoint` → 401 on first call, 200 with JSON on second (stateful responder)

**Assertions:** Final result is Ok with expected payload.

### Test 7: `test_contract_registry_populated_on_fetch_instruments`

**What:** `search_contracts` populates the registry; subsequent lookups work.

**Setup:** MockServer with search + detail mocks for 2 symbols (AAPL, MSFT).

**Flow:** `search_contracts("AAPL")` → `search_contracts("MSFT")` → read `registry()` → verify lookups.

**Assertions:** Registry len == 2, bidirectional conid/symbol lookups succeed.

### Test 8: `test_margin_snapshot_through_portfolio_controller` (ingot-engine)

**What:** MarginSnapshot flows through PortfolioController risk checks.

**Setup:** `PortfolioController` with `MarginConfig { max_margin_utilization: 0.80 }`. `MarginSnapshot` with utilization at 0.85.

**Flow:** `controller.on_margin_update(&snapshot)` → `controller.check_intention(&intention)`

**Assertions:** `check_intention` rejects due to margin utilization > max.

### Test 9: `test_corporate_action_dividend_through_accounting` (ingot-accounting)

**What:** `post_dividend` with `Exchange::Ibkr` produces balanced ledger entries.

**Flow:** `post_dividend(Exchange::Ibkr, venue, currency, symbol, amount, timestamp)`

**Assertions:** Balanced Transaction, correct debit/credit accounts, exchange tag is `Ibkr`.

### Test 10: `test_rollover_scan_to_intention_generation` (ingot-engine)

**What:** RolloverMonitor generates RolloverPlan for near-expiry futures.

**Setup:** Position map with future expiring in 3 days. `RolloverConfig { days_before_expiry: 5 }`. Instruments with far-month contract.

**Flow:** `monitor.scan_for_rollovers(&positions, &instruments, today)`

**Assertions:** Returns 1 RolloverPlan with correct near/far symbols and quantity.

### Test 11: `test_engine_event_variants_exhaustive` (ingot-engine)

**What:** Exhaustive pattern match on all EngineEvent variants — no wildcard `_`.

**Variants (10):** `Ticker`, `OrderBook`, `Fill`, `MarginUpdate`, `ScheduleTrigger`, `RolloverTriggered`, `RolloverCompleted`, `RolloverFailed`, `KillSwitch`, `Shutdown`

**Assertions:** Construct and classify each variant. Adding a variant forces a compile error.

### Test 12: `test_ibkr_rest_concurrent_requests`

**What:** 20 concurrent requests through rate limiter without deadlock.

**Setup:** MockServer with auth + data endpoint. `IbkrRestClient` in `Arc`.

**Flow:** Spawn 20 `tokio::spawn` tasks, `join_all` with timeout.

**Assertions:** All 20 complete within 10s. No panics.

---

## Part D: TDD Implementation Order

### Step 1: Visibility changes + Tests 1–4 (trait satisfaction)

1. **RED:** Create `ibkr_integration.rs` with 4 trait-check tests → won't compile
2. **GREEN:** Apply all visibility changes (A1–A3)
3. **REFACTOR:** `cargo clippy --all-targets --workspace` clean

### Step 2: Test constructor + Test 7 (contract registry)

1. **RED:** Write test 7 → won't compile (`with_base_url` missing)
2. **GREEN:** Add `with_base_url`, remove `#[cfg(test)]` from `SessionManager::with_client`
3. **REFACTOR:** Extract wiremock auth mock helper

### Step 3: Test 6 (session recovery)

1. **RED:** Write test with 401-then-200 mock
2. **GREEN:** Should pass (retry logic exists)
3. **REFACTOR:** Clean up

### Step 4: Test 5 (full order lifecycle)

1. **RED:** Write full lifecycle test with mock chain
2. **GREEN:** Should pass (all REST methods implemented)
3. **REFACTOR:** Extract shared mock setup helpers

### Step 5: Test 12 (concurrent requests)

1. **RED:** Write concurrent test with 20 tasks
2. **GREEN:** Should pass (rate limiter is thread-safe)
3. **REFACTOR:** Add timeout guard

### Step 6: Test 8 (margin through controller) — in ingot-engine

1. **RED:** Create `ingot-engine/tests/ibkr_integration.rs`, write test 8
2. **GREEN:** Should pass (PortfolioController from 1f.8)

### Step 7: Test 9 (dividend accounting) — in ingot-accounting

1. **RED:** Create `ingot-accounting/tests/ibkr_integration.rs`, write test 9
2. **GREEN:** Should pass (post_dividend from 1f.9)

### Step 8: Tests 10–11 (rollover + exhaustive events) — in ingot-engine

1. **RED:** Write tests 10 and 11
2. **GREEN:** Should pass (RolloverMonitor from 1f.10, all EngineEvent variants exist)
3. **REFACTOR:** Final cleanup

### Step 9: Final verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --workspace
cargo nextest run --workspace
cargo check --all-targets --workspace
cargo bench --no-run
```

---

## Files Summary

### New files (3)
- `crates/ingot-connectivity/tests/ibkr_integration.rs` — tests 1–7, 12
- `crates/ingot-engine/tests/ibkr_integration.rs` — tests 8, 10, 11
- `crates/ingot-accounting/tests/ibkr_integration.rs` — test 9

### Modified files (7)
- `crates/ingot-connectivity/src/ibkr/mod.rs` — 4 modules `pub(crate)` → `pub`
- `crates/ingot-connectivity/src/ibkr/rest.rs` — struct visibility + `with_base_url` method
- `crates/ingot-connectivity/src/ibkr/tws.rs` — struct visibility `pub(crate)` → `pub`
- `crates/ingot-connectivity/src/ibkr/contract_registry.rs` — struct visibility
- `crates/ingot-connectivity/src/ibkr/error.rs` — enum visibility
- `crates/ingot-connectivity/src/ibkr/session.rs` — remove `#[cfg(test)]` from `with_client`
- `crates/ingot-connectivity/src/lib.rs` — add IBKR re-exports
