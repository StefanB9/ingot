# Technical Design Document: Phase 1f.5 — IBKR Client Portal REST: AccountProvider + Margin

## 1. Context

Phase 1f.4 is complete (OrderExecutor). Phase 1f.5 adds `AccountProvider` trait implementation on `IbkrRestClient` and a new `MarginSnapshot` type with computed margin metrics. This requires a new `IbkrTrade` response model, mapper functions for balances/positions/trades/margin, and the `MarginSnapshot` struct with methods.

**Design decisions (from interactive planning):**
- **Balance mapping**: `cash_balance` → total, `settled_cash` → available, `held = total - available`. If `settled_cash` is None, available = total, held = 0.
- **`available_margin()`**: Returns `excess_liquidity` (standard IBKR margin cushion, consistent with `is_margin_call()` check).
- **Trade history `since`**: Fetch all from `/iserver/account/trades`, filter client-side where `trade_time >= since`. No IBKR-specific param hacking.

## 2. Files

### New File (1)
- `crates/ingot-connectivity/src/ibkr/margin.rs` — `MarginSnapshot` struct + methods

### Modified Files (5)
- `crates/ingot-connectivity/src/ibkr/models.rs` — Add `IbkrTrade`
- `crates/ingot-connectivity/src/ibkr/mapper.rs` — Add balance/position/trade/margin mapper functions
- `crates/ingot-connectivity/src/ibkr/rest.rs` — Add `impl AccountProvider for IbkrRestClient` + `get_margin` method
- `crates/ingot-connectivity/src/ibkr/mod.rs` — Add `pub(crate) mod margin;`
- `crates/ingot-connectivity/Cargo.toml` — Add `proptest` dev-dependency

## 3. Type Definitions

### 3.1 New model (models.rs)

#### IbkrTrade — for GET /iserver/account/trades
```rust
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct IbkrTrade {
    pub execution_id: String,
    pub conid: i64,
    pub side: String,
    pub size: f64,
    pub price: f64,
    pub commission: Option<f64>,
    pub currency: String,
    pub trade_time: String,       // "YYYYMMDD-HH:MM:SS"
    #[serde(rename = "order_ref")]
    pub order_ref: Option<String>,
}
```

### 3.2 MarginSnapshot (margin.rs)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MarginSnapshot {
    pub account_id: String,
    pub initial_margin: Amount,
    pub maintenance_margin: Amount,
    pub excess_liquidity: Amount,
    pub buying_power: Amount,
    pub sma: Option<Amount>,
    pub available_funds: Amount,
    pub net_liquidation: Amount,
    pub timestamp: DateTime<Utc>,
}

impl MarginSnapshot {
    /// Margin utilization ratio: initial_margin / net_liquidation
    /// Returns Percentage (0..1). Returns 0% if net_liquidation is zero.
    pub fn utilization(&self) -> anyhow::Result<Percentage>

    /// True when excess_liquidity <= 0 (margin call territory)
    pub fn is_margin_call(&self) -> bool

    /// Available margin = excess_liquidity
    pub fn available_margin(&self) -> Amount
}
```

### 3.3 New mapper functions (mapper.rs)

#### Balance mapping
```rust
pub(crate) fn ibkr_balance_to_balance(currency_key: &str, bal: &IbkrAccountBalance) -> anyhow::Result<Balance>
// currency via Currency::from_str_lossy(currency_key)
// total = cash_balance.unwrap_or(0.0)
// available = settled_cash.unwrap_or(total)
// held = total - available
```

#### Position mapping
```rust
pub(crate) fn ibkr_position_to_position(
    pos: &IbkrPosition,
    registry: &IbkrContractRegistry,
) -> anyhow::Result<Position>
// symbol from registry.symbol_for_conid(pos.conid)
// side: position > 0 → Buy, < 0 → Sell
// quantity: abs(position) as Quantity
// average_entry_price: avg_cost as Price
// unrealized_pnl: Some(Amount) from unrealized_pnl
// liquidation_price: None
```

#### Trade mapping
```rust
pub(crate) fn ibkr_trade_to_order_fill(
    trade: &IbkrTrade,
    registry: &IbkrContractRegistry,
) -> anyhow::Result<OrderFill>
// order_id: from order_ref if present, else execution_id
// symbol from registry.symbol_for_conid(trade.conid)
// side via ibkr_side_to_order_side(trade.side)
// fill_price, fill_quantity, fee, fee_currency, timestamp, trade_id
```

#### Margin mapping
```rust
pub(crate) fn ibkr_margin_to_snapshot(
    account_id: &str,
    info: &IbkrMarginInfo,
) -> anyhow::Result<MarginSnapshot>
// Extract amount from each Option<IbkrAmountField>, default to 0 if None
// sma: remains Option (None if field missing)
// timestamp: Utc::now()
```

### 3.4 AccountProvider impl (rest.rs)

```rust
impl AccountProvider for IbkrRestClient {
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        // GET /portfolio/{accountId}/ledger → HashMap<String, IbkrAccountBalance>
        // Map each entry, skip zero balances
    }

    async fn get_positions(&self) -> anyhow::Result<Vec<Position>> {
        // GET /portfolio/{accountId}/positions/0 → Vec<IbkrPosition>
        // Map each via ibkr_position_to_position
    }

    async fn get_trade_history(&self, since: Option<DateTime<Utc>>) -> anyhow::Result<Vec<OrderFill>> {
        // GET /iserver/account/trades → Vec<IbkrTrade>
        // Map each, filter by since if provided
    }
}

