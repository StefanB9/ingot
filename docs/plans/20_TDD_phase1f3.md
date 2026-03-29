# Technical Design Document: Phase 1f.3 — IBKR Client Portal REST: MarketDataProvider + Mapper

## 1. Context

Phase 1f.2 is complete (session management, REST client scaffold with get/post/401 retry). Phase 1f.3 implements the mapper layer converting IBKR CP API types to ingot-core types, and the `MarketDataProvider` trait implementation on `IbkrRestClient`.

**Design decisions (from interactive planning):**
- **`fetch_instruments()` strategy**: Returns instruments cached in `IbkrContractRegistry`. A separate non-trait method `search_contracts(query)` populates the registry via `/iserver/secdef/search` + `/iserver/contract/{conid}/info`.
- **Order-related mappers**: Included in 1f.3 alongside market data mappers (pure functions, no extra dependencies). Keeps 1f.4 focused on OrderExecutor wiremock tests.
- **`fetch_trades()`**: Returns unsupported error — CP API does not provide trade-level data.
- **`fetch_order_book()`**: L1 only (best bid/ask from market snapshot). Depth parameter ignored.

## 2. Files

### New Files (1)
- `crates/ingot-connectivity/src/ibkr/mapper.rs` — All conversion functions between IBKR types and ingot-core types

### Modified Files (2)
- `crates/ingot-connectivity/src/ibkr/rest.rs` — `impl MarketDataProvider for IbkrRestClient` + `search_contracts()` method
- `crates/ingot-connectivity/src/ibkr/mod.rs` — add `pub(crate) mod mapper;`

## 3. Type Definitions

### 3.1 Mapper Functions (mapper.rs)

#### Asset class mapping
```rust
pub(crate) fn sec_type_to_asset_class(sec_type: &str) -> anyhow::Result<AssetClass>
// STK → Equity, OPT → Option, FUT → Future, CASH → Forex, BOND → Bond
// Unknown → Err(IbkrError::UnsupportedSecType)
```

#### Currency mapping
```rust
pub(crate) fn parse_currency(s: &str) -> Currency
// USD → Currency::USD, EUR → Currency::EUR, GBP → Currency::GBP, etc.
// Unknown → Currency::Other(SmolStr::new(s))
```

#### Contract → Instrument
```rust
pub(crate) fn contract_detail_to_instrument(
    detail: &IbkrContractDetail,
) -> anyhow::Result<Instrument>
```
Maps to correct `InstrumentDetails` variant based on `sec_type`:
- **STK** → `InstrumentDetails::Equity { isin: None, lot_size: Quantity(1), fractional: false }`
- **FUT** → `InstrumentDetails::Future { underlying: None, expiry, multiplier, settlement: Physical }`
- **OPT** → `InstrumentDetails::Option { underlying, strike, right, expiry, multiplier, style: American }`
- **CASH** → `InstrumentDetails::Forex { pip_size: Price(0.0001) }` (default, major pairs)
- **BOND** → `InstrumentDetails::Bond { face_value: Amount(1000), coupon_rate: Percentage(0), maturity }`

Parses `expiry` (YYYYMMDD → `NaiveDate`), `multiplier` (String → Decimal), `strike` (String → Decimal), `right` ("C"/"P" → `OptionRight`).

#### Market data conversions
```rust
pub(crate) fn market_snapshot_to_ticker(
    symbol: Symbol,
    snap: &IbkrMarketSnapshot,
) -> anyhow::Result<TickerSnapshot>
// Parses string fields "31"=last, "84"=bid, "86"=ask, "87"=volume
// Missing bid/ask/last → error. Missing volume → Quantity(0).

pub(crate) fn snapshot_to_order_book(
    symbol: Symbol,
    snap: &IbkrMarketSnapshot,
) -> anyhow::Result<OrderBookSnapshot>
// L1 only: single bid level, single ask level from snapshot fields

pub(crate) fn history_bar_to_ohlcv(
    symbol: Symbol,
    interval: &str,
    bar: &IbkrHistoryBar,
) -> anyhow::Result<OhlcvBar>
// bar.t (unix epoch) → DateTime<Utc>, bar.o/h/l/c → Price, bar.v → Quantity
```

