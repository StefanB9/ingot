# Technical Design Document: Phase 1a — Primitives & Storage

## 1. Context

Phase 1a establishes the foundational domain types and database schema that every subsequent phase builds on. The core challenge is designing a universal `Instrument` abstraction that spans two fundamentally different broker APIs (Kraken and IBKR) across multiple asset classes (equities, options, futures, forex, crypto spot/futures/margin). Getting these types wrong means expensive refactors in every downstream crate.

## 2. Crate Structure

Phase 1a introduces two new crates alongside the existing `ingot-primitives`:

```
crates/
├── ingot-primitives/     # Newtypes, enums, and zero-dependency domain types
├── ingot-core/           # Instrument model, domain logic, shared traits
└── ingot-storage/        # sqlx, migrations, TimescaleDB schema, repository traits
```

### Crate Responsibilities

| Crate | Dependencies | Role |
|-------|-------------|------|
| `ingot-primitives` | `rust_decimal`, `thiserror`, `serde`, `smol_str` | Newtypes, enums, error types. Zero async. No I/O. |
| `ingot-core` | `ingot-primitives`, `chrono`, `anyhow` | Instrument model, domain traits, currency handling |
| `ingot-storage` | `ingot-primitives`, `ingot-core`, `sqlx`, `tokio`, `tracing` | Database access, migrations, repository pattern |

### Dependency Layering Rule
`ingot-primitives` has no internal dependencies — it is the leaf. `ingot-core` depends only on `ingot-primitives`. `ingot-storage` depends on both. No crate in this layer depends on broker-specific types.

## 3. Domain Types (`ingot-primitives`)

### 3.1 Financial Newtypes

All financial quantities use `rust_decimal::Decimal`. Each newtype enforces domain semantics at the type level.

```
Price(Decimal)       — a per-unit price in the quote currency
Quantity(Decimal)    — a number of units (shares, contracts, coins)
Amount(Decimal)      — a monetary amount in a specific currency (price * quantity)
Percentage(Decimal)  — a ratio (0.0 to 1.0), used for weights, margins, fees
```

**Traits to derive/implement:** `Debug`, `Display`, `Clone`, `Copy`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Serialize`, `Deserialize`.

**Arithmetic:** Implement only the operations that are dimensionally valid:
- `Price * Quantity -> Amount`
- `Amount + Amount -> Amount` (same currency only — enforced at call site, not type level)
- `Amount * Percentage -> Amount`
- `Quantity + Quantity -> Quantity`
- No `Price + Price` (meaningless without context)

**Construction:** Fallible constructors returning `Result<Self>`. `Quantity` must be non-negative. `Percentage` must be in `[0, 1]`.

### 3.2 Symbol Type

```
Symbol — a canonical instrument identifier string
```

Internally backed by `smol_str::SmolStr` (inline for <=23 bytes, heap-allocated otherwise). This avoids `String` cloning on hot paths while supporting variable-length symbols.

**Normalization:** `Symbol` stores the broker-native canonical form. Cross-broker resolution is handled by lookup tables in `ingot-core`, not by the `Symbol` type itself.

- Kraken spot: store the REST API key (e.g., `XXBTZUSD`)
- Kraken futures: store the symbol field (e.g., `PF_SOLUSD`)
- IBKR: store `conId` as the canonical identifier (numeric, unique across all IBKR instruments)

### 3.3 Currency

```rust
enum Currency {
    USD, EUR, GBP, JPY, CHF, CAD, AUD, NZD, HKD, SGD,  // fiat
    BTC, ETH, SOL, XRP, ADA, DOT, AVAX, MATIC, LINK,    // crypto (extensible)
    Other(SmolStr),                                       // escape hatch
}
```

`Copy` for the known variants. The `Other` variant handles any currency not in the enum without requiring code changes.

### 3.4 Core Enums

```rust
enum AssetClass {
    Equity,
    Option,
    Future,
    Forex,
    CryptoSpot,
    CryptoFuture,
    Bond,
}