impl IbkrRestClient {
    pub async fn get_margin(&self) -> anyhow::Result<MarginSnapshot> {
        // GET /portfolio/{accountId}/summary → IbkrMarginInfo
        // Map via ibkr_margin_to_snapshot
    }
}
```

## 4. TDD Steps (15 tests)

### Mapper tests (4 in mapper.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 1 | `test_ibkr_position_to_position_long` | Positive position → Buy side, correct quantity/price/pnl |
| 2 | `test_ibkr_position_to_position_short` | Negative position → Sell side, abs quantity |
| 3 | `test_ibkr_balance_to_balance` | cash_balance → total, settled_cash → available, held = diff |
| 4 | `test_ibkr_margin_to_snapshot` | IbkrMarginInfo → MarginSnapshot with all fields |

### MarginSnapshot tests (5 in margin.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 5 | `test_margin_utilization` | initial_margin=50k, net_liq=100k → 50% utilization |
| 6 | `test_margin_is_margin_call_true` | excess_liquidity = -100 → true |
| 7 | `test_margin_is_margin_call_false` | excess_liquidity = 5000 → false |
| 8 | `test_margin_available_margin` | excess_liquidity = 25000 → Amount(25000) |
| 9 | `test_margin_serde_roundtrip` | Serialize → deserialize → equal |

### REST wiremock tests (5 in rest.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 10 | `test_get_balances` | GET /portfolio/{id}/ledger → Vec<Balance> with correct mapping |
| 11 | `test_get_positions` | GET /portfolio/{id}/positions/0 → Vec<Position> with symbol lookup |
| 12 | `test_get_positions_empty` | GET returns [] → Ok(vec![]) |
| 13 | `test_get_trade_history` | GET /iserver/account/trades → Vec<OrderFill> with correct fields |
| 14 | `test_get_margin` | GET /portfolio/{id}/summary → MarginSnapshot |

### Property test (1 in margin.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 15 | `prop_test_margin_utilization_bounded` | For any positive init_margin and net_liq, utilization is in [0, 1] |

## 5. Implementation Order

### Step 1: IbkrTrade model (models.rs)
Add `IbkrTrade` struct. No tests — validated by wiremock tests later.

### Step 2: Mapper functions (tests 1-4)
Add `ibkr_balance_to_balance`, `ibkr_position_to_position`, `ibkr_trade_to_order_fill`, `ibkr_margin_to_snapshot`. Write 4 tests.

### Step 3: MarginSnapshot struct + methods (tests 5-9, 15)
Create `ibkr/margin.rs`, add to `mod.rs`. Implement `MarginSnapshot` with `utilization()`, `is_margin_call()`, `available_margin()`. Write 5 unit tests + 1 proptest. Add proptest dev-dependency to Cargo.toml.

### Step 4: AccountProvider impl (tests 10-13)
Implement `get_balances`, `get_positions`, `get_trade_history` on `IbkrRestClient`. Write 4 wiremock tests.

### Step 5: get_margin method (test 14)
Implement `get_margin` on `IbkrRestClient`. Write 1 wiremock test.

### Step 6: Verify
```bash
SQLX_OFFLINE=true cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --all-targets --workspace
SQLX_OFFLINE=true cargo nextest run -p ingot-connectivity
SQLX_OFFLINE=true cargo check --all-targets --workspace
SQLX_OFFLINE=true cargo bench --no-run
```

## 6. Key Design Decisions

1. **Balance field mapping**: `cash_balance` → total, `settled_cash` → available, `held = total - available`. More accurate than Kraken's `available = total` pattern since IBKR distinguishes settled vs unsettled cash.
2. **`available_margin()` = `excess_liquidity`**: Consistent with `is_margin_call()` using the same field. Excess liquidity is the standard IBKR margin cushion metric.
3. **Client-side trade filtering**: IBKR's `/iserver/account/trades` has no `since` query param. Fetch all (~7 days), filter in-memory. Simple and correct.
4. **`IbkrTrade` model**: New response struct for trades endpoint. Maps to `OrderFill` with `order_ref` as order_id fallback to `execution_id`.
5. **MarginSnapshot as `pub(crate)`**: Internal to connectivity crate. If needed externally later, can be promoted.
6. **Proptest for utilization**: Validates that for any positive margin/liquidation values, utilization stays bounded in [0, 1].

## 7. Key Files (reference)
- `crates/ingot-connectivity/src/ibkr/models.rs` — IBKR response/request types
- `crates/ingot-connectivity/src/ibkr/mapper.rs` — All conversion functions
- `crates/ingot-connectivity/src/ibkr/rest.rs` — IbkrRestClient with trait impls
- `crates/ingot-connectivity/src/ibkr/margin.rs` — NEW: MarginSnapshot
- `crates/ingot-connectivity/src/ibkr/contract_registry.rs` — conid ↔ symbol lookup
- `crates/ingot-connectivity/src/traits.rs` — AccountProvider trait definition
- `crates/ingot-core/src/balance.rs` — Balance type
- `crates/ingot-core/src/position.rs` — Position type
- `crates/ingot-core/src/order.rs` — OrderFill type
- `crates/ingot-primitives/src/newtypes.rs` — Amount, Price, Quantity, Percentage
