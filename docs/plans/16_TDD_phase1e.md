# Technical Design Document: Phase 1e — Backtesting

## 1. Context

Phases 1a–1d delivered primitives, storage, broker connectivity (Kraken + PaperExchange), accounting (double-entry ledger, posting engine, NAV), and the execution engine (strategy trait, risk controller, order manager, scheduler, kill switch). Phase 1e builds the **backtesting framework**: an event-driven backtester that replays historical market data through strategies, simulates fills, and computes performance metrics.

The backtester enables strategy validation against historical data before live deployment — a prerequisite for responsible systematic trading.

## 2. Crate Structure

New crate `ingot-backtest` added to workspace:

```
ingot-primitives (no deps)
    ↓
ingot-core (→ primitives)
    ↓
ingot-accounting (→ core, primitives)
    ↓
ingot-connectivity (→ core, storage, primitives)  ← fill_model made pub
    ↓
ingot-engine (→ connectivity, accounting, core, primitives)
    ↓
ingot-backtest (→ engine, connectivity, accounting, core, primitives)  ← NEW
```

No dependency on `ingot-storage` — the backtester accepts data as `Vec<OhlcvBar>` / `Vec<Tick>`, decoupled from the database.

## 3. Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Clock/time | Separate `BacktestExchange` with injected timestamps | Avoids modifying PaperExchange/Engine; backtester is simpler without background tasks |
| Fill model reuse | Make `fill_model` functions `pub` in `ingot-connectivity` | DRY; same slippage/fee/partial-fill logic reused without duplication |
| Event loop | Custom synchronous loop (not `Engine::run()`) | Deterministic, fast, no async overhead; avoids `Utc::now()` and `tokio::signal::ctrl_c()` issues |
| Data source | Accept `Vec<OhlcvBar>` / `Vec<Tick>` directly | Decoupled from storage; caller loads data however they want |
| Metrics scope | Full suite (CAGR, Sharpe, Sortino, Calmar, Max DD, Volatility, Win Rate, Profit Factor, Avg Win/Loss, Trade Count, Total Return) | Comprehensive strategy evaluation |
| Equity curve | Snapshot after every fill | Natural granularity — equity changes only when trades execute |
| Determinism | Required `u64` RNG seed | Every backtest with same seed + data produces identical results |

## 4. Sub-Phase Breakdown

### 1e.1: Crate Scaffold + Config + Error + InMemoryLedger

**New files:**
- `crates/ingot-backtest/Cargo.toml`
- `crates/ingot-backtest/src/lib.rs`
- `crates/ingot-backtest/src/config.rs`
- `crates/ingot-backtest/src/error.rs`
- `crates/ingot-backtest/src/ledger.rs`

**Changes to existing files:**
- `Cargo.toml` (root) — add `ingot-backtest` to workspace members
- `crates/ingot-connectivity/src/paper/fill_model.rs` — change `pub(crate)` → `pub` on all 4 functions
- `crates/ingot-connectivity/src/paper/mod.rs` — make `fill_model` module `pub`

**Types:**

```rust
// config.rs
pub struct BacktestConfig {
    pub initial_balances: Vec<(Currency, Decimal)>,
    pub base_currency: Currency,
    pub slippage_bps: Decimal,
    pub maker_fee_bps: Decimal,
    pub taker_fee_bps: Decimal,
    pub partial_fill_probability: Decimal,
    pub rng_seed: u64,
    pub risk: RiskConfig,                // from ingot-engine
    pub smart_order: SmartOrderConfig,   // from ingot-engine
}

// error.rs
#[derive(Debug, thiserror::Error)]
pub enum BacktestError {
    #[error("no data provided")]
    NoData,
    #[error("no strategies registered")]
    NoStrategies,
    #[error("data is not sorted by time")]
    UnsortedData,
    #[error("engine error: {0}")]
    Engine(#[from] EngineError),
    #[error("accounting error: {0}")]
    Accounting(#[from] AccountingError),
    #[error("backtest exchange error: {0}")]
    Exchange(String),
}

// ledger.rs — implements LedgerWriter from ingot-engine
pub struct InMemoryLedger {
    transactions: Vec<Transaction>,
}

impl InMemoryLedger {
    pub fn new() -> Self;
    pub fn transactions(&self) -> &[Transaction];
    pub fn into_transactions(self) -> Vec<Transaction>;
}

impl LedgerWriter for InMemoryLedger {
    async fn write_transaction(&self, txn: &Transaction) -> anyhow::Result<()>;
}
```

