# Technical Design Document (TDD)

**Project Name:** Ingot — Portfolio Management & Systematic Trading Workstation
**Scope:** Phase 2 — Persistent Storage & Audit Trail
**Date:** February 28, 2026
**Version:** 1.0

---

## 1. Overview

This document specifies the technical design for Phase 2 of Ingot: adding persistent storage to the trading workstation. Phase 1 delivered a working in-memory system (ledger, engine, exchange connectivity, IPC). Phase 2 makes that system durable — surviving restarts, recording an auditable history of every financial event, and storing historical market data for future backtesting and analysis.

### 1.1 Scope

| In Scope | Out of Scope |
|:---|:---|
| PostgreSQL integration for transactional data | Portfolio optimizer (Phase 4) |
| QuestDB integration for time-series market data | Backtesting engine (Phase 5) |
| Append-only audit trail with tamper-evident chain hash | Multi-broker support (Phase 3) |
| Historical OHLCV bar ingestion (Kraken REST) | L2/L3 market data storage |
| Ledger state recovery on server restart | Automatic gap-fill / scheduled ingestion |
| CLI commands: audit queries, trade history export, OHLCV ingestion | Strategy parameter persistence |
| GUI additions: trade log view with filtering | User authentication / multi-tenancy |

### 1.2 Design Decisions

These decisions were made prior to drafting and are not revisited in this document:

| Decision | Choice | Rationale |
|:---|:---|:---|
| Database architecture | PostgreSQL + QuestDB | Purpose-built for each workload. PG: ACID transactions. QuestDB: 1M+ rows/sec time-series ingestion. |
| PostgreSQL access | `sqlx` (compile-time checked SQL) | Zero ORM overhead, full SQL control, async-native, built-in migrations. Matches project's no-abstraction-overhead philosophy. |
| Crate layout | Single `ingot-storage` crate | Clean single dependency for engine/server. Unified config and connection management. |
| Audit trail | Append-only table with SHA-256 chain hash | Simple, proven, queryable via JSONB. Chain hash provides tamper evidence. |
| Write pattern | Async write-behind via channel | Zero I/O latency in engine hot path. Natural batching for throughput. |
| State recovery | Full rebuild from PostgreSQL | Replays transactions through existing Ledger validation. No separate snapshot format needed. |
| Storage API | Concrete `StorageService` struct | Monomorphic, no trait indirection. Consistent with `Exchange` and `TradingEngine<E>` patterns. |
| OHLCV ingestion | Background CLI command | User-controlled, on-demand with progress reporting. Server handles pagination and rate limiting. |
| DB testing | `testcontainers-rs` | Real databases in Docker. No mock drift. Each test gets a clean instance. |
| Export formats | CSV + JSON | CSV for spreadsheet/tax tools, JSON for programmatic consumption. Both via CLI command. |

---

## 2. Architecture

### 2.1 Updated Dependency Graph

```
ingot-primitives  (zero deps, #[repr(C)] Copy types)
    ├── ingot-config          (clap, dotenvy)
    ├── ingot-core            (accounting, execution, feed, api)
    │   └── ingot-storage     (sqlx, questdb-rs)  ← NEW
    │       └── ingot-connectivity  (Exchange trait, Kraken, Paper)
    │           └── ingot-engine    (TradingEngine<E>, strategies)
    │               └── ingot-server (daemon, IPC threads, storage task)  ← MODIFIED
    ├── ingot-ipc             (iceoryx2 #[repr(C)] types)  ← MODIFIED (new commands)
    │   ├── ingot-cli         (REPL client)  ← MODIFIED (new commands)
    │   └── ingot-gui         (egui client)  ← MODIFIED (trade log view)
```

**New crate:** `ingot-storage` sits between `ingot-core` and `ingot-connectivity` in the dependency chain.

**Modified crates:**
- `ingot-server`: Owns the storage task, passes `StorageService` to engine, runs migrations on startup, recovers ledger from DB.
- `ingot-ipc`: New `IpcCommand` variants for audit queries, trade history, OHLCV ingestion, export.
- `ingot-cli`: New REPL commands for audit, history, ingest, export.
- `ingot-gui`: Trade log panel with filtering.

### 2.2 Runtime Architecture (Phase 2)

```
                                    ┌─────────────┐
                                    │  PostgreSQL  │
                                    │  (port 5432) │
                                    └──────▲───────┘
                                           │ sqlx (async)
┌──────────┐  iceoryx2   ┌────────────────┐│
│  CLI     │◄───────────►│  ingot-server  ││  ┌─────────────┐
│  (REPL)  │  shared mem │                ││  │   QuestDB   │
└──────────┘             │  ┌───────────┐ ││  │  (port 9009) │
                         │  │  Engine   │ ││  └──────▲───────┘
┌──────────┐  iceoryx2   │  │  Event    │ ││         │ ILP (TCP)
│   GUI    │◄───────────►│  │  Loop     │ ││         │
│  (egui)  │  shared mem │  └─────┬─────┘ ││  ┌──────┴───────┐
└──────────┘             │        │        ││  │  Storage     │
                         │        │ mpsc   ││  │  Task        │
                         │        ▼        ▼│  │  (tokio)     │
                         │  ┌─────────────┐ │  └──────────────┘
                         │  │  Storage    │─┘         ▲
                         │  │  Channel    │───────────┘
                         │  │  (mpsc)     │  StorageEvent
                         │  └─────────────┘
                         └────────────────┘
```

