# Phase 1f TDD Plan: IBKR Integration

## Context

Phases 1a-1e delivered primitives, Kraken connectivity, accounting, execution engine, and backtesting. Phase 1f adds **Interactive Brokers (IBKR)** as a second real broker, extending the platform to equities, options, futures, forex, and bonds. This phase also adds derivative rollover automation, corporate action processing, and margin monitoring.

**User decisions:**
- **API**: Both Client Portal API (REST) AND TWS Socket API (real-time streaming) -- layered
- **Scope**: Full PRD scope (adapter + rollovers + corporate actions + margin)
- **Rollovers**: Engine service (`RolloverMonitor` in `ingot-engine`)
- **Corporate Actions**: Full suite (dividends, splits, bond coupons, mergers/spinoffs)

## Architecture Overview

### Layered API Strategy
- **Client Portal API (REST)**: Instrument search, account info, balances, positions, order placement, historical data, margin info
- **TWS Socket API (TCP)**: Real-time market data (ticks, order book), execution reports, account updates

`IbkrRestClient` handles request/response; `IbkrTwsStream` handles streaming. Both share an `IbkrContractRegistry` for `ContractId <-> Symbol` mapping.

### Crate Placement
```
ingot-connectivity/src/config.rs      (IbkrConfig added alongside Kraken configs)
ingot-connectivity/src/ibkr/
  mod.rs, error.rs, models.rs, contract_registry.rs,
  session.rs, mapper.rs, rest.rs, margin.rs,
  tws_codec.rs, tws_models.rs, tws.rs

ingot-engine/src/rollover.rs          (RolloverMonitor)
ingot-engine/src/config.rs            (MarginConfig, RolloverConfig)
ingot-accounting/src/posting.rs       (corporate action posting functions)
```

### Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| ContractId mapping | Bidirectional `HashMap<i64, Symbol>` + `HashMap<Symbol, i64>` | IBKR uses i64 conid; need fast lookup both directions |
| Session management | `SessionManager` with background keepalive | CP Gateway sessions expire after 5 min idle |
| TWS protocol | Custom codec with `tokio::io::AsyncRead/Write` | TWS uses length-prefixed binary messages |
| Margin types | `MarginSnapshot` struct in `ibkr/margin.rs` | IM, MM, Excess Liquidity, Buying Power are IBKR-specific |
| Corporate actions | New `TransactionType` variants + posting functions | Dividends, splits, coupons, mergers are accounting events |
| Rollovers | `RolloverMonitor` engine service | Watches expiry dates, emits close/open intention pairs through controller pipeline |

---

## Sub-Phase Breakdown

### 1f.1: IBKR Types, Config, Error, Response Models, ContractRegistry

**New files:** `ibkr/mod.rs`, `ibkr/config.rs`, `ibkr/error.rs`, `ibkr/models.rs`, `ibkr/contract_registry.rs`
**Modified:** `ingot-connectivity/src/lib.rs` (add `pub mod ibkr`)

**Key types:**
- `IbkrConfig { account_id, cp_gateway_url, tws_host, tws_port, client_id, session_keepalive_secs }`
- `IbkrError` enum: `SessionExpired`, `ContractNotFound(i64)`, `PacingViolation`, `OrderRejected`, `TwsConnection`, `TwsDecode`, `UnsupportedSecType`, `NotAuthenticated`, `MarginUnavailable`
- `IbkrContractRegistry` with `register()`, `symbol_for_conid()`, `conid_for_symbol()`, `instrument_for_conid()`
- CP API response models: `IbkrContractSearchResult`, `IbkrContractDetail`, `IbkrMarketSnapshot`, `IbkrOrderRequest`, `IbkrOrderStatus`, `IbkrAccountBalance`, `IbkrPosition`, `IbkrMarginInfo`, `IbkrHistoryBar`

