# Product Requirements Document (PRD)

**Project Name:** Ingot — Portfolio Management & Systematic Trading Workstation
**Date:** February 28, 2026
**Version:** 1.0

---

## 1. Executive Summary

Ingot is a high-performance portfolio management and systematic trading workstation built in Rust. It unifies multi-broker connectivity, double-entry accounting, and automated strategy execution into a single platform designed for long-term fundamental and quantitative strategies.

The system is **not** a high-frequency trading platform. It targets disciplined, systematic trading across mixed time horizons (days to months) with an emphasis on correctness, reliability, and engineering quality over raw latency competition. Performance remains a first-class engineering value — zero-waste architecture, sub-10ms tick-to-order where possible, and high throughput — but the competitive edge comes from strategy quality and robust risk management, not co-location speed.

**Key differentiators:**
- **Multi-broker/exchange:** Unified interface across crypto exchanges (Kraken first) and traditional brokers (Interactive Brokers), extensible to any venue.
- **Portfolio-level optimization:** Strategies emit signals; a portfolio optimizer (mean-variance, risk parity, Kelly criterion) makes final allocation decisions.
- **Full audit trail:** Every order, fill, position change, and portfolio event is logged immutably for regulatory compliance and strategy analysis.
- **Equal CLI + GUI:** Both interfaces are first-class citizens, updated in lockstep with every feature for immediate manual testing.

---

## 2. System Scope

### 2.1 Asset Classes

| Asset Class | Phase | Notes |
|:---|:---|:---|
| Crypto Spot | Phase 1 (active) | Kraken initially, extensible to other CEXs |
| Equities & ETFs | Phase 3 | Via Interactive Brokers |
| Options | Phase 4 | Via IBKR; pricing models + Greeks |
| Futures & Perpetuals | Phase 4 | Via IBKR + crypto exchanges |
| Forex | Phase 4 | Via IBKR; also needed for cross-rate synthesis |
| Commodities | Phase 5 | Via IBKR |
| DeFi / On-Chain | Phase 6+ | Deferred. EVM + Solana DEX integration. |

### 2.2 Trading Modes

- **Automated (primary):** Strategies run continuously, emit signals, portfolio optimizer executes. The core use case.
- **Discretionary (manual):** Manual order entry via CLI or GUI for tactical overrides and testing.
- **Hybrid:** Manual overrides on top of automated strategies (e.g., pause a strategy, adjust position limits).

### 2.3 Strategy Horizons

The platform supports mixed time horizons within a single portfolio:

- **Swing (days to weeks):** Momentum, mean reversion, breakout systems. Requires intraday data.
- **Position (weeks to months):** Factor-based, value, macro. Requires daily bars + fundamental data.
- **Strategies may overlap instruments.** Multiple strategies can hold positions in the same asset simultaneously. The portfolio optimizer reconciles conflicting signals.

### 2.4 Deployment Model

Single-machine deployment. The server daemon, storage, and client interfaces (CLI/GUI) all run on the same host. The iceoryx2 shared-memory IPC architecture serves this model. Network transport is not in scope but the IPC abstraction layer should not preclude it.

---

## 3. Functional Requirements

### 3.1 Portfolio Management System (PMS)

The PMS is the "Ledger of Truth" — the authoritative record of all positions, balances, and performance.

* **FR-PMS-01: Double-Entry Multi-Currency Accounting**
    * All asset movements tracked via double-entry bookkeeping across arbitrary currency denominations (fiat and crypto).
    * **Cross-Rate Resolution:** Automatic synthesis of cross-rates using real-time market feeds (e.g., valuing JPY-denominated assets against a USD base currency).
    * **Decimal Precision:** All monetary values use `rust_decimal::Decimal` (128-bit fixed-point). No floating-point arithmetic in financial calculations.