**Data flow for a trade:**
1. Engine receives tick → strategy emits order → exchange fills order
2. Engine calls `record_fill()` → updates in-memory Ledger
3. Engine sends `StorageEvent::TransactionPosted { tx }` + `StorageEvent::AuditEntry { event }` to storage channel
4. Storage task receives events, batches them, writes to PostgreSQL
5. Simultaneously, engine sends `StorageEvent::Tick { ticker }` for every incoming tick
6. Storage task writes ticks to QuestDB via ILP

---

## 3. PostgreSQL Schema

### 3.1 Accounts Table

```sql
CREATE TABLE accounts (
    id            UUID        PRIMARY KEY,
    name          TEXT        NOT NULL,
    account_type  TEXT        NOT NULL CHECK (account_type IN (
                      'Asset', 'Liability', 'Equity', 'Revenue', 'Expense'
                  )),
    currency_code TEXT        NOT NULL,
    decimals      SMALLINT    NOT NULL DEFAULT 2,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_accounts_currency ON accounts (currency_code);
CREATE INDEX idx_accounts_type ON accounts (account_type);
```

Maps directly to `ingot_core::accounting::Account`. The `id` is the existing `Uuid` from the in-memory `Account`.

### 3.2 Transactions Table

```sql
CREATE TABLE transactions (
    id          UUID        PRIMARY KEY,
    posted_at   TIMESTAMPTZ NOT NULL,
    description TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_transactions_posted_at ON transactions (posted_at);
```

### 3.3 Entries Table

```sql
CREATE TABLE entries (
    id             BIGSERIAL   PRIMARY KEY,
    transaction_id UUID        NOT NULL REFERENCES transactions(id),
    account_id     UUID        NOT NULL REFERENCES accounts(id),
    side           TEXT        NOT NULL CHECK (side IN ('Debit', 'Credit')),
    amount         NUMERIC     NOT NULL,
    currency_code  TEXT        NOT NULL,
    decimals       SMALLINT    NOT NULL DEFAULT 2
);

CREATE INDEX idx_entries_transaction ON entries (transaction_id);
CREATE INDEX idx_entries_account ON entries (account_id);
```

Entries are stored flat, not as JSONB. This enables efficient balance queries and per-account history without JSON extraction.

### 3.4 Order History Table

```sql
CREATE TABLE order_history (
    id            BIGSERIAL   PRIMARY KEY,
    exchange_id   TEXT,
    client_id     TEXT,
    symbol        TEXT        NOT NULL,
    side          TEXT        NOT NULL CHECK (side IN ('Buy', 'Sell')),
    order_type    TEXT        NOT NULL CHECK (order_type IN ('Market', 'Limit')),
    quantity      NUMERIC     NOT NULL,
    price         NUMERIC,
    status        TEXT        NOT NULL,
    submitted_at  TIMESTAMPTZ NOT NULL,
    filled_at     TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_order_history_symbol ON order_history (symbol);
CREATE INDEX idx_order_history_submitted ON order_history (submitted_at);
CREATE INDEX idx_order_history_status ON order_history (status);
```

### 3.5 Audit Log Table

```sql
CREATE TABLE audit_log (
    id         BIGSERIAL   PRIMARY KEY,
    timestamp  TIMESTAMPTZ NOT NULL DEFAULT now(),
    event_type TEXT        NOT NULL,
    actor      TEXT        NOT NULL,
    entity_id  UUID,
    payload    JSONB       NOT NULL,
    checksum   BYTEA       NOT NULL
);

CREATE INDEX idx_audit_log_timestamp ON audit_log (timestamp);
CREATE INDEX idx_audit_log_event_type ON audit_log (event_type);
CREATE INDEX idx_audit_log_entity_id ON audit_log (entity_id);
```

**Event types:**
- `account_created` — New account added to ledger
- `transaction_posted` — Double-entry transaction committed
- `order_placed` — Order submitted to exchange
- `order_filled` — Order execution confirmed
- `order_rejected` — Order rejected by exchange
- `engine_started` — Server startup (with recovery info)
- `engine_shutdown` — Graceful shutdown
- `ohlcv_ingested` — Historical data batch loaded

**Actor values:**
- `engine` — Automated engine operations
- `strategy:<name>` — Strategy-initiated (e.g., `strategy:quoteboard`)
- `manual` — User-initiated via CLI or GUI
- `system` — Server lifecycle events

**Chain hash algorithm:**
```
checksum[0] = SHA-256(payload[0])
checksum[n] = SHA-256(checksum[n-1] || payload[n])
```

The chain can be verified by reading all rows in sequence and recomputing each hash. A broken chain indicates tampering or data corruption.

### 3.6 Append-Only Enforcement

PostgreSQL-level enforcement via restricted role:

```sql
-- Application connects as ingot_app role
CREATE ROLE ingot_app LOGIN PASSWORD '...';

-- Full read access
GRANT SELECT ON ALL TABLES IN SCHEMA public TO ingot_app;

-- Insert-only on audit_log (no UPDATE, no DELETE)
GRANT INSERT ON audit_log TO ingot_app;
GRANT USAGE, SELECT ON SEQUENCE audit_log_id_seq TO ingot_app;

-- Full write on other tables
GRANT INSERT, UPDATE, DELETE ON accounts, transactions, entries, order_history TO ingot_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO ingot_app;
```

The `ingot_app` role can never UPDATE or DELETE rows from `audit_log`. Administrative access (schema migrations, maintenance) uses a separate privileged role.

---

## 4. QuestDB Schema

### 4.1 Ticks Table