**TDD Steps:**
1. `test_backtest_config_construction` — Build BacktestConfig with all fields
2. `test_backtest_config_serde_roundtrip` — Serialize/deserialize roundtrip
3. `test_backtest_error_display` — All BacktestError variants have correct Display
4. `test_backtest_error_from_engine` — EngineError converts to BacktestError
5. `test_backtest_error_from_accounting` — AccountingError converts to BacktestError
6. `test_in_memory_ledger_new_empty` — New ledger has zero transactions
7. `test_in_memory_ledger_write_transaction` — Write a transaction, verify stored
8. `test_in_memory_ledger_multiple_writes` — Write 3 transactions, verify all stored in order
9. `test_in_memory_ledger_into_transactions` — Consume ledger, get owned Vec

**Verification:** fmt, clippy, nextest, check, bench --no-run

---

### 1e.2: BacktestExchange (Order Executor)

**New files:**
- `crates/ingot-backtest/src/exchange.rs`

**BacktestExchange** implements `OrderExecutor` (from ingot-connectivity) with historical-timestamp-aware fill simulation. Unlike PaperExchange, it is synchronous (no background matching loop) and accepts timestamps from the data feed.

```rust
pub struct BacktestExchange {
    config: BacktestConfig,
    balances: HashMap<Currency, Balance>,
    positions: HashMap<Symbol, Position>,
    open_orders: HashMap<OrderId, OpenOrder>,
    completed_orders: HashMap<OrderId, OpenOrder>,
    fills: Vec<OrderFill>,
    last_prices: HashMap<Symbol, Price>,
    current_time: DateTime<Utc>,
    next_order_id: u64,
    next_fill_id: u64,
    rng: StdRng,
}

impl BacktestExchange {
    pub fn new(config: &BacktestConfig) -> Result<Self, BacktestError>;

    /// Advance time and set current price. Called by the backtest loop
    /// for each data point. Returns fills for any limit orders that crossed.
    pub fn on_price_update(
        &mut self,
        symbol: &Symbol,
        price: Price,
        timestamp: DateTime<Utc>,
    ) -> Vec<OrderFill>;

    /// Current simulated time.
    pub fn current_time(&self) -> DateTime<Utc>;

    /// All fills generated during the backtest.
    pub fn fills(&self) -> &[OrderFill];

    /// Current positions snapshot.
    pub fn positions(&self) -> &HashMap<Symbol, Position>;

    /// Current balances snapshot.
    pub fn balances(&self) -> &HashMap<Currency, Balance>;
}

impl OrderExecutor for BacktestExchange { ... }
```

**Key behavior:**
- `place_order()` for Market orders: fill immediately at `last_prices[symbol]` with slippage (reuses `fill_model::apply_slippage`, `calculate_fee`)
- `place_order()` for Limit orders: store as open order, match on subsequent `on_price_update()` calls (reuses `fill_model::tick_crosses_limit`)
- Partial fills via `fill_model::partial_fill_quantity` with seeded RNG
- Balance tracking: debit/credit on fills, hold on limit order placement
- All timestamps use `current_time` (not `Utc::now()`)