enum Exchange {
    Kraken,
    KrakenFutures,
    IBKR,
    Paper,          // PaperExchange adapter
}

enum OrderSide { Buy, Sell }

enum OrderType {
    Market,
    Limit,
    StopLoss,
    StopLossLimit,
    TakeProfit,
    TakeProfitLimit,
}

enum TimeInForce {
    GoodTilCancelled,
    ImmediateOrCancel,
    FillOrKill,
    Day,
    GoodTilDate(chrono::DateTime<Utc>),
}

enum OptionRight { Call, Put }
```

All enums: `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash`, `Serialize`, `Deserialize`.
`OrderSide`, `OrderType`, `AssetClass`, `Exchange`, `OptionRight`: also `Display` via `strum` or manual impl.

### 3.5 Error Types

```rust
// ingot-primitives/src/error.rs
#[derive(Debug, thiserror::Error)]
enum PrimitiveError {
    #[error("invalid quantity: {0} (must be non-negative)")]
    InvalidQuantity(Decimal),

    #[error("invalid percentage: {0} (must be in [0, 1])")]
    InvalidPercentage(Decimal),

    #[error("unknown currency: {0}")]
    UnknownCurrency(String),

    #[error("symbol cannot be empty")]
    EmptySymbol,
}
```

## 4. Instrument Model (`ingot-core`)

### 4.1 Design Decision: Enum vs Trait

**Choice: Enum with shared fields + variant-specific data.**

A trait-based approach (`dyn Instrument`) would require dynamic dispatch and lose pattern matching. An enum keeps everything on the stack, allows exhaustive matching, and is simpler to serialize/store. The set of asset classes is bounded and known.

### 4.2 Instrument Structure

```rust
struct Instrument {
    // Universal fields — every instrument has these
    symbol: Symbol,
    asset_class: AssetClass,
    exchange: Exchange,
    base_currency: Currency,      // what you're buying/selling
    quote_currency: Currency,     // what you're pricing in
    tick_size: Price,             // minimum price increment
    display_name: SmolStr,       // human-readable (e.g., "AAPL", "BTC/USD", "ES Mar26")

    // Asset-class-specific details
    details: InstrumentDetails,
}

enum InstrumentDetails {
    Equity {
        isin: Option<SmolStr>,
        lot_size: Quantity,          // minimum order increment (usually 1 for stocks)
        fractional: bool,            // whether fractional shares are supported
    },
    Option {
        underlying: Symbol,          // the underlying instrument's symbol
        strike: Price,
        right: OptionRight,
        expiry: chrono::NaiveDate,
        multiplier: Decimal,         // typically 100 for equity options
        style: OptionStyle,          // American vs European
    },
    Future {
        underlying: Option<Symbol>,  // optional reference to underlying
        expiry: chrono::NaiveDate,
        multiplier: Decimal,         // contract multiplier (e.g., 50 for ES)
        settlement: SettlementType,  // Cash vs Physical
    },
    Forex {
        pip_size: Price,             // minimum meaningful price change
    },
    CryptoSpot {
        order_min: Quantity,         // minimum order size in base currency
        cost_min: Amount,            // minimum order cost in quote currency
        lot_decimals: u8,            // volume precision
        margin_eligible: bool,
        leverage_tiers: Vec<u8>,     // available leverage levels (empty = no margin)
    },
    CryptoFuture {
        contract_type: CryptoContractType,
        expiry: Option<chrono::DateTime<Utc>>,  // None = perpetual
        max_position_size: Quantity,
        initial_margin: Percentage,              // base tier
        maintenance_margin: Percentage,           // base tier
    },
    Bond {
        face_value: Amount,
        coupon_rate: Percentage,
        maturity: chrono::NaiveDate,
    },
}

