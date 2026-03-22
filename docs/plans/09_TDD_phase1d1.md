# Implementation Plan: Phase 1d.1 — Crate Scaffold, Engine Types, Config & Error

## Context

Phase 1d builds the execution engine. Sub-phase 1d.1 creates the `ingot-engine` crate and establishes all foundational types: `StrategyId`, `OrderIntention`, `RiskDecision`, `EngineEvent`, config structs (`EngineConfig`, `RiskConfig`, `SmartOrderConfig`), the `EngineError` enum, and the `LedgerWriter` trait.

## What Already Exists

- `OrderRequest`, `OrderFill`, `OrderId`, `OrderStatus` — `ingot-core/src/order.rs`
- `TickerSnapshot`, `OrderBookSnapshot` — `ingot-core/src/market_data.rs`
- `Position`, `Balance` — `ingot-core/src/position.rs`, `ingot-core/src/balance.rs`
- `Transaction` — `ingot-accounting/src/transaction.rs`
- `AccountingError` — `ingot-accounting/src/error.rs` (pattern to follow for EngineError)
- `AccountingConfig` — `ingot-accounting/src/config.rs` (pattern to follow for EngineConfig)
- `Percentage::new(Decimal) -> Result` validates `[0, 1]` — `ingot-primitives/src/newtypes.rs`
- `SmolStr` used for string newtypes throughout (OrderId, Symbol, etc.)
- Workspace lints: `unwrap_used=deny`, `expect_used=deny`, `panic=deny`, `todo=deny`, `clippy::pedantic=warn`

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Duration serialization | Store as `u64` milliseconds (`fallback_timeout_ms`) | `std::time::Duration` doesn't impl Serialize; avoids `serde_with` dependency |
| `StrategyId` pattern | Mirror `OrderId(SmolStr)` with empty rejection | Consistent with existing codebase pattern |
| `EngineError` | `thiserror` enum, one Display test per variant | Matches `AccountingError` pattern exactly |
| `LedgerWriter` trait | RPITIT (`impl Future`) in `ingot-engine` | Matches all trait patterns in codebase; storage implements it |
| `RiskDecision` | Enum with `Approved` / `Rejected { reason }` | Simple, testable, no allocation when approved |
| `EngineEvent` | Not Serialize/Deserialize (internal only) | Events are transient, never persisted |

## New Files

| File | Contents |
|------|----------|
| `crates/ingot-engine/Cargo.toml` | Crate manifest |
| `crates/ingot-engine/src/lib.rs` | Module declarations + re-exports |
| `crates/ingot-engine/src/types.rs` | `StrategyId`, `OrderIntention`, `RiskDecision`, `EngineEvent` |
| `crates/ingot-engine/src/config.rs` | `EngineConfig`, `RiskConfig`, `SmartOrderConfig` |
| `crates/ingot-engine/src/error.rs` | `EngineError` |
| `crates/ingot-engine/src/traits.rs` | `LedgerWriter` |

## Modified Files

| File | Change |
|------|--------|
| `Cargo.toml` (root) | Add `"crates/ingot-engine"` to workspace members |

## Type Definitions

### `types.rs`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StrategyId(SmolStr);

