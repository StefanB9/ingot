# Implementation Plan: Phase 1f.7 — TWS Socket: StreamProvider

## Context
Phase 1f.6 is complete (TwsCodec, TwsIncoming, IbkrTws scaffold). Phase 1f.7 adds the full StreamProvider implementation for the IBKR TWS binary socket: TCP connection with handshake, single read loop dispatching to broadcast channels, market data/depth subscriptions via TWS request messages, execution report forwarding, and an order book manager. Follows the Kraken `KrakenSpotWs<S>` pattern adapted for TWS's single-socket binary protocol. Also moves OrderBookManager to a shared module for reuse.

## Design Decisions
1. **Single read loop**: TWS uses one TCP socket for all data. One read loop dispatches `TwsIncoming` variants to appropriate broadcast channels.
2. **OrderBookManager**: Move existing `kraken/book_manager.rs` to `connectivity/src/book_manager.rs` for reuse by both Kraken and IBKR.
3. **Full TWS handshake**: `connect()` sends `"API\0"` + `"v100..176\0"` + `START_API` message, waits for `NextValidId` to confirm connection. Mock TCP server in tests replicates this handshake.
4. **TwsInner with Arc**: Following Kraken pattern — `Arc<TwsInner>` holds `AtomicI32` for `next_req_id`, `Mutex<HashMap<i32, Symbol>>` for req_id→symbol tracking, `Arc<Mutex<IbkrContractRegistry>>` for conid resolution, broadcast channels, and shutdown watch.

## Files

### Moved Files (1)
- `crates/ingot-connectivity/src/kraken/book_manager.rs` → `crates/ingot-connectivity/src/book_manager.rs`

### Modified Files (4)
- `crates/ingot-connectivity/src/ibkr/tws.rs` — Full `connect()`, `disconnect()`, `StreamProvider` impl, `TwsInner`, read loop, dispatch, TWS subscription messages
- `crates/ingot-connectivity/src/ibkr/mod.rs` — (no changes needed, tws already registered)
- `crates/ingot-connectivity/src/lib.rs` — Add `pub(crate) mod book_manager;` at top level
- `crates/ingot-connectivity/src/kraken/spot/ws.rs` — Update import from `super::book_manager` → `crate::book_manager`

## Type Definitions

### TwsInner (in tws.rs)

```rust
use std::sync::atomic::AtomicI32;

struct TwsInner {
    // Broadcast channels (same capacities as Kraken)
    trade_tx: broadcast::Sender<Tick>,
    ticker_tx: broadcast::Sender<TickerSnapshot>,
    book_tx: broadcast::Sender<OrderBookSnapshot>,
    exec_tx: broadcast::Sender<OrderFill>,

    // Shutdown
    shutdown_tx: watch::Sender<bool>,
    read_task: Mutex<Option<JoinHandle<()>>>,

    // Write half of the Framed stream for sending subscription messages
    write_tx: mpsc::Sender<Vec<String>>,

    // Request ID management
    next_req_id: AtomicI32,
    req_id_to_symbol: Mutex<HashMap<i32, Symbol>>,

    // Ticker state accumulator: req_id → partial snapshot fields
    ticker_state: Mutex<HashMap<i32, PartialTicker>>,

    // Order book manager (shared module)
    book_manager: Mutex<OrderBookManager>,

    // Contract registry for conid → Symbol on execution reports
    registry: Arc<Mutex<IbkrContractRegistry>>,

    // Config
    config: IbkrConfig,
}
```

### PartialTicker (in tws.rs)

```rust
/// Accumulates tick updates into a full TickerSnapshot.
/// Emits to ticker_tx whenever any field updates.
struct PartialTicker {
    symbol: Symbol,
    bid: Option<Price>,
    ask: Option<Price>,
    last: Option<Price>,
    volume: Option<Quantity>,
}

impl PartialTicker {
    fn try_snapshot(&self) -> Option<TickerSnapshot> {
        // Returns Some if bid, ask, and last are all present
    }
}
```

### TWS Tick Type Constants

```rust
const TICK_BID: i32 = 1;
const TICK_ASK: i32 = 2;
const TICK_LAST: i32 = 4;
const TICK_VOLUME: i32 = 8;  // size type
```

### TWS Outgoing Message IDs

```rust
const REQ_MKT_DATA: i32 = 1;
const CANCEL_MKT_DATA: i32 = 2;
const REQ_MKT_DEPTH: i32 = 10;
const CANCEL_MKT_DEPTH: i32 = 11;
const START_API: i32 = 71;
```

## Architecture

