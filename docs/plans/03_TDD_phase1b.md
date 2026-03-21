# Technical Design Document: Phase 1b — Connectivity (Kraken + PaperExchange)

## 1. Context

Phase 1a delivered the foundational domain types (`ingot-primitives`), instrument model (`ingot-core`), and database layer (`ingot-storage`). Phase 1b builds the broker connectivity layer: REST and WebSocket clients for Kraken (spot + futures), a historical data backfill pipeline, and a full PaperExchange simulator. This is the first phase that connects to external systems.

## 2. Crate Structure

Single new crate `ingot-connectivity` with modules per broker. Matches `RUST_LOG` filter `ingot_connectivity=debug`.

```
ingot-primitives (no deps)
    ↓
ingot-core (+ new types: OrderRequest, Balance, Position, TickerSnapshot, etc.)
    ↓
ingot-storage (unchanged)
    ↓
ingot-connectivity (new — depends on ingot-core, ingot-storage, reqwest, tokio-tungstenite)
```

## 3. New Domain Types (`ingot-core`)

These broker-agnostic types are used by traits and all implementations. Added to `ingot-core`.

### 3.1 Order Types (`src/order.rs`)

```rust
/// Unique order identifier — broker-specific string wrapped in a newtype.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(SmolStr);

/// What a strategy or user wants to trade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRequest {
    pub symbol: Symbol,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: Quantity,
    pub limit_price: Option<Price>,     // required for Limit, StopLossLimit, TakeProfitLimit
    pub stop_price: Option<Price>,      // required for StopLoss, StopLossLimit, TakeProfit, TakeProfitLimit
    pub time_in_force: TimeInForce,
}

/// Lifecycle state of a placed order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,          // submitted but not yet acknowledged by exchange
    Open,             // acknowledged, resting on book
    PartiallyFilled,  // some quantity filled
    Filled,           // fully filled
    Cancelled,        // cancelled by user or system
    Rejected,         // rejected by exchange
    Expired,          // expired (TIF)
}

/// Details of a single order execution (fill).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderFill {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub fill_price: Price,
    pub fill_quantity: Quantity,
    pub fee: Amount,
    pub fee_currency: Currency,
    pub timestamp: DateTime<Utc>,
    pub trade_id: Option<SmolStr>,    // exchange-specific trade ID
}

/// A placed order with its current status and fill info.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenOrder {
    pub order_id: OrderId,
    pub request: OrderRequest,
    pub status: OrderStatus,
    pub filled_quantity: Quantity,
    pub remaining_quantity: Quantity,
    pub average_fill_price: Option<Price>,
    pub created_at: DateTime<Utc>,
}
```

### 3.2 Account Types (`src/balance.rs`, `src/position.rs`)

```rust
/// Balance for a single currency across an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub currency: Currency,
    pub total: Amount,         // total balance including held
    pub available: Amount,     // available for trading
    pub held: Amount,          // reserved for open orders
}

/// An open position in a single instrument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    pub side: OrderSide,       // long (Buy) or short (Sell)
    pub quantity: Quantity,
    pub average_entry_price: Price,
    pub unrealized_pnl: Option<Amount>,
    pub liquidation_price: Option<Price>,  // for margin/futures
}
```

### 3.3 Market Data Types (`src/market_data.rs`)

```rust
/// A snapshot of current bid/ask/last for a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickerSnapshot {
    pub symbol: Symbol,
    pub bid: Price,
    pub ask: Price,
    pub last: Price,
    pub volume_24h: Quantity,
    pub timestamp: DateTime<Utc>,
}

/// A single price level in the order book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBookLevel {
    pub price: Price,
    pub quantity: Quantity,
}

/// L2 order book snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub symbol: Symbol,
    pub bids: Vec<OrderBookLevel>,  // sorted descending by price
    pub asks: Vec<OrderBookLevel>,  // sorted ascending by price
    pub timestamp: DateTime<Utc>,
}
```