```sql
CREATE TABLE ticks (
    symbol    SYMBOL,
    price     DOUBLE,
    timestamp TIMESTAMP
) TIMESTAMP(timestamp) PARTITION BY DAY
  WAL
  DEDUP UPSERT KEYS(symbol, timestamp);
```

**Notes:**
- `SYMBOL` type is QuestDB's indexed string type (efficient for high-cardinality filtering).
- `DOUBLE` for price: QuestDB does not support `NUMERIC`/`DECIMAL`. Prices are converted from `rust_decimal::Decimal` to `f64` for storage. The canonical price remains `Decimal` in the application — QuestDB is an analytical store, not the source of truth.
- Partitioned by day for efficient time-range queries and automatic retention.
- WAL mode for concurrent ingestion and reads.
- Deduplication on `(symbol, timestamp)` prevents duplicate ticks on re-ingestion.

### 4.2 OHLCV Bars Table

```sql
CREATE TABLE ohlcv_bars (
    symbol    SYMBOL,
    interval  SYMBOL,
    open      DOUBLE,
    high      DOUBLE,
    low       DOUBLE,
    close     DOUBLE,
    volume    DOUBLE,
    count     INT,
    timestamp TIMESTAMP
) TIMESTAMP(timestamp) PARTITION BY MONTH
  WAL
  DEDUP UPSERT KEYS(symbol, interval, timestamp);
```

**Interval values:** `1m`, `5m`, `15m`, `30m`, `1h`, `4h`, `1d`, `1w`

### 4.3 Ingestion Protocol

QuestDB ingestion uses the InfluxDB Line Protocol (ILP) over TCP (port 9009):

```
ticks,symbol=BTC-USD price=67432.50 1709164800000000000
ohlcv_bars,symbol=BTC-USD,interval=1h open=67400.0,high=67500.0,low=67350.0,close=67432.5,volume=123.45,count=1847 1709164800000000000
```

The `questdb-rs` crate provides a type-safe Rust API for constructing ILP messages. Ingestion is fire-and-forget (no response) which suits the async write-behind pattern.

---

## 5. `ingot-storage` Crate Design

### 5.1 Crate Structure

```
crates/ingot-storage/
├── Cargo.toml
├── src/
│   ├── lib.rs          (re-exports, StorageConfig)
│   ├── postgres/
│   │   ├── mod.rs      (PgStorage struct, connection pool)
│   │   ├── accounts.rs (account CRUD)
│   │   ├── transactions.rs (transaction + entry persistence)
│   │   ├── orders.rs   (order history persistence)
│   │   ├── audit.rs    (audit log writes, chain hash, queries)
│   │   └── recovery.rs (ledger rebuild on startup)
│   ├── questdb/
│   │   ├── mod.rs      (QuestDbStorage struct, ILP sender)
│   │   ├── ticks.rs    (tick ingestion)
│   │   └── ohlcv.rs    (OHLCV bar ingestion + queries)
│   ├── service.rs      (StorageService: unified facade)
│   ├── event.rs        (StorageEvent enum)
│   └── export.rs       (CSV + JSON export functions)
├── migrations/
│   ├── 001_create_accounts.sql
│   ├── 002_create_transactions.sql
│   ├── 003_create_entries.sql
│   ├── 004_create_order_history.sql
│   └── 005_create_audit_log.sql
└── tests/
    └── integration.rs  (testcontainers-based tests)
```

### 5.2 Dependencies

All versions are centralized in the root `[workspace.dependencies]` per the workspace dependency rules. The `ingot-storage` member crate references them with `workspace = true` and appends only the features it specifically needs beyond the workspace baseline.

**New entries added to root `[workspace.dependencies]`:**

```toml
# Root Cargo.toml — [workspace.dependencies] additions
csv = { version = "1", default-features = false }
questdb-rs = { version = "4", default-features = false }
sqlx = { version = "0.8", default-features = false }
testcontainers = { version = "0.23", default-features = false }
testcontainers-modules = { version = "0.11", default-features = false }
```

**Member crate `Cargo.toml`:**

```toml
# crates/ingot-storage/Cargo.toml

[dependencies]
ingot-core = { path = "../ingot-core" }
ingot-primitives = { path = "../ingot-primitives" }

# Workspace deps — no versions here (Rule 1)
# Additional features appended locally (Rule 3)
anyhow = { workspace = true }
chrono = { workspace = true }
csv = { workspace = true }
questdb-rs = { workspace = true, features = ["ilp-over-tcp"] }
rust_decimal = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
sqlx = { workspace = true, features = [
    "chrono", "macros", "migrate", "postgres",
    "runtime-tokio", "tls-rustls", "uuid"
] }
tracing = { workspace = true }
uuid = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
testcontainers = { workspace = true }
testcontainers-modules = { workspace = true, features = ["postgres"] }
tokio = { workspace = true }
```

Note: `sqlx` features (`runtime-tokio`, `tls-rustls`, `postgres`, etc.) are appended locally because only `ingot-storage` needs them — no other crate in the workspace uses `sqlx`. Similarly, `questdb-rs` features are local to this crate. This follows Rule 3 (granular feature flags).

### 5.3 `StorageConfig`

