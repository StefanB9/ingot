# Bug Fix: GUI NAV Always Shows "Awaiting Server Data"

## Context

The GUI subscribes to the NAV pub/sub IPC stream and displays `last_nav` when it receives an `IpcEvent::Nav`. However, the engine never broadcasts `WsMessage::Nav` events — it only broadcasts `Tick` and `OrderUpdate`. All the IPC infrastructure (types, publisher handler, GUI subscriber) is fully wired; the engine simply never emits the event.

The `IpcCommand::QueryNav` request/response path works correctly for on-demand queries, but the GUI relies on the pub/sub stream, not polling.

**Root cause:** `TradingEngine::handle_tick()` and `record_fill()` broadcast ticks and order updates respectively, but neither computes or broadcasts NAV.

**Branch:** `fix/nav-broadcast`
**Base:** `dev`

---

## Tasks

### 1. Add `broadcast_nav` helper to `TradingEngine`

**File:** `crates/ingot-engine/src/lib.rs`

Add a private method that computes NAV in USD and broadcasts it. Reuses the existing `Ledger::net_asset_value()` and the tick broadcast pattern.

```rust
async fn broadcast_nav(&self) {
    let Some(ref tx) = self.event_tx else { return };
    let ledger = self.ledger.read().await;
    let board = self.market_data.read().await;
    match ledger.net_asset_value(&board, &Currency::usd()) {
        Ok(nav) => {
            let _ = tx.send(WsMessage::Nav(NavUpdate {
                amount: nav.amount,
                currency: nav.currency,
            }));
        }
        Err(e) => {
            warn!(error = %e, "failed to compute NAV for broadcast");
        }
    }
}
```

### 2. Call `broadcast_nav` after tick processing

**File:** `crates/ingot-engine/src/lib.rs`, `handle_tick()`

After the existing tick broadcast (after the `if let Some(tx) = &self.event_tx` block that sends `WsMessage::Tick`), call `self.broadcast_nav().await`. This ensures NAV updates flow to the GUI on every price change.

**Note:** The `market_data` write lock from strategy dispatch is already released at this point, so taking read locks for NAV is safe.

### 3. Call `broadcast_nav` after order fill recording

**File:** `crates/ingot-engine/src/lib.rs`, `record_fill()`

After a successful `post_order_fill` (in the `Ok(transaction)` arm), call `self.broadcast_nav().await`. This ensures NAV reflects fills immediately.

### 4. Add imports

**File:** `crates/ingot-engine/src/lib.rs`

Add `NavUpdate` to the existing `ingot_core::api` import (alongside `TickerUpdate`, `WsMessage`).

### 5. Add test for NAV broadcast

**File:** `crates/ingot-engine/tests/strategy_integration.rs`

Test `test_engine_broadcasts_nav_on_tick`:
1. Create engine with broadcast channel, subscribe to receiver
2. Send a tick
3. Verify receiver gets both `WsMessage::Tick` and `WsMessage::Nav`
4. Verify the NAV amount matches expected value (10000 USD seed)

---

## File Change Summary

| File | Action |
|------|--------|
| `crates/ingot-engine/src/lib.rs` | Add `broadcast_nav()` helper, call from `handle_tick` and `record_fill`, add `NavUpdate` import |
| `crates/ingot-engine/tests/strategy_integration.rs` | New test: `test_engine_broadcasts_nav_on_tick` |

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
- `test_engine_broadcasts_nav_on_tick` passes
- GUI receives NAV updates via the existing pub/sub stream
- No performance regression (NAV calc is read-only, low overhead at 1-5 ticks/s)