* **FR-PMS-02: Performance Metrics**
    * Real-time NAV (Net Asset Value) calculation across all positions and currencies.
    * Risk-adjusted return metrics: Sharpe Ratio, Sortino Ratio, Calmar Ratio, Maximum Drawdown.
    * **Attribution Analysis:** Decompose returns by asset class, strategy, currency exposure, and time period.

* **FR-PMS-03: Audit Trail**
    * Every order submission, fill, cancellation, position change, and ledger transaction must be logged with timestamp, source (strategy/manual), and full context.
    * Audit log must be append-only and tamper-evident.
    * Exportable in standard formats (CSV, JSON) for external review.

* **FR-PMS-04: Tax Lot Tracking**
    * Support for FIFO, LIFO, HIFO, and Specific Lot identification methods.
    * Exportable trade history with cost basis and realized gain/loss for tax filing.

### 3.2 Connectivity Layer

* **FR-CON-01: Exchange Trait Abstraction**
    * All broker/exchange integrations implement a unified `Exchange` trait covering: authentication, market data subscription, order submission, order status, position queries, and balance queries.
    * The trait must be generic enough to support WebSocket-based crypto exchanges, FIX-protocol brokers, and REST-only APIs without leaking protocol details.
    * **Extensibility is paramount.** Adding a new broker/exchange should require implementing the trait and writing an adapter — no changes to engine, strategies, or portfolio logic.

* **FR-CON-02: Kraken Integration (Phase 1)**
    * Full spot trading: market and limit orders, order status, balance queries.
    * Real-time WebSocket feed: ticker updates, trade stream.
    * REST API: order placement, account info, historical OHLCV data.
    * **Recommended crates:** `tokio-tungstenite` (WebSocket), `reqwest` (REST), `hmac`/`sha2` (auth signing).

* **FR-CON-03: Interactive Brokers Integration (Phase 3)**
    * Gateway integration via IBKR's Client Portal API or TWS API.
    * Support for equities, options, futures, and forex order routing.
    * Real-time market data and historical bar requests.
    * **Rationale:** IBKR provides the broadest multi-asset coverage of any retail broker, with global market access.
    * **Gateway Abstraction:** Session management and protocol complexity (TWS socket protocol) must not block the core async event loop. Run IBKR gateway in a dedicated task with channel-based communication to the engine.

* **FR-CON-04: Market Data Normalization**
    * All venue-specific data formats must be normalized into unified internal types before reaching the engine.
    * **Data hierarchy (designed for all, implement incrementally):**
        * **L1 (Phase 1):** Best bid/ask, last trade price, volume — sufficient for most swing/position strategies.
        * **L2 (Phase 3+):** Order book depth (top N levels) — needed for execution quality analysis.
        * **L3 (Phase 5+):** Full depth snapshots + trade-by-trade — microstructure analysis.
        * **Fundamental (Phase 4+):** Earnings, financial ratios, macro indicators — value/macro strategies.
    * **OHLCV Bars:** Historical candle data ingestion and storage for backtesting and indicator computation.

* **FR-CON-05: Additional Exchange Adapters (Future)**
    * The architecture must support future adapters for: Binance, Coinbase, Bybit, Alpaca, and others.
    * Each adapter is an independent module implementing the `Exchange` trait.

### 3.3 Strategy Framework

* **FR-STR-01: Strategy Trait**
    * Strategies implement a Rust `Strategy` trait with lifecycle hooks: `on_tick`, `on_bar`, `on_fill`, `on_timer`.
    * Strategies emit **signals** (target position, conviction score) rather than raw orders. The portfolio optimizer translates signals into orders.
    * Strategies are compiled into the binary for maximum performance. No runtime interpretation overhead.

* **FR-STR-02: Configuration-Driven Parameters**
    * Strategy parameters (thresholds, lookback periods, position limits) are defined in TOML configuration files.
    * Parameter changes do not require recompilation — the engine reloads config on signal or command.
    * **Schema validation:** Each strategy defines its parameter schema; invalid configs are rejected at load time, not at runtime.

