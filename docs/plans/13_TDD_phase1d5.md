# Implementation Plan: Phase 1d.5 — Engine Orchestrator

## Context

Phases 1d.1–1d.4 delivered all engine building blocks: types, config, error, LedgerWriter trait, Strategy trait + NoopStrategy, PortfolioController, and OrderManager. Phase 1d.5 wires them together into the **Engine** — the central event-driven runtime that consumes market data channels, dispatches events to strategies, gates intentions through risk checks, submits orders, and posts fills to the ledger.

## Design Decisions (user-confirmed)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| `post_fill` params | Add `exchange: Exchange` + `venue: SmolStr` to `EngineConfig` | Single config source of truth |
| Test strategy | `MockStrategy` in test module (`#[cfg(test)]`) | No test code in production |
| Integration test | Include PaperExchange test in 1d.5 | Proves orchestrator end-to-end |
| Shutdown | `tokio::sync::watch<bool>` channel | Simple, clonable handle; full kill switch deferred to 1d.7 |
| Symbol→currency mapping | `register_symbol(symbol, base_currency, quote_currency)` | Needed for `post_fill`; skip accounting if symbol not registered (log warn) |
| `is_short` | Hardcoded `false` | Spot-only for MVP |
| MockStrategy location | `StrategyKind::Mock` variant behind `#[cfg(test)]` in `strategy.rs` | Allows tests to use enum dispatch |

## Files to Modify

| File | Change |
|------|--------|
| `crates/ingot-engine/src/config.rs` | Add `exchange: Exchange`, `venue: SmolStr` to `EngineConfig`; update serde test |
| `crates/ingot-engine/src/controller.rs` | Add `pub fn positions(&self) -> &HashMap<Symbol, Position>` accessor |
| `crates/ingot-engine/src/strategy.rs` | Add `#[cfg(test)] MockStrategy` + `#[cfg(test)] StrategyKind::Mock` variant + delegation arms |
| `crates/ingot-engine/src/lib.rs` | Add `pub mod engine;`, `pub use engine::Engine;`, remove `#[allow(dead_code)]` |

## New Files

| File | Contents |
|------|----------|
| `crates/ingot-engine/src/engine.rs` | `Engine<E, L>` struct, event loop, all handlers, 10 tests |

## Type Definition

```rust
pub struct Engine<E, L> {
    strategies: Vec<StrategyKind>,
    controller: PortfolioController,
    order_manager: OrderManager,
    executor: E,
    ledger_writer: L,
    config: EngineConfig,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    latest_tickers: HashMap<Symbol, TickerSnapshot>,
    latest_order_books: HashMap<Symbol, OrderBookSnapshot>,
    symbol_currencies: HashMap<Symbol, (Currency, Currency)>,
}
```

## EngineConfig Changes

```rust
pub struct EngineConfig {
    pub risk: RiskConfig,
    pub base_currency: Currency,
    pub smart_order: SmartOrderConfig,
    pub exchange: Exchange,      // NEW
    pub venue: SmolStr,          // NEW — e.g., "spot"
}
```

## Event Loop (`run` method)

```rust
// Init all strategies → handle_ticker/book/fill in tokio::select! → shutdown via watch
```

## Handler Logic

- `handle_ticker`: cache → build context → strategies.on_ticker → process_intention
- `handle_order_book`: cache → build context → strategies.on_order_book → process_intention
- `handle_fill`: order_manager.on_fill → controller.on_fill → post_fill accounting → strategies.on_fill → process_intention
- `process_intention`: controller.check_intention → if Approved: order_manager.submit_order
- `build_context`: positions from controller, tickers/books from cache, empty balances

## Test Summary (10 tests)

| # | Test | Type | Validates |
|---|------|------|-----------|
| 1 | `test_engine_new` | sync | Engine constructs with valid config |
| 2 | `test_register_strategy` | sync | NoopStrategy registered successfully |
| 3 | `test_register_duplicate_strategy_rejected` | sync | Same StrategyId → DuplicateStrategyId error |
| 4 | `test_engine_processes_ticker` | async | Ticker via channel → MockStrategy.on_ticker called |
| 5 | `test_engine_processes_order_book` | async | OrderBook via channel → MockStrategy.on_order_book called |
| 6 | `test_engine_processes_fill` | async | Fill → controller updated, ledger write called |
| 7 | `test_engine_intention_approved_and_submitted` | async | MockStrategy emits intention → approved → executor called |
| 8 | `test_engine_intention_rejected` | async | Large intention → rejected → executor NOT called |
| 9 | `test_engine_shutdown_signal` | async | Shutdown watch → loop exits cleanly |
| 10 | `test_engine_end_to_end_with_paper_exchange` | async | PaperExchange + MockLedgerWriter, full flow |

## Verification

```bash
export SQLX_OFFLINE=true
cargo fmt --all -- --check
cargo clippy --all-targets --workspace        # zero warnings
cargo nextest run -p ingot-engine             # 10 new + 62 existing = ~72 tests
cargo check --all-targets --workspace
cargo bench --no-run
```