**TDD Steps (27 tests):**
1. `test_ibkr_config_serde_roundtrip`
2. `test_ibkr_config_defaults` -- minimal JSON fills defaults
3. `test_ibkr_error_display_*` (9 tests, one per variant)
4. `test_deserialize_contract_search_result`
5. `test_deserialize_contract_detail_equity`
6. `test_deserialize_contract_detail_future`
7. `test_deserialize_contract_detail_option`
8. `test_deserialize_market_snapshot` / `_partial`
9. `test_deserialize_order_status`, `_account_balance`, `_position`, `_margin_info`, `_history_bar`
10. `test_contract_registry_register_and_lookup`, `_missing_conid`, `_missing_symbol`, `_len_and_is_empty`, `_overwrite`

---

### 1f.2: Client Portal REST -- Session Management

**New files:** `ibkr/session.rs`
**Modified:** `ibkr/rest.rs` (initial scaffold)

**Key types:**
- `SessionManager { http, config, state: Arc<RwLock<SessionState>>, keepalive_handle, shutdown_tx }`
- `SessionState` enum: `Unauthenticated`, `Authenticated`, `Expired`
- `IbkrRestClient { session, config, registry, rate_limiter }` with `get<T>()`, `post<T>()` helpers
- CP Gateway requires HTTPS with self-signed cert (`danger_accept_invalid_certs`)

**TDD Steps (12 tests):**
1. `test_session_state_display`
2. `test_session_manager_initial_state_unauthenticated`
3. `test_session_authenticate_success` (wiremock: `/iserver/auth/status` returns authenticated)
4. `test_session_authenticate_failure`
5. `test_session_check_status_authenticated` / `_expired`
6. `test_session_tickle_success` (wiremock: `/tickle`)
7. `test_session_ensure_authenticated_when_already_authed` / `_re_auths_when_expired`
8. `test_ibkr_rest_client_construction`
9. `test_ibkr_rest_client_get_injects_session`
10. `test_ibkr_rest_client_handles_401_retry`

---

### 1f.3: Client Portal REST -- MarketDataProvider

**New files:** `ibkr/mapper.rs`
**Modified:** `ibkr/rest.rs` (MarketDataProvider impl)

**Key mappings (mapper.rs):**
- `sec_type_to_asset_class`: STK->Equity, OPT->Option, FUT->Future, CASH->Forex, BOND->Bond
- `contract_detail_to_instrument`: IbkrContractDetail -> Instrument with correct InstrumentDetails variant
- `market_snapshot_to_ticker`, `history_bar_to_ohlcv`
- `order_side_to_ibkr`, `order_type_to_ibkr`, `tif_to_ibkr`, `ibkr_interval`

**Endpoints:**
- `fetch_instruments` -> GET `/iserver/secdef/search` + GET `/iserver/contract/{conid}/info`
- `fetch_ohlcv` -> GET `/iserver/marketdata/history`
- `fetch_ticker` -> GET `/iserver/marketdata/snapshot?fields=31,84,86,87`
- `fetch_order_book` -> GET `/iserver/marketdata/snapshot` (L1 only from REST)
- `fetch_trades` -> returns unsupported error (CP API limitation)

**TDD Steps (24 tests):**
1-6. `test_sec_type_to_asset_class_*` (stk, opt, fut, cash, bond, unknown)
7-11. `test_contract_detail_to_instrument_*` (equity, future, option, forex, bond)
12-13. `test_market_snapshot_to_ticker` / `_partial_fields`
14. `test_history_bar_to_ohlcv`
15-19. Mapper tests: `order_side`, `order_type`, `tif`, `interval`, `interval_unsupported`
20-24. wiremock: `fetch_instruments`, `fetch_ohlcv`, `fetch_ticker`, `fetch_order_book`, `fetch_trades_unsupported`

---

### 1f.4: Client Portal REST -- OrderExecutor

**Modified:** `ibkr/rest.rs` (OrderExecutor impl), `ibkr/mapper.rs` (order status mapping)

**Key behavior:** IBKR may return confirmation prompts requiring POST `/iserver/reply/{replyId}` with `{"confirmed": true}`. Adapter handles this transparently.