## 4. Broker Trait Hierarchy (`ingot-connectivity/src/traits.rs`)

Split into four capability traits. Uses native `async fn` in traits (edition 2024). Dyn dispatch deferred — use generics or enum dispatch for now.

```rust
/// Fetch instrument metadata and market snapshots.
pub trait MarketDataProvider {
    async fn fetch_instruments(&self) -> Result<Vec<Instrument>>;
    async fn fetch_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<OhlcvBar>>;
    async fn fetch_trades(
        &self,
        symbol: &Symbol,
        since: Option<DateTime<Utc>>,
    ) -> Result<(Vec<Tick>, Option<DateTime<Utc>>)>;  // returns (ticks, next_cursor)
    async fn fetch_ticker(&self, symbol: &Symbol) -> Result<TickerSnapshot>;
    async fn fetch_order_book(&self, symbol: &Symbol, depth: u32) -> Result<OrderBookSnapshot>;
}

/// Place, cancel, and query orders.
pub trait OrderExecutor {
    async fn place_order(&self, request: &OrderRequest) -> Result<OrderId>;
    async fn cancel_order(&self, order_id: &OrderId) -> Result<()>;
    async fn cancel_all_orders(&self) -> Result<u32>;
    async fn get_order_status(&self, order_id: &OrderId) -> Result<OpenOrder>;
    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>>;
}

/// Account balances and positions.
pub trait AccountProvider {
    async fn get_balances(&self) -> Result<Vec<Balance>>;
    async fn get_positions(&self) -> Result<Vec<Position>>;
    async fn get_trade_history(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<OrderFill>>;
}

/// Live WebSocket data streams.
pub trait StreamProvider {
    async fn subscribe_trades(
        &self,
        symbols: &[Symbol],
    ) -> Result<broadcast::Receiver<Tick>>;
    async fn subscribe_ticker(
        &self,
        symbols: &[Symbol],
    ) -> Result<broadcast::Receiver<TickerSnapshot>>;
    async fn subscribe_order_book(
        &self,
        symbols: &[Symbol],
        depth: u32,
    ) -> Result<broadcast::Receiver<OrderBookSnapshot>>;
    async fn subscribe_executions(&self) -> Result<mpsc::Receiver<OrderFill>>;
}
```

**Who implements what:**

