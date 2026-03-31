# Phase 1f.10: Derivative Rollovers — RolloverMonitor

## Overview

Add a `RolloverMonitor` service to `ingot-engine` that scans positions for expiring derivative contracts (futures, options, dated crypto futures) and generates close/open intention pairs to roll positions to the next contract month.

## Design Decisions

- **Far month discovery**: Pure `InstrumentRegistry` lookup — find instruments with same underlying, same asset class, nearest expiry after near contract's expiry.
- **Event model**: New `EngineEvent` variants (`RolloverTriggered`, `RolloverCompleted`, `RolloverFailed`).
- **Data access**: Method parameters (pure function style) — no owned references. Matches `PortfolioController::check_intention` pattern.
- **CryptoFuture handling**: `expiry: Option<DateTime<Utc>>` — `None` = perpetual (skip), `Some(dt)` converted to `NaiveDate` via `.date_naive()`. Far month matched by same exchange + `AssetClass::CryptoFuture` + same base/quote currency + nearest dated expiry.
- **Bonds excluded**: Bonds mature rather than roll — `expiry_date` helper returns `None` for bonds.
- **Config placement**: `pub rollover: Option<RolloverConfig>` on `RiskConfig`.

## Files

| File | Change |
|------|--------|
| `crates/ingot-engine/src/rollover.rs` | **New** — RolloverConfig, RolloverPlan, RolloverState, ActiveRollover, RolloverMonitor |
| `crates/ingot-engine/src/config.rs` | Add `rollover: Option<RolloverConfig>` to `RiskConfig` |
| `crates/ingot-engine/src/types.rs` | Add 3 `EngineEvent` variants |
| `crates/ingot-engine/src/error.rs` | Add rollover error variants |
| `crates/ingot-engine/src/lib.rs` | Re-export rollover types |

## Type Definitions

### RolloverConfig

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloverConfig {
    pub days_before_expiry: u32,
    pub max_concurrent_rollovers: usize,
    pub use_limit_orders: bool,
    pub limit_offset_bps: Decimal,
}

impl Default for RolloverConfig {
    fn default() -> Self {
        Self {
            days_before_expiry: 14,
            max_concurrent_rollovers: 5,
            use_limit_orders: false,
            limit_offset_bps: Decimal::ZERO,
        }
    }
}
```

### RolloverPlan

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloverPlan {
    pub near_symbol: Symbol,
    pub far_symbol: Symbol,
    pub quantity: Quantity,
    pub side: OrderSide,
    pub expiry_date: NaiveDate,
    pub planned_date: NaiveDate,
}
```

### RolloverState

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RolloverState {
    Planned,
    ClosingNearMonth,
    NearMonthClosed,
    OpeningFarMonth,
    Complete,
    Failed,
}
```

Display: lowercase with underscores (`"closing_near_month"`, etc.)

### ActiveRollover

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveRollover {
    pub plan: RolloverPlan,
    pub state: RolloverState,
}
```

### RolloverMonitor

```rust
pub struct RolloverMonitor {
    config: RolloverConfig,
    active: HashMap<Symbol, ActiveRollover>,  // keyed by near_symbol
}
```

### New EngineEvent variants

```rust
RolloverTriggered(RolloverPlan),
RolloverCompleted(Symbol),           // near_symbol
RolloverFailed { near_symbol: Symbol, reason: SmolStr },
```

### New EngineError variants

```rust
RolloverFarMonthNotFound { near_symbol: Symbol },
RolloverLimitExceeded { active: usize, max: usize },
```

## Method Signatures

### RolloverMonitor