```rust
/// Configuration for database connections.
pub struct StorageConfig {
    /// PostgreSQL connection string.
    /// Example: "postgres://ingot_app:password@localhost:5432/ingot"
    pub pg_url: String,

    /// QuestDB ILP endpoint (host:port).
    /// Example: "localhost:9009"
    pub questdb_ilp_addr: String,

    /// PostgreSQL connection pool size.
    /// Default: 5
    pub pg_pool_size: u32,

    /// Maximum events to buffer before flushing to PostgreSQL.
    /// Default: 50
    pub pg_batch_size: usize,

    /// Maximum time before flushing buffered events (milliseconds).
    /// Default: 100
    pub flush_interval_ms: u64,
}
```

Loaded from environment variables (consistent with existing `dotenvy` pattern):
- `DATABASE_URL` → `pg_url`
- `QUESTDB_ILP_ADDR` → `questdb_ilp_addr` (default: `localhost:9009`)
- `PG_POOL_SIZE` → `pg_pool_size` (default: `5`)
- `STORAGE_BATCH_SIZE` → `pg_batch_size` (default: `50`)
- `STORAGE_FLUSH_INTERVAL_MS` → `flush_interval_ms` (default: `100`)

### 5.4 `StorageEvent`

The engine sends these to the storage channel. The storage task processes them.

```rust
pub enum StorageEvent {
    /// A new account was created in the ledger.
    AccountCreated {
        account: Account,
    },

    /// A validated transaction was posted to the ledger.
    TransactionPosted {
        transaction: Transaction,
    },

    /// An order was submitted to an exchange.
    OrderPlaced {
        request: OrderRequest,
        acknowledgment: OrderAcknowledgment,
    },

    /// A market data tick was received.
    Tick {
        ticker: Ticker,
    },

    /// Flush all buffered events to storage immediately.
    /// Used during graceful shutdown.
    Flush {
        done: tokio::sync::oneshot::Sender<()>,
    },
}
```

### 5.5 `StorageService`

```rust
pub struct StorageService {
    pg: PgPool,
    questdb: questdb_rs::ingress::Sender,
}

impl StorageService {
    /// Connect to PostgreSQL and QuestDB. Run migrations.
    pub async fn connect(config: &StorageConfig) -> Result<Self>;

    // --- PostgreSQL writes ---

    /// Insert an account row.
    pub async fn save_account(&self, account: &Account) -> Result<()>;

    /// Insert a transaction and its entries in a single DB transaction.
    pub async fn save_transaction(&self, tx: &Transaction) -> Result<()>;

    /// Insert an order history record.
    pub async fn save_order(&self, req: &OrderRequest, ack: &OrderAcknowledgment) -> Result<()>;

    /// Append an audit log entry with chain hash.
    pub async fn append_audit(
        &self,
        event_type: &str,
        actor: &str,
        entity_id: Option<Uuid>,
        payload: &serde_json::Value,
    ) -> Result<()>;

    // --- PostgreSQL reads ---

    /// Load all accounts from the database.
    pub async fn load_accounts(&self) -> Result<Vec<Account>>;

    /// Load all transactions with entries, ordered by posted_at.
    pub async fn load_transactions(&self) -> Result<Vec<Transaction>>;

    /// Rebuild a complete Ledger from persisted state.
    pub async fn recover_ledger(&self) -> Result<Ledger>;

    /// Query order history with optional filters.
    pub async fn query_orders(
        &self,
        symbol: Option<Symbol>,
        since: Option<DateTime<Utc>>,
        limit: Option<i64>,
    ) -> Result<Vec<OrderHistoryRow>>;

    /// Query audit log with optional filters.
    pub async fn query_audit(
        &self,
        event_type: Option<&str>,
        since: Option<DateTime<Utc>>,
        limit: Option<i64>,
    ) -> Result<Vec<AuditRow>>;

    /// Verify audit log chain integrity. Returns the first broken link, if any.
    pub async fn verify_audit_chain(&self) -> Result<Option<AuditChainBreak>>;

    // --- QuestDB writes ---

    /// Ingest a single tick via ILP.
    pub fn ingest_tick(&mut self, ticker: &Ticker) -> Result<()>;

    /// Ingest a batch of OHLCV bars via ILP.
    pub fn ingest_ohlcv_bars(&mut self, bars: &[OhlcvBar]) -> Result<()>;

    /// Flush the QuestDB ILP buffer.
    pub fn flush_questdb(&mut self) -> Result<()>;

    // --- Export ---

    /// Export audit log entries to a CSV writer.
    pub async fn export_audit_csv<W: std::io::Write>(
        &self,
        writer: W,
        since: Option<DateTime<Utc>>,
    ) -> Result<u64>;

    /// Export audit log entries to a JSON writer.
    pub async fn export_audit_json<W: std::io::Write>(
        &self,
        writer: W,
        since: Option<DateTime<Utc>>,
    ) -> Result<u64>;

    /// Export order history to a CSV writer.
    pub async fn export_orders_csv<W: std::io::Write>(
        &self,
        writer: W,
        since: Option<DateTime<Utc>>,
    ) -> Result<u64>;

    /// Export order history to a JSON writer.
    pub async fn export_orders_json<W: std::io::Write>(
        &self,
        writer: W,
        since: Option<DateTime<Utc>>,
    ) -> Result<u64>;
}
```

### 5.6 `OhlcvBar` Type

New domain type in `ingot-core` (or `ingot-storage` if kept internal):

```rust
/// A single OHLCV candlestick bar.
pub struct OhlcvBar {
    pub symbol: Symbol,
    pub interval: BarInterval,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub trade_count: u32,
    pub timestamp: DateTime<Utc>,
}

/// Supported bar intervals.
pub enum BarInterval {
    M1,   // 1 minute
    M5,   // 5 minutes
    M15,  // 15 minutes
    M30,  // 30 minutes
    H1,   // 1 hour
    H4,   // 4 hours
    D1,   // 1 day
    W1,   // 1 week
}
```