### connect() Flow
1. Open `TcpStream` to `config.tws_host:config.tws_port`
2. Send raw handshake bytes: `"API\0"` + `"v100..176\0"` (version range)
3. Wrap stream in `Framed<TcpStream, TwsCodec>`
4. Wait for first message — must be `NextValidId { order_id }`, sets `next_req_id` starting value
5. Split framed stream: read half → read loop, write half → `mpsc::Sender<Vec<String>>`
6. Spawn read loop task
7. Spawn write loop task (reads from `mpsc::Receiver`, encodes and sends)
8. Send `START_API` message: `["71", "2", "{client_id}", ""]`
9. Return `IbkrTws<Connected>`

### Read Loop
Single `tokio::spawn` task:
```
loop {
    tokio::select! {
        msg = framed_read.next() => {
            match TwsIncoming::parse(&msg) {
                TickPrice { req_id, tick_type, price, size } => dispatch_tick(...)
                TickSize { req_id, tick_type, size } => dispatch_tick_size(...)
                MarketDepth { req_id, ... } => dispatch_depth(...)
                ExecutionData { ... } => dispatch_execution(...)
                OrderStatus { ... } => // logged, not dispatched (future use)
                ErrorMessage { ... } => // logged at warn level
                Heartbeat => // trace log
                NextValidId { order_id } => // update next_req_id
            }
        }
        _ = shutdown_rx.changed() => break
    }
}
```

### Dispatch Logic

**dispatch_tick** (TickPrice):
- Look up `req_id_to_symbol[req_id]` → symbol
- If `tick_type == TICK_LAST(4)`: emit `Tick` to `trade_tx`
- If `tick_type ∈ {TICK_BID(1), TICK_ASK(2), TICK_LAST(4)}`: update `PartialTicker`, emit `TickerSnapshot` to `ticker_tx` if complete

**dispatch_tick_size** (TickSize):
- If `tick_type == TICK_VOLUME(8)`: update `PartialTicker.volume`, emit if complete

**dispatch_depth** (MarketDepth):
- Look up symbol from `req_id_to_symbol`
- `operation`: 0=insert, 1=update, 2=delete. `side`: 0=ask, 1=bid
- Apply to `book_manager`, emit snapshot to `book_tx`

**dispatch_execution** (ExecutionData):
- Look up symbol from `registry.conid_to_symbol(conid)`
- Map `side` ("BOT"→Buy, "SLD"→Sell), convert price/shares to `Decimal`
- Emit `OrderFill` to `exec_tx`

### StreamProvider Implementation

**subscribe_trades(symbols)**:
- For each symbol: look up conid via registry, allocate req_id, send REQ_MKT_DATA, track req_id→symbol
- Return `trade_tx.subscribe()`

**subscribe_ticker(symbols)**:
- Same as subscribe_trades (both use REQ_MKT_DATA), but if already subscribed for a symbol, skip duplicate request
- Initialize `PartialTicker` entry for each new req_id
- Return `ticker_tx.subscribe()`

**subscribe_order_book(symbols, depth)**:
- For each symbol: look up conid, allocate req_id, send REQ_MKT_DEPTH with depth, track req_id→symbol
- Return `book_tx.subscribe()`

**subscribe_executions()**:
- No explicit TWS subscription needed — execution reports arrive automatically
- Bridge `exec_tx.subscribe()` → `mpsc::Receiver<OrderFill>` (same as Kraken pattern)

### REQ_MKT_DATA Message Format
```
["1", "11", "{req_id}", "{conid}", "", "STK", "", "0", "", "SMART", "", "USD", "", "0", ""]
```
Fields: [msg_id, version, req_id, conid, symbol, sec_type, expiry, strike, right, exchange, multiplier, currency, local_symbol, generic_tick_list, snapshot]

### REQ_MKT_DEPTH Message Format
```
["10", "5", "{req_id}", "{conid}", "", "STK", "", "0", "", "SMART", "", "USD", "", "{depth}", "0", ""]
```

## TDD Steps (11 tests)

### Shared module test (1 in book_manager.rs after move)

| # | Test Name | Verifies |
|---|-----------|----------|
| 1 | `test_book_manager_still_works_after_move` | Existing Kraken book tests pass from new location (verify via `cargo nextest run`) |

### Connection tests (2 in tws.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 2 | `test_tws_connect_handshake` | connect() sends API+version+START_API, receives NextValidId, returns Connected state |
| 3 | `test_tws_connect_and_disconnect` | connect() → disconnect() → clean shutdown, tasks joined |

### StreamProvider subscribe tests (4 in tws.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 4 | `test_subscribe_trades_returns_receiver` | subscribe_trades sends REQ_MKT_DATA, returns broadcast::Receiver |
| 5 | `test_subscribe_ticker_returns_receiver` | subscribe_ticker sends REQ_MKT_DATA, returns broadcast::Receiver |
| 6 | `test_subscribe_order_book_returns_receiver` | subscribe_order_book sends REQ_MKT_DEPTH, returns broadcast::Receiver |
| 7 | `test_subscribe_executions_returns_receiver` | subscribe_executions returns mpsc::Receiver (no outgoing message) |

### Dispatch tests (3 in tws.rs, using mock TCP server)