* **FR-STR-03: Supported Strategy Types**
    * The framework must be expressive enough for:
        * **Factor/signal-based:** Cross-sectional scoring, ranking, periodic rebalancing.
        * **Rules-based systematic:** Deterministic entry/exit rules (moving averages, RSI, breakouts).
        * **ML/statistical models:** Feature pipelines feeding trained models that output position targets or signals.
    * No strategy type is privileged in the architecture — all use the same trait and signal interface.

* **FR-STR-04: Scripting Layer (Future)**
    * An optional embedded scripting language (e.g., Rhai or Lua) for rapid strategy prototyping without recompilation.
    * Scripted strategies use the same signal interface as compiled strategies.
    * Deferred to a late phase. The Rust trait interface must be designed so scripting wraps it cleanly.

### 3.4 Portfolio Optimizer

* **FR-OPT-01: Signal Aggregation**
    * Receive target position signals from all active strategies.
    * Reconcile conflicting signals on the same instrument (e.g., one strategy is long, another is short).

* **FR-OPT-02: Optimization Engine**
    * Compute optimal portfolio allocation using quantitative methods:
        * **Mean-Variance Optimization:** Markowitz-style, with configurable risk aversion parameter.
        * **Risk Parity:** Equal risk contribution across assets or strategies.
        * **Kelly Criterion:** Optimal position sizing based on edge and variance estimates.
    * The optimizer runs after signal aggregation and before order generation.
    * **Recommended crates:** `nalgebra` (linear algebra), `argmin` (optimization framework). Custom implementations preferred over opaque libraries for auditability.

* **FR-OPT-03: Constraint Enforcement**
    * The optimizer enforces hard constraints before emitting orders:
        * Maximum position size per asset.
        * Maximum total portfolio exposure.
        * Maximum single-order size.
        * Strategy-level allocation limits.
    * Constraint violations are logged and the optimizer degrades gracefully (reduces position rather than rejecting entirely).

### 3.5 Risk Management