---

## 6. Storage Task

### 6.1 Channel and Task Lifecycle

The storage task is a dedicated tokio task spawned by `ingot-server`. It owns the `StorageService` and receives `StorageEvent` messages from the engine via an mpsc channel.

```rust
/// Named capacity constant (consistent with existing pattern).
pub const STORAGE_CHANNEL_CAPACITY: usize = 256;
```

```rust
pub async fn run_storage_task(
    mut service: StorageService,
    mut rx: mpsc::Receiver<StorageEvent>,
    config: &StorageConfig,
) -> Result<()> {
    let mut pg_buffer: Vec<StorageEvent> = Vec::with_capacity(config.pg_batch_size);
    let flush_interval = Duration::from_millis(config.flush_interval_ms);
    let mut flush_timer = tokio::time::interval(flush_interval);

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(StorageEvent::Tick { ticker }) => {
                        // Ticks go directly to QuestDB (fire-and-forget ILP)
                        service.ingest_tick(&ticker)?;
                    }
                    Some(StorageEvent::Flush { done }) => {
                        // Graceful shutdown: flush everything
                        flush_pg_buffer(&mut service, &mut pg_buffer).await?;
                        service.flush_questdb()?;
                        let _ = done.send(());
                    }
                    Some(event) => {
                        // Buffer PostgreSQL events
                        pg_buffer.push(event);
                        if pg_buffer.len() >= config.pg_batch_size {
                            flush_pg_buffer(&mut service, &mut pg_buffer).await?;
                        }
                    }
                    None => {
                        // Channel closed — final flush and exit
                        flush_pg_buffer(&mut service, &mut pg_buffer).await?;
                        service.flush_questdb()?;
                        break;
                    }
                }
            }
            _ = flush_timer.tick() => {
                // Periodic flush for low-activity periods
                if !pg_buffer.is_empty() {
                    flush_pg_buffer(&mut service, &mut pg_buffer).await?;
                }
                service.flush_questdb()?;
            }
        }
    }

    Ok(())
}
```

### 6.2 Batch Flush Logic

```rust
async fn flush_pg_buffer(
    service: &mut StorageService,
    buffer: &mut Vec<StorageEvent>,
) -> Result<()> {
    // Group events by type for efficient batch inserts
    for event in buffer.drain(..) {
        match event {
            StorageEvent::AccountCreated { account } => {
                service.save_account(&account).await?;
                service.append_audit("account_created", "engine", Some(account.id), &payload).await?;
            }
            StorageEvent::TransactionPosted { transaction } => {
                service.save_transaction(&transaction).await?;
                service.append_audit("transaction_posted", "engine", Some(transaction.id), &payload).await?;
            }
            StorageEvent::OrderPlaced { request, acknowledgment } => {
                service.save_order(&request, &acknowledgment).await?;
                let event_type = match acknowledgment.status {
                    OrderStatus::Filled => "order_filled",
                    OrderStatus::Rejected => "order_rejected",
                    _ => "order_placed",
                };
                service.append_audit(event_type, "engine", None, &payload).await?;
            }
            _ => {} // Tick and Flush handled in the main loop
        }
    }
    Ok(())
}
```

### 6.3 Graceful Shutdown

The server's shutdown sequence must flush the storage buffer before exiting:

```
1. Ctrl+C received (or shutdown command)
2. Send EngineCommand::Shutdown to engine
3. Engine processes remaining events, drops storage_tx sender
4. Send StorageEvent::Flush { done } to storage channel
5. Wait for done.recv() (confirms all data persisted)
6. Storage task exits (channel closed)
7. Join IPC threads
8. Server exits
```

---

## 7. State Recovery

### 7.1 Recovery Sequence

On server startup, before the engine event loop begins:

```
1. Connect to PostgreSQL (StorageService::connect)
2. Run migrations (sqlx::migrate!().run(&pool))
3. Attempt ledger recovery:
   a. Load accounts from DB
   b. Load transactions (ordered by posted_at)
   c. Replay through Ledger::prepare_transaction + post_transaction
   d. If DB is empty → fall back to bootstrap_ledger()
4. Create QuoteBoard (empty — repopulated from live ticks)
5. Log recovery summary (account count, transaction count, final NAV)
6. Start engine event loop
```

### 7.2 Recovery in `StorageService`

```rust
impl StorageService {
    pub async fn recover_ledger(&self) -> Result<Ledger> {
        let mut ledger = Ledger::new();

        let accounts = self.load_accounts().await
            .context("Failed to load accounts from database")?;

        if accounts.is_empty() {
            tracing::info!("No persisted state found, bootstrapping fresh ledger");
            return Ok(bootstrap_ledger()?);
        }

        for account in &accounts {
            ledger.add_account(account.clone())
                .with_context(|| format!("Failed to add account {}", account.id))?;
        }

        let transactions = self.load_transactions().await
            .context("Failed to load transactions from database")?;

        for tx in transactions {
            let vtx = ledger.prepare_transaction(tx)
                .context("Failed to validate recovered transaction")?;
            ledger.post_transaction(vtx);
        }

        tracing::info!(
            accounts = accounts.len(),
            transactions = ledger.transaction_count(),
            "Ledger recovered from database"
        );

        // Log audit event for recovery
        self.append_audit("engine_started", "system", None, &serde_json::json!({
            "recovered_accounts": accounts.len(),
            "recovered_transactions": ledger.transaction_count(),
        })).await?;

        Ok(ledger)
    }
}
```