impl StrategyId {
    pub fn new(id: &str) -> Result<Self, EngineError> {
        if id.is_empty() { return Err(EngineError::EmptyStrategyId); }
        Ok(Self(SmolStr::new(id)))
    }
    pub fn as_str(&self) -> &str { self.0.as_str() }
}
impl fmt::Display for StrategyId { /* writes inner str */ }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderIntention {
    pub strategy_id: StrategyId,
    pub request: OrderRequest,
    pub reason: Option<SmolStr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskDecision {
    Approved,
    Rejected { reason: SmolStr },
}
impl fmt::Display for RiskDecision { /* "approved" or "rejected: {reason}" */ }

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Ticker(TickerSnapshot),
    OrderBook(OrderBookSnapshot),
    Fill(OrderFill),
    ScheduleTrigger(StrategyId),
    KillSwitch,
    Shutdown,
}
```

### `config.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub risk: RiskConfig,
    pub base_currency: Currency,
    pub smart_order: SmartOrderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub global_stop_loss: Amount,
    pub max_currency_exposure: Percentage,
    pub max_asset_exposure: Percentage,
    pub max_order_value: Amount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartOrderConfig {
    pub use_mid_price: bool,
    pub offset_bps: Decimal,
    pub fallback_timeout_ms: u64,
}
```

Note: `SmartOrderConfig` uses `fallback_timeout_ms: u64` instead of `Duration` to avoid serde issues. The `OrderManager` (1d.4) will convert to `Duration` internally.

### `error.rs`

```rust
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("strategy not found: {0}")]
    StrategyNotFound(StrategyId),
    #[error("strategy ID cannot be empty")]
    EmptyStrategyId,
    #[error("duplicate strategy ID: {0}")]
    DuplicateStrategyId(StrategyId),
    #[error("risk check rejected: {reason}")]
    RiskRejected { reason: SmolStr },
    #[error("global stop-loss triggered: NAV {nav} below threshold {threshold}")]
    GlobalStopLoss { nav: Amount, threshold: Amount },
    #[error("exposure limit exceeded: {currency} at {current}, max {limit}")]
    ExposureLimitExceeded { currency: Currency, current: Percentage, limit: Percentage },
    #[error("order manager error: {0}")]
    OrderManager(String),
    #[error("engine already running")]
    AlreadyRunning,
    #[error("engine not running")]
    NotRunning,
    #[error("kill switch activated")]
    KillSwitchActivated,
    #[error("smart order: order book empty for {0}")]
    EmptyOrderBook(Symbol),
    #[error("connectivity error: {0}")]
    Connectivity(#[source] anyhow::Error),
    #[error("accounting error: {0}")]
    Accounting(#[from] AccountingError),
}
```

### `traits.rs`

```rust
pub trait LedgerWriter {
    fn write_transaction(
        &self,
        txn: &Transaction,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}
```

### `lib.rs`

```rust
pub mod config;
pub mod error;
pub mod traits;
pub mod types;

pub use config::{EngineConfig, RiskConfig, SmartOrderConfig};
pub use error::EngineError;
pub use traits::LedgerWriter;
pub use types::{EngineEvent, OrderIntention, RiskDecision, StrategyId};
```

## TDD Step Order

### Step 1: Crate scaffold
- `cargo new --lib crates/ingot-engine`
- Add to workspace members in root `Cargo.toml`
- Add dependencies to `crates/ingot-engine/Cargo.toml`
- Verify: `cargo check -p ingot-engine`

### Step 2: `StrategyId` tests + impl
**Red:**
- `test_strategy_id_valid` — `StrategyId::new("my-strat")` succeeds, `as_str()` returns "my-strat"
- `test_strategy_id_empty_rejected` — `StrategyId::new("")` → `Err(EngineError::EmptyStrategyId)`
- `test_strategy_id_display` — `to_string()` returns "my-strat"
- `test_strategy_id_serde_roundtrip` — JSON roundtrip preserves value

**Green:** Implement `StrategyId`

### Step 3: `OrderIntention` + `RiskDecision` tests + impl
**Red:**
- `test_order_intention_construction` — Build with valid StrategyId + OrderRequest
- `test_risk_decision_approved_display` — `RiskDecision::Approved.to_string()` → "approved"
- `test_risk_decision_rejected_display` — `Rejected { reason }.to_string()` → "rejected: {reason}"

**Green:** Implement `OrderIntention`, `RiskDecision`

### Step 4: `EngineEvent` tests
**Red:**
- `test_engine_event_ticker_variant` — Construct `EngineEvent::Ticker(snapshot)`
- `test_engine_event_fill_variant` — Construct `EngineEvent::Fill(fill)`
- `test_engine_event_kill_switch_variant` — Construct `EngineEvent::KillSwitch`
- `test_engine_event_shutdown_variant` — Construct `EngineEvent::Shutdown`
- `test_engine_event_schedule_trigger_variant` — Construct `EngineEvent::ScheduleTrigger(id)`

**Green:** Implement `EngineEvent`

### Step 5: Config tests + impl
**Red:**
- `test_engine_config_serde_roundtrip` — Full EngineConfig JSON roundtrip
- `test_risk_config_serde_roundtrip` — RiskConfig JSON roundtrip
- `test_smart_order_config_serde_roundtrip` — SmartOrderConfig JSON roundtrip

**Green:** Implement config structs

### Step 6: `EngineError` tests + impl
**Red:**
- `test_engine_error_display_strategy_not_found`
- `test_engine_error_display_empty_strategy_id`
- `test_engine_error_display_duplicate_strategy_id`
- `test_engine_error_display_risk_rejected`
- `test_engine_error_display_global_stop_loss`
- `test_engine_error_display_exposure_limit`
- `test_engine_error_display_order_manager`
- `test_engine_error_display_already_running`
- `test_engine_error_display_not_running`
- `test_engine_error_display_kill_switch`
- `test_engine_error_display_empty_order_book`
- `test_engine_error_display_connectivity`
- `test_engine_error_from_accounting` — `AccountingError` converts via `From`

**Green:** Implement `EngineError`

### Step 7: `LedgerWriter` trait
**Red:** (compile test — trait exists and is importable)
- `test_ledger_writer_trait_object_safety` — Verify the trait can be used generically

**Green:** Implement trait definition

### Step 8: lib.rs re-exports
- Wire up all `pub use` re-exports
- Verify all types are accessible from `ingot_engine::*`

### Step 9: Verification
1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-engine` — all tests pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run` — all benchmarks compile
6. No `.unwrap()`, `.expect()`, `panic!()`, `todo!()`

## Dependencies for `ingot-engine/Cargo.toml`

```toml
[package]
name = "ingot-engine"
version.workspace = true
edition.workspace = true
authors.workspace = true

[lints]
workspace = true

[dependencies]
ingot-accounting = { path = "../ingot-accounting" }
ingot-connectivity = { path = "../ingot-connectivity" }
ingot-core = { path = "../ingot-core" }
ingot-primitives = { path = "../ingot-primitives" }
anyhow.workspace = true
chrono.workspace = true
rust_decimal.workspace = true
serde.workspace = true
serde_json.workspace = true
smol_str.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true

[dev-dependencies]
proptest.workspace = true
rust_decimal_macros.workspace = true
serde_json.workspace = true
tokio = { workspace = true, features = ["test-util"] }
```
