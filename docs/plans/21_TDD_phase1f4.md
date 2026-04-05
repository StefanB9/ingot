# Technical Design Document: Phase 1f.4 — IBKR Client Portal REST: OrderExecutor

## 1. Context

Phase 1f.3 is complete (mapper layer + MarketDataProvider). Phase 1f.4 adds `OrderExecutor` trait implementation on `IbkrRestClient`. This requires new IBKR response models for the richer orders endpoint, reverse-mapper functions (IBKR strings → domain types), a `delete` HTTP method, and the confirmation-reply flow unique to IBKR order placement.

**Design decisions (from interactive planning):**
- **Sparse `get_order_status`**: Default to `OrderType::Market` / `TimeInForce::Day` when reconstructing `OrderRequest` from the sparse `/order/status/{id}` endpoint. `get_open_orders` uses the richer `/account/orders` endpoint with real values.
- **`sec_type` in orders**: Look up from registry via `instrument_for_conid` → `asset_class`, then reverse-map with `asset_class_to_sec_type`.
- **Order reply parsing**: Deserialize as `Vec<serde_json::Value>`, inspect for `"order_id"` (success) vs `"id"` (confirmation needed). More robust than untagged enum.

## 2. Files

### Modified Files (3)
- `crates/ingot-connectivity/src/ibkr/models.rs` — Add `IbkrLiveOrder`, `IbkrLiveOrdersResponse`, `IbkrOrderSubmitWrapper`
- `crates/ingot-connectivity/src/ibkr/mapper.rs` — Add status mapper, reverse mappers, order conversion functions
- `crates/ingot-connectivity/src/ibkr/rest.rs` — Add `delete` method, `impl OrderExecutor for IbkrRestClient`

## 3. Type Definitions

### 3.1 New models (models.rs)

#### IbkrLiveOrder — for GET /iserver/account/orders
```rust
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrLiveOrder {
    #[serde(rename = "orderId")]
    pub order_id: String,
    pub conid: i64,
    #[serde(rename = "orderType")]
    pub order_type: String,
    pub side: String,
    pub price: Option<f64>,
    #[serde(rename = "auxPrice")]
    pub aux_price: Option<f64>,
    pub quantity: f64,
    #[serde(rename = "filledQuantity")]
    pub filled_quantity: f64,
    #[serde(rename = "remainingQuantity")]
    pub remaining_quantity: f64,
    pub status: String,
    #[serde(rename = "timeInForce")]
    pub time_in_force: Option<String>,
    pub ticker: Option<String>,
}
```

#### IbkrLiveOrdersResponse
```rust
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrLiveOrdersResponse {
    pub orders: Vec<IbkrLiveOrder>,
}
```

#### IbkrOrderSubmitWrapper
```rust
#[derive(Debug, Serialize)]
pub(crate) struct IbkrOrderSubmitWrapper {
    pub orders: Vec<IbkrOrderRequest>,
}
```

### 3.2 New mapper functions (mapper.rs)

#### Status mapping
```rust
pub(crate) fn ibkr_status_to_order_status(status: &str) -> anyhow::Result<OrderStatus>
// "Submitted" → Open, "Filled" → Filled, "Cancelled" → Cancelled,
// "PreSubmitted" → Pending, "Inactive" → Rejected, unknown → Err
```

#### Asset class reverse mapping
```rust
pub(crate) fn asset_class_to_sec_type(asset_class: AssetClass) -> &'static str
// Equity → "STK", Option → "OPT", Future → "FUT", Forex → "CASH", Bond → "BOND"
// Crypto/Index → "STK" (fallback)
```

#### Reverse mappers (IBKR string → domain type)
```rust
pub(crate) fn ibkr_side_to_order_side(side: &str) -> anyhow::Result<OrderSide>
// "BUY"/"B" → Buy, "SELL"/"S" → Sell, unknown → Err

pub(crate) fn ibkr_order_type_from_str(s: &str) -> anyhow::Result<OrderType>
// "MKT" → Market, "LMT" → Limit, "STP" → StopLoss, "STP LMT" → StopLossLimit, unknown → Err

pub(crate) fn ibkr_tif_from_str(s: &str) -> anyhow::Result<TimeInForce>
// "GTC" → GoodTilCancelled, "IOC" → ImmediateOrCancel, "FOK" → FillOrKill, "DAY" → Day, unknown → Err
```