**Mapper additions:**
- `ibkr_status_to_order_status`: Submitted->Open, Filled->Filled, Cancelled->Cancelled, PreSubmitted->Pending, Inactive->Rejected
- `ibkr_order_to_open_order`

**Endpoints:**
- `place_order` -> POST `/iserver/account/{id}/orders` (+ auto-confirm)
- `cancel_order` -> DELETE `/iserver/account/{id}/order/{orderId}`
- `cancel_all_orders` -> loop: get open orders + cancel each
- `get_order_status` -> GET `/iserver/account/order/status/{orderId}`
- `get_open_orders` -> GET `/iserver/account/orders`

**TDD Steps (16 tests):**
1-5. `test_ibkr_status_to_order_status_*` (submitted, filled, cancelled, presubmitted, inactive)
6. `test_ibkr_order_to_open_order`
7-10. wiremock: `place_order_market`, `place_order_limit`, `place_order_with_confirmation`, `place_order_rejected`
11-12. wiremock: `cancel_order`, `cancel_order_not_found`
13. wiremock: `cancel_all_orders`
14-16. wiremock: `get_order_status`, `get_open_orders`, `get_open_orders_empty`

---

### 1f.5: Client Portal REST -- AccountProvider + Margin

**New files:** `ibkr/margin.rs`
**Modified:** `ibkr/rest.rs` (AccountProvider impl + margin methods)

**Key types:**
```rust
pub struct MarginSnapshot {
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
// Methods: utilization(), is_margin_call(), available_margin()
```

**Endpoints:**
- `get_balances` -> GET `/portfolio/{accountId}/ledger`
- `get_positions` -> GET `/portfolio/{accountId}/positions/0`
- `get_trade_history` -> GET `/iserver/account/trades`
- margin -> GET `/portfolio/{accountId}/summary`

**TDD Steps (15 tests):**
1-4. Mapper: `position_long`, `position_short`, `balance`, `margin_to_snapshot`
5-9. MarginSnapshot: `utilization`, `is_margin_call_true/false`, `available_margin`, `serde_roundtrip`
10-14. wiremock: `get_balances`, `get_positions`, `get_positions_empty`, `get_trade_history`, `get_margin`
15. `prop_test_margin_utilization_bounded`

---

### 1f.6: TWS Socket -- Connection, Auth, Message Protocol

**New files:** `ibkr/tws_codec.rs`, `ibkr/tws_models.rs`, `ibkr/tws.rs` (scaffold)

**TWS binary protocol:** Length-prefixed messages: `[4-byte BE length][null-separated fields]`. Each message type has a numeric ID as first field.

**Key types:**
- `TwsCodec` with `encode(fields)`, `decode(buf)`, `read_message()`, `write_message()`
- `TwsIncoming` enum: `NextValidId`, `TickPrice`, `TickSize`, `OrderStatus`, `ExecutionData`, `MarketDepth`, `ErrorMessage`, `Heartbeat`
- `IbkrTwsStream<S>` with typestate (Disconnected -> Connected)

**TDD Steps (15 tests):**
1-5. Codec: `encode_single_field`, `encode_multiple_fields`, `decode_roundtrip`, `decode_empty`, `decode_malformed`
6-13. Parsing: `next_valid_id`, `tick_price`, `tick_size`, `order_status`, `execution_data`, `market_depth`, `error_message`, `unknown_msg_id`
14. `test_tws_stream_disconnected_state`
15. `prop_test_codec_roundtrip`

---

### 1f.7: TWS Socket -- StreamProvider

**Modified:** `ibkr/tws.rs` (full StreamProvider impl on Connected state)

**Architecture:** Connected stream spawns a read loop that parses messages and dispatches to broadcast channels (tick, ticker, book, exec). Request IDs tracked for subscription correlation.

**TDD Steps (9 tests, using mock TCP server):**
1-4. `subscribe_trades/ticker/order_book/executions_returns_receiver`
5-6. `tick_price_dispatches_to_ticker`, `execution_data_dispatches_to_fills`
7. `market_depth_builds_order_book`
8. `error_message_logged_not_panicked`
9. `shutdown_stops_read_loop`