#### Order-related mappings (for 1f.4)
```rust
pub(crate) fn order_side_to_ibkr(side: OrderSide) -> &'static str
// Buy → "BUY", Sell → "SELL"

pub(crate) fn order_type_to_ibkr(order_type: OrderType) -> anyhow::Result<&'static str>
// Market → "MKT", Limit → "LMT", StopLoss → "STP", StopLossLimit → "STP LMT"
// TakeProfit/TakeProfitLimit → Err (unsupported by IBKR order type mapping)

pub(crate) fn tif_to_ibkr(tif: TimeInForce) -> anyhow::Result<&'static str>
// GoodTilCancelled → "GTC", ImmediateOrCancel → "IOC", Day → "DAY"
// FillOrKill → "FOK", GoodTilDate → Err (requires special IBKR handling)
```

#### Interval mapping
```rust
pub(crate) fn ibkr_interval(interval: &str) -> anyhow::Result<&'static str>
// "1m" → "1min", "5m" → "5mins", "15m" → "15mins", "30m" → "30mins"
// "1h" → "1h", "4h" → "4h", "1d" → "1d", "1w" → "1w"
// Unknown → Err
```

### 3.2 MarketDataProvider impl (rest.rs)

```rust
impl MarketDataProvider for IbkrRestClient {
    async fn fetch_instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        // Return all instruments cached in the registry
        let reg = self.registry().read().await;
        // Collect all Arc<Instrument> values, clone inner
    }

    async fn fetch_ohlcv(&self, symbol, interval, since) -> anyhow::Result<Vec<OhlcvBar>> {
        // GET /iserver/marketdata/history?conid={conid}&bar={bar}&period={period}
        // Lookup conid from registry, map interval, compute period from `since`
    }

    async fn fetch_ticker(&self, symbol) -> anyhow::Result<TickerSnapshot> {
        // GET /iserver/marketdata/snapshot?conids={conid}&fields=31,84,86,87
    }

    async fn fetch_order_book(&self, symbol, depth) -> anyhow::Result<OrderBookSnapshot> {
        // GET /iserver/marketdata/snapshot?conids={conid}&fields=84,86
        // L1 only, depth parameter ignored
    }

    async fn fetch_trades(&self, symbol, since) -> anyhow::Result<(Vec<Tick>, Option<DateTime<Utc>>)> {
        // CP API doesn't support trade-level data
        anyhow::bail!("IBKR Client Portal API does not support trade-level data")
    }
}
```

### 3.3 search_contracts (rest.rs, non-trait)

```rust
impl IbkrRestClient {
    pub async fn search_contracts(&self, query: &str) -> anyhow::Result<Vec<Instrument>> {
        // 1. GET /iserver/secdef/search?symbol={query}
        // 2. For each result, GET /iserver/contract/{conid}/info
        // 3. Map each IbkrContractDetail → Instrument via mapper
        // 4. Register in ContractRegistry
        // 5. Return the instruments
    }
}
```

## 4. TDD Steps (24 tests)

### Mapper tests (19 in mapper.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 1 | `test_sec_type_to_asset_class_stk` | "STK" → Equity |
| 2 | `test_sec_type_to_asset_class_opt` | "OPT" → Option |
| 3 | `test_sec_type_to_asset_class_fut` | "FUT" → Future |
| 4 | `test_sec_type_to_asset_class_cash` | "CASH" → Forex |
| 5 | `test_sec_type_to_asset_class_bond` | "BOND" → Bond |
| 6 | `test_sec_type_to_asset_class_unknown` | "WAR" → Err |
| 7 | `test_contract_detail_to_instrument_equity` | STK detail → Instrument with Equity details |
| 8 | `test_contract_detail_to_instrument_future` | FUT detail → Instrument with expiry, multiplier |
| 9 | `test_contract_detail_to_instrument_option` | OPT detail → Instrument with strike, right, expiry |
| 10 | `test_contract_detail_to_instrument_forex` | CASH detail → Instrument with Forex details |
| 11 | `test_contract_detail_to_instrument_bond` | BOND detail → Instrument with Bond details |
| 12 | `test_market_snapshot_to_ticker` | Full snapshot → TickerSnapshot with all fields |
| 13 | `test_market_snapshot_to_ticker_partial_fields` | Missing volume → defaults to 0 |
| 14 | `test_history_bar_to_ohlcv` | IbkrHistoryBar → OhlcvBar with correct timestamp + prices |
| 15 | `test_order_side_to_ibkr` | Buy → "BUY", Sell → "SELL" |
| 16 | `test_order_type_to_ibkr` | Market → "MKT", Limit → "LMT", StopLoss → "STP", etc. |
| 17 | `test_tif_to_ibkr` | GTC → "GTC", IOC → "IOC", Day → "DAY" |
| 18 | `test_ibkr_interval` | "1m" → "1min", "1h" → "1h", "1d" → "1d" |
| 19 | `test_ibkr_interval_unsupported` | "2m" → Err |