enum CryptoContractType { PerpetualLinear, PerpetualInverse, FixedLinear, FixedInverse }
enum OptionStyle { American, European }
enum SettlementType { Cash, Physical }
```

### 4.3 Instrument Registry

```rust
/// Thread-safe, read-optimized instrument lookup.
/// Populated at startup from broker APIs, then shared via Arc.
struct InstrumentRegistry {
    by_symbol: HashMap<Symbol, Arc<Instrument>>,
    by_asset_class: HashMap<AssetClass, Vec<Symbol>>,
    by_exchange: HashMap<Exchange, Vec<Symbol>>,
}
```

The registry is built once (or refreshed periodically) and shared via `Arc<InstrumentRegistry>` — no locking needed for reads. Broker-specific symbol aliases (e.g., Kraken's `wsname` vs `altname`) are resolved during registry construction, not at query time.

## 5. Database Schema (`ingot-storage`)

### 5.1 Technology

- **Engine:** TimescaleDB (PG17) via existing Docker Compose setup
- **Driver:** `sqlx` with compile-time query checking
- **Migrations:** `sqlx migrate` reversible migrations in `ingot-storage/migrations/`

### 5.2 Tables

#### `instruments` — Instrument master data

```sql
CREATE TABLE instruments (
    symbol          TEXT        PRIMARY KEY,
    asset_class     TEXT        NOT NULL,    -- 'equity', 'option', 'future', etc.
    exchange        TEXT        NOT NULL,    -- 'kraken', 'kraken_futures', 'ibkr'
    base_currency   TEXT        NOT NULL,
    quote_currency  TEXT        NOT NULL,
    tick_size       NUMERIC     NOT NULL,
    display_name    TEXT        NOT NULL,
    details_json    JSONB       NOT NULL,    -- variant-specific fields
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_instruments_exchange ON instruments (exchange);
CREATE INDEX idx_instruments_asset_class ON instruments (asset_class);
```

**Rationale for JSONB `details_json`:** The variant-specific fields differ structurally per asset class. Modeling each variant as a separate table creates join complexity. JSONB gives schema flexibility while keeping a single table. Typed deserialization happens in Rust via `serde_json` → `InstrumentDetails`.

#### `ohlcv` — Candlestick bars (hypertable)

```sql
CREATE TABLE ohlcv (
    time        TIMESTAMPTZ NOT NULL,
    symbol      TEXT        NOT NULL,
    exchange    TEXT        NOT NULL,
    interval    TEXT        NOT NULL,    -- '1m', '5m', '15m', '1h', '4h', '1d'
    open        NUMERIC     NOT NULL,
    high        NUMERIC     NOT NULL,
    low         NUMERIC     NOT NULL,
    close       NUMERIC     NOT NULL,
    volume      NUMERIC     NOT NULL,
    trade_count INTEGER,                -- number of trades in bar (nullable, not all sources provide)
    PRIMARY KEY (time, symbol, exchange, interval)
);

SELECT create_hypertable('ohlcv', by_range('time'));

-- Primary query pattern: fetch bars for a symbol in a time range
CREATE INDEX idx_ohlcv_symbol_time ON ohlcv (symbol, exchange, interval, time DESC);
```

**Hypertable config:**
- Chunk interval: 7 days (balances query speed vs chunk count for minute-level data)
- Compression: enable after 30 days, segment by `symbol, exchange, interval`, order by `time DESC`
- Retention: indefinite (OHLCV data is kept forever)

#### `ticks` — Raw trade/quote data (hypertable)

```sql
CREATE TABLE ticks (
    time        TIMESTAMPTZ NOT NULL,
    symbol      TEXT        NOT NULL,
    exchange    TEXT        NOT NULL,
    price       NUMERIC     NOT NULL,
    quantity    NUMERIC     NOT NULL,
    side        TEXT,                   -- 'buy', 'sell', or NULL if unknown
    trade_id    TEXT,                   -- exchange-specific trade identifier
    PRIMARY KEY (time, symbol, exchange, trade_id)
);

SELECT create_hypertable('ticks', by_range('time'));

CREATE INDEX idx_ticks_symbol_time ON ticks (symbol, exchange, time DESC);
```

**Hypertable config:**
- Chunk interval: 1 day (tick data is high-volume)
- Compression: enable after 7 days, segment by `symbol, exchange`, order by `time DESC`
- Retention: 90 days via `add_retention_policy('ticks', INTERVAL '90 days')`

### 5.3 Continuous Aggregates

Materialize higher-timeframe bars from the base `ohlcv` 1-minute data:

```sql
-- Example: 1-hour bars from 1-minute bars
CREATE MATERIALIZED VIEW ohlcv_1h
WITH (timescaledb.continuous) AS
SELECT
    time_bucket('1 hour', time) AS time,
    symbol,
    exchange,
    FIRST(open, time)           AS open,
    MAX(high)                   AS high,
    MIN(low)                    AS low,
    LAST(close, time)           AS close,
    SUM(volume)                 AS volume,
    SUM(trade_count)            AS trade_count
FROM ohlcv
WHERE interval = '1m'
GROUP BY time_bucket('1 hour', time), symbol, exchange;
```

Similarly for `5m`, `15m`, `4h`, `1d`. These refresh automatically as new data arrives.

### 5.4 Compression & Retention Policies

```sql
-- OHLCV: compress after 30 days, no retention (keep forever)
ALTER TABLE ohlcv SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'symbol, exchange, interval',
    timescaledb.compress_orderby = 'time DESC'
);
SELECT add_compression_policy('ohlcv', INTERVAL '30 days');

