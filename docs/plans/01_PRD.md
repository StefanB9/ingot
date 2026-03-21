# Product Requirements Document (PRD): Ingot Quant & Portfolio Management Suite

## 1. Product Vision
To build a highly performant, memory-safe algorithmic trading and portfolio management system in Rust. The system will bridge traditional finance across all asset classes (Equities, Options, Futures, Forex, Fixed Income via Interactive Brokers) and cryptocurrency (Kraken). It provides a unified, institutional-grade suite to research, backtest, deploy, and autonomously manage trading strategies, portfolio allocations, and derivative lifecycles.

## 2. Target Architecture Overview
The system relies on a strict separation of concerns between execution and management:
* **The Execution Engine (Data/Execution Plane):** A standalone, headless Rust daemon deployed on a VPS/Server. It handles live WebSocket/REST connections to brokers, maintains the internal ledger, executes orders, and runs deployed strategies autonomously 24/7.
* **The Control Plane (CLI & GUI):** The interface used to interact with the Execution Engine. Used for backtesting, visualizing portfolio performance, managing risk parameters, and deploying live strategies.

## 3. Core Features: Phase 1 (MVP)

Phase 1 is broken into incremental sub-phases, each building on the previous:

| Sub-phase | Scope | Key Deliverables |
|-----------|-------|-----------------|
| **1a** | Primitives & Storage | Core domain types, newtypes, TimescaleDB schema, sqlx, tracing |
| **1b** | Connectivity — Kraken | REST/WS client (spot/futures/margin), historical data pipeline, PaperExchange |
| **1c** | Accounting | Multi-currency double-entry ledger, NAV, broker reconciliation |
| **1d** | Execution Engine | Strategy trait, portfolio controller, order execution, scheduler, kill switch |
| **1e** | Backtesting | Event-driven backtester, performance metrics, historical replay |
| **1f** | IBKR Integration | TWS adapter (all asset classes), derivative rollovers, corporate actions |
| **1g** | Control Plane | axum REST/WS API, auth/authz, SvelteKit dashboard, charts |
| **1h** | Ops & Observability | Structured logging, push notifications, data retention, deployment |

### 3.1. Universal Portfolio Management & Execution
* **Multi-Broker Integration:** Full API integration for Interactive Brokers (covering all supported asset classes) and Kraken (Spot, Futures, and Margin trading).
* **Universal Instrument Modeling:** A polymorphic `Instrument` abstraction that standardizes the handling of Equities, FX, and complex Derivatives (handling multipliers, expirations, strikes, and base currencies).
* **Static Rebalancing Engine:** Ability to define target portfolio weights across asset classes and currencies (e.g., 40% SPY, 10% EUR.USD, 20% ES Futures, 30% BTC).
* **Time-Based Execution:** Cron-style job scheduler to trigger portfolio rebalancing.

### 3.2. Derivative Lifecycle Management
* **Autonomous Rollovers:** The execution engine automatically monitors expiring contracts (Futures, Options) and executes rollover logic based on strategy parameters (e.g., rolling to the front month when volume shifts or $X$ days to expiration).
* **Corporate Action Handling:** Graceful handling of standard events (dividends, splits) within the backtester and live portfolio valuation.

### 3.3. Institutional Accounting & Reconciliation
* **Multi-Currency Double-Entry Ledger:** An immutable, double-entry internal accounting system tracking all transactions (trades, fees, funding rates, interest) in their native currencies.
* **Live Base-Currency Valuation (NAV):** Real-time calculation of the portfolio's Net Asset Value in a single user-defined base currency (e.g., USD), using live FX rates.
* **Broker Reconciliation:** Continuous background syncing between the internal ledger and the broker's actual reported balances to immediately detect and flag accounting drift or "ghost" executions.

### 3.4. Quant Suite & Analytics
* **Backtesting Engine:** An event-driven backtester to simulate strategies against historical data.
* **Strategy Interface:** A modular Rust trait/interface allowing new strategies to be written and plugged into the system easily.
* **Performance Benchmarking:** Calculation of key metrics (CAGR, Max Drawdown, Sharpe Ratio, Volatility).
* **Visualization:** Graphical plotting of equity curves, asset allocation, and benchmark comparisons.

## 4. System Architecture & Tech Stack
* **Core Engine:** Rust (`tokio` for async runtime, `tracing` for high-performance structured logging).
* **Data Persistence:** * **TimescaleDB** (running locally on the VPS) for high-performance storage of historical OHLCV data, tick data, and live portfolio state.
  * **Database ORM/Driver:** `sqlx` for compile-time checked SQL queries and async performance.
