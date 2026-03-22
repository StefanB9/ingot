# Implementation Plan: Phase 1d.7 — Kill Switch + Graceful Shutdown

## Context

Phase 1d.6 delivered per-strategy interval timers. The engine's `run()` loop now handles tickers, order books, fills, schedule triggers, and graceful shutdown via `watch<bool>`. However, the engine lacks an **emergency shutdown** mechanism. `EngineEvent::KillSwitch` and `EngineError::KillSwitchActivated` already exist as unused stubs. Phase 1d.7 activates the kill switch: an emergency sequence that halts the controller, shuts down strategies, cancels all open orders, closes all positions, and exits. It also wires OS signal handling (Ctrl+C) into the engine loop.

## Design Decisions (user-confirmed)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Kill switch struct | Separate `KillSwitch` in `kill_switch.rs` | Distinct from graceful shutdown; own watch channel |
| Signal handling | Inside `run()` as `select!` branch | Engine owns full lifecycle; `tokio::signal::ctrl_c()` polled directly |
| `OrderSide::opposite()` | Add to `ingot-primitives` | Reusable, self-documenting, testable |
| Position close errors | Best-effort + `error!` log | Emergency mechanism does as much as possible; never blocks on one failure |
| Kill switch method | `execute_kill_switch_sequence()` on `Engine` | Needs access to controller, strategies, order_manager, executor — free function would have unwieldy param count |
| `run()` return on kill | `Err(EngineError::KillSwitchActivated)` | Signals caller that exit was emergency, not graceful |

## Architecture

```
Engine::run()
  ├── tokio::select! loop:
  │     ├── ticker_rx.recv()       (existing)
  │     ├── book_rx.recv()         (existing)
  │     ├── fill_rx.recv()         (existing)
  │     ├── schedule_rx.recv()     (existing)
  │     ├── kill_switch_rx.changed() => execute_kill_switch_sequence() → break  ← NEW
  │     ├── ctrl_c() => kill_switch.activate()                                  ← NEW
  │     └── shutdown_rx.changed()  (existing)
  └── Shutdown: abort timers, shutdown strategies (existing)
```

Kill switch sequence (`execute_kill_switch_sequence`):
```
1. controller.halt()               — prevent new orders
2. strategy.shutdown() for all     — stop strategy logic
3. order_manager.cancel_all()      — cancel open orders (log error if fails)
4. For each position:              — close with opposite-side market order
   └── executor.place_order(close_order)  (log error if fails, continue)
```

## Files to Modify/Create

| File | Change |
|------|--------|
| `crates/ingot-primitives/src/enums.rs` | Add `OrderSide::opposite()` + test |
| `crates/ingot-engine/src/kill_switch.rs` | **NEW** — `KillSwitch` struct with `activate()`, `is_activated()`, `subscribe()` |
| `crates/ingot-engine/src/engine.rs` | Add `kill_switch` field, `kill_switch()` accessor, `execute_kill_switch_sequence()`, two new `select!` branches, enhance `MockOrderExecutor` with `cancel_all_count` |
| `crates/ingot-engine/src/lib.rs` | Add `pub mod kill_switch;`, re-export `KillSwitch` |
| `Cargo.toml` (workspace root) | Add `"signal"` to tokio workspace features |

## New/Modified Types

### `KillSwitch` (`kill_switch.rs`)

```rust
pub struct KillSwitch {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
}

impl KillSwitch {
    pub fn new() -> Self;
    pub fn activate(&self);           // tx.send(true)
    pub fn is_activated(&self) -> bool; // *rx.borrow()
    pub fn subscribe(&self) -> watch::Receiver<bool>; // rx.clone()
}
```

### `OrderSide::opposite()` (`ingot-primitives/src/enums.rs`)

```rust
impl OrderSide {
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
}
```

### Engine modifications (`engine.rs`)

```rust
// New field
pub struct Engine<E, L> {
    // ... existing ...
    kill_switch: KillSwitch,
}

// New public accessor
pub fn kill_switch(&self) -> &KillSwitch;

// New private method — best-effort, logs errors
async fn execute_kill_switch_sequence(&mut self) -> Result<(), EngineError>;
```

