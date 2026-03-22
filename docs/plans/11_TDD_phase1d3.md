# Implementation Plan: Phase 1d.3 — PortfolioController (Risk Gatekeeper)

## Context

Phases 1d.1–1d.2 delivered the engine crate scaffold (types, config, error, `LedgerWriter` trait) and the strategy abstraction (`Strategy` trait, `StrategyContext`, `NoopStrategy`, `StrategyKind`). Phase 1d.3 introduces the **PortfolioController** — a stateful risk gatekeeper that maintains running position state, tracks NAV, computes per-symbol exposure, and gates every `OrderIntention` against configured risk limits before it reaches the broker.

## Design Decisions (user-confirmed)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Exposure scoping | Per-asset (symbol-level) | `max_asset_exposure` checks per-symbol value / NAV. `max_currency_exposure` unused this phase. |
| Market order pricing | Use latest ticker (ask for buys, bid for sells) | Passed as `&HashMap<Symbol, TickerSnapshot>` to `check_intention()`. Reject if no ticker. |
| Order value denomination | Always in base_currency (USD) | All current pairs are *USD, no cross-rate needed yet. |
| Leverage | Spot-only | Position value = quantity × price. No margin accounting. |
| `check_intention` signature | `(&mut self, ..., tickers: &HashMap<Symbol, TickerSnapshot>)` | Tickers passed externally; controller doesn't manage market data state. `&mut` because NAV check may auto-halt. |
| Exposure ratio math | `position_value.value() / nav.value()` raw Decimal | No `Amount / Amount` operator exists; drop to `Decimal` for division. |
| Struct visibility | `pub(crate)` struct, `pub` methods | Controller is internal to the engine; engine exposes its own API. |

## New Files

| File | Contents |
|------|----------|
| `crates/ingot-engine/src/controller.rs` | `PortfolioController` struct + impl + 12 tests (2 proptest) |

## Modified Files

| File | Change |
|------|--------|
| `crates/ingot-engine/src/lib.rs` | Add `pub(crate) mod controller;` |

## `check_intention()` Logic (fail-fast)

1. If halted → Rejected("controller is halted")
2. If NAV ≤ global_stop_loss → auto-halt + Rejected
3. Resolve order price (limit_price or ticker ask/bid)
4. order_notional = price × quantity
5. If notional > max_order_value → Rejected
6. Projected exposure = (current_position_value ± order_notional) / NAV
7. If projected > max_asset_exposure → Rejected
8. → Approved

## Test Summary (12 tests)

| # | Test | Validates |
|---|------|-----------|
| 1 | `test_controller_new_default_state` | Empty positions, NAV=0, not halted |
| 2 | `test_check_intention_approved_within_limits` | Market buy via ticker → Approved |
| 3 | `test_check_intention_rejected_halted` | Halted → Rejected |
| 4 | `test_check_intention_rejected_nav_below_stop_loss` | NAV below threshold → Rejected + auto-halt |
| 5 | `test_check_intention_rejected_exposure_limit` | 26.8% > 20% max → Rejected |
| 6 | `test_check_intention_rejected_max_order_value` | 67k > 50k max → Rejected |
| 7 | `test_on_fill_updates_positions` | Cumulative position tracking affects risk checks |
| 8 | `test_on_nav_update` | NAV update + auto-halt below stop-loss |
| 9 | `test_on_position_update` | Bulk sync from broker affects risk checks |
| 10 | `test_halt_and_is_halted` | Manual halt toggle |
| 11 | `prop_test_approved_orders_within_limits` | 1000 random valid orders → all Approved |
| 12 | `prop_test_exposure_never_exceeds_limit` | 1000 random fill sequences → exposure bounded |

## Verification

```bash
export SQLX_OFFLINE=true
cargo fmt --all -- --check
cargo clippy --all-targets --workspace        # zero warnings
cargo nextest run -p ingot-engine             # 12 new + 39 existing = 51 tests
cargo check --all-targets --workspace
cargo bench --no-run
```