**TDD Steps:**
1. `test_exchange_new_initial_balances` — Balances match config
2. `test_exchange_new_zero_time` — Initial time is DateTime::UNIX_EPOCH (or configurable)
3. `test_place_market_order_buy_fills_immediately` — Market buy → fill at last price + slippage
4. `test_place_market_order_sell_fills_immediately` — Market sell → fill at last price - slippage
5. `test_place_market_order_fee_calculated` — Fee in fill matches `calculate_fee` with taker_fee_bps
6. `test_place_limit_order_stored` — Limit order stored as open, not filled yet
7. `test_on_price_update_crosses_buy_limit` — Price drops below limit → fill
8. `test_on_price_update_crosses_sell_limit` — Price rises above limit → fill
9. `test_on_price_update_no_cross` — Price doesn't cross → no fill
10. `test_partial_fill_with_seed` — Same seed produces same partial fill quantity
11. `test_cancel_order` — Cancel an open limit order
12. `test_cancel_all_orders` — Cancel all open orders
13. `test_get_order_status` — Query status of tracked order
14. `test_get_open_orders` — List only open orders
15. `test_balance_updated_on_buy_fill` — Quote currency decremented, base position increased
16. `test_balance_updated_on_sell_fill` — Base position decremented, quote currency increased
17. `test_deterministic_fills_same_seed` — Two exchanges with same seed + data produce identical fills
18. `test_deterministic_fills_different_seed` — Different seeds produce different partial fills
19. **proptest:** `prop_test_fill_price_includes_slippage` — For any price/slippage, buy fill price ≥ market price, sell fill price ≤ market price (1000 cases)
20. **proptest:** `prop_test_fee_always_non_negative` — For any price/quantity/fee_bps, fee ≥ 0 (1000 cases)

**Verification:** fmt, clippy, nextest, check, bench --no-run

---

### 1e.3: Data Feed (OhlcvBar → TickerSnapshot conversion)

**New files:**
- `crates/ingot-backtest/src/feed.rs`

Converts historical data into the event stream consumed by the backtest loop.

```rust
/// A single event in the backtest timeline.
#[derive(Debug, Clone)]
pub struct BacktestEvent {
    pub timestamp: DateTime<Utc>,
    pub symbol: Symbol,
    pub ticker: TickerSnapshot,
}

/// Convert OHLCV bars into a sorted sequence of BacktestEvents.
/// Each bar produces a TickerSnapshot with:
///   bid = close (approximation), ask = close, last = close, volume_24h = volume
pub fn ohlcv_to_events(bars: &[OhlcvBar]) -> Result<Vec<BacktestEvent>, BacktestError>;

/// Convert raw ticks into BacktestEvents.
/// Each tick produces a TickerSnapshot with:
///   bid = price, ask = price, last = price, volume_24h = quantity
pub fn ticks_to_events(ticks: &[Tick]) -> Result<Vec<BacktestEvent>, BacktestError>;

/// Merge multiple symbol feeds into a single time-sorted event stream.
pub fn merge_events(feeds: Vec<Vec<BacktestEvent>>) -> Vec<BacktestEvent>;

/// Validate that events are sorted by timestamp.
pub fn validate_sorted(events: &[BacktestEvent]) -> Result<(), BacktestError>;
```

**TDD Steps:**
1. `test_ohlcv_to_events_single_bar` — One bar → one event with correct TickerSnapshot fields
2. `test_ohlcv_to_events_multiple_bars` — Multiple bars → events sorted by time
3. `test_ohlcv_to_events_empty` — Empty slice → BacktestError::NoData
4. `test_ticks_to_events_single_tick` — One tick → one event
5. `test_ticks_to_events_multiple_ticks` — Multiple ticks → events sorted by time
6. `test_ticks_to_events_empty` — Empty slice → BacktestError::NoData
7. `test_merge_events_two_symbols` — Interleave events from two symbols by timestamp
8. `test_merge_events_empty_input` — Empty vec → empty result
9. `test_merge_events_single_feed` — Single feed returned as-is
10. `test_validate_sorted_valid` — Sorted events → Ok
11. `test_validate_sorted_invalid` — Unsorted events → BacktestError::UnsortedData
12. `test_validate_sorted_empty` — Empty → Ok (vacuously true)

**Verification:** fmt, clippy, nextest, check, bench --no-run

---

### 1e.4: BacktestRunner (Synchronous Event Loop)