### MockOrderExecutor enhancement (test-only)

```rust
struct MockOrderExecutor {
    placed: Arc<Mutex<Vec<OrderRequest>>>,
    cancel_all_count: Arc<Mutex<u32>>,  // NEW
    next_order_id: String,
}
```

## TDD Step Order (12 tests)

| # | Test | File | Type | Validates |
|---|------|------|------|-----------|
| 1 | `test_order_side_opposite` | primitives/enums.rs | sync | `Buy.opposite() == Sell`, `Sell.opposite() == Buy` |
| 2 | `test_kill_switch_initially_inactive` | kill_switch.rs | sync | `is_activated()` returns false after `new()` |
| 3 | `test_kill_switch_activate` | kill_switch.rs | sync | `activate()` → `is_activated()` returns true |
| 4 | `test_kill_switch_subscribe_receives_signal` | kill_switch.rs | async | `subscribe()` receiver notified on `activate()` |
| 5 | `test_engine_kill_switch_accessor` | engine.rs | sync | `engine.kill_switch().is_activated()` is false |
| 6 | `test_engine_kill_switch_halts_controller` | engine.rs | async | Kill switch → controller is halted after exit |
| 7 | `test_engine_kill_switch_shuts_down_strategies` | engine.rs | async | Kill switch → `MockStrategyState.shutdown_called` is true |
| 8 | `test_engine_kill_switch_cancels_all_orders` | engine.rs | async | Kill switch → `cancel_all_count > 0` on mock executor |
| 9 | `test_engine_kill_switch_closes_positions` | engine.rs | async | Buy position → Sell Market close order placed |
| 10 | `test_engine_kill_switch_closes_sell_positions` | engine.rs | async | Sell position → Buy Market close order placed |
| 11 | `test_engine_kill_switch_no_positions_no_close_orders` | engine.rs | async | No positions → only cancel, no close orders |
| 12 | `test_engine_kill_switch_returns_error` | engine.rs | async | `run()` returns `Err(KillSwitchActivated)` |

## Implementation Sequence

| Step | Action | Files |
|------|--------|-------|
| 1 | Add `OrderSide::opposite()` + test 1 | `ingot-primitives/src/enums.rs` |
| 2 | Add `"signal"` to tokio workspace features | `Cargo.toml` (root) |
| 3 | Create `KillSwitch` struct + tests 2–4 | `kill_switch.rs`, `lib.rs` |
| 4 | Add `kill_switch` field to `Engine`, accessor + test 5 | `engine.rs` |
| 5 | Enhance `MockOrderExecutor` with `cancel_all_count` | `engine.rs` (test module) |
| 6 | Implement `execute_kill_switch_sequence()` + tests 6–11 | `engine.rs` |
| 7 | Wire kill switch + Ctrl+C `select!` branches + test 12 | `engine.rs` |
| 8 | Add re-exports | `lib.rs` |

## Key Implementation Details

### Seeding controller positions for tests
The controller's `on_fill()` method creates/updates positions. To seed a position for kill switch tests, call `controller.on_fill()` with a synthetic fill before starting the engine. Alternatively, use `controller.on_position_update()` for bulk sync. Since controller is private (`pub(crate)`), tests in `engine.rs` can access it through `engine.controller`.

### Borrow checker in `execute_kill_switch_sequence`
Iterating `self.controller.positions()` while calling `self.executor.place_order()` requires cloning positions first:
```rust
let positions: Vec<Position> = self.controller.positions().values().cloned().collect();
for position in &positions {
    let close_order = OrderRequest { side: position.side.opposite(), ... };
    if let Err(e) = self.executor.place_order(&close_order).await {
        error!("failed to close position {}: {e}", position.symbol);
    }
}
```

### Ctrl+C branch
```rust
_ = tokio::signal::ctrl_c() => {
    warn!("Ctrl+C received, activating kill switch");
    self.kill_switch.activate();
    // Kill switch branch fires on next select! iteration
}
```

## Verification

```bash
export SQLX_OFFLINE=true
cargo fmt --all -- --check
cargo clippy --all-targets --workspace
cargo nextest run --workspace              # includes ingot-primitives test
cargo check --all-targets --workspace
cargo bench --no-run
```