-- Ticks: compress after 7 days, drop after 90 days
ALTER TABLE ticks SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'symbol, exchange',
    timescaledb.compress_orderby = 'time DESC'
);
SELECT add_compression_policy('ticks', INTERVAL '7 days');
SELECT add_retention_policy('ticks', INTERVAL '90 days');
```

### 5.5 Repository Pattern

```rust
// ingot-storage/src/lib.rs
// Each repository is a thin async wrapper over sqlx queries.

trait InstrumentRepository {
    async fn upsert(&self, instrument: &Instrument) -> Result<()>;
    async fn get_by_symbol(&self, symbol: &Symbol) -> Result<Option<Instrument>>;
    async fn list_by_exchange(&self, exchange: Exchange) -> Result<Vec<Instrument>>;
    async fn list_by_asset_class(&self, asset_class: AssetClass) -> Result<Vec<Instrument>>;
}

trait OhlcvRepository {
    async fn insert_batch(&self, bars: &[OhlcvBar]) -> Result<u64>;
    async fn get_range(
        &self,
        symbol: &Symbol,
        exchange: Exchange,
        interval: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<OhlcvBar>>;
}

trait TickRepository {
    async fn insert_batch(&self, ticks: &[Tick]) -> Result<u64>;
    async fn get_range(
        &self,
        symbol: &Symbol,
        exchange: Exchange,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Tick>>;
}
```

Implementations use `sqlx::PgPool` and compile-time checked queries via `sqlx::query!` / `sqlx::query_as!`.

## 6. Workspace Dependencies

New entries for `[workspace.dependencies]` in root `Cargo.toml`:

```toml
[workspace.dependencies]
# Serialization
serde = { version = "1", default-features = false, features = ["derive"] }
serde_json = { version = "1", default-features = false, features = ["std"] }

# Financial math
rust_decimal = { version = "1", default-features = false, features = ["serde"] }

# String handling
smol_str = { version = "0.3", default-features = false, features = ["serde"] }

# Error handling
thiserror = { version = "2", default-features = false }
anyhow = { version = "1", default-features = false, features = ["std"] }

# Date/time
chrono = { version = "0.4", default-features = false, features = ["serde", "clock"] }

# Database
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "postgres", "chrono", "rust_decimal", "json"] }