**New files:**
- `crates/ingot-backtest/src/runner.rs`
- `crates/ingot-backtest/src/result.rs`

The core backtest loop. Iterates through events, dispatches to strategies via PortfolioController and OrderManager (reusing ingot-engine types), and collects results.

```rust
// result.rs
#[derive(Debug, Clone)]
pub struct EquityPoint {
    pub timestamp: DateTime<Utc>,
    pub nav: Amount,
    pub cash: Amount,
    pub positions_value: Amount,
}

#[derive(Debug, Clone)]
pub struct BacktestResult {
    pub fills: Vec<OrderFill>,
    pub transactions: Vec<Transaction>,
    pub equity_curve: Vec<EquityPoint>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub initial_capital: Amount,
    pub final_capital: Amount,
}

// runner.rs
pub struct BacktestRunner {
    config: BacktestConfig,
    strategies: Vec<StrategyKind>,
    symbol_currencies: HashMap<Symbol, (Currency, Currency)>,
}

impl BacktestRunner {
    pub fn new(config: BacktestConfig) -> Self;

    pub fn register_strategy(&mut self, strategy: StrategyKind) -> Result<(), BacktestError>;

    pub fn register_symbol(&mut self, symbol: Symbol, base: Currency, quote: Currency);

    /// Run the backtest synchronously over the provided events.
    pub fn run(&mut self, events: Vec<BacktestEvent>) -> Result<BacktestResult, BacktestError>;
}
```

**Internal run() logic (synchronous, no channels):**
1. Create `BacktestExchange` from config
2. Create `PortfolioController` from config.risk
3. Create `OrderManager` from config.smart_order
4. Init all strategies (call `strategy.init(ctx)`)
5. For each `BacktestEvent` in order:
   a. Call `exchange.on_price_update()` → collect limit order fills
   b. For each fill: update order_manager, controller, post_fill → ledger, dispatch to strategies
   c. Build `StrategyContext` with `event.timestamp` (not `Utc::now()`)
   d. Call `strategy.on_ticker()` → collect intentions
   e. For each intention: `controller.check_intention()` → if approved, `order_manager.submit_order()`
   f. Process any immediate fills from market orders
   g. After each fill: record `EquityPoint` snapshot
6. Shutdown all strategies
7. Collect and return `BacktestResult`

**TDD Steps:**
1. `test_runner_new` — Runner constructs with config
2. `test_runner_register_strategy` — Register a NoopStrategy
3. `test_runner_register_duplicate_strategy_rejected` — Same StrategyId twice → error
4. `test_runner_run_no_strategies_error` — Run with no strategies → BacktestError::NoStrategies
5. `test_runner_run_no_data_error` — Run with empty events → BacktestError::NoData
6. `test_runner_run_noop_strategy` — Run with NoopStrategy → result has zero fills, initial == final capital
7. `test_runner_run_single_fill` — Custom strategy that buys on first ticker → one fill in result
8. `test_runner_equity_curve_recorded_on_fill` — After fill, equity curve has entry with correct timestamp
9. `test_runner_context_uses_event_timestamp` — StrategyContext.timestamp matches event time, not wall clock
10. `test_runner_risk_rejection_no_fill` — Strategy intention exceeding risk limit → rejected, no fill
11. `test_runner_multiple_symbols` — Two symbols interleaved → correct dispatch per symbol
12. `test_runner_limit_order_fills_on_cross` — Strategy places limit → fills when price crosses in later event
13. `test_runner_deterministic_same_seed` — Same data + seed → identical BacktestResult
14. `test_runner_accounting_integration` — Fills generate correct ledger transactions via InMemoryLedger
15. **Integration test:** `test_runner_end_to_end_buy_and_sell` — Strategy buys then sells → result shows two fills, correct PnL in equity curve

**Verification:** fmt, clippy, nextest, check, bench --no-run

---

### 1e.5: Performance Metrics

**New files:**
- `crates/ingot-backtest/src/metrics.rs`