* **FR-RSK-01: Kill Switch (Phase 1)**
    * Immediate cancellation of all open orders and halt of all strategy execution on command (CLI, GUI, or programmatic trigger).
    * Triggered manually or automatically on connectivity loss (Dead Man's Switch).

* **FR-RSK-02: Order-Level Guards (Phase 1)**
    * Maximum single-order size (configurable per asset).
    * Maximum order rate (orders per minute) to prevent runaway strategies.
    * Fat-finger protection: reject orders that deviate >X% from current market price.

* **FR-RSK-03: Portfolio-Level Risk (Future)**
    * Drawdown limits with automatic deleveraging.
    * Value-at-Risk (VaR) monitoring.
    * Correlation-based exposure alerts.
    * Per-strategy risk budgets.

### 3.6 Autotrading Engine

* **FR-ATE-01: Async Event Loop**
    * Non-blocking, `tokio`-based runtime. Market data ingestion decoupled from strategy execution via channels.
    * Lock-free messaging where possible. Named channel capacity constants.
    * The engine must handle burst throughput (100,000+ ticks/sec) without data loss.

* **FR-ATE-02: Execution Pipeline**
    * Pipeline: Market Data → Normalization → Strategy Dispatch → Signal Aggregation → Portfolio Optimization → Order Generation → Execution → Fill Processing → Ledger Update.
    * Each stage is independently benchmarked. No stage may introduce unnecessary allocations.

* **FR-ATE-03: Paper Trading**
    * Built-in paper exchange (simulated fills) for strategy testing on live market data without real capital.
    * Paper exchange must faithfully simulate: fill latency, partial fills, and order rejection scenarios.

### 3.7 Backtesting Engine (Phase 5)

* **FR-BT-01: Historical Simulation**
    * Replay historical market data through the same strategy and portfolio optimization pipeline used in live trading.
    * Strategies should not know whether they are running live or in backtest — same trait, same interface.

* **FR-BT-02: Simulation Fidelity**
    * Configurable fill models: immediate fill, latency-adjusted, queue-position-based.
    * Slippage and commission modeling.
    * Transaction cost analysis.

* **FR-BT-03: Results & Analysis**
    * Equity curve, drawdown chart, trade log.
    * Performance metrics matching live PMS metrics (Sharpe, Sortino, etc.).
    * Exportable results for comparison across strategy variants.

### 3.8 User Interfaces

* **FR-UI-01: Interface Parity**
    * CLI and GUI are developed in lockstep. Every engine feature gets corresponding controls and displays in **both** interfaces in the same development phase.
    * Both interfaces connect to the engine via iceoryx2 shared-memory IPC. They are thin clients — no business logic in the UI layer.

* **FR-UI-02: Command Line Interface (CLI)**
    * REPL-based control: start/stop engine, place manual orders, query positions and balances, audit ledger, manage strategies.
    * Scriptable: commands can be piped or scripted for automation.
    * Tabular output for positions, orders, and performance data.
    * **Recommended crate:** `clap` (argument parsing), `ratatui` for rich terminal rendering if needed.

* **FR-UI-03: Graphical User Interface (GUI)**
    * Real-time dashboard: NAV, open positions, open orders, connection status, strategy states.
    * Order entry panel for manual/discretionary trading.
    * Charting: price charts with indicator overlays, equity curve, P&L breakdown.
    * Strategy monitor: per-strategy signals, positions, and performance.
    * **Recommended framework:** `egui` (immediate-mode GUI). Rationale: pure Rust, cross-platform, GPU-accelerated, no C++ bindings.

### 3.9 Derivatives & Volatility Analytics (Phase 4)

* **FR-DER-01: Options Pricing Models**
    * Black-Scholes and binomial tree pricing for vanilla options.
    * Real-time Greeks calculation: Delta, Gamma, Theta, Vega, Rho.

* **FR-DER-02: Volatility Surfaces**
    * Implied volatility surface construction from option chain data.
    * Real-time surface updates as new quotes arrive.
    * GUI visualization of volatility surfaces.

### 3.10 Wallet & Custody (Phase 6+, Deferred)

* **FR-WAL-01: HD Wallet Management**
    * BIP-39/BIP-44 hierarchical deterministic key management for multi-chain support.
    * Encrypted key storage with zeroized memory after signing.
    * Deferred alongside DeFi integration.

---

## 4. Non-Functional Requirements

### 4.1 Performance

Performance is a first-class engineering value. The system is not competing on HFT latency but refuses to waste cycles.

* **NFR-PERF-01: Tick-to-Order Latency**
    * Target: sub-10ms from market data arrival to order submission under normal conditions.
    * Zero-copy deserialization for market data. Stack-allocated types for hot-path data structures.

* **NFR-PERF-02: Throughput**
    * The ingestion pipeline must sustain 100,000+ ticks/sec burst throughput without backpressure data loss.
    * Channel-based architecture with named capacity constants to prevent unbounded memory growth.

* **NFR-PERF-03: Zero-Waste Architecture**
    * No unnecessary heap allocations in hot paths (tick processing, strategy dispatch, order routing).
    * `rust_decimal::Decimal` for all financial math — never `f64`.
    * Borrowed lifetimes over cloning. `Cow<'static, str>` for usually-static strings.
    * Pre-allocated buffers. `SmallVec` for bounded collections. Stack arrays for fixed-size identifiers.
    * Every hot-path function has a corresponding benchmark. No merge if existing benchmarks regress >5%.

### 4.2 Reliability & Safety

* **NFR-REL-01: Panic-Free Execution**
    * The trading daemon must never panic. All arithmetic is fallible (`checked_*` or `Decimal`). `.unwrap()`, `.expect()`, and `panic!()` are forbidden at the lint level across all targets (lib, bin, tests, benchmarks).

* **NFR-REL-02: Dead Man's Switch**
    * Automatic cancellation of all open orders if exchange connectivity or data feed is lost for a configurable duration.

* **NFR-REL-03: Graceful Degradation**
    * If one exchange adapter fails, the engine continues operating with remaining connections.
    * Strategy errors are isolated — a single strategy fault does not halt the engine.

* **NFR-REL-04: Deterministic Execution**
    * Fixed-point arithmetic ensures identical results across CPU architectures.
    * Strategy execution order is deterministic for reproducible behavior.

### 4.3 Data Storage

* **NFR-DATA-01: Time-Series Storage**
    * High-volume market data (ticks, OHLCV bars, depth snapshots) stored in a columnar database optimized for compression and analytical queries.
    * **Recommended:** QuestDB (high-ingestion columnar DB with SQL interface, native Rust client via ILP). Rationale: purpose-built for time-series financial data, excellent compression, fast aggregation queries.

* **NFR-DATA-02: Transactional Storage**
    * Portfolio state, trade history, audit log, configuration, and strategy metadata stored in an ACID-compliant relational database.
    * **Recommended:** PostgreSQL (mature, ACID, excellent Rust ecosystem via `sqlx`). Rationale: battle-tested for financial data, strong transaction guarantees, rich query capabilities.

* **NFR-DATA-03: Audit Persistence**
    * The audit trail (FR-PMS-03) must be persisted to the transactional database with append-only semantics.
    * Audit records must survive application restarts and crashes.

### 4.4 Observability

* **NFR-OBS-01: Structured Logging**
    * All components emit structured traces via `tracing` with `#[instrument]` on public async functions.
    * Log levels: `error` (failures), `warn` (degraded state), `info` (lifecycle), `debug` (protocol detail).
    * Configurable via `RUST_LOG` environment variable.

* **NFR-OBS-02: Metrics**
    * Key operational metrics exposed for monitoring: tick rate, order latency, strategy cycle time, queue depths, error rates.

### 4.5 Security

* **NFR-SEC-01: Credential Management**
    * API keys and secrets loaded from environment variables or encrypted config files. Never hardcoded, never logged.

* **NFR-SEC-02: Memory Safety**
    * `unsafe` code is forbidden at the lint level. Rust's ownership model is the primary security mechanism.

---

## 5. Architectural Components

| Component | Responsibility | Key Characteristics | Recommended Technology |
|:---|:---|:---|:---|
| **Engine Core** | Event loop, strategy dispatch, signal processing | Async (`tokio`), channel-based, deterministic | `tokio`, broadcast/mpsc channels |
| **Portfolio Optimizer** | Signal aggregation, allocation, constraint enforcement | Mean-variance, risk parity, Kelly | `nalgebra`, `argmin` |
| **Ledger** | Double-entry accounting, NAV, audit trail | Multi-currency, `Decimal`, append-only audit | `rust_decimal` |
| **Connectivity** | Exchange/broker adapters, data normalization | Modular `Exchange` trait, WebSocket/REST/FIX | `tokio-tungstenite`, `reqwest` |
| **Data Plane** | Feed handling, normalization, distribution | Zero-copy parsing, lock-free queues | Stack-allocated types, channels |
| **Storage** | Persistence for time-series + transactional data | Hybrid: columnar + relational | QuestDB (ILP), PostgreSQL (`sqlx`) |
| **IPC Layer** | Client-server communication | Shared-memory, zero-copy | `iceoryx2` |
| **CLI** | Terminal-based control & monitoring | REPL, tabular output, scriptable | `clap`, `ratatui` |
| **GUI** | Visual workstation & dashboards | Real-time charts, order entry, strategy monitor | `egui` |

---

## 6. Implementation Roadmap

### Phase 1: Foundation & Live Crypto Trading
*Status: Largely complete. Refine and harden.*

- Double-entry accounting engine with multi-currency support.
- Kraken spot trading: WebSocket feed, REST order execution, paper exchange.
- Engine event loop with strategy trait and channel-based architecture.
- Client-server decoupling via iceoryx2 shared-memory IPC.
- Kill switch and basic order-level guards.
- **UI:** CLI REPL for balance queries, manual orders, engine control. GUI dashboard with NAV, ticker, and order panels.

### Phase 2: Persistent Storage & Audit Trail

- PostgreSQL integration for portfolio state, trade history, and audit log.
- QuestDB integration for time-series market data (ticks, OHLCV bars).
- Historical data ingestion pipeline (Kraken REST historical bars).
- Audit trail: append-only log of all orders, fills, and position changes.
- Portfolio state recovery on restart from persistent storage.
- **UI:** CLI commands for audit log queries and trade history export. GUI trade log view with filtering.

### Phase 3: Multi-Broker & Equities

- Interactive Brokers adapter implementing the `Exchange` trait.
- Equity and ETF trading via IBKR (US markets initially).
- Cross-rate synthesis for multi-currency portfolio valuation.
- L2 order book data support in the data normalization layer.
- **UI:** CLI connection management for multiple brokers. GUI multi-venue connection panel, per-venue order routing.

### Phase 4: Portfolio Optimizer & Derivatives

- Portfolio optimization engine: mean-variance, risk parity, Kelly criterion.
- Strategy signal aggregation and conflict resolution.
- Options pricing models (Black-Scholes, binomial) and Greeks.
- Implied volatility surface construction.
- IBKR options and futures trading.
- Performance metrics: Sharpe, Sortino, Calmar, drawdown, attribution analysis.
- **UI:** CLI strategy signal viewer, optimizer status. GUI volatility surface visualization, performance dashboard, attribution charts.

### Phase 5: Backtesting & Advanced Risk

- Backtesting engine: historical replay through live strategy pipeline.
- Fill simulation models (latency-adjusted, queue-position).
- Slippage and commission modeling.
- Portfolio-level risk management: VaR, drawdown limits, automatic deleveraging.
- Per-strategy risk budgets.
- Tax lot tracking (FIFO, LIFO, HIFO, Specific Lot) and tax report export.
- **UI:** CLI backtesting commands and result summaries. GUI equity curve plotting, trade replay, risk dashboard.

### Phase 6: DeFi & On-Chain Integration (Deferred)

- EVM integration (Ethereum, Arbitrum, Base): DEX swaps, liquidity monitoring.
- Solana integration: on-chain order book and AMM execution.
- HD wallet management (BIP-39/BIP-44) with encrypted key storage.
- On-chain position tracking integrated into the portfolio ledger.
- **UI:** CLI wallet commands, on-chain transaction status. GUI DeFi position panel, wallet management.

### Phase 7: Scripting & Extensibility (Deferred)

- Embedded scripting language (Rhai or Lua) for strategy prototyping.
- Scripted strategies use the same signal interface as compiled strategies.
- Additional exchange adapters (Binance, Coinbase, Bybit, Alpaca).
- Plugin architecture for custom data sources and indicators.

---

## 7. Success Criteria

| Criterion | Measure |
|:---|:---|
| **Correctness** | Zero ledger discrepancies. Double-entry invariants hold under all conditions. |
| **Reliability** | Trading daemon runs for weeks without restart. No panics, no data loss. |
| **Performance** | Sub-10ms tick-to-order. 100K+ ticks/sec sustained. Zero hot-path allocations. |
| **Extensibility** | Adding a new exchange adapter requires only trait implementation — no engine changes. |
| **Auditability** | Complete, tamper-evident history of every financial event. Exportable for tax/compliance. |
| **Usability** | Both CLI and GUI provide full feature access. Manual testing possible at every development phase. |