#### Order conversion: rich endpoint (get_open_orders)
```rust
pub(crate) fn ibkr_live_order_to_open_order(
    live: &IbkrLiveOrder,
    registry: &IbkrContractRegistry,
) -> anyhow::Result<OpenOrder>
```
Maps to `OpenOrder`:
- Symbol from `registry.symbol_for_conid(live.conid)`
- Side via `ibkr_side_to_order_side(live.side)`
- OrderType via `ibkr_order_type_from_str(live.order_type)`
- TimeInForce via `ibkr_tif_from_str(live.time_in_force.unwrap_or("DAY"))`
- `limit_price` from `live.price` (if LMT or STP LMT)
- `stop_price` from `live.aux_price` (if STP or STP LMT)
- `created_at` = `Utc::now()` (IBKR doesn't return creation timestamp)
- `average_fill_price` = None (not in this endpoint)

#### Order conversion: sparse endpoint (get_order_status)
```rust
pub(crate) fn ibkr_order_status_to_open_order(
    status: &IbkrOrderStatus,
    registry: &IbkrContractRegistry,
) -> anyhow::Result<OpenOrder>
```
Maps to `OpenOrder`:
- Defaults `OrderType::Market`, `TimeInForce::Day` (not available from this endpoint)
- Side via `ibkr_side_to_order_side(status.side)`
- `average_fill_price` from `avg_price` (if > 0 and filled_quantity > 0)
- `created_at` = `Utc::now()`

### 3.3 delete method (rest.rs)

```rust
pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T>
// Same 401 retry + rate limiting pattern as get/post
```

### 3.4 OrderExecutor impl (rest.rs)

```rust
impl OrderExecutor for IbkrRestClient {
    async fn place_order(&self, request: &OrderRequest) -> anyhow::Result<OrderId> {
        // 1. Look up conid from registry
        // 2. Look up instrument for sec_type via asset_class_to_sec_type
        // 3. Build IbkrOrderRequest + IbkrOrderSubmitWrapper
        // 4. POST /iserver/account/{id}/orders
        // 5. Parse Vec<serde_json::Value> reply:
        //    - "order_id" field → success, return OrderId
        //    - "id" field → POST /iserver/reply/{replyId} with {confirmed: true}
        //    - else → bail
    }

    async fn cancel_order(&self, order_id: &OrderId) -> anyhow::Result<()> {
        // DELETE /iserver/account/{id}/order/{orderId}
    }

    async fn cancel_all_orders(&self) -> anyhow::Result<u32> {
        // GET /iserver/account/orders → loop cancel each → return count
    }

    async fn get_order_status(&self, order_id: &OrderId) -> anyhow::Result<OpenOrder> {
        // GET /iserver/account/order/status/{orderId}
        // Map via ibkr_order_status_to_open_order (sparse, defaults Market/Day)
    }

    async fn get_open_orders(&self) -> anyhow::Result<Vec<OpenOrder>> {
        // GET /iserver/account/orders
        // Map each via ibkr_live_order_to_open_order (rich data)
    }
}
```

## 4. TDD Steps (16 tests)

### Mapper tests (6 in mapper.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 1 | `test_ibkr_status_to_order_status_submitted` | "Submitted" → Open |
| 2 | `test_ibkr_status_to_order_status_filled` | "Filled" → Filled |
| 3 | `test_ibkr_status_to_order_status_cancelled` | "Cancelled" → Cancelled |
| 4 | `test_ibkr_status_to_order_status_presubmitted` | "PreSubmitted" → Pending |
| 5 | `test_ibkr_status_to_order_status_inactive` | "Inactive" → Rejected |
| 6 | `test_ibkr_live_order_to_open_order` | Full IbkrLiveOrder → OpenOrder with correct symbol, side, order_type, tif, quantities |

### REST wiremock tests (10 in rest.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 7 | `test_place_order_market` | POST /iserver/account/{id}/orders → direct success → OrderId |
| 8 | `test_place_order_limit` | POST with LMT, price, GTC → OrderId |
| 9 | `test_place_order_with_confirmation` | First reply needs confirmation → auto POST /iserver/reply/{id} → OrderId |
| 10 | `test_place_order_rejected` | POST returns HTTP 400 → Err |
| 11 | `test_cancel_order` | DELETE /iserver/account/{id}/order/{orderId} → Ok(()) |
| 12 | `test_cancel_order_not_found` | DELETE returns HTTP 404 → Err |
| 13 | `test_cancel_all_orders` | GET open orders (2) → DELETE each → Ok(2) |
| 14 | `test_get_order_status` | GET /iserver/account/order/status/{id} → OpenOrder (sparse, defaults Market/Day) |
| 15 | `test_get_open_orders` | GET /iserver/account/orders → Vec<OpenOrder> with rich data |
| 16 | `test_get_open_orders_empty` | GET returns empty orders → Ok(vec![]) |

## 5. Implementation Order

### Step 1: New models (models.rs)
Add `IbkrLiveOrder`, `IbkrLiveOrdersResponse`, `IbkrOrderSubmitWrapper`. No new tests — validated implicitly by wiremock tests.

### Step 2: Status mapper (tests 1-5)
Add `ibkr_status_to_order_status`. Write 5 tests first.

### Step 3: Reverse mappers + asset_class_to_sec_type
Add `ibkr_side_to_order_side`, `ibkr_order_type_from_str`, `ibkr_tif_from_str`, `asset_class_to_sec_type`. Tested indirectly through test 6.

### Step 4: Order conversion mappers (test 6)
Add `ibkr_live_order_to_open_order` and `ibkr_order_status_to_open_order`. Write test 6.

### Step 5: delete method (rest.rs)
Add `delete<T>` with same 401 retry + rate limiting. Tested via test 11.

### Step 6: place_order (tests 7-10)
Write 4 wiremock tests. Implement place_order with confirmation-reply flow.

### Step 7: cancel_order (tests 11-12)
Write 2 wiremock tests. Implement cancel_order using delete method.

### Step 8: cancel_all_orders (test 13)
Write wiremock test. Implement via get_open_orders + loop cancel.

### Step 9: get_order_status + get_open_orders (tests 14-16)
Write 3 wiremock tests. Implement both methods.

### Step 10: Verify
```bash
SQLX_OFFLINE=true cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --all-targets --workspace
SQLX_OFFLINE=true cargo nextest run -p ingot-connectivity
SQLX_OFFLINE=true cargo check --all-targets --workspace
SQLX_OFFLINE=true cargo bench --no-run
```

## 6. Key Design Decisions

1. **Two models for order data**: `IbkrOrderStatus` (existing, sparse) for individual status queries, `IbkrLiveOrder` (new, rich) for listing open orders. Avoids polluting the existing model with many optional fields.
2. **`serde_json::Value` for reply parsing**: IBKR order placement returns different shapes (success vs confirmation needed). Inspecting `Value` is more robust than fragile untagged enum deserialization.
3. **`delete<T>` generic**: Follows the same pattern as `get<T>` and `post<T>` for consistency. Returns deserialized response for inspection if needed.
4. **`cancel_all_orders` via loop**: GET all open orders, DELETE each individually. If one fails mid-loop, error propagates. Matches the Kraken implementation pattern.
5. **`created_at = Utc::now()`**: Neither IBKR orders endpoint provides a creation timestamp in a parseable format. Using current time as approximation.
6. **`sec_type` from registry**: Look up the instrument's `asset_class` from the contract registry, reverse-map to IBKR sec_type string. Correctly handles futures, options, forex orders — not just equities.

## 7. Key Files (reference)
- `crates/ingot-connectivity/src/ibkr/models.rs` — IBKR response/request types
- `crates/ingot-connectivity/src/ibkr/mapper.rs` — All conversion functions (existing order_side/type/tif + new reverse mappers)
- `crates/ingot-connectivity/src/ibkr/rest.rs` — IbkrRestClient with get/post/delete + trait impls
- `crates/ingot-connectivity/src/ibkr/contract_registry.rs` — conid ↔ symbol ↔ instrument lookup
- `crates/ingot-connectivity/src/traits.rs` — OrderExecutor trait definition
- `crates/ingot-core/src/order.rs` — OrderId, OrderRequest, OpenOrder, OrderStatus
- `crates/ingot-primitives/src/enums.rs` — OrderSide, OrderType, TimeInForce, AssetClass