Computes performance statistics from a `BacktestResult`.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub total_return: Percentage,
    pub cagr: Percentage,
    pub max_drawdown: Percentage,
    pub sharpe_ratio: Decimal,
    pub sortino_ratio: Decimal,
    pub calmar_ratio: Decimal,
    pub volatility: Percentage,
    pub win_rate: Percentage,
    pub profit_factor: Decimal,
    pub avg_win: Amount,
    pub avg_loss: Amount,
    pub total_trades: u32,
    pub winning_trades: u32,
    pub losing_trades: u32,
}

impl PerformanceMetrics {
    /// Compute all metrics from a BacktestResult.
    pub fn from_result(
        result: &BacktestResult,
        risk_free_rate: Decimal,
    ) -> Result<Self, BacktestError>;
}

// Internal helper functions (pub for testing):
pub(crate) fn compute_returns(equity_curve: &[EquityPoint]) -> Vec<Decimal>;
pub(crate) fn compute_total_return(initial: Amount, final_val: Amount) -> Percentage;
pub(crate) fn compute_cagr(initial: Amount, final_val: Amount, years: Decimal) -> Percentage;
pub(crate) fn compute_max_drawdown(equity_curve: &[EquityPoint]) -> Percentage;
pub(crate) fn compute_sharpe(returns: &[Decimal], risk_free_rate: Decimal) -> Decimal;
pub(crate) fn compute_sortino(returns: &[Decimal], risk_free_rate: Decimal) -> Decimal;
pub(crate) fn compute_volatility(returns: &[Decimal]) -> Percentage;
pub(crate) fn compute_calmar(cagr: Percentage, max_dd: Percentage) -> Decimal;
pub(crate) fn compute_trade_stats(fills: &[OrderFill]) -> TradeStats;
```

**TDD Steps:**
1. `test_total_return_positive` — 10000 → 12000 = 20%
2. `test_total_return_negative` — 10000 → 8000 = -20%
3. `test_total_return_zero` — 10000 → 10000 = 0%
4. `test_cagr_one_year` — 10000 → 12000 over 1 year = 20%
5. `test_cagr_two_years` — 10000 → 14400 over 2 years ≈ 20%
6. `test_cagr_partial_year` — Correct for fractional years
7. `test_max_drawdown_no_drawdown` — Monotonically increasing equity → 0%
8. `test_max_drawdown_simple` — 100 → 120 → 90 → 110 → max DD = 25% (from 120 to 90)
9. `test_max_drawdown_multiple_drawdowns` — Picks the largest drawdown
10. `test_sharpe_ratio_positive` — Known returns → expected Sharpe
11. `test_sharpe_ratio_zero_volatility` — Constant returns → Decimal::ZERO (guarded)
12. `test_sortino_ratio_positive` — Only penalizes downside volatility
13. `test_sortino_ratio_no_downside` — No negative returns → Decimal::ZERO (guarded)
14. `test_calmar_ratio` — CAGR / Max DD
15. `test_calmar_ratio_zero_drawdown` — Max DD = 0 → Decimal::ZERO (guarded)
16. `test_volatility_constant_returns` — All returns same → 0%
17. `test_volatility_varied_returns` — Known returns → expected volatility
18. `test_win_rate_all_winners` — 5 winning trades → 100%
19. `test_win_rate_mixed` — 3 wins, 2 losses → 60%
20. `test_win_rate_no_trades` — Zero trades → 0%
21. `test_profit_factor_positive` — Gross profit / gross loss
22. `test_profit_factor_no_losses` — All wins → Decimal::ZERO (guarded, represents infinity)
23. `test_avg_win_avg_loss` — Correct averages of winning/losing trade PnL
24. `test_from_result_integration` — Full BacktestResult → PerformanceMetrics computed correctly
25. **proptest:** `prop_test_total_return_symmetry` — return(initial, final) is consistent with return(final, initial) being its inverse (1000 cases)
26. **proptest:** `prop_test_max_drawdown_never_exceeds_100_pct` — For any equity curve, max DD ∈ [0%, 100%] (1000 cases)
27. **proptest:** `prop_test_win_rate_bounded` — Win rate always ∈ [0%, 100%] (1000 cases)
28. **proptest:** `prop_test_sharpe_finite` — Sharpe ratio is always finite for non-zero-length return series (1000 cases)

**Verification:** fmt, clippy, nextest, check, bench --no-run

---

### 1e.6: Public API + Integration Tests

**Modified files:**
- `crates/ingot-backtest/src/lib.rs` — wire all modules, define public API

**New files:**
- `crates/ingot-backtest/tests/integration.rs`

**Public API (lib.rs re-exports):**
```rust
pub mod config;
pub mod error;
pub mod exchange;
pub mod feed;
pub mod ledger;
pub mod metrics;
pub mod result;
pub mod runner;

