# Implementation Plan: Phase 1d.6 — Scheduler (Interval-Based Per-Strategy Timers)

## Context

Phase 1d.5 delivered the `Engine<E, L>` orchestrator with a `tokio::select!` event loop processing tickers, order books, fills, and shutdown signals. The `Strategy` trait already defines `on_schedule` and `EngineEvent::ScheduleTrigger(StrategyId)` exists. Phase 1d.6 activates scheduling: per-strategy interval timers that fire `on_schedule` callbacks on configurable intervals.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Schedule registration | Separate `register_schedule()` method | Keeps `register_strategy` unchanged |
| Interval representation | `u64` milliseconds | Avoids `Duration` serde issues |
| Timer pattern | Spawned tasks + shared `mpsc::Sender<StrategyId>` | One new `select!` branch; independent tasks |
| First tick | Consume immediate first tick in timer task | Prevents spurious `on_schedule` at t=0 |
| Shutdown | `shutdown_rx.changed()` + abort `JoinHandle`s | Belt-and-suspenders cleanup |
| Test time control | `#[tokio::test(start_paused = true)]` | Deterministic; `test-util` feature |

## TDD Tests (8)

1. `test_engine_error_display_invalid_schedule_interval` — error Display
2. `test_schedule_config_construction` — new() + interval_duration() + zero rejection
3. `test_schedule_config_serde_roundtrip` — serialize/deserialize
4. `test_register_schedule_unknown_strategy` — rejects unregistered strategy
5. `test_register_schedule_duplicate` — rejects duplicate schedule
6. `test_scheduler_fires_at_interval` — advance 250ms, on_schedule_count >= 2
7. `test_scheduler_multiple_strategies_different_intervals` — independent firing
8. `test_scheduler_stops_on_shutdown` — timers stop, count stable

## Verification

```bash
export SQLX_OFFLINE=true
cargo fmt --all -- --check
cargo clippy --all-targets --workspace
cargo nextest run -p ingot-engine
cargo check --all-targets --workspace
cargo bench --no-run
```