---

### 1f.8: Margin Monitoring -- Extend PortfolioController

**Modified:** `ingot-engine/src/config.rs`, `controller.rs`, `types.rs`, `error.rs`

**Key types:**
```rust
pub struct MarginConfig {
    pub max_margin_utilization: Percentage,   // e.g., 0.80
    pub warn_margin_utilization: Percentage,  // e.g., 0.60
    pub min_excess_liquidity: Amount,
    pub poll_interval_ms: u64,
}
// Add to RiskConfig as: pub margin: Option<MarginConfig>
```

**Controller changes:**
- `on_margin_update(&mut self, snapshot: &MarginSnapshot)` -- stores latest margin state
- `check_intention()` gains new step: reject if margin utilization > max or excess liquidity < min
- Auto-halt if margin call detected (excess_liquidity < 0)
- Add `MarginUpdate(MarginSnapshot)` to `EngineEvent`

**TDD Steps (11 tests):**
1-2. `test_margin_config_serde_roundtrip`, `_in_risk_config`
3. `test_on_margin_update_stores_snapshot`
4-6. `test_check_intention_approved_with_margin_headroom`, `_rejected_margin_utilization`, `_rejected_low_excess_liquidity`
7. `test_margin_update_auto_halts_on_margin_call`
8. `test_no_margin_config_skips_margin_check`
9-10. `test_engine_event_margin_update_variant`, `_error_margin_variants_display`
11. `prop_test_margin_check_never_approves_above_limit`

---

### 1f.9: Corporate Actions -- Accounting Extensions

**Modified:** `ingot-accounting/src/types.rs`, `posting.rs`, `lib.rs`

**TransactionType additions:** `Dividend`, `StockSplit`, `BondCoupon`, `Merger`, `Spinoff`

**New posting functions:**
- `post_dividend(exchange, venue, currency, symbol, amount, timestamp)` -- Debit: asset, Credit: revenue:dividend
- `post_stock_split(exchange, venue, symbol, currency, old_qty, new_qty, timestamp)` -- metadata-only, balanced against revenue:split
- `post_bond_coupon(exchange, venue, currency, symbol, amount, timestamp)` -- Debit: asset, Credit: revenue:coupon
- `post_merger(exchange, venue, old_symbol, old_currency, old_qty, new_symbol, new_currency, new_qty, cash_consideration, timestamp)` -- Close old position, open new, optional cash
- `post_spinoff(exchange, venue, parent_symbol, new_symbol, new_currency, new_qty, cost_basis_allocation, timestamp)` -- New position entry

**TDD Steps (20 tests):**
1-6. Display + serde for new TransactionType variants
7-9. `test_post_dividend_*` (balanced entries, zero amount rejected, correct accounts)
10-12. `test_post_stock_split_*` (forward 4:1, reverse 1:10, same_quantity_rejected)
13-14. `test_post_bond_coupon_*` (balanced entries, correct accounts)
15-16. `test_post_merger_*` (shares only, with cash consideration)
17-18. `test_post_spinoff_*` (no cost basis, with cost basis allocation)
19-20. `prop_test_post_dividend_always_validates`, `prop_test_post_bond_coupon_always_validates`

---

### 1f.10: Derivative Rollovers -- RolloverMonitor

**New files:** `ingot-engine/src/rollover.rs`
**Modified:** `ingot-engine/src/lib.rs`, `config.rs`, `types.rs`

**Key types:**
```rust
pub struct RolloverConfig {
    pub days_before_expiry: u32,
    pub max_concurrent_rollovers: usize,
    pub use_limit_orders: bool,
    pub limit_offset_bps: Decimal,
}

pub struct RolloverPlan {
    pub near_symbol: Symbol,
    pub far_symbol: Symbol,
    pub quantity: Quantity,
    pub side: OrderSide,
    pub expiry_date: NaiveDate,
    pub planned_date: NaiveDate,
}

pub enum RolloverState { Planned, ClosingNearMonth, NearMonthClosed, OpeningFarMonth, Complete, Failed }

pub struct RolloverMonitor { config, active_rollovers }
// Methods: scan_for_rollovers(), close_intention(), open_intention(), on_fill(), find_far_month()
```

