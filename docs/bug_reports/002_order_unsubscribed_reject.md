# Bug Fix: Order Returned as "Filled" for Unsubscribed Symbol

## Context

When an order is placed for a symbol that is not subscribed (no market data), `PaperExchange::place_order` returns `OrderStatus::Filled` unconditionally. The engine sends this ack to the client, then calls `record_fill()` which fails silently because `QuoteBoard.convert()` has no rate for the symbol. The client sees "filled" but the ledger is never updated.

**Root cause:** No pre-validation in `handle_order_request`. The ack is sent before `record_fill` completes, and `record_fill` errors are logged but not propagated.

**Branch:** `fix/order-unsubscribed-reject`
**Base:** `dev`

---

## Tasks

### 1. Add market data pre-check in `handle_order_request`

**File:** `crates/ingot-engine/src/lib.rs`

Before calling `self.exchange.place_order(order)`, check that market data exists for the symbol when the order has no explicit price (market order). If the QuoteBoard has no rate, return an error through the ack channel instead of proceeding.

```rust
// At the start of handle_order_request, before place_order:
if order.price.is_none() {
    let Some((base_code, quote_code)) = order.symbol.parts() else {
        // reject — invalid symbol
    };
    let base = Currency::from_code(base_code);
    let quote = Currency::from_code(quote_code);
    let one_base = Money::new(Amount::from(dec!(1)), base);
    let board = self.market_data.read().await;
    if board.convert(&one_base, &quote).is_err() {
        warn!(symbol = %order.symbol, "rejecting market order: no price data");
        if let Some(tx) = ack_tx {
            let _ = tx.send(OrderAcknowledgment {
                exchange_id: String::new(),
                client_id: None,
                timestamp: Utc::now(),
                status: OrderStatus::Rejected,
            });
        }
        return;
    }
}
```

**Dep:** `OrderStatus::Rejected` — check if this variant exists already. If not, add it to `ingot-primitives`.

### 2. Add `OrderStatus::Rejected` variant (if needed)

**File:** `crates/ingot-primitives/src/lib.rs` (or wherever `OrderStatus` is defined)

Add `Rejected` to the enum if it doesn't exist. Update Display, Serialize/Deserialize, and any `#[repr(C)]` attributes.

### 3. Add unit test for market order rejection

**File:** `crates/ingot-engine/tests/strategy_integration.rs` (or inline tests)

Test `test_engine_rejects_market_order_without_price_data`:
1. Create engine with paper exchange, no subscriptions
2. Send a market order (no price) for BTC-USD
3. Verify the ack has `OrderStatus::Rejected`
4. Verify the ledger is unchanged

Test `test_engine_accepts_limit_order_without_subscription`:
1. Create engine with paper exchange, no subscriptions
2. Send a limit order (explicit price) for BTC-USD
3. Verify the ack has `OrderStatus::Filled`

---

## File Change Summary

| File | Action |
|------|--------|
| `crates/ingot-primitives/src/lib.rs` | Add `Rejected` variant to `OrderStatus` (if missing) |
| `crates/ingot-engine/src/lib.rs` | Pre-check market data in `handle_order_request` |
| `crates/ingot-engine/tests/strategy_integration.rs` | 2 new tests: rejection + limit order passthrough |

---

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --workspace
cargo nextest run --workspace
cargo check --all-targets --workspace
cargo bench --no-run
```

Key checks:
- Market order for unsubscribed symbol returns `Rejected` (not `Filled`)
- Limit order with explicit price still works regardless of subscription
- Existing order tests still pass