### 7.3 First-Run vs Recovery

The server distinguishes between first run and recovery based on whether the `accounts` table has rows:

| Scenario | Behavior |
|:---|:---|
| Empty database (first run) | `bootstrap_ledger()` creates default accounts + initial deposit. Accounts and bootstrap transaction are persisted via the storage channel. |
| Populated database (restart) | Full replay via `recover_ledger()`. No `bootstrap_ledger()` call. |
| Corrupted database | Recovery fails with error. Server refuses to start. Manual intervention required. |

---

## 8. Engine Integration

### 8.1 Changes to `TradingEngine<E>`

The engine gains a storage channel sender:

```rust
pub struct TradingEngine<E: Exchange> {
    ledger: Arc<RwLock<Ledger>>,
    market_data: Arc<RwLock<QuoteBoard>>,
    exchange: E,
    strategies: Vec<Box<dyn Strategy>>,
    event_tx: Option<broadcast::Sender<WsMessage>>,
    storage_tx: Option<mpsc::Sender<StorageEvent>>,  // NEW
}
```

**Injection point:** The server creates the channel and passes the sender to the engine via a new `.with_storage(storage_tx)` builder method (mirroring the existing `.with_broadcast()` pattern).

### 8.2 Storage Event Emission Points

| Engine Method | Storage Event | When |
|:---|:---|:---|
| `ensure_accounts_for_symbol` | `AccountCreated` | New account added to ledger |
| `record_fill` (after `post_order_fill`) | `TransactionPosted` | Ledger transaction posted |
| `handle_order_request` | `OrderPlaced` | Order submitted + ack received |
| `handle_tick` | `Tick` | Every incoming market data tick |

All sends are non-blocking (`storage_tx.try_send()`). If the channel is full (storage task is behind), ticks are dropped (acceptable — ticks are best-effort for storage). Financial events (transactions, orders) use `.send().await` to apply backpressure rather than lose data.

### 8.3 Storage Event Sending Strategy

```rust
// Ticks: best-effort, drop if channel full
if let Some(ref storage_tx) = self.storage_tx {
    let _ = storage_tx.try_send(StorageEvent::Tick { ticker: tick.clone() });
}

// Financial events: backpressure (await send)
if let Some(ref storage_tx) = self.storage_tx {
    storage_tx.send(StorageEvent::TransactionPosted { transaction })
        .await
        .context("Storage channel closed")?;
}
```

---

## 9. Historical Data Ingestion

### 9.1 Kraken OHLCV REST API

**Endpoint:** `GET https://api.kraken.com/0/public/OHLC`

**Parameters:**
- `pair` — Kraken pair format (e.g., `XBTUSD`)
- `interval` — Minutes: 1, 5, 15, 30, 60, 240, 1440, 10080
- `since` — Unix timestamp (returns bars after this time)

**Response:** Array of `[time, open, high, low, close, vwap, volume, count]`

**Limits:**
- Returns up to 720 bars per request.
- No authentication required (public endpoint).
- Rate limit: ~1 request/second for public endpoints.

### 9.2 Ingestion Flow

```
CLI: "ingest ohlcv BTC-USD 1h --since 2025-01-01"
  |
  | IpcCommand::IngestOhlcv { symbol, interval, since }
  v
Server: ipc_command thread receives request
  |
  | EngineCommand::IngestOhlcv { symbol, interval, since, progress_tx }
  v
Engine: spawns background tokio task
  |
  | Loop:
  |   1. Convert symbol to KrakenPair
  |   2. GET /0/public/OHLC?pair=...&interval=...&since=...
  |   3. Parse response into Vec<OhlcvBar>
  |   4. Send StorageEvent::OhlcvBatch { bars } to storage channel
  |   5. Update since = last bar timestamp
  |   6. If < 720 bars returned → done
  |   7. Sleep 1s (rate limit)
  |   8. Report progress via progress_tx
  v
Storage task: ingest_ohlcv_bars(&bars) → QuestDB ILP
  |
  v
CLI: receives progress updates, displays to user
```

### 9.3 New IPC Types

```rust
// New IpcCommand variant
pub enum IpcCommand {
    // ... existing variants ...
    IngestOhlcv {
        symbol: Symbol,
        interval: u8,       // BarInterval discriminant
        since_secs: i64,    // Unix timestamp
    },
}

// New IpcCommandResponse variant
pub enum IpcCommandResponse {
    // ... existing variants ...
    IngestProgress {
        symbol: Symbol,
        bars_ingested: u64,
        complete: bool,
    },
}
```

---

## 10. New IPC Commands

### 10.1 Summary of New Commands

| Command | Request | Response | Description |
|:---|:---|:---|:---|
| `QueryAudit` | `{ event_type, since, limit }` | `AuditEntries { entries, count }` | Query audit log with filters |
| `QueryOrders` | `{ symbol, since, limit }` | `OrderEntries { entries, count }` | Query order history |
| `ExportAudit` | `{ format, since, path }` | `ExportResult { path, count }` | Export audit log to file |
| `ExportOrders` | `{ format, since, path }` | `ExportResult { path, count }` | Export order history to file |
| `IngestOhlcv` | `{ symbol, interval, since }` | `IngestProgress { bars, complete }` | Ingest historical bars |
| `VerifyAudit` | `{}` | `AuditVerifyResult { valid, break_at }` | Verify audit chain integrity |