```rust
impl RolloverMonitor {
    pub fn new(config: RolloverConfig) -> Self

    /// Scan positions for instruments expiring within the rollover window.
    /// Returns plans for positions not already being rolled.
    pub fn scan_for_rollovers(
        &self,
        positions: &HashMap<Symbol, Position>,
        registry: &InstrumentRegistry,
        today: NaiveDate,
    ) -> Vec<RolloverPlan>

    /// Find the far month contract:
    /// - Future/Option: same underlying + same asset class + nearest expiry after near's
    /// - CryptoFuture: same exchange + same base/quote currency + nearest dated expiry after near's
    /// Returns Err(RolloverFarMonthNotFound) if no candidate found.
    pub fn find_far_month(
        near: &Instrument,
        registry: &InstrumentRegistry,
    ) -> Result<Symbol, EngineError>

    /// Generate close intention for the near month position.
    pub fn close_intention(
        plan: &RolloverPlan,
        strategy_id: &StrategyId,
        config: &RolloverConfig,
    ) -> OrderIntention

    /// Generate open intention for the far month position.
    pub fn open_intention(
        plan: &RolloverPlan,
        strategy_id: &StrategyId,
        config: &RolloverConfig,
    ) -> OrderIntention

    /// Start tracking a rollover (move to ClosingNearMonth).
    pub fn begin_rollover(&mut self, plan: RolloverPlan) -> Result<(), EngineError>

    /// Advance rollover state machine on fill events.
    /// Returns the new state if a tracked rollover advanced.
    pub fn on_fill(&mut self, fill: &OrderFill) -> Option<(Symbol, RolloverState)>

    /// Mark a rollover as failed and remove from active tracking.
    pub fn fail_rollover(&mut self, near_symbol: &Symbol, reason: &str)

    /// Read-only access to active rollovers.
    pub fn active_rollovers(&self) -> &HashMap<Symbol, ActiveRollover>

    /// Check if a symbol is currently being rolled.
    pub fn is_rolling(&self, symbol: &Symbol) -> bool
}
```

### Expiry extraction helper (private)

```rust
/// Extract expiry date from an instrument's details. Returns None for
/// equities, forex, crypto spot, perpetual crypto futures, and bonds.
fn expiry_date(instrument: &Instrument) -> Option<NaiveDate> {
    match &instrument.details {
        InstrumentDetails::Future { expiry, .. } => Some(*expiry),
        InstrumentDetails::Option { expiry, .. } => Some(*expiry),
        InstrumentDetails::CryptoFuture { expiry: Some(dt), .. } => Some(dt.date_naive()),
        _ => None,
    }
}

/// Extract the underlying symbol for matching far month contracts.
fn underlying_symbol(instrument: &Instrument) -> Option<&Symbol> {
    match &instrument.details {
        InstrumentDetails::Future { underlying, .. } => underlying.as_ref(),
        InstrumentDetails::Option { underlying, .. } => Some(underlying),
        _ => None,
    }
}
```

## Scan Logic

`scan_for_rollovers` iterates positions, looks up each in the registry, extracts expiry, checks if `expiry - today <= days_before_expiry`, skips already-rolling symbols, then calls `find_far_month` for each. Failed far-month lookups are silently skipped (logged at warn level).

## State Machine

```
Planned -> ClosingNearMonth -> NearMonthClosed -> OpeningFarMonth -> Complete
                 |                    |                   |
                 +--------------------+-------------------+--> Failed
```

`on_fill` logic:
- If `fill.symbol == plan.near_symbol` and state is `ClosingNearMonth` → advance to `NearMonthClosed`
- If `fill.symbol == plan.far_symbol` and state is `OpeningFarMonth` → advance to `Complete`, remove from active

## Config Changes (config.rs)

Add to `RiskConfig`:
```rust
pub rollover: Option<RolloverConfig>,
```

Update `Default` impl and existing tests.

## TDD Cycles (20 tests)

### Cycle 0: RolloverConfig (2 tests)

| # | Test | Verifies |
|---|------|----------|
| 1 | `test_rollover_config_serde_roundtrip` | JSON serialize/deserialize with all fields |
| 2 | `test_rollover_config_defaults_sensible` | Default: days=14, max=5, limit_orders=false, offset=0 |

