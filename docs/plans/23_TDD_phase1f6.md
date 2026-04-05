# Technical Design Document: Phase 1f.6 — TWS Socket: Connection, Auth, Message Protocol

## 1. Context

Phase 1f.5 is complete (AccountProvider + Margin). Phase 1f.6 adds the IBKR TWS binary socket protocol layer: a codec for length-prefixed null-separated messages, an enum for incoming message types with parsing, and a connection scaffold with typestate pattern. This is the foundation for Phase 1f.7 (StreamProvider).

**Design decisions (from interactive planning):**
- **Codec implementation**: Use `tokio-util` (codec feature) + `bytes` crate for standard Tokio Decoder/Encoder traits.
- **Decode output**: `TwsRawMessage { msg_id: i32, fields: Vec<String> }` — codec handles framing + field splitting, parser handles semantics.
- **Typestate naming**: `IbkrTws<S>` following Kraken's `KrakenSpotWs<S>` / `KrakenFuturesWs<S>` pattern.

## 2. Files

### New Files (3)
- `crates/ingot-connectivity/src/ibkr/tws_codec.rs` — `TwsCodec` (Decoder/Encoder), `TwsRawMessage`
- `crates/ingot-connectivity/src/ibkr/tws_models.rs` — `TwsIncoming` enum with `parse()`, message ID constants
- `crates/ingot-connectivity/src/ibkr/tws.rs` — `IbkrTws<S>` typestate scaffold (Disconnected/Connected)

### Modified Files
- `crates/ingot-connectivity/src/ibkr/mod.rs` — Add 3 new modules
- `crates/ingot-connectivity/Cargo.toml` — Add `tokio-util`, `bytes` dependencies
- `Cargo.toml` (workspace root) — Add `tokio-util`, `bytes` to `[workspace.dependencies]`

## 3. Type Definitions

### 3.1 TwsCodec (tws_codec.rs)

#### TwsRawMessage
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TwsRawMessage {
    pub msg_id: i32,
    pub fields: Vec<String>,
}
```

#### TwsCodec
```rust
#[derive(Debug, Default)]
pub(crate) struct TwsCodec;

impl Decoder for TwsCodec {
    type Item = TwsRawMessage;
    type Error = anyhow::Error;
    // Wire: [4-byte BE u32 length][field0\0field1\0...fieldN\0]
    // Returns Ok(None) if insufficient bytes
}

impl Encoder<Vec<String>> for TwsCodec {
    type Error = anyhow::Error;
    // Joins fields with null separator, trailing null, 4-byte BE length prefix
}
```

### 3.2 TwsIncoming (tws_models.rs)

```rust
pub(crate) const MSG_TICK_PRICE: i32 = 1;
pub(crate) const MSG_TICK_SIZE: i32 = 2;
pub(crate) const MSG_ORDER_STATUS: i32 = 3;
pub(crate) const MSG_ERR_MSG: i32 = 4;
pub(crate) const MSG_NEXT_VALID_ID: i32 = 9;
pub(crate) const MSG_EXECUTION_DATA: i32 = 11;
pub(crate) const MSG_MARKET_DEPTH: i32 = 12;
pub(crate) const MSG_HEARTBEAT: i32 = 49;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TwsIncoming {
    NextValidId { order_id: i32 },
    TickPrice { req_id: i32, tick_type: i32, price: f64, size: f64 },
    TickSize { req_id: i32, tick_type: i32, size: f64 },
    OrderStatus { order_id: i32, status: String, filled: f64, remaining: f64, avg_fill_price: f64 },
    ExecutionData { req_id: i32, order_id: i32, conid: i64, side: String, shares: f64, price: f64, exec_id: String, time: String },
    MarketDepth { req_id: i32, position: i32, operation: i32, side: i32, price: f64, size: f64 },
    ErrorMessage { id: i32, code: i32, message: String },
    Heartbeat,
}
```

Parse via `TwsIncoming::parse(raw: &TwsRawMessage) -> anyhow::Result<Self>`, matching on `raw.msg_id`. Each branch skips the version field (fields[0]) and extracts subsequent fields by index.

### 3.3 IbkrTws<S> (tws.rs)

```rust
pub struct Disconnected;
pub struct Connected;

pub(crate) struct IbkrTws<S = Disconnected> {
    config: IbkrConfig,
    _state: PhantomData<S>,
}

impl IbkrTws<Disconnected> {
    pub fn new(config: IbkrConfig) -> Self { ... }
    pub fn config(&self) -> &IbkrConfig { ... }
}
```

## 4. TDD Steps (15 tests)

### Codec tests (5 in tws_codec.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 1 | `test_encode_single_field` | Single field → correct length prefix + null-terminated bytes |
| 2 | `test_encode_multiple_fields` | Multiple fields → null-separated with correct length |
| 3 | `test_decode_roundtrip` | Encode → decode produces same msg_id + fields |
| 4 | `test_decode_incomplete` | Partial buffer → Ok(None), no data consumed |
| 5 | `test_decode_malformed_msg_id` | Non-numeric first field → Err |

### Message parsing tests (8 in tws_models.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 6 | `test_parse_next_valid_id` | msg_id=9 → NextValidId { order_id: 42 } |
| 7 | `test_parse_tick_price` | msg_id=1 → TickPrice with correct fields |
| 8 | `test_parse_tick_size` | msg_id=2 → TickSize with correct fields |
| 9 | `test_parse_order_status` | msg_id=3 → OrderStatus with status, filled, remaining |
| 10 | `test_parse_execution_data` | msg_id=11 → ExecutionData with all fields |
| 11 | `test_parse_market_depth` | msg_id=12 → MarketDepth with all fields |
| 12 | `test_parse_error_message` | msg_id=4 → ErrorMessage with id, code, message |
| 13 | `test_parse_unknown_msg_id` | msg_id=999 → Err |

### Scaffold test (1 in tws.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 14 | `test_tws_disconnected_state` | IbkrTws::new() → config accessible |

### Property test (1 in tws_codec.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 15 | `prop_test_codec_roundtrip` | For any msg_id and string fields, encode→decode roundtrips |

## 5. Implementation Order

### Step 1: Add workspace dependencies
### Step 2: TwsCodec + tests 1-5, 15
### Step 3: TwsIncoming + tests 6-13
### Step 4: IbkrTws scaffold + test 14
### Step 5: Wire up modules
### Step 6: Verify

## 6. TWS Protocol Reference

### Wire Format
- Length prefix: 4-byte big-endian u32
- Payload: null-separated UTF-8 string fields
- First field: message type ID (integer as string)
- Second field (most messages): version number

### Message Field Layouts
- **NextValidId (9)**: [version, order_id]
- **TickPrice (1)**: [version, req_id, tick_type, price, size, attribs]
- **TickSize (2)**: [version, req_id, tick_type, size]
- **OrderStatus (3)**: [version, order_id, status, filled, remaining, avg_fill_price, ...]
- **ExecutionData (11)**: [version, req_id, order_id, conid, symbol, sec_type, ..., exec_id, time, ..., side, shares, price, ...]
- **MarketDepth (12)**: [version, req_id, position, operation, side, price, size]
- **ErrorMessage (4)**: [version, id, code, message, ...]
- **Heartbeat (49)**: [version]

## 7. Key Files (reference)
- `crates/ingot-connectivity/src/ibkr/error.rs` — IbkrError::TwsConnection, TwsDecode
- `crates/ingot-connectivity/src/kraken/spot/ws.rs` — Reference: typestate pattern
- `crates/ingot-connectivity/src/traits.rs` — StreamProvider trait (target for 1f.7)