### 10.2 IPC Type Additions

New `#[repr(C)]` types in `ingot-ipc` for the above commands, following the existing pattern of `FixedId` for strings and bool+value for optionals. Audit entries and order history rows are returned as fixed-size structs (not JSONB — that stays in PostgreSQL only).

---

## 11. CLI Additions

### 11.1 New REPL Commands

```
audit [--type <event_type>] [--since <date>] [--limit <n>]
    Query and display audit log entries.
    Default: last 50 entries.

audit verify
    Verify the audit log chain hash integrity.

audit export --format <csv|json> [--since <date>] [--output <path>]
    Export audit log to file.
    Default output: audit_<date>.csv or audit_<date>.json

history [--symbol <SYM>] [--since <date>] [--limit <n>]
    Query and display order history.
    Default: last 50 orders.

history export --format <csv|json> [--since <date>] [--output <path>]
    Export order history to file.

ingest ohlcv <SYMBOL> <INTERVAL> [--since <date>]
    Ingest historical OHLCV bars from the exchange.
    Intervals: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w
    Shows progress bar during ingestion.
```

### 11.2 Display Format

Audit log and order history are displayed as tables using aligned columns:

```
> audit --limit 3
TIMESTAMP            EVENT TYPE          ACTOR    ENTITY ID
2026-02-28 14:30:01  transaction_posted  engine   a1b2c3d4-...
2026-02-28 14:30:01  order_filled        engine   e5f6a7b8-...
2026-02-28 14:29:58  order_placed        manual   e5f6a7b8-...

> history --symbol BTC-USD --limit 3
TIMESTAMP            SYMBOL   SIDE  TYPE    QTY      PRICE       STATUS
2026-02-28 14:30:01  BTC-USD  Buy   Market  0.001    67432.50    Filled
2026-02-28 14:25:00  BTC-USD  Sell  Limit   0.002    68000.00    Filled
2026-02-28 14:20:00  BTC-USD  Buy   Market  0.001    67100.00    Filled
```

---

## 12. GUI Additions

### 12.1 Trade Log Panel

A new panel in the egui GUI displaying order history in a scrollable table:

- Columns: Timestamp, Symbol, Side, Type, Quantity, Price, Status
- Color coding: Buy (green), Sell (red), Rejected (gray)
- Filtering: symbol dropdown, date range, status filter
- Auto-refresh: subscribes to `SERVICE_ORDERS` IPC pub/sub for real-time updates
- Manual refresh button for historical data

### 12.2 Audit Log Tab

An optional tab (collapsed by default) showing recent audit events:

- Columns: Timestamp, Event Type, Actor, Entity ID
- Chain status indicator (green check / red warning based on last verification)
- Export button (triggers file save dialog for CSV/JSON)

---

## 13. Testing Strategy

### 13.1 Test Categories

| Category | Framework | Scope |
|:---|:---|:---|
| Schema migrations | testcontainers + sqlx | All migrations apply cleanly to fresh DB |
| CRUD operations | testcontainers + sqlx | Each `StorageService` method |
| Audit chain hash | testcontainers | Insert N events, verify chain, corrupt one, verify detects break |
| Ledger recovery | testcontainers | Persist accounts + transactions, recover, compare NAV |
| QuestDB ingestion | testcontainers | Ingest ticks, query back, verify values |
| OHLCV parsing | unit tests | Parse Kraken OHLC response JSON into `OhlcvBar` |
| Storage task | integration test | Send events to channel, verify they reach the database |
| Export | unit tests | Verify CSV/JSON output format against known input |
| Round-trip fidelity | proptest (1000 cases) | `Account` → DB → `Account` preserves all fields |
| Decimal precision | proptest (1000 cases) | `Decimal` → PostgreSQL `NUMERIC` → `Decimal` is lossless |

### 13.2 Test Infrastructure

```rust
/// Shared test helper: spin up PostgreSQL, run migrations, return pool.
async fn test_pg_pool() -> Result<(PgPool, ContainerAsync<Postgres>)> {
    let container = Postgres::default().start().await?;
    let url = format!(
        "postgres://postgres:postgres@localhost:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok((pool, container))
}
```

Each test function calls `test_pg_pool()` to get an isolated database instance. The container is dropped (and destroyed) when the test completes.

---

## 14. Configuration

### 14.1 Environment Variables (New)

| Variable | Default | Description |
|:---|:---|:---|
| `DATABASE_URL` | *(required)* | PostgreSQL connection string |
| `QUESTDB_ILP_ADDR` | `localhost:9009` | QuestDB ILP endpoint |
| `PG_POOL_SIZE` | `5` | PostgreSQL connection pool max connections |
| `STORAGE_BATCH_SIZE` | `50` | Max events buffered before flush |
| `STORAGE_FLUSH_INTERVAL_MS` | `100` | Max time before periodic flush |

### 14.2 Server Config Update

`ServerConfig` (in `ingot-config`) gains storage-related fields:

```rust
pub struct ServerConfig {
    // ... existing fields ...
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: Option<String>,

    #[arg(long, env = "QUESTDB_ILP_ADDR", default_value = "localhost:9009")]
    pub questdb_addr: String,
}
```

When `database_url` is `None`, the server runs in memory-only mode (current behavior). This preserves backward compatibility and allows running without databases during development.

---

## 15. Migration Strategy

### 15.1 sqlx Migrations

Migrations live in `crates/ingot-storage/migrations/` and are embedded in the binary via `sqlx::migrate!()`. They run automatically on server startup before ledger recovery.

