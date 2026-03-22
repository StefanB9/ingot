# Technical Design Document: Phase 1d — Execution Engine

## 1. Context

Phases 1a–1c delivered primitives, storage, broker connectivity (Kraken + PaperExchange), and the accounting layer (double-entry ledger, posting engine, NAV, reconciliation). Phase 1d builds the **execution engine**: the core runtime that orchestrates strategies, enforces risk constraints, manages order lifecycle, and provides scheduling and emergency controls.

The engine is the central nervous system — it wires together connectivity (market data + order execution), accounting (ledger posting), and strategy logic into a single event-driven runtime.

## 2. Crate Structure

New crate `ingot-engine` added to workspace:

```
ingot-primitives (no deps)
    ↓
ingot-core (→ primitives)
    ↓
ingot-accounting (→ core, primitives)
    ↓
ingot-storage (→ core, primitives, accounting)
    ↓
ingot-connectivity (→ core, storage, primitives)
    ↓
ingot-engine (→ connectivity, accounting, core, primitives)  ← NEW
```

Storage interaction is injected via a `LedgerWriter` trait defined in `ingot-engine`, implemented by `PgLedgerRepository` in `ingot-storage`. The engine does NOT depend on `ingot-storage` directly.

## 3. Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Strategy dispatch | Enum dispatch (`StrategyKind`) | Zero overhead; PRD says "binary restart required" to add strategies; matches Rust idioms; no `async_trait` needed |
| Event model | Channel-based `tokio::select!` loop | Matches existing PaperExchange pattern; multiple broadcast/mpsc receivers + timer + shutdown watch |
| Risk gatekeeper | Stateful `PortfolioController` | Maintains running positions, exposure, NAV; avoids re-querying broker on every intention check |
| Storage coupling | `LedgerWriter` trait injection | Engine stays decoupled from Postgres; testable with mocks |
| Scheduler | Simple `tokio::time::interval` | Sufficient for MVP; cron expressions can be added later |
| Kill switch | Cancel all orders + close all positions | Matches PRD "close derivative positions"; triggered via watch channel + OS signals |
| Smart orders | Mid-price from L1 order book | Included in Phase 1d; core to execution quality |

## 4. Sub-Phase Breakdown

### 1d.1: Crate Scaffold + Engine Types + Config
- New `ingot-engine` crate with `cargo new`
- Domain types: `StrategyId`, `OrderIntention`, `RiskDecision`, `EngineEvent`
- Config: `EngineConfig`, `RiskConfig`
- Error type: `EngineError` (thiserror)
- `LedgerWriter` trait

### 1d.2: Strategy Trait + StrategyContext
- `Strategy` trait with lifecycle methods: `init`, `on_ticker`, `on_order_book`, `on_fill`, `on_schedule`, `shutdown`
- `StrategyContext` — read-only view of positions, balances, latest market data
- `StrategyKind` enum dispatch wrapper
- `NoopStrategy` for testing

### 1d.3: PortfolioController (Risk Gatekeeper)
- Stateful risk engine: positions, exposure per currency, NAV tracking
- `check_intention()` → `RiskDecision` (Approve/Reject with reason)
- Risk rules: global stop-loss (NAV threshold), exposure limits per asset/currency, max order size
- `on_fill()`, `on_nav_update()` state mutations

### 1d.4: OrderManager + Smart Limit Orders
- Bridges approved intentions → broker `OrderExecutor`
- Order lifecycle tracking (pending → open → filled/cancelled)
- Smart limit order: compute mid-price from `OrderBookSnapshot`, configurable TIF fallback
- Fill → accounting integration (calls `post_fill` → `LedgerWriter`)

### 1d.5: Engine Orchestrator
- `Engine` struct: wires strategies, controller, order manager, market data feeds
- `tokio::select!` event loop consuming ticker, order book, fills, timer, shutdown
- Strategy registration and lifecycle management
- Event dispatch to strategies and controller

### 1d.6: Scheduler
- `ScheduleConfig` per strategy (interval-based)
- Timer management within the event loop
- Strategy-specific schedule triggers via `on_schedule`

### 1d.7: Kill Switch + Graceful Shutdown
- `KillSwitch` struct with `activate()` method
- Action sequence: halt strategies → cancel all orders → close positions → flush ledger
- OS signal handler (SIGTERM/SIGINT via `tokio::signal`)
- Graceful shutdown with timeout

---

## 5. Detailed Type Definitions

### 5.1 Engine Types (`src/types.rs`)

