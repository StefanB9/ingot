# Implementation Plan: Phase 1d.4 — OrderManager + Smart Limit Orders

## Context

Phases 1d.1–1d.3 delivered the engine scaffold, strategy abstraction, and PortfolioController. Phase 1d.4 introduces the **OrderManager** — it bridges risk-approved `OrderIntention`s to the broker via `OrderExecutor`, tracks order lifecycle, computes smart limit prices from the order book, and detects stale orders for fallback.

The fill → accounting flow (`post_fill` → `LedgerWriter`) is handled by the Engine orchestrator (1d.5), NOT by OrderManager. OrderManager just tracks and submits orders.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Smart price math | `(bid.value() + ask.value()) / Decimal::TWO` | No `Price + Price` operator; drop to `Decimal` for mid-price. |
| Offset direction | Buy: `mid + offset`, Sell: `mid - offset` | Positive offset = more aggressive. |
| Smart trigger | Only converts `Market` orders when `use_mid_price && book.is_some()` | Explicit Limit orders pass through unchanged. |
| `on_fill` return | `Option<&TrackedOrder>` | `None` for untracked fills. |
| `stale_orders` | Returns `Vec<OrderId>`, caller acts | Engine orchestrator decides fallback. |
| Mock executor | `Arc<std::sync::Mutex<Vec<...>>>` in tests | OK in tests; no `.unwrap()`. |
| Struct visibility | `pub(crate)` | Internal to engine. |

## New Files

| File | Contents |
|------|----------|
| `crates/ingot-engine/src/order_manager.rs` | `OrderManager`, `TrackedOrder`, mock executor, 11 tests |

## Modified Files

| File | Change |
|------|--------|
| `crates/ingot-engine/src/lib.rs` | Add `pub(crate) mod order_manager;` |

## Test Summary (11 tests)

| # | Test | Validates |
|---|------|-----------|
| 1 | `test_compute_limit_price_buy_mid` | Mid-price = (67000+67010)/2 = 67005 |
| 2 | `test_compute_limit_price_sell_mid` | Same mid for sell (offset=0) |
| 3 | `test_compute_limit_price_with_offset` | 10 bps offset applied correctly |
| 4 | `test_compute_limit_price_empty_book_error` | Empty book → EmptyOrderBook error |
| 5 | `test_submit_order_market` | Market order passed through unchanged |
| 6 | `test_submit_order_smart_limit` | Market → Limit with mid-price |
| 7 | `test_on_fill_updates_tracked_order` | Fill sets status to Filled |
| 8 | `test_on_fill_unknown_order_ignored` | Unknown fill → None |
| 9 | `test_cancel_all_cancels_tracked` | Executor called, statuses → Cancelled |
| 10 | `test_stale_orders_identifies_expired` | Expired orders detected |
| 11 | `prop_test_mid_price_between_bid_ask` | Mid always between bid/ask (1000 cases) |

## Verification

```bash
export SQLX_OFFLINE=true
cargo fmt --all -- --check
cargo clippy --all-targets --workspace        # zero warnings
cargo nextest run -p ingot-engine             # 11 new + 51 existing = 62 tests
cargo check --all-targets --workspace
cargo bench --no-run
```
