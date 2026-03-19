# Product Requirements Document (PRD): Rust Quant & Portfolio Management Suite
## 1. Product Vision
To build a highly performant, memory-safe algorithmic trading and portfolio management system in Rust. The system will bridge traditional finance (Interactive Brokers) and cryptocurrency (Kraken), providing a unified suite to research, backtest, deploy, and monitor trading strategies and portfolio allocations.

## 2. Target Architecture Overview
The system relies on a strict separation of concerns between execution and management:
* **The Execution Engine (Data/Execution Plane):** A standalone, headless Rust daemon deployed on a VPS/Server. It handles live WebSocket/REST connections to brokers, maintains state, executes orders, and runs deployed strategies autonomously 24/7.
* **The Control Plane (CLI & GUI):** The interface used to interact with the Execution Engine. Used for backtesting new strategies, visualizing portfolio performance, generating reports, managing risk parameters, and deploying/stopping live strategies.

## 3. Core Features: Phase 1 (MVP)

### 3.1. Portfolio Management & Execution
* **Dual-Broker Integration:** Initial integrations for Interactive Brokers (Equities/ETFs/Futures) and Kraken (Crypto).
* **Static Rebalancing Engine:** Ability to define target portfolio weights (e.g., 60% SPY, 40% BTC).
* **Time-Based Execution:** Cron-style job scheduler to trigger portfolio rebalancing (e.g., the 1st of every month).

### 3.2. Quant Suite & Analytics
* **Backtesting Engine:** An event-driven backtester to simulate strategies against historical data, ensuring the logic used in testing is identical to live trading.
* **Strategy Interface:** A modular Rust trait/interface allowing new strategies to be written and plugged into the system easily.
* **Performance Benchmarking:** Calculation of key metrics for both backtests and live portfolios (CAGR, Max Drawdown, Sharpe Ratio, Volatility).
* **Visualization:** Graphical plotting of portfolio equity curves, asset allocation pie charts, and benchmark comparisons.

## 4. System Architecture & Tech Stack
* **Core Engine:** Rust (`tokio` for async runtime, `tracing` for high-performance structured logging).
* **Data Persistence:** * **TimescaleDB** (running locally on the VPS) for high-performance storage of historical OHLCV data, tick data, and live portfolio state.
  * **Database ORM/Driver:** `sqlx` for compile-time checked SQL queries and async performance.
* **Control Plane (UI):** A web-based dashboard for remote management.
  * **Backend API:** `actix-web` serving REST endpoints (historical data/settings) and WebSockets (live updates).
  * **Frontend:** Svelte (or SvelteKit) with TradingView Lightweight Charts for a low-latency, highly reactive web dashboard.
* **Inter-Process Communication:** The `actix-web` server and the Core Execution Engine run within the same Rust binary/workspace, sharing state safely via `tokio::sync` channels (e.g., `mpsc` for UI-to-engine commands, `broadcast` for market data to UI).

## 5. Risk Management & Execution Hierarchy
The system utilizes a hierarchical execution model to separate alpha generation from risk management:
* **Level 1: Strategy Instances:** Isolated, modular algorithms that consume market data and emit *intentions* (e.g., "Strategy A wants to buy 10 shares of AAPL").
* **Level 2: The Controller (Portfolio Manager):** The centralized risk gatekeeper that intercepts all strategy intentions, evaluates them against global constraints, and routes them to the broker.
* **Core Risk Constraints:**
  * **Global Stop-Loss:** Liquidate or halt trading if the total portfolio drops below a specific fiat value or percentage.
  * **Exposure Limits:** Prevent any single asset from exceeding a defined percentage of the total portfolio.
  * **Kill Switch:** A hard-stop API endpoint triggered via the UI to immediately cancel all open orders and halt the engine.

## 6. Data Pipeline
* **Historical Data:** Dedicated Rust worker threads that connect to external APIs to download historical data and batch-insert it into TimescaleDB.
* **Live Data:** WebSocket connections to Interactive Brokers and Kraken to stream real-time price updates into the Strategies and append them to TimescaleDB.

## 7. Strategy Lifecycle & Deployment Pipeline
The system supports multiple strategy definitions, rolling out in phases:
* **Phase 1: Code-Level (MVP):** Strategies are written directly in Rust, implementing a core `Strategy` trait. Requires a recompile and restart of the execution engine. Maximizes execution speed and type safety.
* **Phase 2: Configuration-Driven:** The compiled Rust strategy accepts a configuration file (e.g., TOML/JSON). Users can update target portfolio weights via the Svelte UI, and the engine hot-reloads parameters without restarting.
* **Phase 3: Hybrid Scripting (Future):** Integration of a scripting engine (e.g., `mlua` or `rhai`) allowing quants to write custom logic in the web UI that is parsed and executed by the Rust engine on the fly.

## 8. Order Execution Mechanics
* **Fractional Share Support:** The engine natively calculates and supports fractional shares for both crypto and equities, precisely rounding to the maximum allowed decimal places per asset and broker API specifications.
* **Default Order Type (Limit Orders):** Rebalancing orders are dispatched as Limit Orders to minimize slippage.
  * **Pricing Logic:** The engine fetches the L1 Order Book and calculates a smart limit price (e.g., mid-price).
  * **Fallback Logic:** Configurable Time-In-Force (TIF) settings. If a limit order remains unfilled, it can be canceled and replaced at a more aggressive price, or fall back to a Market Order.
* **Extended Order Types:** Capable of handling broker-specific order types (Market, Stop-Loss, Trailing Stop, Iceberg) via Interactive Brokers and Kraken.

## 9. Observability, Logging & Alerting
* **Structured Logging:** Uses the Rust `tracing` crate to output high-performance, machine-readable JSON logs for all system events, errors, and order state changes.
* **Push Notification Alerting:** Integration with a push notification provider (e.g., Pushover, Telegram Bot, or Discord Webhooks) to alert the user without needing to check the dashboard.
  * **INFO:** Routine rebalancing completed successfully.
  * **WARNING:** Non-critical anomalies (e.g., WebSocket disconnects/reconnects).
  * **CRITICAL:** Requires immediate human intervention (e.g., API Key Expired, Margin Requirement Failed, Global Drawdown Limit Reached).