* **Control Plane (UI):** A web-based dashboard for remote management.
  * **Backend API:** `axum` serving REST endpoints and WebSockets (live updates). Tower-based middleware for auth, logging, and rate limiting.
  * **Frontend:** SvelteKit with TradingView Lightweight Charts and Tailwind CSS.
* **Inter-Process Communication:** The `axum` server and the Core Execution Engine run within the same Rust binary/workspace, sharing state safely via `tokio::sync` channels.

## 5. Risk Management & Execution Hierarchy
* **Level 1: Strategy Instances:** Isolated, modular algorithms that emit *intentions* (e.g., "Strategy A wants to buy 1 ES Future").
* **Level 2: The Controller (Portfolio Manager):** The centralized risk gatekeeper that intercepts all intentions, evaluates them against global constraints, assesses margin impact, and routes them.
* **Core Risk Constraints:**
  * **Global Stop-Loss:** Liquidate or halt trading if total NAV drops below a specific threshold.
  * **Margin Monitoring:** Real-time tracking of Initial and Maintenance Margin requirements across all broker accounts to prevent forced liquidations.
  * **Exposure Limits:** Prevent any single asset or currency from exceeding a defined percentage.
  * **Kill Switch:** A hard-stop API endpoint triggered via the UI to immediately cancel open orders, close derivative positions, and halt the engine.

## 6. Data Pipeline
* **Historical Data:** Dedicated Rust worker threads that connect to external APIs to download historical data and batch-insert it into TimescaleDB Hypertables.
* **Live Data:** WebSocket connections to stream real-time price updates into the Strategies and append them to TimescaleDB.

## 7. Strategy Lifecycle & Deployment Pipeline
* **Phase 1: Code-Level (MVP):** Strategies written directly in Rust, implementing a core `Strategy` trait. Updating or adding a strategy requires a binary restart. The engine must handle this gracefully: drain open orders, persist strategy state, and reconnect WebSocket streams on restart.
* **Phase 2: Configuration-Driven:** The compiled Rust strategy accepts a configuration file (TOML/JSON) for hot-reloading parameters without restart.
* **Phase 3: Hybrid Scripting (Future):** Integration of a scripting engine (`mlua` or `rhai`) allowing custom logic injection via the web UI.

## 8. Order Execution Mechanics
* **Fractional Support:** Engine natively calculates and supports fractional shares (for crypto/equities allowed by the broker).
* **Smart Limit Orders:** Default execution uses L1 Order Book data to calculate smart limit prices (e.g., mid-price), with configurable Time-In-Force (TIF) fallback logic.
* **Derivative Routing:** Native support for multi-leg option orders, combo routing, and future spread executions via IBKR.

## 9. Observability, Logging & Alerting
* **Structured Logging:** High-performance, machine-readable JSON logs for all system events.
* **Push Notification Alerting:** Integration with a push notification provider (e.g., Pushover, Telegram Bot).
  * **INFO:** Routine rebalancing, rollovers completed, daily ledger reconciliation passed.
  * **WARNING:** Minor API disconnects, ledger drift < 0.1% detected.
  * **CRITICAL:** API Key Expired, Margin Call Warning, Kill Switch Activated.

## 10. Paper Trading & Simulation
* **PaperExchange Adapter:** A simulated exchange adapter implementing the same broker trait interface as live adapters. Simulates order fills (market fills instantly, limit fills when price crosses) without any broker connectivity.
* **Strategy Validation:** All strategies must be validated against the PaperExchange before live deployment.
* **Realistic Simulation:** Configurable slippage, latency, and partial fill modeling.

## 11. Authentication & Authorization
* **API Authentication:** JWT or API-key based authentication for all Control Plane endpoints. The web UI controls real capital — unauthenticated access is not permitted.
* **Role-Based Access:** At minimum, distinguish between read-only (monitoring) and read-write (trading, configuration) access levels.
* **Session Management:** Secure token rotation, expiration, and revocation.

## 12. Data Retention & Backup
* **Tick Data Retention:** Configurable retention window (default: 90 days). Automated cleanup via TimescaleDB continuous aggregate policies and data retention policies.
* **OHLCV Retention:** Indefinite retention. Compressed via TimescaleDB native compression after aging threshold.
* **Ledger Backup:** The double-entry ledger is append-only and must be backed up on a configurable schedule (e.g., daily pg_dump or WAL-based streaming replication).
* **Disaster Recovery:** Documented procedure to restore the engine from a database backup, including ledger integrity verification.

## 13. Deployment
* **Containerization:** The execution engine and TimescaleDB are deployed as Docker containers via Docker Compose on the target VPS.
* **Graceful Shutdown:** On SIGTERM, the engine drains open orders, persists strategy state, and flushes pending ledger entries before stopping.
* **Health Checks:** HTTP health endpoint exposed for container orchestration and external monitoring.