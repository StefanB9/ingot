# Open Issues

## 004 — GUI shows no NAV until first tick

**Severity:** Low
**Component:** `ingot-engine`, `ingot-gui`

The GUI starts with "Awaiting Server Data" for NAV because `broadcast_nav` only fires after ticks or fills. On startup (before any subscription produces a tick), the GUI receives nothing.

**Options:**
- GUI queries `IpcCommand::QueryNav` once on startup to seed `last_nav`
- Engine broadcasts an initial NAV when a broadcast subscriber connects

---

## 005 — No session tracking / exchange subscriptions are permanent

**Severity:** Medium
**Component:** `ingot-engine`, `ingot-connectivity`

When a GUI or CLI client disconnects, any exchange subscriptions it triggered remain active. The `Exchange` trait has `subscribe_ticker` but no `unsubscribe_ticker`. `PaperExchange` spawns a `JoinHandle` per symbol that runs indefinitely. There is no per-client session state — subscriptions are engine-global.

**Impact:** Ticker streams accumulate over the server lifetime. Reconnecting clients may trigger duplicate subscriptions (though `PaperExchange` deduplicates via `HashMap<Symbol, JoinHandle>`).

**Options:**
- Add `unsubscribe_ticker` to the `Exchange` trait
- Track subscriptions per client session with reference counting
- Accept engine-global subscriptions as intentional (simplest, reasonable for a single-user workstation)

---

## 006 — NAV calculation fails when ledger has non-USD accounts without market data

**Severity:** Medium
**Component:** `ingot-core`

`Ledger::net_asset_value()` iterates all asset/liability accounts. If any account holds a non-denomination currency (e.g. BTC) and the `QuoteBoard` has no rate for it (because BTC-USD is not subscribed), `convert()` returns `Err(CurrencyMismatch)` and the entire NAV computation fails. `broadcast_nav` logs a warning and skips the broadcast.

This means: after a BTC buy fill, if the server restarts without re-subscribing to BTC-USD, NAV is unavailable until subscription resumes.

**Options:**
- Skip accounts with missing rates and compute a partial NAV (with a warning flag)
- Auto-subscribe to all currencies present in the ledger on engine startup
- Require subscriptions before allowing trades (already partially addressed by fix #006 rejecting market orders without price data)
