# Implementation Plan: Phase 1d.2 — Strategy Trait, StrategyContext, NoopStrategy, StrategyKind

## Context

Phase 1d.1 established the `ingot-engine` crate with foundational types (`StrategyId`, `OrderIntention`, `RiskDecision`, `EngineEvent`), config, error, and `LedgerWriter` trait. Phase 1d.2 introduces the **strategy abstraction** — the trait that all trading strategies implement, the context object they receive, a no-op test strategy, and the enum dispatch wrapper.

## What Already Exists

- `StrategyId`, `OrderIntention` — `ingot-engine/src/types.rs`
- `EngineError` — `ingot-engine/src/error.rs`
- `Position` (Clone, not Copy) — `ingot-core/src/position.rs`
- `Balance` (Clone, not Copy) — `ingot-core/src/balance.rs`
- `TickerSnapshot`, `OrderBookSnapshot` (Clone) — `ingot-core/src/market_data.rs`
- `OrderFill`, `OrderId` — `ingot-core/src/order.rs`
- `Symbol` (Clone, Hash, Eq — **not Copy**) — `ingot-primitives/src/symbol.rs`

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Trait methods | Synchronous (`&mut self` → `Vec<OrderIntention>`) | Strategies are pure decision logic, no I/O; engine handles async |
| `StrategyContext` | Owned `Vec`/`HashMap` fields, `#[derive(Debug, Clone)]` | Snapshot semantics; engine builds context before passing to strategy |
| `StrategyContext` not Serialize | No `Serialize`/`Deserialize` | Internal transient snapshot, never persisted (matches `EngineEvent` pattern) |
| `latest_ticker()` signature | Takes `&Symbol` not `Symbol` | `Symbol` is **not** `Copy`; taking by ref avoids forcing callers to clone |
| `NoopStrategy` visibility | `pub` | Needed by integration tests and external test harnesses |
| `StrategyKind` derives | `Debug` only (not Clone) | Future strategy variants may not be Clone |
| Extra test for `on_order_book` | Yes — 9 tests total | TDD plan listed 8 but omitted `on_order_book` coverage for NoopStrategy |

## New Files

| File | Contents |
|------|----------|
| `crates/ingot-engine/src/strategy.rs` | `StrategyContext`, `Strategy` trait, `NoopStrategy`, `StrategyKind` + 9 tests |

## Modified Files

| File | Change |
|------|--------|
| `crates/ingot-engine/src/lib.rs` | Add `pub mod strategy;` + re-exports |

## Type Definitions

### `StrategyContext`

```rust
#[derive(Debug, Clone)]
pub struct StrategyContext {
    pub positions: Vec<Position>,
    pub balances: Vec<Balance>,
    pub latest_tickers: HashMap<Symbol, TickerSnapshot>,
    pub latest_order_books: HashMap<Symbol, OrderBookSnapshot>,
    pub timestamp: DateTime<Utc>,
}

impl StrategyContext {
    pub fn latest_ticker(&self, symbol: &Symbol) -> Option<&TickerSnapshot> {
        self.latest_tickers.get(symbol)
    }

    pub fn latest_order_book(&self, symbol: &Symbol) -> Option<&OrderBookSnapshot> {
        self.latest_order_books.get(symbol)
    }
}
```

### `Strategy` trait

```rust
pub trait Strategy {
    fn id(&self) -> &StrategyId;
    fn init(&mut self, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_ticker(&mut self, ticker: &TickerSnapshot, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_order_book(&mut self, book: &OrderBookSnapshot, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_fill(&mut self, fill: &OrderFill, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_schedule(&mut self, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn shutdown(&mut self);
}
```

### `NoopStrategy`

```rust
#[derive(Debug, Clone)]
pub struct NoopStrategy {
    id: StrategyId,
}

impl NoopStrategy {
    pub fn new(id: StrategyId) -> Self { Self { id } }
}

impl Strategy for NoopStrategy {
    // All event methods return Vec::new(), shutdown is no-op
}
```

### `StrategyKind`

```rust
#[derive(Debug)]
pub enum StrategyKind {
    Noop(NoopStrategy),
    // Future variants added here
}

impl Strategy for StrategyKind {
    // Delegates every method via match on self
}
```

### `lib.rs` additions

```rust
pub mod strategy;

pub use strategy::{NoopStrategy, Strategy, StrategyContext, StrategyKind};
```

## TDD Step Order

### Step 1: StrategyContext construction
**Red:** `test_strategy_context_construction` — build with positions, balances, tickers; assert field lengths.
**Green:** Define `StrategyContext` struct with `#[derive(Debug, Clone)]`.

### Step 2: StrategyContext lookup
**Red:** `test_strategy_context_latest_ticker_lookup` — `latest_ticker(&symbol)` returns `Some` for known, `None` for unknown.
**Green:** Implement `latest_ticker()` and `latest_order_book()` helper methods.

### Step 3: NoopStrategy identity
**Red:** `test_noop_strategy_id` — `NoopStrategy::new(id).id()` returns correct StrategyId.
**Green:** Define `Strategy` trait, `NoopStrategy` struct, implement `Strategy for NoopStrategy`.

### Step 4–8: NoopStrategy event methods
Each verifies the method returns an empty Vec:
- `test_noop_strategy_init_empty`
- `test_noop_strategy_on_ticker_empty`
- `test_noop_strategy_on_order_book_empty`
- `test_noop_strategy_on_fill_empty`
- `test_noop_strategy_on_schedule_empty`

These are all green from Step 3 implementation. Written as verification tests.

### Step 9: StrategyKind delegation
**Red:** `test_strategy_kind_delegates_to_noop` — exercises all 7 `Strategy` methods through `StrategyKind::Noop`, verifying delegation.
**Green:** Define `StrategyKind` enum + `Strategy for StrategyKind` impl with match delegation.

### Step 10: lib.rs re-exports + verification
- Add `pub mod strategy;` and `pub use` re-exports to `lib.rs`
- Run full verification suite

## Test Summary (9 tests)

| # | Test | Validates |
|---|------|-----------|
| 1 | `test_strategy_context_construction` | StrategyContext fields populated correctly |
| 2 | `test_strategy_context_latest_ticker_lookup` | `latest_ticker()` returns Some/None correctly |
| 3 | `test_noop_strategy_id` | NoopStrategy returns constructed StrategyId |
| 4 | `test_noop_strategy_init_empty` | `init()` returns empty Vec |
| 5 | `test_noop_strategy_on_ticker_empty` | `on_ticker()` returns empty Vec |
| 6 | `test_noop_strategy_on_order_book_empty` | `on_order_book()` returns empty Vec |
| 7 | `test_noop_strategy_on_fill_empty` | `on_fill()` returns empty Vec |
| 8 | `test_noop_strategy_on_schedule_empty` | `on_schedule()` returns empty Vec |
| 9 | `test_strategy_kind_delegates_to_noop` | All 7 Strategy methods delegate through StrategyKind::Noop |

## Verification

```bash
export SQLX_OFFLINE=true
cargo fmt --all -- --check
cargo clippy --all-targets --workspace        # zero warnings
cargo nextest run -p ingot-engine             # 9 new + 30 existing = 39 tests
cargo check --all-targets --workspace
cargo bench --no-run
```