### Cycle 1: RolloverState + RolloverPlan (2 tests)

| # | Test | Verifies |
|---|------|----------|
| 3 | `test_rollover_state_display_all_variants` | Display output for all 6 states |
| 4 | `test_rollover_plan_serde_roundtrip` | RolloverPlan JSON round-trip |

### Cycle 2: EngineEvent + EngineError variants (1 test)

| # | Test | Verifies |
|---|------|----------|
| 5 | `test_engine_event_rollover_variants` | Construction and pattern matching of RolloverTriggered/Completed/Failed |

### Cycle 3: expiry_date helper (implicit, tested via scan)

### Cycle 4: scan_for_rollovers (6 tests)

| # | Test | Verifies |
|---|------|----------|
| 6 | `test_scan_no_positions` | Empty positions → empty result |
| 7 | `test_scan_no_expiring` | Positions in equities (no expiry) → empty result |
| 8 | `test_scan_future_within_window` | Future expiring in 10 days (window=14) → plan generated |
| 9 | `test_scan_option_within_window` | Option expiring in 7 days → plan generated |
| 10 | `test_scan_equity_ignored` | Equity position skipped even if in registry |
| 11 | `test_scan_already_rolling` | Position already in active_rollovers → skipped |

### Cycle 5: find_far_month (2 tests)

| # | Test | Verifies |
|---|------|----------|
| 12 | `test_find_far_month_futures` | Given ESM26 (Jun), finds ESU26 (Sep) — nearest expiry after Jun |
| 13 | `test_find_far_month_not_found` | No matching far month → Err(RolloverFarMonthNotFound) |

### Cycle 6: Intention generation (4 tests)

| # | Test | Verifies |
|---|------|----------|
| 14 | `test_close_intention_sell_for_long` | Long position → close with Sell, near_symbol, Market order |
| 15 | `test_close_intention_buy_for_short` | Short position → close with Buy |
| 16 | `test_open_intention_buy_for_long` | Long rollover → open with Buy, far_symbol |
| 17 | `test_open_intention_sell_for_short` | Short rollover → open with Sell, far_symbol |

### Cycle 7: on_fill state machine (3 tests)

| # | Test | Verifies |
|---|------|----------|
| 18 | `test_on_fill_advances_close_to_near_month_closed` | Fill on near_symbol in ClosingNearMonth → NearMonthClosed |
| 19 | `test_on_fill_advances_open_to_complete` | Fill on far_symbol in OpeningFarMonth → Complete, removed from active |
| 20 | `test_on_fill_unrelated_ignored` | Fill on unrelated symbol → None returned |

### Cycle 8: Proptest (1 test)

| # | Test | Verifies |
|---|------|----------|
| 21 | `prop_test_scan_only_finds_within_window` | For random today + random expiries, scanned positions always have expiry within [today, today + days_before_expiry] |

### Cycle 9: RiskConfig integration

Update `test_risk_config_serde_roundtrip` and `test_risk_config_default` in config.rs to include `rollover` field.

### Cycle 10: lib.rs re-exports

## Test Helpers

```rust
fn sample_future(symbol_str: &str, expiry: NaiveDate, underlying: &str)
    -> Result<Instrument, Box<dyn std::error::Error>>

fn sample_option(symbol_str: &str, expiry: NaiveDate, underlying: &str)
    -> Result<Instrument, Box<dyn std::error::Error>>

fn sample_equity(symbol_str: &str)
    -> Result<Instrument, Box<dyn std::error::Error>>

fn sample_position(symbol_str: &str, side: OrderSide, qty: Decimal)
    -> Result<(Symbol, Position), Box<dyn std::error::Error>>

fn sample_fill(symbol_str: &str, side: OrderSide)
    -> Result<OrderFill, Box<dyn std::error::Error>>
```

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -p ingot-engine
cargo nextest run -p ingot-engine
cargo nextest run --workspace --exclude ingot-storage  # full regression
```