# Async runtime
tokio = { version = "1", default-features = false }

# Observability
tracing = { version = "0.1", default-features = false }
```

## 7. File Layout

```
crates/
├── ingot-primitives/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # re-exports
│       ├── newtypes.rs         # Price, Quantity, Amount, Percentage
│       ├── symbol.rs           # Symbol newtype
│       ├── currency.rs         # Currency enum
│       ├── enums.rs            # AssetClass, Exchange, OrderSide, OrderType, TimeInForce, etc.
│       └── error.rs            # PrimitiveError
│
├── ingot-core/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # re-exports
│       ├── instrument.rs       # Instrument struct + InstrumentDetails enum
│       ├── instrument_registry.rs  # InstrumentRegistry
│       ├── ohlcv.rs            # OhlcvBar struct
│       └── tick.rs             # Tick struct
│
└── ingot-storage/
    ├── Cargo.toml
    ├── migrations/
    │   ├── 001_create_instruments.up.sql
    │   ├── 001_create_instruments.down.sql
    │   ├── 002_create_ohlcv.up.sql
    │   ├── 002_create_ohlcv.down.sql
    │   ├── 003_create_ticks.up.sql
    │   ├── 003_create_ticks.down.sql
    │   ├── 004_policies.up.sql
    │   └── 004_policies.down.sql
    └── src/
        ├── lib.rs              # re-exports, PgPool setup
        ├── instrument_repo.rs  # InstrumentRepository impl
        ├── ohlcv_repo.rs       # OhlcvRepository impl
        └── tick_repo.rs        # TickRepository impl
```

## 8. Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Instrument model | Struct + details enum | Stack-allocated, exhaustive matching, serializable. Asset classes are bounded. |
| Symbol internals | `SmolStr` | Inline up to 23 bytes (covers all ticker symbols), avoids heap allocation in hot paths |
| Currency enum | Known variants + `Other(SmolStr)` | Covers common currencies without code changes; `Other` is the escape hatch |
| Details storage in DB | JSONB column | Variant-specific fields differ structurally; avoids join-heavy per-variant tables |
| OHLCV chunk interval | 7 days | Balances query performance vs chunk management for minute-level data |
| Tick chunk interval | 1 day | High-volume data needs smaller chunks for efficient compression and retention |
| Repository pattern | Async traits | Decouples storage from domain; enables test doubles for integration tests |

## 9. Resolved Design Questions

1. **`smol_str` version:** Use `0.3.x` (latest stable). If edition 2024 incompatibility surfaces on first `cargo check`, swap to `compact_str`.
2. **sqlx offline mode:** Commit `.sqlx/` from day one. Run `cargo sqlx prepare --workspace` after each migration change. This is the query verification safety net without CI.
3. **Continuous aggregates:** Created in migrations — they are schema, not optional tooling. `cargo sqlx migrate run` is the single source of truth for database state.
4. **Instrument registry refresh:** On startup + periodic refresh (every 6 hours). Swap `Arc<InstrumentRegistry>` atomically so readers are never stalled. Catches new listings, delistings, and expiring contracts without excessive API calls.

## 10. Verification Plan

1. `cargo check --all-targets --workspace` — all three crates compile
2. `cargo clippy --all-targets --workspace` — zero warnings
3. `cargo nextest run --workspace` — all tests pass (TDD: tests written first)
4. `cargo fmt --all -- --check` — formatting clean
5. Database: `docker compose up -d` → `cargo sqlx migrate run` → verify tables/hypertables exist
6. Round-trip test: insert an `Instrument` → read it back → assert equality
7. Round-trip test: batch-insert OHLCV bars → query range → assert data integrity
8. Round-trip test: batch-insert ticks → query range → assert data integrity
9. Verify compression/retention policies are active via `timescaledb_information.jobs`