| # | Test Name | Verifies |
|---|-----------|----------|
| 8 | `test_tick_price_dispatches_to_trade` | Mock sends TickPrice(LAST) → Tick received on trade_rx |
| 9 | `test_tick_price_dispatches_to_ticker` | Mock sends TickPrice(BID)+TickPrice(ASK)+TickPrice(LAST) → TickerSnapshot on ticker_rx |
| 10 | `test_market_depth_builds_order_book` | Mock sends MarketDepth inserts → OrderBookSnapshot on book_rx |

### Error handling test (1 in tws.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 11 | `test_error_message_logged_not_panicked` | Mock sends ErrorMessage → no panic, trade still dispatched after |

## Mock TCP Server Pattern

```rust
/// Creates a mock TWS server that performs the handshake and sends predefined messages.
async fn mock_tws_server(
    messages: Vec<TwsRawMessage>,
    registry_entries: Vec<(i64, Symbol)>,
) -> anyhow::Result<(String, u16, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut framed = Framed::new(stream, TwsCodec);

        // 1. Read handshake: "API\0" + version (raw bytes, not framed)
        // (skip reading raw bytes for simplicity — just read first framed message)

        // 2. Send NextValidId { order_id: 1 }
        framed.send(vec!["9".into(), "1".into(), "1".into()]).await.unwrap();

        // 3. Read START_API message from client
        let _ = framed.next().await;

        // 4. Read subscription requests, then send test messages
        for _ in 0..messages.len() {
            let _ = tokio::time::timeout(Duration::from_millis(200), framed.next()).await;
        }
        for msg in messages {
            // Encode as raw fields: [msg_id_str, field0, field1, ...]
            let mut fields = vec![msg.msg_id.to_string()];
            fields.extend(msg.fields);
            framed.send(fields).await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    Ok(("127.0.0.1".into(), port, task))
}
```

Note: The handshake bytes ("API\0" + version) are raw (not length-prefixed). `connect()` sends them before wrapping in `Framed`. The mock server needs to handle this — it reads the raw handshake bytes first, then switches to `Framed` mode for subsequent messages.

## Implementation Order

### Step 1: Move OrderBookManager to shared location
- Move `crates/ingot-connectivity/src/kraken/book_manager.rs` → `crates/ingot-connectivity/src/book_manager.rs`
- Add `pub(crate) mod book_manager;` to `src/lib.rs`
- Update `kraken/spot/ws.rs` import: `use crate::book_manager::OrderBookManager;`
- Remove `pub(crate) mod book_manager;` from `kraken/mod.rs`
- Verify existing Kraken tests pass

### Step 2: Handshake + connect/disconnect + tests 2-3
- Add `TwsInner`, broadcast channels, `PartialTicker`, TWS constants
- Implement `connect()`: TCP connect → raw handshake → Framed → read NextValidId → spawn read/write loops → START_API → return Connected
- Implement `disconnect()`: shutdown signal, await task with timeout
- Create `mock_tws_server` test helper
- Write tests 2-3

### Step 3: StreamProvider subscribe methods + tests 4-7
- Implement `subscribe_trades`, `subscribe_ticker`, `subscribe_order_book`, `subscribe_executions`
- REQ_MKT_DATA and REQ_MKT_DEPTH message builders
- Write tests 4-7

### Step 4: Dispatch logic + tests 8-11
- Implement `dispatch_tick`, `dispatch_tick_size`, `dispatch_depth`, `dispatch_execution` in read loop
- PartialTicker accumulation logic
- OrderBookManager integration for depth
- Write tests 8-11

### Step 5: Verify
```bash
SQLX_OFFLINE=true cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --all-targets --workspace
SQLX_OFFLINE=true cargo nextest run -p ingot-connectivity
SQLX_OFFLINE=true cargo check --all-targets --workspace
SQLX_OFFLINE=true cargo bench --no-run
```

## Key Files (reference)
- `crates/ingot-connectivity/src/ibkr/tws.rs` — MODIFIED: full StreamProvider, TwsInner, read loop
- `crates/ingot-connectivity/src/book_manager.rs` — MOVED from kraken/
- `crates/ingot-connectivity/src/ibkr/tws_codec.rs` — Existing: TwsCodec, TwsRawMessage (from 1f.6)
- `crates/ingot-connectivity/src/ibkr/tws_models.rs` — Existing: TwsIncoming, message constants (from 1f.6)
- `crates/ingot-connectivity/src/ibkr/contract_registry.rs` — Existing: conid↔Symbol mapping
- `crates/ingot-connectivity/src/ibkr/error.rs` — Existing: IbkrError::TwsConnection, TwsDecode
- `crates/ingot-connectivity/src/kraken/spot/ws.rs` — Reference: Kraken WS pattern (import update needed)
- `crates/ingot-connectivity/src/traits.rs` — StreamProvider trait definition