```rust
/// Unique identifier for a registered strategy.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StrategyId(SmolStr);

impl StrategyId {
    pub fn new(id: &str) -> Result<Self, EngineError>;
    pub fn as_str(&self) -> &str;
}

/// A strategy's request to trade — the controller evaluates this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderIntention {
    pub strategy_id: StrategyId,
    pub request: OrderRequest,
    pub reason: Option<SmolStr>,
}

/// Result of a risk check on an OrderIntention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskDecision {
    Approved,
    Rejected { reason: SmolStr },
}

/// Events flowing through the engine event loop.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Ticker(TickerSnapshot),
    OrderBook(OrderBookSnapshot),
    Fill(OrderFill),
    ScheduleTrigger(StrategyId),
    KillSwitch,
    Shutdown,
}
```

### 5.2 Config (`src/config.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub risk: RiskConfig,
    pub base_currency: Currency,
    pub smart_order: SmartOrderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Halt all trading if NAV drops below this amount.
    pub global_stop_loss: Amount,
    /// Maximum exposure per currency as a fraction of NAV (0.0–1.0).
    pub max_currency_exposure: Percentage,
    /// Maximum exposure per single asset as a fraction of NAV.
    pub max_asset_exposure: Percentage,
    /// Maximum single order size (quote currency value).
    pub max_order_value: Amount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartOrderConfig {
    /// Use mid-price from order book for limit orders.
    pub use_mid_price: bool,
    /// Basis points offset from mid-price (positive = more aggressive).
    pub offset_bps: Decimal,
    /// Fallback to market order after this duration.
    pub fallback_timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    pub strategy_id: StrategyId,
    pub interval: Duration,
}
```

### 5.3 Error Type (`src/error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("strategy not found: {0}")]
    StrategyNotFound(StrategyId),
    #[error("strategy ID cannot be empty")]
    EmptyStrategyId,
    #[error("duplicate strategy ID: {0}")]
    DuplicateStrategyId(StrategyId),
    #[error("risk check rejected: {reason}")]
    RiskRejected { reason: SmolStr },
    #[error("global stop-loss triggered: NAV {nav} below threshold {threshold}")]
    GlobalStopLoss { nav: Amount, threshold: Amount },
    #[error("exposure limit exceeded: {currency} at {current}, max {limit}")]
    ExposureLimitExceeded { currency: Currency, current: Percentage, limit: Percentage },
    #[error("order manager error: {0}")]
    OrderManager(String),
    #[error("engine already running")]
    AlreadyRunning,
    #[error("engine not running")]
    NotRunning,
    #[error("kill switch activated")]
    KillSwitchActivated,
    #[error("smart order: order book empty for {0}")]
    EmptyOrderBook(Symbol),
    #[error("connectivity error: {0}")]
    Connectivity(#[source] anyhow::Error),
    #[error("accounting error: {0}")]
    Accounting(#[from] ingot_accounting::AccountingError),
}
```

### 5.4 Strategy Trait (`src/strategy.rs`)

```rust
/// Read-only snapshot of engine state available to strategies.
pub struct StrategyContext {
    pub positions: Vec<Position>,
    pub balances: Vec<Balance>,
    pub latest_tickers: HashMap<Symbol, TickerSnapshot>,
    pub latest_order_books: HashMap<Symbol, OrderBookSnapshot>,
    pub timestamp: DateTime<Utc>,
}

/// Core strategy interface. Strategies emit OrderIntentions; the engine evaluates and routes them.
pub trait Strategy {
    fn id(&self) -> &StrategyId;
    fn init(&mut self, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_ticker(&mut self, ticker: &TickerSnapshot, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_order_book(&mut self, book: &OrderBookSnapshot, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_fill(&mut self, fill: &OrderFill, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_schedule(&mut self, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn shutdown(&mut self);
}

/// Enum dispatch wrapper for concrete strategy implementations.
pub enum StrategyKind {
    Noop(NoopStrategy),
    // Future variants added here per concrete strategy
}

impl Strategy for StrategyKind {
    // Delegates to inner variant
}
```

### 5.5 PortfolioController (`src/controller.rs`)

```rust
pub struct PortfolioController {
    positions: HashMap<Symbol, Position>,
    exposure_by_currency: HashMap<Currency, Amount>,
    current_nav: Amount,
    config: RiskConfig,
    halted: bool,
}

impl PortfolioController {
    pub fn new(config: RiskConfig) -> Self;
    pub fn check_intention(&self, intention: &OrderIntention) -> RiskDecision;
    pub fn on_fill(&mut self, fill: &OrderFill);
    pub fn on_nav_update(&mut self, nav: Amount);
    pub fn on_position_update(&mut self, positions: &[Position]);
    pub fn is_halted(&self) -> bool;
    pub fn halt(&mut self);
    pub fn current_nav(&self) -> Amount;
}
```

### 5.6 OrderManager (`src/order_manager.rs`)

```rust
pub struct OrderManager {
    tracked_orders: HashMap<OrderId, TrackedOrder>,
    smart_config: SmartOrderConfig,
}

#[derive(Debug, Clone)]
pub struct TrackedOrder {
    pub order_id: OrderId,
    pub intention: OrderIntention,
    pub status: OrderStatus,
    pub submitted_at: DateTime<Utc>,
}

impl OrderManager {
    pub fn new(config: SmartOrderConfig) -> Self;

    /// Compute a smart limit price from order book mid-price + offset.
    pub fn compute_limit_price(
        &self,
        side: OrderSide,
        book: &OrderBookSnapshot,
    ) -> Result<Price, EngineError>;

    /// Submit an approved intention to the broker.
    pub async fn submit_order<E: OrderExecutor>(
        &mut self,
        intention: OrderIntention,
        book: Option<&OrderBookSnapshot>,
        executor: &E,
    ) -> Result<OrderId, EngineError>;

    /// Record a fill against a tracked order.
    pub fn on_fill(&mut self, fill: &OrderFill) -> Option<&TrackedOrder>;

    /// Cancel all tracked open orders.
    pub async fn cancel_all<E: OrderExecutor>(
        &mut self,
        executor: &E,
    ) -> Result<u32, EngineError>;

    /// Get all orders that need TIF fallback (timeout exceeded).
    pub fn stale_orders(&self, now: DateTime<Utc>) -> Vec<OrderId>;
}
```

### 5.7 LedgerWriter Trait (`src/traits.rs`)

```rust
/// Trait for persisting accounting transactions. Implemented by ingot-storage.
pub trait LedgerWriter {
    fn write_transaction(
        &self,
        txn: &Transaction,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}
```

### 5.8 Kill Switch (`src/kill_switch.rs`)

```rust
pub struct KillSwitch {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
}

impl KillSwitch {
    pub fn new() -> Self;
    pub fn activate(&self);
    pub fn subscribe(&self) -> watch::Receiver<bool>;
    pub fn is_activated(&self) -> bool;
}

/// Executes the kill switch action sequence.
pub async fn execute_kill_switch<E: OrderExecutor>(
    executor: &E,
    positions: &[Position],
    order_manager: &mut OrderManager,
) -> Result<(), EngineError>;
```

### 5.9 Engine (`src/engine.rs`)

```rust
pub struct Engine<E, L> {
    strategies: Vec<StrategyKind>,
    controller: PortfolioController,
    order_manager: OrderManager,
    executor: E,
    ledger_writer: L,
    config: EngineConfig,
    kill_switch: KillSwitch,
    schedules: Vec<ScheduleConfig>,
}

impl<E, L> Engine<E, L>
where
    E: OrderExecutor + AccountProvider + Send + Sync,
    L: LedgerWriter + Send + Sync,
{
    pub fn new(
        executor: E,
        ledger_writer: L,
        config: EngineConfig,
    ) -> Self;

    pub fn register_strategy(&mut self, strategy: StrategyKind, schedule: Option<ScheduleConfig>) -> Result<(), EngineError>;

    /// Main event loop — runs until shutdown or kill switch.
    pub async fn run(
        &mut self,
        ticker_rx: broadcast::Receiver<TickerSnapshot>,
        book_rx: broadcast::Receiver<OrderBookSnapshot>,
        fill_rx: mpsc::Receiver<OrderFill>,
    ) -> Result<(), EngineError>;

    pub fn kill_switch(&self) -> &KillSwitch;
}
```

---

## 6. Sub-Phase TDD Steps

### 1d.1: Crate Scaffold + Engine Types + Config

**New files:**
- `crates/ingot-engine/Cargo.toml`
- `crates/ingot-engine/src/lib.rs`
- `crates/ingot-engine/src/types.rs`
- `crates/ingot-engine/src/config.rs`
- `crates/ingot-engine/src/error.rs`
- `crates/ingot-engine/src/traits.rs`

**TDD Steps:**
1. `test_strategy_id_valid` — StrategyId::new("my-strat") succeeds
2. `test_strategy_id_empty_rejected` — StrategyId::new("") → EngineError::EmptyStrategyId
3. `test_strategy_id_display` — Display impl
4. `test_strategy_id_serde_roundtrip`
5. `test_order_intention_construction` — Build with OrderRequest + StrategyId
6. `test_risk_decision_display` — Approved/Rejected display
7. `test_engine_event_variants` — Each EngineEvent variant constructable
8. `test_engine_config_serde_roundtrip`
9. `test_risk_config_serde_roundtrip`
10. `test_smart_order_config_serde_roundtrip`
11. `test_engine_error_display` — All EngineError variants have correct Display
12. `test_engine_error_from_accounting` — AccountingError converts to EngineError

**Verification:** fmt, clippy, nextest, check, bench --no-run

### 1d.2: Strategy Trait + StrategyContext

**New files:**
- `crates/ingot-engine/src/strategy.rs`

**TDD Steps:**
1. `test_strategy_context_construction` — Build StrategyContext with positions, balances, tickers
2. `test_noop_strategy_id` — NoopStrategy returns correct id
3. `test_noop_strategy_init_empty` — init() returns empty vec
4. `test_noop_strategy_on_ticker_empty` — on_ticker() returns empty vec
5. `test_noop_strategy_on_fill_empty` — on_fill() returns empty vec
6. `test_noop_strategy_on_schedule_empty` — on_schedule() returns empty vec
7. `test_strategy_kind_delegates_to_noop` — StrategyKind::Noop delegates all methods correctly
8. `test_strategy_context_latest_ticker_lookup` — Lookup by symbol

**Verification:** fmt, clippy, nextest, check, bench --no-run

### 1d.3: PortfolioController (Risk Gatekeeper)

**New files:**
- `crates/ingot-engine/src/controller.rs`

**TDD Steps:**
1. `test_controller_new_default_state` — Initial state: no positions, nav=0, not halted
2. `test_check_intention_approved_within_limits` — Normal order → Approved
3. `test_check_intention_rejected_halted` — Controller halted → Rejected
4. `test_check_intention_rejected_nav_below_stop_loss` — NAV below threshold → Rejected
5. `test_check_intention_rejected_exposure_limit` — Would exceed currency exposure → Rejected
6. `test_check_intention_rejected_max_order_value` — Order too large → Rejected
7. `test_on_fill_updates_positions` — Fill updates internal position tracking
8. `test_on_nav_update` — NAV update triggers halt if below stop-loss
9. `test_on_position_update` — Bulk position sync recalculates exposure
10. `test_halt_and_is_halted` — halt() sets halted flag
11. **proptest:** `prop_test_approved_orders_within_limits` — Random valid orders within limits always approved (1000 cases)
12. **proptest:** `prop_test_exposure_never_exceeds_limit` — After sequence of fills, exposure stays within configured limits (1000 cases)

**Verification:** fmt, clippy, nextest, check, bench --no-run

### 1d.4: OrderManager + Smart Limit Orders

**New files:**
- `crates/ingot-engine/src/order_manager.rs`

**TDD Steps:**
1. `test_compute_limit_price_buy_mid` — Buy side: (best_bid + best_ask) / 2
2. `test_compute_limit_price_sell_mid` — Sell side: same mid-price
3. `test_compute_limit_price_with_offset` — Mid-price + offset_bps applied correctly
4. `test_compute_limit_price_empty_book_error` — Empty order book → EngineError::EmptyOrderBook
5. `test_submit_order_market` — Market order submitted directly (no smart pricing)
6. `test_submit_order_smart_limit` — Limit order uses computed mid-price
7. `test_on_fill_updates_tracked_order` — Fill marks order as partially/fully filled
8. `test_on_fill_unknown_order_ignored` — Fill for untracked order returns None
9. `test_cancel_all_cancels_tracked` — cancel_all calls executor.cancel_all_orders
10. `test_stale_orders_identifies_expired` — Orders past fallback_timeout are returned
11. **proptest:** `prop_test_mid_price_between_bid_ask` — Computed mid-price always between best bid and best ask (1000 cases)

**Verification:** fmt, clippy, nextest, check, bench --no-run

### 1d.5: Engine Orchestrator

**New files:**
- `crates/ingot-engine/src/engine.rs`

**TDD Steps:**
1. `test_engine_new` — Engine constructs with executor + ledger_writer + config
2. `test_register_strategy` — Register a NoopStrategy, verify it's stored
3. `test_register_duplicate_strategy_rejected` — Same StrategyId twice → error
4. `test_engine_processes_ticker` — Send ticker through channel → strategy.on_ticker called
5. `test_engine_processes_fill` — Fill → post_fill called on ledger_writer, controller updated
6. `test_engine_intention_approved_and_submitted` — Strategy emits intention → controller approves → order submitted
7. `test_engine_intention_rejected` — Strategy emits intention → controller rejects → order NOT submitted
8. `test_engine_shutdown_signal` — Shutdown watch triggers clean exit
9. `test_engine_processes_order_book` — Order book update → strategy.on_order_book called
10. Integration test: `test_engine_end_to_end_with_paper_exchange` — Wire Engine with PaperExchange + mock LedgerWriter, submit fill via strategies, verify ledger writes

**Verification:** fmt, clippy, nextest, check, bench --no-run

### 1d.6: Scheduler

**New files:**
- `crates/ingot-engine/src/scheduler.rs`

**TDD Steps:**
1. `test_schedule_config_construction` — ScheduleConfig with strategy_id + interval
2. `test_schedule_config_serde_roundtrip`
3. `test_scheduler_fires_at_interval` — Timer fires after configured interval, triggers on_schedule
4. `test_scheduler_multiple_strategies_different_intervals` — Two strategies with different intervals fire independently
5. `test_scheduler_stops_on_shutdown` — Shutdown signal stops all timers

**Verification:** fmt, clippy, nextest, check, bench --no-run

### 1d.7: Kill Switch + Graceful Shutdown

**New files:**
- `crates/ingot-engine/src/kill_switch.rs`

**TDD Steps:**
1. `test_kill_switch_initially_inactive` — is_activated() returns false
2. `test_kill_switch_activate` — activate() → is_activated() returns true
3. `test_kill_switch_subscribe` — Subscriber receives activation signal
4. `test_execute_kill_switch_cancels_orders` — Calls cancel_all on executor
5. `test_execute_kill_switch_closes_positions` — Submits market close orders for each position
6. `test_execute_kill_switch_no_positions` — Empty positions → only cancel orders, no close submissions
7. `test_engine_kill_switch_integration` — Kill switch triggers full shutdown sequence in engine
8. `test_graceful_shutdown_on_signal` — SIGTERM handling (test with tokio::signal mock)

**Verification:** fmt, clippy, nextest, check, bench --no-run

---

## 7. Final Verification (per sub-phase)

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-engine` — all tests pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run` — all benchmarks compile
6. No `.unwrap()`, `.expect()`, `panic!()`, `todo!()` in production code

## 8. Critical Files

| File | Action |
|------|--------|
| `Cargo.toml` (root) | Add `ingot-engine` to workspace members |
| `crates/ingot-engine/Cargo.toml` | New crate with dependencies |
| `crates/ingot-engine/src/lib.rs` | Module declarations + re-exports |
| `crates/ingot-engine/src/types.rs` | StrategyId, OrderIntention, RiskDecision, EngineEvent |
| `crates/ingot-engine/src/config.rs` | EngineConfig, RiskConfig, SmartOrderConfig, ScheduleConfig |
| `crates/ingot-engine/src/error.rs` | EngineError |
| `crates/ingot-engine/src/traits.rs` | LedgerWriter trait |
| `crates/ingot-engine/src/strategy.rs` | Strategy trait, StrategyContext, StrategyKind, NoopStrategy |
| `crates/ingot-engine/src/controller.rs` | PortfolioController |
| `crates/ingot-engine/src/order_manager.rs` | OrderManager, TrackedOrder, smart limit pricing |
| `crates/ingot-engine/src/engine.rs` | Engine orchestrator with event loop |
| `crates/ingot-engine/src/scheduler.rs` | Interval-based scheduling |
| `crates/ingot-engine/src/kill_switch.rs` | KillSwitch + execute_kill_switch |

## 9. Dependencies for `ingot-engine`

```toml
[dependencies]
ingot-connectivity = { path = "../ingot-connectivity" }
ingot-accounting = { path = "../ingot-accounting" }
ingot-core = { path = "../ingot-core" }
ingot-primitives = { path = "../ingot-primitives" }
tokio.workspace = true      # sync channels, time, signal
chrono.workspace = true
rust_decimal.workspace = true
serde.workspace = true
serde_json.workspace = true
smol_str.workspace = true
tracing.workspace = true
anyhow.workspace = true
thiserror.workspace = true

[dev-dependencies]
proptest.workspace = true
tokio = { workspace = true, features = ["test-util"] }
rust_decimal_macros.workspace = true
```