### REST wiremock tests (5 in rest.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 20 | `test_search_contracts` | wiremock: search + detail endpoints → instruments returned + registered in cache |
| 21 | `test_fetch_ohlcv` | wiremock: history endpoint → Vec<OhlcvBar> |
| 22 | `test_fetch_ticker` | wiremock: snapshot endpoint → TickerSnapshot |
| 23 | `test_fetch_order_book` | wiremock: snapshot endpoint → OrderBookSnapshot (L1) |
| 24 | `test_fetch_trades_unsupported` | Returns error without making HTTP call |

## 5. Implementation Order

### Step 1: sec_type_to_asset_class + parse_currency (tests 1-6)
Create mapper.rs with asset class mapping and currency parsing. Write all 6 tests first.

### Step 2: contract_detail_to_instrument (tests 7-11)
Write 5 tests for each sec_type variant. Implement the full mapping function with expiry/multiplier/strike parsing.

### Step 3: market_snapshot_to_ticker + snapshot_to_order_book (tests 12-13)
Write ticker tests. Implement parsing of IBKR's string numeric fields to Decimal.

### Step 4: history_bar_to_ohlcv (test 14)
Write test. Implement timestamp conversion and price mapping.

### Step 5: Order-related mappers (tests 15-17)
Write tests for order_side, order_type, tif mappings.

### Step 6: Interval mapping (tests 18-19)
Write tests. Implement ibkr_interval().

### Step 7: Module wiring
Add `pub(crate) mod mapper;` to ibkr/mod.rs.

### Step 8: search_contracts (test 20)
Write wiremock test. Implement the non-trait search method on IbkrRestClient.

### Step 9: MarketDataProvider impl (tests 21-24)
Write wiremock tests for fetch_ohlcv, fetch_ticker, fetch_order_book. Write error test for fetch_trades. Implement the trait.

### Step 10: Verify
```bash
SQLX_OFFLINE=true cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --all-targets --workspace
SQLX_OFFLINE=true cargo nextest run -p ingot-connectivity
SQLX_OFFLINE=true cargo check --all-targets --workspace
SQLX_OFFLINE=true cargo bench --no-run
```

## 6. Key Design Decisions

1. **`fetch_instruments()` returns cached**: IBKR has millions of contracts. The registry is populated by explicit `search_contracts()` calls. `fetch_instruments()` just returns what's there.
2. **`fetch_trades()` unsupported**: CP API doesn't provide tick-level trade data. Returns error.
3. **`fetch_order_book()` L1 only**: CP snapshot only has best bid/ask. Returns single-level book. Depth parameter ignored.
4. **Default instrument details**: For fields not available from IBKR (ISIN, pip_size, face_value), use sensible defaults.
5. **String → Decimal parsing**: IBKR returns many numeric fields as strings. Mapper handles parsing with context errors.
6. **Order mappers pre-created**: Pure functions for 1f.4 use, no runtime dependencies.

## 7. Key Files (reference)
- `crates/ingot-connectivity/src/ibkr/models.rs` — IBKR response types (IbkrContractDetail, IbkrMarketSnapshot, IbkrHistoryBar)
- `crates/ingot-connectivity/src/ibkr/rest.rs` — IbkrRestClient with get/post
- `crates/ingot-connectivity/src/ibkr/contract_registry.rs` — IbkrContractRegistry
- `crates/ingot-connectivity/src/kraken/spot/mapper.rs` — Reference pattern for mapper functions
- `crates/ingot-core/src/instrument.rs` — Instrument, InstrumentDetails
- `crates/ingot-core/src/market_data.rs` — TickerSnapshot, OrderBookSnapshot
- `crates/ingot-core/src/ohlcv.rs` — OhlcvBar
- `crates/ingot-primitives/src/enums.rs` — AssetClass, OrderType, TimeInForce, OrderSide