| Adapter | MarketDataProvider | OrderExecutor | AccountProvider | StreamProvider |
|---------|-------------------|---------------|-----------------|----------------|
| KrakenSpotAdapter | ✓ | ✓ | ✓ | ✓ |
| KrakenFuturesAdapter | ✓ | ✓ | ✓ | ✓ |
| PaperExchange | ✗ (uses real feed) | ✓ | ✓ | ✗ (consumes, doesn't produce) |

## 5. Kraken Spot REST Client

### 5.1 Authentication

HMAC-SHA512 signing for private endpoints:
```
API-Key: <api_key>
API-Sign: base64(HMAC-SHA512(
    key = base64_decode(api_secret),
    msg = url_path + SHA256(nonce + post_data)
))
```

Module: `src/kraken/auth.rs`

### 5.2 API Mapping

| Kraken Endpoint | Trait Method | Maps To |
|----------------|-------------|---------|
| `GET /0/public/AssetPairs` | `fetch_instruments()` | `Vec<Instrument>` with `CryptoSpot` details |
| `GET /0/public/OHLC` | `fetch_ohlcv()` | `Vec<OhlcvBar>` (max 720 bars per call) |
| `GET /0/public/Trades` | `fetch_trades()` | `Vec<Tick>` with cursor for pagination |
| `GET /0/public/Ticker` | `fetch_ticker()` | `TickerSnapshot` |
| `GET /0/public/Depth` | `fetch_order_book()` | `OrderBookSnapshot` |
| `POST /0/private/Balance` | `get_balances()` | `Vec<Balance>` |
| `POST /0/private/AddOrder` | `place_order()` | `OrderId` |
| `POST /0/private/CancelOrder` | `cancel_order()` | `()` |
| `POST /0/private/CancelAll` | `cancel_all_orders()` | `u32` (count) |
| `POST /0/private/OpenOrders` | `get_open_orders()` | `Vec<OpenOrder>` |
| `POST /0/private/TradesHistory` | `get_trade_history()` | `Vec<OrderFill>` |
| `POST /0/private/GetWebSocketsToken` | (internal) | WS auth token |

### 5.3 Symbol Mapping

Kraken REST returns `AssetPairs` with fields: `base`, `quote`, `wsname`, `altname`, `pair_decimals`, `lot_decimals`, `ordermin`, `leverage_buy`, `leverage_sell`, `fees`, `fees_maker`.

Mapping to `Instrument`:
- `symbol` → REST pair key (e.g., `XXBTZUSD`)
- `asset_class` → `AssetClass::CryptoSpot`
- `exchange` → `Exchange::Kraken`
- `base_currency` → from `base` field, through `Currency::from_str_lossy` (handles XBT→BTC)
- `quote_currency` → from `quote` field
- `tick_size` → `Price::new(Decimal::new(1, pair_decimals))`
- `display_name` → `altname` field
- `details.order_min` → `ordermin` field
- `details.cost_min` → from pair info or hardcoded per pair
- `details.lot_decimals` → `lot_decimals` field
- `details.margin_eligible` → `leverage_buy.len() > 0`
- `details.leverage_tiers` → `leverage_buy` array

Module: `src/kraken/spot/mapper.rs`

### 5.4 Rate Limiting

Kraken uses a decaying counter: each call adds weight, counter decays ~0.33/second. Max 15 for public, max 20 for private.

Implementation: Token bucket in `src/rate_limiter.rs`. Per-client instance. Acquire before each request, async wait if exhausted.

```rust
pub struct RateLimiter {
    max_tokens: u32,
    tokens: AtomicU32,
    refill_rate: Duration,  // time per token refill
}

impl RateLimiter {
    pub async fn acquire(&self, weight: u32) -> Result<()>;
}
```

## 6. Kraken Spot WebSocket Client

### 6.1 Architecture

```
                    ┌──────────────────┐
                    │  KrakenSpotWs    │
                    │  Manager         │
                    │                  │
                    │ ┌──────────────┐ │    broadcast::Receiver<Tick>
  wss://ws.kraken ──┤ │  read loop   │ ├──► broadcast::Receiver<TickerSnapshot>
       .com/v2      │ │  (tokio task) │ │    broadcast::Receiver<OrderBookSnapshot>
                    │ └──────────────┘ │
                    │                  │
  wss://ws-auth. ───┤ ┌──────────────┐ │    mpsc::Receiver<OrderFill>
  kraken.com/v2     │ │  auth loop   │ ├──►
                    │ │  (tokio task) │ │
                    │ └──────────────┘ │
                    └──────────────────┘
```

- Two WS connections: public (trades, ticker, book) and private (executions)
- Background tokio tasks read messages and dispatch to broadcast/mpsc channels
- Automatic reconnection with exponential backoff (1s → 2s → 4s → ... → 60s cap)
- Heartbeat/ping-pong handling
- Subscription management: subscribe/unsubscribe channels dynamically

### 6.2 Message Deserialization

Kraken WS v2 JSON messages → serde deserialization into Kraken-specific structs → mapping to domain types.

Kraken-specific message structs live in `src/kraken/spot/models.rs` (not in `ingot-core`).

### 6.3 Typestate

Applied to the WS manager:
```rust
pub struct KrakenSpotWs<S = Disconnected> {
    config: KrakenSpotConfig,
    _state: PhantomData<S>,
}

impl KrakenSpotWs<Disconnected> {
    pub fn new(config: KrakenSpotConfig) -> Self;
    pub async fn connect(self) -> Result<KrakenSpotWs<Connected>>;
}

impl KrakenSpotWs<Connected> {
    pub async fn subscribe_trades(&self, symbols: &[Symbol]) -> Result<broadcast::Receiver<Tick>>;
    pub async fn disconnect(self) -> KrakenSpotWs<Disconnected>;
}
```

## 7. Kraken Futures

Separate REST + WS client with different auth (HMAC-SHA256 vs SHA512) and different API base URL.

### 7.1 REST API Mapping

| Endpoint | Maps To |
|----------|---------|
| `GET /api/v3/instruments` | `Vec<Instrument>` with `CryptoFuture` details |
| `GET /api/v3/tickers` | `TickerSnapshot` |
| `GET /api/v3/orderbook` | `OrderBookSnapshot` |
| `POST /api/v3/sendorder` | `OrderId` |
| `POST /api/v3/cancelorder` | `()` |
| `GET /api/v3/openpositions` | `Vec<Position>` |
| `GET /api/v3/accounts` | `Vec<Balance>` |

### 7.2 Futures Instrument Mapping

- `contract_type` → from symbol prefix: `PF_` = PerpetualLinear, `FI_` = FixedLinear, `PI_` = PerpetualInverse
- `expiry` → None for perpetuals, parsed from symbol suffix for dated (e.g., `_260327`)
- `initial_margin`, `maintenance_margin` → from margin schedule in instruments response
- `max_position_size` → from instruments response

### 7.3 Futures WebSocket

- `wss://futures.kraken.com/ws/v1`
- Challenge-response auth: receive challenge → sign with SHA-256 → send back
- Feeds: `ticker`, `book`, `trade`, `fills`
- Ping every 60s (mandatory)

## 8. Historical Data Pipeline (`src/backfill/`)

### 8.1 BackfillWorker

```rust
pub struct BackfillWorker {
    market_data: Arc<dyn MarketDataProvider>,   // Kraken REST client
    ohlcv_repo: PgOhlcvRepository,
    tick_repo: PgTickRepository,
    rate_limiter: Arc<RateLimiter>,
}

impl BackfillWorker {
    /// Fetch and store OHLCV bars for a symbol, paginating from `since` to now.
    pub async fn backfill_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        since: DateTime<Utc>,
    ) -> Result<u64>;

    /// Fetch and store trade ticks for a symbol, paginating via cursor.
    pub async fn backfill_ticks(
        &self,
        symbol: &Symbol,
        since: DateTime<Utc>,
    ) -> Result<u64>;
}
```

- Kraken `/public/OHLC` returns max 720 bars per call with a `since` parameter → paginate by advancing `since` to the last bar's timestamp
- Kraken `/public/Trades` returns trades with a `since` nonce for cursor-based pagination
- Rate-limit aware: uses shared `RateLimiter` instance
- Returns count of inserted records
- Idempotent: relies on `ON CONFLICT DO NOTHING` in storage layer

## 9. PaperExchange (`src/paper/`)

### 9.1 Design

Full simulated exchange implementing `OrderExecutor` + `AccountProvider`.

```rust
pub struct PaperExchange {
    config: PaperExchangeConfig,
    balances: RwLock<HashMap<Currency, Balance>>,
    positions: RwLock<HashMap<Symbol, Position>>,
    open_orders: RwLock<HashMap<OrderId, OpenOrder>>,
    fills: RwLock<Vec<OrderFill>>,
    next_order_id: AtomicU64,
    price_feed: broadcast::Receiver<Tick>,  // from live or replay
    fill_tx: mpsc::Sender<OrderFill>,       // notifies consumers of fills
}
```

### 9.2 Fill Simulation

- **Market orders**: Fill instantly at `last_price ± slippage`. Slippage = configurable basis points applied to price.
- **Limit orders**: Resting in `open_orders`. A background task consumes the `price_feed` and checks each tick: if tick price crosses a limit order's price, generate a fill.
- **Latency simulation**: Configurable delay (`tokio::time::sleep`) before processing each order.
- **Partial fills**: Configurable probability that a fill is partial (random split of remaining quantity).

### 9.3 Balance Tracking

- Initialize with configurable balances per currency
- On order placement: move `quantity * price` from `available` to `held`
- On fill: deduct from `held`, update position, apply fees
- On cancel: return `held` to `available`

### 9.4 Configuration

```rust
pub struct PaperExchangeConfig {
    pub initial_balances: Vec<(Currency, Decimal)>,
    pub slippage_bps: Decimal,                // basis points (e.g., 5 = 0.05%)
    pub latency_ms: u64,                      // simulated processing delay
    pub partial_fill_probability: Decimal,     // 0.0 to 1.0
    pub maker_fee_bps: Decimal,               // maker fee in basis points
    pub taker_fee_bps: Decimal,               // taker fee in basis points
}
```

## 10. Error Types (`src/error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConnectivityError {
    #[error("HTTP request failed: {0}")]
    Http(#[source] reqwest::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("authentication failed: {reason}")]
    AuthenticationFailed { reason: String },

    #[error("rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("API error [{code}]: {message}")]
    ApiError { code: String, message: String },

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("connection lost")]
    ConnectionLost,

    #[error("order rejected: {reason}")]
    OrderRejected { reason: String },

    #[error("insufficient balance: need {required}, have {available}")]
    InsufficientBalance { required: Amount, available: Amount },

    #[error("symbol not found: {0}")]
    SymbolNotFound(Symbol),

    #[error("deserialization failed: {0}")]
    Deserialization(#[source] serde_json::Error),
}
```

Tests required for Display output of each variant (per CLAUDE.md).

## 11. Configuration (`src/config.rs`)

```rust
pub struct KrakenSpotConfig {
    pub api_key: String,
    pub api_secret: String,
    pub rest_url: String,       // default: https://api.kraken.com
    pub ws_url: String,         // default: wss://ws.kraken.com/v2
    pub ws_auth_url: String,    // default: wss://ws-auth.kraken.com/v2
}

pub struct KrakenFuturesConfig {
    pub api_key: String,
    pub api_secret: String,
    pub rest_url: String,       // default: https://futures.kraken.com/derivatives/api/v3
    pub ws_url: String,         // default: wss://futures.kraken.com/ws/v1
}
```

Config values sourced from environment variables (existing `.env.example` pattern). URLs configurable for wiremock testing.

## 12. Workspace Dependencies (New)

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
tokio-tungstenite = { version = "0.26", default-features = false, features = ["rustls-tls-native-roots"] }
hmac = { version = "0.12", default-features = false }
sha2 = { version = "0.10", default-features = false }
base64 = { version = "0.22", default-features = false, features = ["std"] }
url = { version = "2.5", default-features = false }
futures-util = { version = "0.3", default-features = false, features = ["sink"] }
wiremock = { version = "0.6", default-features = false }  # dev-dep
```

Update `tokio` workspace features to include `sync`, `time`, `net`, `io-util`.

## 13. File Layout

```
crates/ingot-core/src/
├── lib.rs                      # + re-exports for new types
├── order.rs                    # OrderId, OrderRequest, OrderStatus, OrderFill, OpenOrder (NEW)
├── balance.rs                  # Balance (NEW)
├── position.rs                 # Position (NEW)
├── market_data.rs              # TickerSnapshot, OrderBookLevel, OrderBookSnapshot (NEW)
├── instrument.rs               # (existing)
├── instrument_registry.rs      # (existing)
├── ohlcv.rs                    # (existing)
└── tick.rs                     # (existing)

crates/ingot-connectivity/
├── Cargo.toml
└── src/
    ├── lib.rs                  # re-exports
    ├── traits.rs               # MarketDataProvider, OrderExecutor, AccountProvider, StreamProvider
    ├── error.rs                # ConnectivityError
    ├── config.rs               # KrakenSpotConfig, KrakenFuturesConfig, PaperExchangeConfig
    ├── rate_limiter.rs         # Token bucket rate limiter
    ├── kraken/
    │   ├── mod.rs
    │   ├── auth.rs             # HMAC-SHA512 (spot) + HMAC-SHA256 (futures) signing
    │   ├── spot/
    │   │   ├── mod.rs
    │   │   ├── rest.rs         # KrakenSpotRestClient
    │   │   ├── ws.rs           # KrakenSpotWs<Disconnected/Connected>
    │   │   ├── models.rs       # Kraken JSON response structs
    │   │   └── mapper.rs       # Kraken → domain type conversions
    │   └── futures/
    │       ├── mod.rs
    │       ├── rest.rs         # KrakenFuturesRestClient
    │       ├── ws.rs           # KrakenFuturesWs<Disconnected/Connected>
    │       ├── models.rs       # Futures-specific JSON structs
    │       └── mapper.rs       # Futures → domain type conversions
    ├── paper/
    │   ├── mod.rs
    │   ├── exchange.rs         # PaperExchange struct + OrderExecutor/AccountProvider impls
    │   └── fill_model.rs       # Slippage, latency, partial fill logic
    └── backfill/
        ├── mod.rs
        └── worker.rs           # BackfillWorker for historical OHLCV/tick ingestion
```

## 14. Sub-Phase Breakdown

### 1b.1: Crate scaffold + domain types + broker traits
**Deliverables:**
- Create `ingot-connectivity` crate
- Add new domain types to `ingot-core`: OrderId, OrderRequest, OrderStatus, OrderFill, OpenOrder, Balance, Position, TickerSnapshot, OrderBookLevel, OrderBookSnapshot
- Define the four broker traits in `ingot-connectivity/src/traits.rs`
- Define `ConnectivityError` enum with Display tests
- Define config structs
- Unit tests for all new domain types (construction, serde round-trips)

### 1b.2: Kraken Spot REST — public market data
**Deliverables:**
- `reqwest`-based HTTP client with configurable base URL
- Rate limiter implementation
- `/public/AssetPairs` → `fetch_instruments()` with full CryptoSpot mapping
- `/public/OHLC` → `fetch_ohlcv()`
- `/public/Trades` → `fetch_trades()` with cursor pagination
- `/public/Ticker` → `fetch_ticker()`
- `/public/Depth` → `fetch_order_book()`
- Kraken JSON response models and mappers
- `wiremock` tests for each endpoint (happy path + error cases)
- Implements `MarketDataProvider` trait

### 1b.3: Kraken Spot REST — authenticated
**Deliverables:**
- HMAC-SHA512 auth module with nonce generation
- `/private/Balance` → `get_balances()`
- `/private/AddOrder` → `place_order()`
- `/private/CancelOrder` → `cancel_order()`
- `/private/CancelAll` → `cancel_all_orders()`
- `/private/OpenOrders` → `get_open_orders()`
- `/private/TradesHistory` → `get_trade_history()`
- `/private/GetWebSocketsToken` → (internal, for WS auth)
- Implements `OrderExecutor` + `AccountProvider` traits
- `wiremock` tests for each endpoint including auth header validation

### 1b.4: Kraken Spot WebSocket
**Deliverables:**
- `tokio-tungstenite` WS client with typestate (`Disconnected` → `Connected`)
- Public WS: trade, ticker, book feed subscriptions
- Private WS: execution feed (uses token from REST)
- Message deserialization → domain type mapping
- Broadcast channels for trade/ticker/book fan-out
- mpsc channel for execution reports
- Auto-reconnection with exponential backoff
- Heartbeat/ping-pong handling
- Implements `StreamProvider` trait
- Integration tests with mock WS server

### 1b.5: Historical data backfill pipeline
**Deliverables:**
- `BackfillWorker` struct using `MarketDataProvider` + storage repos
- OHLCV backfill with pagination (720-bar pages from Kraken)
- Tick backfill with cursor-based pagination
- Rate-limit aware scheduling
- Idempotent (ON CONFLICT DO NOTHING in storage)
- Integration test with wiremock + testcontainers

### 1b.6: Kraken Futures REST + WS
**Deliverables:**
- `KrakenFuturesRestClient` with HMAC-SHA256 auth
- `/instruments` → `fetch_instruments()` with CryptoFuture mapping
- `/tickers`, `/orderbook` → market data
- `/sendorder`, `/cancelorder` → order execution
- `/accounts`, `/openpositions` → account data
- Futures WS with challenge-response auth
- Implements all four traits for futures
- `wiremock` tests

### 1b.7: PaperExchange
**Deliverables:**
- `PaperExchange` struct with in-memory state
- Market order fills (instant, with slippage)
- Limit order matching (price-crossing via tick feed)
- Balance tracking (available/held/total)
- Position tracking (entry price, quantity, unrealized PnL)
- Fee calculation (maker/taker)
- Latency simulation
- Partial fill modeling
- Implements `OrderExecutor` + `AccountProvider`
- Comprehensive unit tests (all order types, edge cases, insufficient balance)

## 15. Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Crate structure | Single `ingot-connectivity` | Matches RUST_LOG convention, keeps traits near impls, split later if needed |
| Trait design | Split capability traits | PaperExchange only needs OrderExecutor + AccountProvider; IBKR may not support StreamProvider initially |
| Async traits | Native `async fn` in traits (edition 2024) | No `async_trait` crate needed; dyn dispatch deferred to enum wrapper when needed |
| HTTP client | `reqwest` | High-level, well-maintained, sufficient for REST APIs |
| WebSocket | `tokio-tungstenite` | Tokio-native async, widely used |
| WS architecture | Separate public + private connections | Kraken requires different URLs; isolation prevents auth issues from disrupting market data |
| Typestate | Applied to WS clients | Prevents calling subscribe methods before connecting; REST is stateless (no typestate) |
| Rate limiting | In-process token bucket | Simple, no external deps; Kraken's model is a decaying counter which maps naturally |
| PaperExchange price feed | `broadcast::Receiver<Tick>` | Decoupled from data source; works with live WS or historical replay |
| Kraken response types | In `ingot-connectivity`, not `ingot-core` | Broker-specific JSON shapes shouldn't leak into domain types |
| Error handling | `ConnectivityError` (thiserror) wrapped in `anyhow::Result` at public API boundary | Matchable errors for retry logic; anyhow for consumers who don't need to match |

## 16. Testing Strategy

| Layer | Tool | Approach |
|-------|------|----------|
| Domain types (ingot-core) | `cargo nextest` | Construction validation, serde round-trips, display output |
| REST clients | `wiremock` | Mock Kraken responses; test happy path, error responses, rate limiting |
| WS clients | Mock WS server | Test connection, subscription, message parsing, reconnection |
| Backfill pipeline | `wiremock` + `testcontainers` | Mock Kraken REST → verify data lands in DB correctly |
| PaperExchange | Unit tests | All order types, balance updates, fill simulation, edge cases |
| Auth modules | Unit tests | Known test vectors for HMAC-SHA512 and HMAC-SHA256 |
| Rate limiter | Unit tests | Token acquisition, exhaustion, refill timing |

Per CLAUDE.md: `wiremock`-based integration tests for exchange adapters. Every `thiserror` variant gets a Display test. No `.unwrap()` / `.expect()` / `panic!()`.

## 17. Verification Plan

1. `cargo check --all-targets --workspace` — all crates compile
2. `cargo clippy --all-targets --workspace` — zero warnings
3. `cargo nextest run --workspace` — all tests pass
4. `cargo fmt --all -- --check` — formatting clean
5. `wiremock` tests verify each Kraken endpoint mapping
6. Auth test vectors match Kraken documentation
7. PaperExchange: place market order → verify balance update + fill event
8. PaperExchange: place limit order → feed crossing tick → verify fill
9. Backfill: mock OHLCV response → verify bars stored in TimescaleDB
10. WS: connect → subscribe → receive mock message → verify domain type output