**Migration order:**
1. `001_create_accounts.sql` — Accounts table
2. `002_create_transactions.sql` — Transactions table
3. `003_create_entries.sql` — Entries table with foreign keys
4. `004_create_order_history.sql` — Order history table
5. `005_create_audit_log.sql` — Audit log table with indexes
6. `006_create_roles.sql` — Application role with restricted permissions

### 15.2 QuestDB Tables

QuestDB tables are created via SQL over the PostgreSQL wire protocol (port 8812) during `StorageService::connect()`. QuestDB supports `CREATE TABLE IF NOT EXISTS`, making this idempotent.

### 15.3 Offline Mode for `cargo build`

Since `sqlx` compile-time checking requires a live database, the project uses sqlx's offline mode:

```bash
# Developer runs this once to cache query metadata:
cargo sqlx prepare --workspace

# Cached metadata stored in .sqlx/ directory (committed to git)
# Subsequent builds use cached metadata, no live DB needed
```

The `.sqlx/` directory is committed to the repository so that `cargo build` works without a running PostgreSQL instance.

---

## 16. Implementation Phases

Phase 2 is broken into sequential sub-steps. Each step is a self-contained PR targeting `dev`.

### Step 2.1: `ingot-storage` Crate Skeleton + PostgreSQL Schema

- Create `crates/ingot-storage/` with `Cargo.toml` and module structure.
- Write all SQL migrations (001–006).
- Implement `StorageConfig` and `StorageService::connect()`.
- Implement `save_account()`, `load_accounts()` with compile-time checked queries.
- **Tests:** testcontainers tests for migrations and account CRUD.

### Step 2.2: Transaction + Entry Persistence

- Implement `save_transaction()` (single DB transaction for tx + entries).
- Implement `load_transactions()` (join entries, reconstruct `Transaction` objects).
- **Tests:** Round-trip fidelity tests, proptest for Decimal precision.

### Step 2.3: Ledger Recovery

- Implement `recover_ledger()` in `StorageService`.
- Modify `ingot-server` startup to attempt recovery before `bootstrap_ledger()`.
- **Tests:** Persist N transactions, restart, verify NAV matches.

### Step 2.4: Storage Event Channel + Storage Task

- Define `StorageEvent` enum.
- Implement `run_storage_task()` with batching and periodic flush.
- Add `storage_tx` to `TradingEngine`, emit events from `record_fill`, `ensure_accounts_for_symbol`, `handle_order_request`.
- Wire storage task into `ingot-server` startup and shutdown.
- **Tests:** Integration test: send events → verify in database.

### Step 2.5: Audit Trail

- Implement `append_audit()` with SHA-256 chain hash.
- Implement `query_audit()` and `verify_audit_chain()`.
- Add audit event emission to all storage event flush paths.
- **Tests:** Chain hash verification, tamper detection, query filtering.

### Step 2.6: Order History

- Implement `save_order()` and `query_orders()`.
- Emit `OrderPlaced` storage events from engine.
- **Tests:** Order round-trip, query filtering by symbol/date/status.

### Step 2.7: QuestDB Tick Ingestion

- Implement `ingest_tick()` via ILP.
- Wire tick events into storage task.
- **Tests:** testcontainers with QuestDB, ingest ticks, query back.

### Step 2.8: OHLCV Historical Ingestion

- Add Kraken OHLCV REST endpoint to `ingot-connectivity`.
- Implement `ingest_ohlcv_bars()` in `StorageService`.
- Add `IngestOhlcv` IPC command and engine command.
- Implement paginated fetching with rate limiting.
- **Tests:** wiremock-based test for Kraken OHLC endpoint, ingestion pipeline test.

### Step 2.9: Export (CSV + JSON)

- Implement `export_audit_csv()`, `export_audit_json()`, `export_orders_csv()`, `export_orders_json()`.
- **Tests:** Known input → expected output format.

### Step 2.10: CLI Commands

- Add `audit`, `audit verify`, `audit export`, `history`, `history export`, `ingest ohlcv` commands.
- Add corresponding IPC command variants to `ingot-ipc`.
- Add dispatch handlers in `ingot-server/ipc_command.rs`.
- **Tests:** End-to-end IPC round-trip for each new command.

### Step 2.11: GUI Trade Log Panel

- Add trade log panel with order history table.
- Add audit log tab.
- Subscribe to `SERVICE_ORDERS` for real-time updates.
- Add export buttons.

---

## 17. Risk Assessment

| Risk | Impact | Mitigation |
|:---|:---|:---|
| Storage task crash loses buffered events | Financial events not persisted | Backpressure on financial events (await send). Ticks are best-effort (acceptable loss). |
| PostgreSQL unavailable at startup | Server cannot start | Clear error message. Fall back to memory-only mode if `database_url` is `None`. |
| QuestDB unavailable | Tick data not stored | Log warning, continue operating. Tick storage is non-critical for trading. |
| Chain hash broken after crash | Audit integrity uncertain | `verify_audit_chain()` command. Last event before crash may be incomplete — recovery logs gap. |
| sqlx offline mode stale | Build fails | `.sqlx/` directory kept in sync via `cargo sqlx prepare` after schema changes. |
| testcontainers Docker not available | Tests skip or fail | Document Docker requirement. Tests are integration tests, not blocking unit tests. |
| Decimal → f64 precision loss in QuestDB | Analytical queries slightly imprecise | QuestDB is analytical only. Source of truth is PostgreSQL (`NUMERIC`). Document the tradeoff. |