**EngineEvent additions:** `RolloverTriggered(RolloverPlan)`, `RolloverCompleted`, `RolloverFailed`

**TDD Steps (20 tests):**
1-2. Config: `serde_roundtrip`, `defaults_sensible`
3-8. Scanning: `no_positions`, `no_expiring`, `future_within_window`, `option_within_window`, `equity_ignored`, `already_rolling`
9-10. Far month: `find_far_month_futures`, `_not_found`
11-14. Intentions: `close_sell_for_long`, `close_buy_for_short`, `open_buy_for_long`, `open_sell_for_short`
15-17. Fill handling: `advances_close_to_open`, `advances_open_to_complete`, `unrelated_ignored`
18-19. State: `transitions`, `engine_event_variants`
20. `prop_test_scan_only_finds_within_window`

---

### 1f.11: Integration Tests + Public API

**New files:** `ingot-connectivity/tests/ibkr_integration.rs`
**Modified:** `ingot-connectivity/src/lib.rs`, `ibkr/mod.rs` (public re-exports)

**TDD Steps (12 integration tests):**
1-4. Trait satisfaction: `IbkrRestClient` as MarketDataProvider/OrderExecutor/AccountProvider, `IbkrTwsStream<Connected>` as StreamProvider
5. `test_full_order_lifecycle` -- place -> status -> fills via TWS -> cancel
6. `test_session_recovery_on_401`
7. `test_contract_registry_populated_on_fetch_instruments`
8. `test_margin_snapshot_through_portfolio_controller`
9. `test_corporate_action_dividend_through_accounting`
10. `test_rollover_scan_to_intention_generation`
11. `test_engine_event_variants_exhaustive`
12. `test_ibkr_rest_concurrent_requests`

---

## Implementation Order

```
1f.1 (types) --> 1f.2 (session) --> 1f.3 (MarketData) --> 1f.4 (OrderExecutor) --> 1f.5 (Account+Margin)
1f.1 (types) --> 1f.6 (TWS codec) --> 1f.7 (StreamProvider)
1f.5 (Margin) --> 1f.8 (Margin in Engine)
(independent)    1f.9 (Corporate Actions)
1f.3 + 1f.4 --> 1f.10 (Rollovers)
All above   --> 1f.11 (Integration tests)
```

1f.3/1f.4/1f.5 can proceed sequentially after 1f.2. 1f.6/1f.7 can proceed in parallel with 1f.3-1f.5. 1f.9 is completely independent.

## File Summary

**New files (15):** All in `ingot-connectivity/src/ibkr/` (12 files) + `ingot-engine/src/rollover.rs` + `ingot-connectivity/tests/ibkr_integration.rs`

**Modified files (10):** `ingot-connectivity/src/lib.rs`, `ingot-accounting/src/types.rs`, `ingot-accounting/src/posting.rs`, `ingot-accounting/src/lib.rs`, `ingot-engine/src/config.rs`, `ingot-engine/src/controller.rs`, `ingot-engine/src/types.rs`, `ingot-engine/src/error.rs`, `ingot-engine/src/lib.rs`

**No new workspace dependencies needed** -- tokio (net, io-util), reqwest, serde already available.

## Test Summary

| Category | Count |
|----------|-------|
| Types, config, error display | ~60 |
| JSON deserialization | ~25 |
| Mapper conversions | ~20 |
| REST (wiremock) | ~30 |
| TWS protocol | ~15 |
| Accounting posting | ~20 |
| Margin/risk | ~15 |
| Rollover | ~20 |
| Integration | ~12 |
| **Total** | **~217** |

## Verification (per sub-phase)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --workspace
cargo nextest run --workspace
cargo check --all-targets --workspace
cargo bench --no-run
```