// Convenience re-exports
pub use config::BacktestConfig;
pub use error::BacktestError;
pub use metrics::PerformanceMetrics;
pub use result::BacktestResult;
pub use runner::BacktestRunner;
```

**TDD Steps (integration tests):**
1. `test_full_backtest_buy_hold_strategy` — Strategy buys on first bar and holds → positive return on rising prices, negative on falling
2. `test_full_backtest_mean_reversion_strategy` — Strategy buys when price drops 5%, sells when it rises 5% → verifies multiple fills and round-trip PnL
3. `test_full_backtest_metrics_computed` — After backtest, PerformanceMetrics has all fields populated and valid
4. `test_full_backtest_deterministic` — Run same backtest twice with same seed → identical results (fills, equity curve, metrics)
5. `test_full_backtest_risk_limits_respected` — Strategy tries to exceed max_order_value → orders rejected, fewer fills than attempts

**Verification (full suite):**
```
cargo fmt --all -- --check
cargo clippy --all-targets --workspace
cargo nextest run --workspace
cargo check --all-targets --workspace
cargo bench --no-run
```

---

## 5. Dependencies for `ingot-backtest`

```toml
[dependencies]
ingot-engine = { path = "../ingot-engine" }
ingot-connectivity = { path = "../ingot-connectivity" }
ingot-accounting = { path = "../ingot-accounting" }
ingot-core = { path = "../ingot-core" }
ingot-primitives = { path = "../ingot-primitives" }
chrono = { workspace = true }
rust_decimal = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
smol_str = { workspace = true }
tracing = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
rand = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
rust_decimal_macros = { workspace = true }
tokio = { workspace = true, features = ["test-util", "macros"] }
```

## 6. Critical Files

| File | Action |
|------|--------|
| `Cargo.toml` (root) | Add `ingot-backtest` to workspace members |
| `crates/ingot-connectivity/src/paper/fill_model.rs` | Change `pub(crate)` → `pub` on all 4 functions |
| `crates/ingot-connectivity/src/paper/mod.rs` | Make `fill_model` module `pub` |
| `crates/ingot-backtest/Cargo.toml` | New crate |
| `crates/ingot-backtest/src/lib.rs` | Module declarations + re-exports |
| `crates/ingot-backtest/src/config.rs` | BacktestConfig |
| `crates/ingot-backtest/src/error.rs` | BacktestError |
| `crates/ingot-backtest/src/ledger.rs` | InMemoryLedger (implements LedgerWriter) |
| `crates/ingot-backtest/src/exchange.rs` | BacktestExchange (implements OrderExecutor) |
| `crates/ingot-backtest/src/feed.rs` | OhlcvBar/Tick → BacktestEvent conversion |
| `crates/ingot-backtest/src/runner.rs` | BacktestRunner synchronous event loop |
| `crates/ingot-backtest/src/result.rs` | BacktestResult, EquityPoint |
| `crates/ingot-backtest/src/metrics.rs` | PerformanceMetrics computation |

## 7. Verification (per sub-phase)

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-backtest` — all tests pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run` — all benchmarks compile
6. No `.unwrap()`, `.expect()`, `panic!()`, `todo!()` in production code
