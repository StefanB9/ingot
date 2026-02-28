# Implementation Plan

**Project Name:** Ingot — Portfolio Management & Systematic Trading Workstation
**Scope:** Phase 2 — Persistent Storage & Audit Trail
**Date:** February 28, 2026
**Version:** 1.0

---

## Overview

This document is the function-level implementation plan for Phase 2. Each step is a self-contained PR targeting `dev`. Steps are executed sequentially — each depends on the previous. Every task specifies exact files, function signatures, SQL, tests to write first (TDD), and acceptance criteria.

### Design Decisions (from TDD)

| Decision | Choice |
|:---|:---|
| Databases | PostgreSQL + QuestDB |
| PG access | `sqlx` (compile-time checked) |
| Crate | Single `ingot-storage` |
| Audit trail | Append-only table, SHA-256 chain hash |
| Write pattern | Async write-behind via mpsc channel |
| Recovery | Full rebuild from PostgreSQL |
| Storage API | Concrete `StorageService` struct |
| OHLCV ingestion | CLI command → server command thread |
| DB testing | `testcontainers-rs` |
| Export | CSV + JSON |
| `OhlcvBar` location | `ingot-core::feed` |
| sqlx offline | `.sqlx/` committed, `cargo sqlx prepare` |
| PR strategy | One branch per sub-step (11 PRs) |

### Branch Plan

```
dev
 ├─ feature/storage-dev-setup      (Step 0)
 ├─ feature/storage-skeleton       (Step 2.1)
 ├─ feature/transaction-persist    (Step 2.2)
 ├─ feature/ledger-recovery        (Step 2.3)
 ├─ feature/storage-task           (Step 2.4)
 ├─ feature/audit-trail            (Step 2.5)
 ├─ feature/order-history          (Step 2.6)
 ├─ feature/questdb-ticks          (Step 2.7)
 ├─ feature/ohlcv-ingestion        (Step 2.8)
 ├─ feature/export                 (Step 2.9)
 ├─ feature/cli-storage-cmds       (Step 2.10)
 └─ feature/gui-trade-log          (Step 2.11)
```

---

## Step 0: Dev Environment Setup

**Branch:** `feature/storage-dev-setup`

Sets up the development database infrastructure. No Rust code changes.

### Task 0.1: Create `docker-compose.yml`

**File:** `docker-compose.yml` (repo root)

```yaml
services:
  postgres:
    image: postgres:17
    ports:
      - "5432:5432"
    environment:
      POSTGRES_DB: ingot
      POSTGRES_USER: ingot_app
      POSTGRES_PASSWORD: ingot_dev
    volumes:
      - pgdata:/var/lib/postgresql/data

  questdb:
    image: questdb/questdb:8.2.3
    ports:
      - "9009:9009"
      - "8812:8812"
      - "9000:9000"
    volumes:
      - questdata:/var/lib/questdb

volumes:
  pgdata:
  questdata:
```

### Task 0.2: Create `.env.example`

**File:** `.env.example` (repo root)

```env
# PostgreSQL
DATABASE_URL=postgres://ingot_app:ingot_dev@localhost:5432/ingot

# QuestDB
QUESTDB_ILP_ADDR=localhost:9009

# Storage tuning (optional)
PG_POOL_SIZE=5
STORAGE_BATCH_SIZE=50
STORAGE_FLUSH_INTERVAL_MS=100
```

### Task 0.3: Update `.gitignore`

**File:** `.gitignore`

Add:
```
.env
!.env.example
```

Do **not** ignore `.sqlx/` — it must be committed.

### Acceptance Criteria

- [ ] `docker compose up -d` starts PostgreSQL 17 and QuestDB 8.2.3
- [ ] `psql postgres://ingot_app:ingot_dev@localhost:5432/ingot` connects successfully
- [ ] QuestDB web console accessible at `http://localhost:9000`
- [ ] `.env.example` documents all required environment variables

---

## Step 2.1: `ingot-storage` Crate Skeleton + Account CRUD

**Branch:** `feature/storage-skeleton`
**Depends on:** Step 0

### Task 2.1.1: Create crate structure

**Files to create:**

```
crates/ingot-storage/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── config.rs
│   ├── service.rs
│   ├── postgres/
│   │   ├── mod.rs
│   │   └── accounts.rs
│   └── questdb/
│       └── mod.rs
└── migrations/
    └── (empty, populated in Task 2.1.3)
```

**File: `crates/ingot-storage/Cargo.toml`**

```toml
[package]
name = "ingot-storage"
version.workspace = true
edition.workspace = true
authors.workspace = true

[lints]
workspace = true

[dependencies]
ingot-core = { path = "../ingot-core" }
ingot-primitives = { path = "../ingot-primitives" }

anyhow = { workspace = true }
chrono = { workspace = true }
rust_decimal = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true, features = ["std"] }
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

**File: root `Cargo.toml`** — Add to `[workspace]` members and `[workspace.dependencies]`:

```toml
# New workspace member
members = [
    # ... existing ...
    "crates/ingot-storage",
]

# New workspace dependencies
sqlx = { version = "0.8", default-features = false }
testcontainers = { version = "0.23", default-features = false }
testcontainers-modules = { version = "0.11", default-features = false }
```

**File: `src/lib.rs`**

```rust
pub mod config;
pub mod service;

mod postgres;
mod questdb;
```

**Acceptance:** `cargo check -p ingot-storage` compiles.

### Task 2.1.2: Implement `StorageConfig`

**File:** `crates/ingot-storage/src/config.rs`

```rust
/// Configuration for database connections.
pub struct StorageConfig {
    /// PostgreSQL connection string.
    pub pg_url: String,
    /// QuestDB ILP endpoint (host:port).
    pub questdb_ilp_addr: String,
    /// PostgreSQL connection pool max connections.
    pub pg_pool_size: u32,
    /// Max events to buffer before flushing to PostgreSQL.
    pub pg_batch_size: usize,
    /// Max time (ms) before flushing buffered events.
    pub flush_interval_ms: u64,
}
```

```rust
impl StorageConfig {
    /// Load configuration from environment variables.
    /// Falls back to defaults for optional values.
    pub fn from_env() -> Result<Self>;
}
```

**Environment variable mapping:**

| Variable | Field | Default |
|:---|:---|:---|
| `DATABASE_URL` | `pg_url` | *(required)* |
| `QUESTDB_ILP_ADDR` | `questdb_ilp_addr` | `localhost:9009` |
| `PG_POOL_SIZE` | `pg_pool_size` | `5` |
| `STORAGE_BATCH_SIZE` | `pg_batch_size` | `50` |
| `STORAGE_FLUSH_INTERVAL_MS` | `flush_interval_ms` | `100` |

**Tests (write first):**

```
test_storage_config_from_env_all_set
test_storage_config_from_env_defaults
test_storage_config_from_env_missing_database_url_errors
test_storage_config_pg_pool_size_parse_error
```

**Acceptance:** Config loads from env vars with correct defaults and errors on missing required values.

### Task 2.1.3: Write account migration

**File:** `crates/ingot-storage/migrations/001_create_accounts.sql`

```sql
CREATE TABLE IF NOT EXISTS accounts (
    id            UUID        PRIMARY KEY,
    name          TEXT        NOT NULL,
    account_type  TEXT        NOT NULL CHECK (account_type IN (
                      'Asset', 'Liability', 'Equity', 'Revenue', 'Expense'
                  )),
    currency_code TEXT        NOT NULL,
    decimals      SMALLINT    NOT NULL DEFAULT 2,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_accounts_currency ON accounts (currency_code);
CREATE INDEX IF NOT EXISTS idx_accounts_type ON accounts (account_type);
```

**Acceptance:** `cargo sqlx migrate run` creates the table. `cargo sqlx prepare --workspace` generates `.sqlx/` cache.

### Task 2.1.4: Implement `save_account()`

**File:** `crates/ingot-storage/src/postgres/accounts.rs`

```rust
use sqlx::PgPool;

use ingot_core::accounting::Account;

/// Persist an account to PostgreSQL. Idempotent (ON CONFLICT DO NOTHING).
pub async fn save_account(pool: &PgPool, account: &Account) -> Result<()>;
```

**SQL:**

```sql
INSERT INTO accounts (id, name, account_type, currency_code, decimals)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (id) DO NOTHING
```

**Type mapping:**
- `account.id` → `Uuid`
- `account.name` → `&str`
- `account.account_type` → `&str` (via Display/as_str impl)
- `account.currency.code` → `&str` (via `CurrencyCode::as_str()`)
- `account.currency.decimals` → `i16`

**Tests (write first):**

```
test_save_account_persists_all_fields
test_save_account_idempotent_on_duplicate_id
test_save_account_different_ids_both_persist
```

**Acceptance:** Account inserted into DB. Duplicate ID silently ignored. All fields correct.

### Task 2.1.5: Implement `load_accounts()`

**File:** `crates/ingot-storage/src/postgres/accounts.rs`

```rust
/// Load all accounts from PostgreSQL, ordered by created_at.
pub async fn load_accounts(pool: &PgPool) -> Result<Vec<Account>>;
```

**SQL:**

```sql
SELECT id, name, account_type, currency_code, decimals
FROM accounts
ORDER BY created_at ASC
```

**Row mapping struct:**

```rust
struct AccountRow {
    id: Uuid,
    name: String,
    account_type: String,
    currency_code: String,
    decimals: i16,
}
```

Implement `TryFrom<AccountRow> for Account` to reconstruct the domain type. Parse `account_type` string back to `AccountType` enum. Reconstruct `Currency` from `currency_code` and `decimals`.

**Tests (write first):**

```
test_load_accounts_empty_db_returns_empty_vec
test_load_accounts_returns_all_saved_accounts
test_load_accounts_preserves_field_values
test_load_accounts_ordered_by_created_at
```

**Proptest (write first):**

```
proptest_account_round_trip — Generate random Account (arbitrary name, account_type, currency),
save to DB, load back, assert all fields equal. 1000 cases.
```

**Acceptance:** Accounts round-trip through the database with all fields preserved. Empty DB returns empty `Vec`.

### Task 2.1.6: Stub `StorageService`

**File:** `crates/ingot-storage/src/service.rs`

```rust
use sqlx::PgPool;

/// Unified storage facade. Holds connection pools for PostgreSQL and QuestDB.
pub struct StorageService {
    pg: PgPool,
    // questdb field added in Step 2.7
}

impl StorageService {
    /// Connect to PostgreSQL. Run migrations. QuestDB connection deferred to Step 2.7.
    pub async fn connect(config: &StorageConfig) -> Result<Self>;

    /// Insert an account. Delegates to postgres::accounts::save_account.
    pub async fn save_account(&self, account: &Account) -> Result<()>;

    /// Load all accounts. Delegates to postgres::accounts::load_accounts.
    pub async fn load_accounts(&self) -> Result<Vec<Account>>;
}
```

`connect()` creates a `PgPool` with `PgPoolOptions::new().max_connections(config.pg_pool_size)`, connects, runs `sqlx::migrate!().run(&pool)`, and returns `Self`.

**Tests (write first):**

```
test_storage_service_connect_runs_migrations
test_storage_service_save_and_load_account_round_trip
```

**Acceptance:** `StorageService::connect()` creates pool, runs migrations, exposes account CRUD.

### Task 2.1.7: Test infrastructure helper

**File:** `crates/ingot-storage/tests/helpers/mod.rs` (or inline in test files)

```rust
/// Spin up a PostgreSQL container, run migrations, return pool + container.
/// Container is dropped (destroyed) when the test completes.
pub async fn test_pg_pool() -> Result<(PgPool, ContainerAsync<Postgres>)> {
    let container = Postgres::default().start().await
        .context("Failed to start PostgreSQL testcontainer")?;
    let port = container.get_host_port_ipv4(5432).await
        .context("Failed to get PostgreSQL port")?;
    let url = format!("postgres://postgres:postgres@localhost:{port}/postgres");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .context("Failed to connect to test PostgreSQL")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("Failed to run migrations")?;
    Ok((pool, container))
}
```

**Acceptance:** Helper compiles and is reusable across all integration tests in the crate.

### Task 2.1.8: Generate sqlx offline cache

After all migrations and queries are implemented:

```bash
docker compose up -d
export DATABASE_URL=postgres://ingot_app:ingot_dev@localhost:5432/ingot
cargo sqlx migrate run
cargo sqlx prepare --workspace
git add .sqlx/
```

**Acceptance:** `cargo build -p ingot-storage` succeeds without a running database (using `.sqlx/` cache).

### Step 2.1 PR Checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --workspace` — zero warnings
- [ ] `cargo test --workspace` — all tests pass (existing 163 + new)
- [ ] `cargo check --all-targets --workspace`
- [ ] `cargo bench --no-run`
- [ ] `.sqlx/` directory committed
- [ ] All new public items have doc comments
- [ ] No `.unwrap()`, `.expect()`, `panic!()`, `todo!()`

---

## Step 2.2: Transaction + Entry Persistence

**Branch:** `feature/transaction-persist`
**Depends on:** Step 2.1

### Task 2.2.1: Write transaction migration

**File:** `crates/ingot-storage/migrations/002_create_transactions.sql`

```sql
CREATE TABLE IF NOT EXISTS transactions (
    id          UUID        PRIMARY KEY,
    posted_at   TIMESTAMPTZ NOT NULL,
    description TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_transactions_posted_at ON transactions (posted_at);
```

### Task 2.2.2: Write entries migration

**File:** `crates/ingot-storage/migrations/003_create_entries.sql`

```sql
CREATE TABLE IF NOT EXISTS entries (
    id             BIGSERIAL   PRIMARY KEY,
    transaction_id UUID        NOT NULL REFERENCES transactions(id),
    account_id     UUID        NOT NULL REFERENCES accounts(id),
    side           TEXT        NOT NULL CHECK (side IN ('Debit', 'Credit')),
    amount         NUMERIC     NOT NULL,
    currency_code  TEXT        NOT NULL,
    decimals       SMALLINT    NOT NULL DEFAULT 2,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_entries_transaction ON entries (transaction_id);
CREATE INDEX IF NOT EXISTS idx_entries_account ON entries (account_id);
```

### Task 2.2.3: Implement `save_transaction()`

**File:** `crates/ingot-storage/src/postgres/transactions.rs`

```rust
/// Persist a transaction and all its entries in a single database transaction.
/// The DB transaction ensures atomicity — either all entries are saved or none.
pub async fn save_transaction(pool: &PgPool, tx: &Transaction) -> Result<()>;
```

**Implementation outline:**
1. Begin a PostgreSQL transaction (`pool.begin()`)
2. INSERT into `transactions` (id, posted_at, description)
3. For each entry in `tx.entries`: INSERT into `entries`
4. Commit

**SQL (transaction header):**

```sql
INSERT INTO transactions (id, posted_at, description)
VALUES ($1, $2, $3)
ON CONFLICT (id) DO NOTHING
```

**SQL (each entry):**

```sql
INSERT INTO entries (transaction_id, account_id, side, amount, currency_code, decimals)
VALUES ($1, $2, $3, $4, $5, $6)
```

**Type mapping for entries:**
- `entry.amount` → `Decimal` (maps to PostgreSQL `NUMERIC` natively via sqlx)
- `entry.side` → `&str` ("Debit" or "Credit")
- `entry.currency.code` → `&str`
- `entry.currency.decimals` → `i16`

**Tests (write first):**

```
test_save_transaction_persists_header_fields
test_save_transaction_persists_all_entries
test_save_transaction_idempotent_on_duplicate_id
test_save_transaction_atomic_on_entry_failure
```

The atomicity test should verify that if an entry INSERT fails (e.g., bad account_id FK), the transaction header is also rolled back.

**Acceptance:** Transaction + entries persisted atomically. Duplicate ID silently ignored.

### Task 2.2.4: Implement `load_transactions()`

**File:** `crates/ingot-storage/src/postgres/transactions.rs`

```rust
/// Load all transactions with their entries, ordered by posted_at ASC.
/// Returns fully reconstructed Transaction objects.
pub async fn load_transactions(pool: &PgPool) -> Result<Vec<Transaction>>;
```

**Implementation outline:**
1. SELECT all transactions ordered by `posted_at ASC`
2. SELECT all entries ordered by `transaction_id`
3. Group entries by `transaction_id` using a single pass
4. Construct `Transaction` objects with `SmallVec<Entry, 4>`

**SQL (transactions):**

```sql
SELECT id, posted_at, description FROM transactions ORDER BY posted_at ASC
```

**SQL (entries):**

```sql
SELECT transaction_id, account_id, side, amount, currency_code, decimals
FROM entries
ORDER BY transaction_id, id ASC
```

**Row mapping structs:**

```rust
struct TransactionRow {
    id: Uuid,
    posted_at: DateTime<Utc>,
    description: String,
}

struct EntryRow {
    transaction_id: Uuid,
    account_id: Uuid,
    side: String,
    amount: Decimal,
    currency_code: String,
    decimals: i16,
}
```

Implement `TryFrom` conversions for both. Group entries into their parent transactions using a single-pass merge (both are ordered by transaction_id).

**Tests (write first):**

```
test_load_transactions_empty_db_returns_empty
test_load_transactions_returns_all_saved
test_load_transactions_entries_grouped_correctly
test_load_transactions_ordered_by_posted_at
test_load_transactions_entry_fields_preserved
```

**Proptest (write first):**

```
proptest_decimal_round_trip — Generate random Decimal values (positive, negative, high precision),
save as entry amount, load back, assert exact equality. 1000 cases. This validates that
Decimal → PostgreSQL NUMERIC → Decimal is lossless.
```

**Acceptance:** Transactions with all entries round-trip through DB. Entry ordering preserved. Decimal precision lossless.

### Task 2.2.5: Add to `StorageService`

**File:** `crates/ingot-storage/src/service.rs`

Add methods:

```rust
impl StorageService {
    /// Persist a transaction and its entries atomically.
    pub async fn save_transaction(&self, tx: &Transaction) -> Result<()>;

    /// Load all transactions with entries, ordered by posted_at.
    pub async fn load_transactions(&self) -> Result<Vec<Transaction>>;
}
```

Both delegate to `postgres::transactions::*`.

### Task 2.2.6: Regenerate sqlx offline cache

```bash
cargo sqlx prepare --workspace
git add .sqlx/
```

### Step 2.2 PR Checklist

- [ ] All standard checks (fmt, clippy, test, check, bench --no-run)
- [ ] `.sqlx/` updated
- [ ] Proptest for Decimal fidelity passes (1000 cases)
- [ ] Atomicity test verifies rollback on partial failure

---

## Step 2.3: Ledger Recovery

**Branch:** `feature/ledger-recovery`
**Depends on:** Step 2.2

### Task 2.3.1: Implement `recover_ledger()`

**File:** `crates/ingot-storage/src/postgres/recovery.rs`

```rust
use ingot_core::accounting::Ledger;

/// Rebuild a complete Ledger from persisted state.
///
/// 1. Load all accounts from the database.
/// 2. If no accounts found, return a fresh bootstrapped ledger.
/// 3. Add each account to the ledger.
/// 4. Load all transactions (ordered by posted_at).
/// 5. Replay each transaction through prepare_transaction + post_transaction.
/// 6. Return the fully reconstructed ledger.
pub async fn recover_ledger(pool: &PgPool) -> Result<Ledger>;
```

**Implementation:**

```rust
pub async fn recover_ledger(pool: &PgPool) -> Result<Ledger> {
    let accounts = accounts::load_accounts(pool).await
        .context("Failed to load accounts during recovery")?;

    if accounts.is_empty() {
        tracing::info!("No persisted state, bootstrapping fresh ledger");
        // Calls ingot_engine::bootstrap_ledger() or equivalent
        // (may need to extract bootstrap logic to ingot-core)
        anyhow::bail!("Fresh ledger bootstrap delegated to caller");
    }

    let mut ledger = Ledger::new();

    for account in &accounts {
        ledger.add_account(account.clone())
            .with_context(|| format!("Failed to add recovered account {}", account.id))?;
    }

    let transactions = transactions::load_transactions(pool).await
        .context("Failed to load transactions during recovery")?;

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

    Ok(ledger)
}
```

**Tests (write first):**

```
test_recover_ledger_empty_db_signals_fresh
test_recover_ledger_single_account_no_transactions
test_recover_ledger_full_round_trip
test_recover_ledger_nav_matches_original
test_recover_ledger_multiple_transactions_replayed_in_order
```

The critical test: `test_recover_ledger_nav_matches_original`:
1. Create a ledger with `bootstrap_ledger()`
2. Post several transactions (including `post_order_fill`)
3. Calculate NAV
4. Save all accounts and transactions to DB
5. Call `recover_ledger()`
6. Calculate NAV on recovered ledger
7. Assert NAVs are exactly equal

**Acceptance:** Recovered ledger has identical accounts, balances, and NAV as the original.

### Task 2.3.2: Add to `StorageService`

**File:** `crates/ingot-storage/src/service.rs`

```rust
impl StorageService {
    /// Rebuild ledger from database. Returns Err if DB is empty (caller should bootstrap).
    pub async fn recover_ledger(&self) -> Result<Ledger>;
}
```

### Task 2.3.3: Modify `ingot-server` startup

**File:** `crates/ingot-server/src/main.rs`

**Current flow:**
```rust
let ledger = bootstrap_ledger()?;
```

**New flow:**
```rust
let storage = StorageService::connect(&storage_config).await?;

let ledger = match storage.recover_ledger().await {
    Ok(ledger) => {
        info!("Ledger recovered from database");
        ledger
    }
    Err(_) => {
        info!("No persisted state, bootstrapping fresh ledger");
        let ledger = bootstrap_ledger()?;
        // Persist the bootstrapped accounts and initial transaction
        // (will be sent via storage channel once Task 2.4 is complete)
        ledger
    }
};
```

**Add `ingot-storage` to `ingot-server` dependencies:**

```toml
# crates/ingot-server/Cargo.toml
ingot-storage = { path = "../ingot-storage" }
```

**Tests:**

No new tests in ingot-server (it's a binary crate). The recovery logic is tested in ingot-storage. Verify manually:
1. Start server (fresh DB) → bootstraps ledger, logs "bootstrapping"
2. Place an order, record fill
3. Restart server → recovers ledger, logs "recovered", NAV preserved

**Acceptance:** Server recovers ledger from PostgreSQL on restart. Fresh start bootstraps as before.

### Task 2.3.4: Regenerate sqlx offline cache

```bash
cargo sqlx prepare --workspace
git add .sqlx/
```

### Step 2.3 PR Checklist

- [ ] All standard checks
- [ ] `.sqlx/` updated
- [ ] NAV fidelity test passes (original NAV == recovered NAV)
- [ ] Server startup tested manually (fresh + restart)

---

## Step 2.4: Storage Event Channel + Storage Task

**Branch:** `feature/storage-task`
**Depends on:** Step 2.3

### Task 2.4.1: Define `StorageEvent`

**File:** `crates/ingot-storage/src/event.rs`

```rust
use ingot_core::{
    accounting::Transaction,
    accounting::Account,
    execution::{OrderAcknowledgment, OrderRequest},
    feed::Ticker,
};

/// Events sent from the engine to the storage task via mpsc channel.
pub enum StorageEvent {
    /// A new account was created in the ledger.
    AccountCreated { account: Account },

    /// A validated transaction was posted to the ledger.
    TransactionPosted { transaction: Transaction },

    /// An order was submitted to an exchange and acknowledged.
    OrderPlaced {
        request: OrderRequest,
        acknowledgment: OrderAcknowledgment,
    },

    /// A market data tick was received.
    Tick { ticker: Ticker },

    /// Flush all buffered events to storage immediately (graceful shutdown).
    Flush { done: tokio::sync::oneshot::Sender<()> },
}
```

**Named channel capacity constant:**

```rust
/// Capacity for the storage event channel.
/// Large enough to absorb burst tick rates without blocking the engine.
pub const STORAGE_CHANNEL_CAPACITY: usize = 256;
```

**Acceptance:** `StorageEvent` compiles, is `Send`, and can be sent over `tokio::sync::mpsc`.

### Task 2.4.2: Implement `run_storage_task()`

**File:** `crates/ingot-storage/src/task.rs`

```rust
/// Run the storage background task. Receives events from the engine and
/// persists them to PostgreSQL (batched) and QuestDB (fire-and-forget).
///
/// Exits when the channel is closed (all senders dropped).
pub async fn run_storage_task(
    service: StorageService,
    mut rx: mpsc::Receiver<StorageEvent>,
    config: &StorageConfig,
) -> Result<()>;
```

**Implementation outline:**

```rust
pub async fn run_storage_task(
    service: StorageService,
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
                    Some(StorageEvent::Tick { .. }) => {
                        // QuestDB ingestion — deferred to Step 2.7
                        // For now, drop ticks silently
                    }
                    Some(StorageEvent::Flush { done }) => {
                        flush_pg_buffer(&service, &mut pg_buffer).await?;
                        let _ = done.send(());
                    }
                    Some(event) => {
                        pg_buffer.push(event);
                        if pg_buffer.len() >= config.pg_batch_size {
                            flush_pg_buffer(&service, &mut pg_buffer).await?;
                        }
                    }
                    None => {
                        // Channel closed — final flush
                        flush_pg_buffer(&service, &mut pg_buffer).await?;
                        break;
                    }
                }
            }
            _ = flush_timer.tick() => {
                if !pg_buffer.is_empty() {
                    flush_pg_buffer(&service, &mut pg_buffer).await?;
                }
            }
        }
    }

    tracing::info!("Storage task exiting");
    Ok(())
}
```

**Implement `flush_pg_buffer()`:**

```rust
async fn flush_pg_buffer(
    service: &StorageService,
    buffer: &mut Vec<StorageEvent>,
) -> Result<()> {
    for event in buffer.drain(..) {
        match event {
            StorageEvent::AccountCreated { account } => {
                service.save_account(&account).await
                    .context("Failed to persist account")?;
            }
            StorageEvent::TransactionPosted { transaction } => {
                service.save_transaction(&transaction).await
                    .context("Failed to persist transaction")?;
            }
            StorageEvent::OrderPlaced { .. } => {
                // Order history — deferred to Step 2.6
            }
            _ => {}
        }
    }
    Ok(())
}
```

**Tests (write first):**

```
test_storage_task_processes_account_created
test_storage_task_processes_transaction_posted
test_storage_task_flushes_on_batch_size
test_storage_task_flushes_on_timer
test_storage_task_flushes_on_channel_close
test_storage_task_flush_command_drains_buffer
```

Test pattern: create channel, send events, verify they appear in the database.

**Acceptance:** Storage task batches and persists events. Graceful shutdown flushes remaining buffer.

### Task 2.4.3: Add `storage_tx` to `TradingEngine<E>`

**File:** `crates/ingot-engine/src/lib.rs`

**Add field:**

```rust
pub struct TradingEngine<E: Exchange> {
    // ... existing fields ...
    storage_tx: Option<mpsc::Sender<StorageEvent>>,
}
```

**Add builder method (mirrors `.with_broadcast()`):**

```rust
impl<E: Exchange> TradingEngine<E> {
    /// Attach a storage event channel for persistence.
    pub fn with_storage(mut self, storage_tx: mpsc::Sender<StorageEvent>) -> Self {
        self.storage_tx = Some(storage_tx);
        self
    }
}
```

**Add `ingot-storage` dependency to `ingot-engine`:**

```toml
# crates/ingot-engine/Cargo.toml
ingot-storage = { path = "../ingot-storage" }
```

**Acceptance:** Engine compiles with optional storage channel. Existing tests unaffected.

### Task 2.4.4: Emit storage events from engine

**File:** `crates/ingot-engine/src/lib.rs`

**Emit `AccountCreated` from `ensure_accounts_for_symbol()`:**

After each `ledger.add_account(account)` call, if `storage_tx` is `Some`:

```rust
if let Some(ref storage_tx) = self.storage_tx {
    // Best-effort for account creation (unlikely to be backlogged)
    let _ = storage_tx.try_send(StorageEvent::AccountCreated {
        account: account.clone(),
    });
}
```

**Emit `TransactionPosted` from `record_fill()`:**

After `ledger.post_transaction(vtx)`, the engine needs access to the original `Transaction`. Since `post_transaction` consumes `ValidatedTransaction`, we need to clone the transaction before validation, or extract it. Check the current implementation to determine the best approach.

```rust
if let Some(ref storage_tx) = self.storage_tx {
    storage_tx.send(StorageEvent::TransactionPosted {
        transaction: transaction.clone(),
    }).await
    .context("Storage channel closed")?;
}
```

**Emit `OrderPlaced` from `handle_order_request()`:**

After `exchange.place_order()` returns:

```rust
if let Some(ref storage_tx) = self.storage_tx {
    storage_tx.send(StorageEvent::OrderPlaced {
        request: order.clone(),
        acknowledgment: ack.clone(),
    }).await
    .context("Storage channel closed")?;
}
```

**Emit `Tick` from `handle_tick()`:**

```rust
if let Some(ref storage_tx) = self.storage_tx {
    // Best-effort: drop ticks if storage is behind
    let _ = storage_tx.try_send(StorageEvent::Tick {
        ticker: tick.clone(),
    });
}
```

**Sending strategy:**
- `Tick`: `try_send()` — best-effort, drop if channel full (ticks are high-volume, non-critical)
- `AccountCreated`: `try_send()` — low volume, unlikely to backlog
- `TransactionPosted`, `OrderPlaced`: `.send().await` — backpressure, financial data must not be lost

**Tests (write first):**

```
test_engine_emits_account_created_on_new_symbol
test_engine_emits_transaction_posted_on_fill
test_engine_emits_order_placed_on_order
test_engine_emits_tick_on_market_data
test_engine_runs_without_storage_channel
```

**Acceptance:** Engine sends storage events when channel is attached. Runs normally without one.

### Task 2.4.5: Wire storage task into `ingot-server`

**File:** `crates/ingot-server/src/main.rs`

**Current server startup (simplified):**

```rust
let engine = TradingEngine::new(ledger, market_data, exchange, strategies)
    .with_broadcast();
let handle = EngineHandle::new(...);
// spawn IPC threads
// spawn engine task
```

**New startup:**

```rust
let (storage_tx, storage_rx) = mpsc::channel(STORAGE_CHANNEL_CAPACITY);

let engine = TradingEngine::new(ledger, market_data, exchange, strategies)
    .with_broadcast()
    .with_storage(storage_tx.clone());

let handle = EngineHandle::new(...);

// Spawn storage task
let storage_task = tokio::spawn(run_storage_task(
    storage_service.clone(), // StorageService with PgPool (Clone via Arc)
    storage_rx,
    &storage_config,
));

// ... spawn IPC threads, engine task ...
```

**Shutdown sequence update:**

```rust
// 1. Send engine shutdown
handle.shutdown().await?;

// 2. Flush storage
let (flush_tx, flush_rx) = oneshot::channel();
let _ = storage_tx.send(StorageEvent::Flush { done: flush_tx }).await;
let _ = flush_rx.await; // Wait for flush to complete

// 3. Drop storage_tx to close channel
drop(storage_tx);

// 4. Await storage task
storage_task.await??;

// 5. Join IPC threads
```

**Acceptance:** Server starts with storage task. Shutdown flushes all buffered events before exiting.

### Task 2.4.6: Make `StorageService` cloneable

**File:** `crates/ingot-storage/src/service.rs`

`PgPool` is already `Clone` (it's an `Arc` internally). Derive or implement `Clone` for `StorageService`:

```rust
#[derive(Clone)]
pub struct StorageService {
    pg: PgPool,
}
```

This is needed because the server needs to share the service between the storage task (writes) and the IPC command thread (reads like `recover_ledger`, `query_audit`, etc.).

**Acceptance:** `StorageService` implements `Clone`.

### Task 2.4.7: Regenerate sqlx offline cache

```bash
cargo sqlx prepare --workspace
git add .sqlx/
```

### Step 2.4 PR Checklist

- [ ] All standard checks
- [ ] `.sqlx/` updated
- [ ] Engine tests pass with and without storage channel
- [ ] Storage task tests verify batching and flush behavior
- [ ] Server compiles with storage task wired in

---

## Step 2.5: Audit Trail

**Branch:** `feature/audit-trail`
**Depends on:** Step 2.4

### Task 2.5.1: Write audit log migration

**File:** `crates/ingot-storage/migrations/004_create_audit_log.sql`

```sql
CREATE TABLE IF NOT EXISTS audit_log (
    id         BIGSERIAL   PRIMARY KEY,
    timestamp  TIMESTAMPTZ NOT NULL DEFAULT now(),
    event_type TEXT        NOT NULL,
    actor      TEXT        NOT NULL,
    entity_id  UUID,
    payload    JSONB       NOT NULL,
    checksum   BYTEA       NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp ON audit_log (timestamp);
CREATE INDEX IF NOT EXISTS idx_audit_log_event_type ON audit_log (event_type);
CREATE INDEX IF NOT EXISTS idx_audit_log_entity_id ON audit_log (entity_id);
```

### Task 2.5.2: Implement `append_audit()`

**File:** `crates/ingot-storage/src/postgres/audit.rs`

```rust
use sha2::{Sha256, Digest};

/// Append an audit log entry with chain hash.
///
/// The checksum is SHA-256(previous_checksum || current_payload_bytes).
/// For the first entry, previous_checksum is 32 zero bytes.
pub async fn append_audit(
    pool: &PgPool,
    event_type: &str,
    actor: &str,
    entity_id: Option<Uuid>,
    payload: &serde_json::Value,
) -> Result<()>;
```

**Implementation outline:**

1. Fetch the last checksum from the audit log:

```sql
SELECT checksum FROM audit_log ORDER BY id DESC LIMIT 1
```

2. If no previous entry, use `[0u8; 32]` as the initial checksum.

3. Compute new checksum:

```rust
let payload_bytes = serde_json::to_vec(payload)?;
let mut hasher = Sha256::new();
hasher.update(&prev_checksum);
hasher.update(&payload_bytes);
let checksum = hasher.finalize();
```

4. Insert:

```sql
INSERT INTO audit_log (event_type, actor, entity_id, payload, checksum)
VALUES ($1, $2, $3, $4, $5)
```

**Tests (write first):**

```
test_append_audit_first_entry_uses_zero_seed
test_append_audit_chain_hash_depends_on_previous
test_append_audit_all_fields_persisted
test_append_audit_entity_id_nullable
```

**Acceptance:** Audit entries are appended with correct chain hashes. Each hash depends on the previous.

### Task 2.5.3: Implement `query_audit()`

**File:** `crates/ingot-storage/src/postgres/audit.rs`

```rust
/// Row returned from audit log queries.
pub struct AuditRow {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub actor: String,
    pub entity_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

/// Query audit log with optional filters.
pub async fn query_audit(
    pool: &PgPool,
    event_type: Option<&str>,
    since: Option<DateTime<Utc>>,
    limit: Option<i64>,
) -> Result<Vec<AuditRow>>;
```

**SQL (dynamic query built with conditional WHERE clauses):**

```sql
SELECT id, timestamp, event_type, actor, entity_id, payload
FROM audit_log
WHERE ($1::TEXT IS NULL OR event_type = $1)
  AND ($2::TIMESTAMPTZ IS NULL OR timestamp >= $2)
ORDER BY timestamp DESC
LIMIT $3
```

Default limit: 50.

**Tests (write first):**

```
test_query_audit_no_filters_returns_all
test_query_audit_filter_by_event_type
test_query_audit_filter_by_since
test_query_audit_respects_limit
test_query_audit_combined_filters
test_query_audit_empty_db_returns_empty
```

**Acceptance:** Audit queries return correct entries with all filter combinations.

### Task 2.5.4: Implement `verify_audit_chain()`

**File:** `crates/ingot-storage/src/postgres/audit.rs`

```rust
/// Result of audit chain verification.
pub struct AuditChainBreak {
    /// The ID of the entry where the chain broke.
    pub broken_at_id: i64,
    /// Expected checksum (computed from previous entry).
    pub expected: Vec<u8>,
    /// Actual checksum stored in the database.
    pub actual: Vec<u8>,
}

/// Verify the audit log chain hash integrity.
/// Returns None if the chain is valid, or the first break point.
pub async fn verify_audit_chain(pool: &PgPool) -> Result<Option<AuditChainBreak>>;
```

**Implementation:**

1. SELECT all entries ordered by `id ASC` (streaming with `fetch()` to avoid loading all into memory)
2. Start with `prev_checksum = [0u8; 32]`
3. For each entry, compute `SHA-256(prev_checksum || payload_bytes)` and compare to stored checksum
4. If mismatch, return `AuditChainBreak`
5. If all match, return `None`

**Tests (write first):**

```
test_verify_chain_valid_returns_none
test_verify_chain_detects_tampered_payload
test_verify_chain_detects_tampered_checksum
test_verify_chain_empty_log_returns_none
test_verify_chain_single_entry_valid
```

The tamper test manually UPDATEs a payload in the DB (using a privileged connection) and verifies the chain detects it.

**Acceptance:** Chain verification correctly detects tampering and returns the break point.

### Task 2.5.5: Integrate audit into storage task flush

**File:** `crates/ingot-storage/src/task.rs`

Update `flush_pg_buffer()` to append audit entries alongside data writes:

```rust
StorageEvent::AccountCreated { account } => {
    service.save_account(&account).await?;
    service.append_audit(
        "account_created",
        "engine",
        Some(account.id),
        &serde_json::to_value(&account)?,
    ).await?;
}
StorageEvent::TransactionPosted { transaction } => {
    service.save_transaction(&transaction).await?;
    service.append_audit(
        "transaction_posted",
        "engine",
        Some(transaction.id),
        &serde_json::to_value(&transaction)?,
    ).await?;
}
StorageEvent::OrderPlaced { request, acknowledgment } => {
    // Order persistence added in Step 2.6
    let event_type = match acknowledgment.status {
        OrderStatus::Filled => "order_filled",
        OrderStatus::Rejected => "order_rejected",
        _ => "order_placed",
    };
    service.append_audit(
        event_type,
        "engine",
        None,
        &serde_json::to_value(&(&request, &acknowledgment))?,
    ).await?;
}
```

**Acceptance:** Every persisted data event also creates an audit trail entry.

### Task 2.5.6: Add to `StorageService`

```rust
impl StorageService {
    pub async fn append_audit(&self, event_type: &str, actor: &str, entity_id: Option<Uuid>, payload: &serde_json::Value) -> Result<()>;
    pub async fn query_audit(&self, event_type: Option<&str>, since: Option<DateTime<Utc>>, limit: Option<i64>) -> Result<Vec<AuditRow>>;
    pub async fn verify_audit_chain(&self) -> Result<Option<AuditChainBreak>>;
}
```

### Task 2.5.7: Regenerate sqlx offline cache

### Step 2.5 PR Checklist

- [ ] All standard checks
- [ ] `.sqlx/` updated
- [ ] Chain hash tests pass (valid chain, tamper detection)
- [ ] Audit entries created for every storage event type
- [ ] Query filtering tests pass

---

## Step 2.6: Order History

**Branch:** `feature/order-history`
**Depends on:** Step 2.5

### Task 2.6.1: Write order history migration

**File:** `crates/ingot-storage/migrations/005_create_order_history.sql`

```sql
CREATE TABLE IF NOT EXISTS order_history (
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

CREATE INDEX IF NOT EXISTS idx_order_history_symbol ON order_history (symbol);
CREATE INDEX IF NOT EXISTS idx_order_history_submitted ON order_history (submitted_at);
CREATE INDEX IF NOT EXISTS idx_order_history_status ON order_history (status);
```

### Task 2.6.2: Implement `save_order()`

**File:** `crates/ingot-storage/src/postgres/orders.rs`

```rust
/// Persist an order and its acknowledgment to the order history table.
pub async fn save_order(
    pool: &PgPool,
    request: &OrderRequest,
    ack: &OrderAcknowledgment,
) -> Result<()>;
```

**SQL:**

```sql
INSERT INTO order_history (exchange_id, client_id, symbol, side, order_type, quantity, price, status, submitted_at, filled_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
```

**Type mapping:**
- `symbol` → `Symbol::as_str()`
- `side` → `"Buy"` or `"Sell"`
- `quantity` → `Decimal` from `Quantity`
- `price` → `Option<Decimal>` from `Option<Price>`
- `status` → `OrderStatus` as string
- `submitted_at` → `ack.timestamp`
- `filled_at` → `ack.timestamp` if status is `Filled`, else `None`

**Tests (write first):**

```
test_save_order_market_buy
test_save_order_limit_sell_with_price
test_save_order_rejected_status
test_save_order_null_price_for_market
```

### Task 2.6.3: Implement `query_orders()`

**File:** `crates/ingot-storage/src/postgres/orders.rs`

```rust
/// Row returned from order history queries.
pub struct OrderHistoryRow {
    pub id: i64,
    pub exchange_id: Option<String>,
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    pub quantity: Decimal,
    pub price: Option<Decimal>,
    pub status: String,
    pub submitted_at: DateTime<Utc>,
    pub filled_at: Option<DateTime<Utc>>,
}

/// Query order history with optional filters.
pub async fn query_orders(
    pool: &PgPool,
    symbol: Option<&str>,
    since: Option<DateTime<Utc>>,
    limit: Option<i64>,
) -> Result<Vec<OrderHistoryRow>>;
```

**SQL:**

```sql
SELECT id, exchange_id, symbol, side, order_type, quantity, price, status, submitted_at, filled_at
FROM order_history
WHERE ($1::TEXT IS NULL OR symbol = $1)
  AND ($2::TIMESTAMPTZ IS NULL OR submitted_at >= $2)
ORDER BY submitted_at DESC
LIMIT $3
```

**Tests (write first):**

```
test_query_orders_no_filters
test_query_orders_filter_by_symbol
test_query_orders_filter_by_since
test_query_orders_respects_limit
test_query_orders_empty_db
```

### Task 2.6.4: Integrate into storage task flush

**File:** `crates/ingot-storage/src/task.rs`

Update the `OrderPlaced` arm in `flush_pg_buffer()`:

```rust
StorageEvent::OrderPlaced { request, acknowledgment } => {
    service.save_order(&request, &acknowledgment).await
        .context("Failed to persist order")?;
    // audit entry already handled in Step 2.5
    // ...
}
```

### Task 2.6.5: Add to `StorageService`

```rust
impl StorageService {
    pub async fn save_order(&self, request: &OrderRequest, ack: &OrderAcknowledgment) -> Result<()>;
    pub async fn query_orders(&self, symbol: Option<&str>, since: Option<DateTime<Utc>>, limit: Option<i64>) -> Result<Vec<OrderHistoryRow>>;
}
```

### Task 2.6.6: Regenerate sqlx offline cache

### Step 2.6 PR Checklist

- [ ] All standard checks
- [ ] `.sqlx/` updated
- [ ] Order round-trip tests pass
- [ ] Query filtering tests pass

---

## Step 2.7: QuestDB Tick Ingestion

**Branch:** `feature/questdb-ticks`
**Depends on:** Step 2.6

### Task 2.7.1: Add `questdb-rs` to workspace

**File:** root `Cargo.toml`

```toml
# [workspace.dependencies]
questdb-rs = { version = "4", default-features = false }
```

**File:** `crates/ingot-storage/Cargo.toml`

```toml
questdb-rs = { workspace = true, features = ["ilp-over-tcp"] }
```

### Task 2.7.2: Implement tick ingestion

**File:** `crates/ingot-storage/src/questdb/ticks.rs`

```rust
use questdb::ingress::{Buffer, Sender, TimestampNanos};

use ingot_core::feed::Ticker;

/// Ingest a single tick into the QuestDB ILP buffer.
/// The buffer must be flushed periodically (caller's responsibility).
pub fn buffer_tick(buffer: &mut Buffer, ticker: &Ticker) -> Result<()>;
```

**ILP format:**

```
ticks,symbol=BTC-USD price=67432.5 1709164800000000000
```

**Implementation:**

```rust
pub fn buffer_tick(buffer: &mut Buffer, ticker: &Ticker) -> Result<()> {
    let price_f64 = ticker.price.to_f64()
        .context("Failed to convert Decimal price to f64 for QuestDB")?;
    let nanos = ticker.timestamp.timestamp_nanos_opt()
        .context("Timestamp out of range for nanoseconds")?;

    buffer
        .table("ticks")?
        .symbol("symbol", ticker.symbol.as_str())?
        .column_f64("price", price_f64)?
        .at(TimestampNanos::new(nanos))?;

    Ok(())
}
```

**Tests (write first):**

```
test_buffer_tick_produces_valid_ilp
test_buffer_tick_symbol_encoding
test_buffer_tick_price_conversion
```

Note: Full integration test with QuestDB testcontainer deferred — QuestDB testcontainer support is less mature than PostgreSQL. Unit test the buffer construction. Manual integration testing with `docker compose up -d`.

**Acceptance:** Ticks are correctly formatted as ILP and buffered.

### Task 2.7.3: Add QuestDB connection to `StorageService`

**File:** `crates/ingot-storage/src/service.rs`

```rust
pub struct StorageService {
    pg: PgPool,
    questdb_buffer: Buffer,
    questdb_sender: Sender,
}
```

Update `connect()`:

```rust
pub async fn connect(config: &StorageConfig) -> Result<Self> {
    // PostgreSQL
    let pg = PgPoolOptions::new()
        .max_connections(config.pg_pool_size)
        .connect(&config.pg_url).await?;
    sqlx::migrate!().run(&pg).await?;

    // QuestDB
    let questdb_sender = Sender::from_conf(
        format!("tcp::addr={};", config.questdb_ilp_addr)
    ).context("Failed to connect to QuestDB")?;
    let questdb_buffer = Buffer::new();

    Ok(Self { pg, questdb_buffer, questdb_sender })
}
```

Add methods:

```rust
impl StorageService {
    /// Buffer a tick for QuestDB ingestion.
    pub fn ingest_tick(&mut self, ticker: &Ticker) -> Result<()>;

    /// Flush the QuestDB ILP buffer to the server.
    pub fn flush_questdb(&mut self) -> Result<()>;
}
```

Note: `StorageService` is no longer `Clone` once it holds a `Sender` (which is not `Clone`). The server must share the `PgPool` separately for read operations on the IPC command thread. Refactor:

```rust
pub struct StorageService {
    pg: PgPool,
    questdb_buffer: Buffer,
    questdb_sender: Sender,
}

/// Read-only handle for query operations. Clone-friendly.
#[derive(Clone)]
pub struct StorageReader {
    pg: PgPool,
}

impl StorageService {
    /// Create a read-only handle for query operations.
    pub fn reader(&self) -> StorageReader;
}
```

**Acceptance:** StorageService connects to QuestDB. Ticks are buffered and flushed.

### Task 2.7.4: Integrate tick ingestion into storage task

**File:** `crates/ingot-storage/src/task.rs`

Update the `Tick` arm:

```rust
Some(StorageEvent::Tick { ticker }) => {
    if let Err(e) = service.ingest_tick(&ticker) {
        tracing::warn!(%e, "Failed to buffer tick for QuestDB");
    }
}
```

Update the timer arm to also flush QuestDB:

```rust
_ = flush_timer.tick() => {
    if !pg_buffer.is_empty() {
        flush_pg_buffer(&service, &mut pg_buffer).await?;
    }
    if let Err(e) = service.flush_questdb() {
        tracing::warn!(%e, "Failed to flush QuestDB buffer");
    }
}
```

And in the `Flush` and channel-closed branches.

**Acceptance:** Ticks flow from engine → storage channel → storage task → QuestDB.

### Task 2.7.5: Create QuestDB tables on startup

**File:** `crates/ingot-storage/src/questdb/mod.rs`

```rust
/// Create QuestDB tables if they don't exist.
/// Uses PostgreSQL wire protocol (port 8812) for DDL.
pub async fn ensure_questdb_tables(pg_wire_url: &str) -> Result<()>;
```

**SQL (via PostgreSQL wire protocol):**

```sql
CREATE TABLE IF NOT EXISTS ticks (
    symbol    SYMBOL,
    price     DOUBLE,
    timestamp TIMESTAMP
) TIMESTAMP(timestamp) PARTITION BY DAY WAL DEDUP UPSERT KEYS(symbol, timestamp);
```

Note: This is executed once during `StorageService::connect()`. If QuestDB is unavailable, log a warning and continue (tick storage is non-critical).

**Acceptance:** QuestDB tables created on first startup. Subsequent startups are idempotent.

### Step 2.7 PR Checklist

- [ ] All standard checks
- [ ] Tick ILP formatting unit tests pass
- [ ] Manual integration test: ticks appear in QuestDB web console
- [ ] StorageReader for read operations, StorageService for writes

---

## Step 2.8: OHLCV Historical Ingestion

**Branch:** `feature/ohlcv-ingestion`
**Depends on:** Step 2.7

### Task 2.8.1: Add `OhlcvBar` and `BarInterval` to `ingot-core`

**File:** `crates/ingot-core/src/feed/ohlcv.rs` (new file)

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use ingot_primitives::Symbol;

/// Supported candlestick bar intervals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BarInterval {
    M1,
    M5,
    M15,
    M30,
    H1,
    H4,
    D1,
    W1,
}

impl BarInterval {
    /// Convert to Kraken API interval parameter (minutes).
    pub fn to_kraken_minutes(self) -> u32;

    /// Display string for QuestDB SYMBOL column.
    pub fn as_str(self) -> &'static str;
}

/// A single OHLCV candlestick bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
```

**Update** `crates/ingot-core/src/feed/mod.rs` to re-export:

```rust
pub mod ohlcv;
pub mod ticker;

pub use ohlcv::{BarInterval, OhlcvBar};
pub use ticker::Ticker;
```

**Tests (write first):**

```
test_bar_interval_to_kraken_minutes
test_bar_interval_as_str
test_bar_interval_all_variants_round_trip_serde
```

**Acceptance:** `OhlcvBar` and `BarInterval` available from `ingot_core::feed`.

### Task 2.8.2: Add Kraken OHLCV REST endpoint to `ingot-connectivity`

**File:** `crates/ingot-connectivity/src/kraken/rest.rs`

```rust
/// Fetch OHLCV bars from Kraken REST API.
///
/// Returns up to 720 bars per call. Paginate using the returned `last` timestamp.
/// Returns (bars, last_timestamp) where last_timestamp is used as `since` for the next page.
pub async fn fetch_ohlcv(
    client: &reqwest::Client,
    base_url: &str,
    symbol: Symbol,
    interval: BarInterval,
    since: Option<i64>,
) -> Result<(Vec<OhlcvBar>, Option<i64>)>;
```

**Kraken API:**
- `GET /0/public/OHLC?pair=XBTUSD&interval=60&since=1709164800`
- Response: `{"error":[], "result": {"XXBTZUSD": [[time, open, high, low, close, vwap, volume, count], ...], "last": 1709251200}}`

**Implementation:**
1. Convert `Symbol` to `KrakenPair` format (no separator variant)
2. Convert `BarInterval` to minutes
3. Build URL with query parameters
4. Deserialize response
5. Parse each array element into `OhlcvBar` (strings → `Decimal`)
6. Return bars + last timestamp for pagination

**Tests (write first, wiremock-based):**

```
test_fetch_ohlcv_parses_kraken_response
test_fetch_ohlcv_handles_empty_response
test_fetch_ohlcv_returns_last_for_pagination
test_fetch_ohlcv_maps_symbol_correctly
```

**Acceptance:** Kraken OHLCV response correctly parsed into `Vec<OhlcvBar>`.

### Task 2.8.3: Implement OHLCV bar ingestion in storage

**File:** `crates/ingot-storage/src/questdb/ohlcv.rs`

```rust
/// Buffer a batch of OHLCV bars for QuestDB ingestion.
pub fn buffer_ohlcv_bars(buffer: &mut Buffer, bars: &[OhlcvBar]) -> Result<()>;
```

**ILP format:**

```
ohlcv_bars,symbol=BTC-USD,interval=1h open=67400.0,high=67500.0,low=67350.0,close=67432.5,volume=123.45,count=1847i 1709164800000000000
```

Also create the OHLCV table in `ensure_questdb_tables()`:

```sql
CREATE TABLE IF NOT EXISTS ohlcv_bars (
    symbol    SYMBOL,
    interval  SYMBOL,
    open      DOUBLE,
    high      DOUBLE,
    low       DOUBLE,
    close     DOUBLE,
    volume    DOUBLE,
    count     INT,
    timestamp TIMESTAMP
) TIMESTAMP(timestamp) PARTITION BY MONTH WAL DEDUP UPSERT KEYS(symbol, interval, timestamp);
```

Add to `StorageService`:

```rust
impl StorageService {
    /// Ingest a batch of OHLCV bars into QuestDB.
    pub fn ingest_ohlcv_bars(&mut self, bars: &[OhlcvBar]) -> Result<()>;
}
```

**Tests (write first):**

```
test_buffer_ohlcv_bar_produces_valid_ilp
test_buffer_ohlcv_bars_batch
test_buffer_ohlcv_bar_interval_encoding
```

**Acceptance:** OHLCV bars correctly formatted as ILP and ingested into QuestDB.

### Task 2.8.4: Implement ingestion orchestration in `ingot-server`

**File:** `crates/ingot-server/src/ipc_command.rs`

The IPC command thread handles `IngestOhlcv` by spawning a background tokio task:

```rust
IpcCommand::IngestOhlcv { symbol, interval, since_secs } => {
    let client = reqwest::Client::new();
    let storage = storage_reader.clone(); // or dedicated ingestion handle
    let base_url = /* kraken base URL */;

    rt_handle.spawn(async move {
        let mut since = Some(since_secs);
        let mut total_bars: u64 = 0;

        loop {
            let (bars, last) = fetch_ohlcv(&client, &base_url, symbol, interval, since)
                .await?;

            if bars.is_empty() { break; }

            total_bars += bars.len() as u64;
            storage.ingest_ohlcv_bars(&bars)?;
            storage.flush_questdb()?;

            tracing::info!(symbol = %symbol, bars = total_bars, "OHLCV ingestion progress");

            if bars.len() < 720 { break; } // Last page
            since = last;

            // Rate limit: 1 request per second
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        tracing::info!(symbol = %symbol, total = total_bars, "OHLCV ingestion complete");
        Ok::<_, anyhow::Error>(total_bars)
    });

    IpcCommandResponse::IngestProgress {
        symbol,
        bars_ingested: 0,
        complete: false,
    }
}
```

Note: Initial response is immediate (acknowledges the command). Progress is logged server-side. Full progress reporting via IPC is a refinement — for Phase 2, the CLI gets an acknowledgment and the server logs progress.

**Acceptance:** `ingest ohlcv` command triggers background Kraken fetching and QuestDB ingestion.

### Task 2.8.5: Add new IPC types

**File:** `crates/ingot-ipc/src/types.rs`

```rust
// Add to IpcCommand enum
IngestOhlcv {
    symbol: Symbol,
    interval: u8,      // BarInterval discriminant
    since_secs: i64,   // Unix timestamp
},

// Add to IpcCommandResponse enum
IngestProgress {
    symbol: Symbol,
    bars_ingested: u64,
    complete: bool,
},
```

### Step 2.8 PR Checklist

- [ ] All standard checks
- [ ] wiremock tests for Kraken OHLCV parsing
- [ ] OHLCV ILP formatting unit tests
- [ ] Manual integration test: bars appear in QuestDB

---

## Step 2.9: Export (CSV + JSON)

**Branch:** `feature/export`
**Depends on:** Step 2.6

### Task 2.9.1: Add `csv` to workspace

**File:** root `Cargo.toml`

```toml
csv = { version = "1", default-features = false }
```

**File:** `crates/ingot-storage/Cargo.toml`

```toml
csv = { workspace = true }
```

### Task 2.9.2: Implement audit export

**File:** `crates/ingot-storage/src/export.rs`

```rust
/// Export audit log entries to CSV format.
pub async fn export_audit_csv<W: std::io::Write>(
    pool: &PgPool,
    writer: W,
    since: Option<DateTime<Utc>>,
) -> Result<u64>;

/// Export audit log entries to JSON format (array of objects).
pub async fn export_audit_json<W: std::io::Write>(
    pool: &PgPool,
    writer: W,
    since: Option<DateTime<Utc>>,
) -> Result<u64>;
```

**CSV columns:** `timestamp,event_type,actor,entity_id,payload`

**JSON format:** Array of `AuditRow` objects.

**Tests (write first):**

```
test_export_audit_csv_headers
test_export_audit_csv_row_format
test_export_audit_csv_empty_db
test_export_audit_json_valid_json
test_export_audit_json_preserves_payload
test_export_audit_json_empty_db_returns_empty_array
```

### Task 2.9.3: Implement order history export

**File:** `crates/ingot-storage/src/export.rs`

```rust
/// Export order history to CSV format.
pub async fn export_orders_csv<W: std::io::Write>(
    pool: &PgPool,
    writer: W,
    since: Option<DateTime<Utc>>,
) -> Result<u64>;

/// Export order history to JSON format (array of objects).
pub async fn export_orders_json<W: std::io::Write>(
    pool: &PgPool,
    writer: W,
    since: Option<DateTime<Utc>>,
) -> Result<u64>;
```

**CSV columns:** `timestamp,symbol,side,order_type,quantity,price,status,exchange_id`

**Tests (write first):**

```
test_export_orders_csv_headers
test_export_orders_csv_row_format
test_export_orders_json_valid_json
test_export_orders_json_null_price_for_market
```

### Task 2.9.4: Add to `StorageService` / `StorageReader`

```rust
impl StorageReader {
    pub async fn export_audit_csv<W: std::io::Write>(&self, writer: W, since: Option<DateTime<Utc>>) -> Result<u64>;
    pub async fn export_audit_json<W: std::io::Write>(&self, writer: W, since: Option<DateTime<Utc>>) -> Result<u64>;
    pub async fn export_orders_csv<W: std::io::Write>(&self, writer: W, since: Option<DateTime<Utc>>) -> Result<u64>;
    pub async fn export_orders_json<W: std::io::Write>(&self, writer: W, since: Option<DateTime<Utc>>) -> Result<u64>;
}
```

### Step 2.9 PR Checklist

- [ ] All standard checks
- [ ] CSV output matches expected format
- [ ] JSON output is valid and parseable
- [ ] Empty DB produces valid empty output

---

## Step 2.10: CLI Commands

**Branch:** `feature/cli-storage-cmds`
**Depends on:** Steps 2.5, 2.6, 2.8, 2.9

### Task 2.10.1: Add new IPC command variants

**File:** `crates/ingot-ipc/src/types.rs`

Add to `IpcCommand`:

```rust
QueryAudit {
    has_event_type: bool,
    event_type: FixedId,      // only valid if has_event_type
    has_since: bool,
    since_secs: i64,          // only valid if has_since
    limit: u16,
},
QueryOrders {
    has_symbol: bool,
    symbol: Symbol,            // only valid if has_symbol
    has_since: bool,
    since_secs: i64,           // only valid if has_since
    limit: u16,
},
VerifyAudit,
ExportAudit {
    format: u8,                // 0 = CSV, 1 = JSON
    has_since: bool,
    since_secs: i64,
    path: FixedId,             // output file path (truncated to 64 chars)
},
ExportOrders {
    format: u8,
    has_since: bool,
    since_secs: i64,
    path: FixedId,
},
IngestOhlcv {
    symbol: Symbol,
    interval: u8,
    since_secs: i64,
},
```

Add to `IpcCommandResponse`:

```rust
AuditEntries {
    count: u16,
    // Entries returned via a separate mechanism (too large for fixed-size IPC).
    // For Phase 2: return count only, entries printed server-side to log,
    // or serialize to a temp file and return path.
},
OrderEntries {
    count: u16,
},
AuditVerifyResult {
    valid: bool,
    broken_at_id: i64,       // 0 if valid
},
ExportResult {
    count: u64,
    path: FixedId,
},
IngestProgress {
    symbol: Symbol,
    bars_ingested: u64,
    complete: bool,
},
```

**Design note on IPC data size:** The fixed-size `#[repr(C)]` IPC types have limited capacity. For audit/order query results, consider:
- Return count + write results to a shared temp file (path in response)
- Or return a summary and print details server-side
- Or add a streaming IPC service for large results

For Phase 2, the pragmatic approach: queries return count, and the CLI uses a **separate request/response cycle per entry** (add a `QueryAuditPage { offset, limit }` variant), or the server writes results to stdout/log and the CLI displays the count.

**Recommended for Phase 2:** The server serializes query results to a temp file (JSON), returns the path in `FixedId`, and the CLI reads+displays the file. This avoids the fixed-size IPC constraint cleanly.

### Task 2.10.2: Add IPC command dispatch handlers

**File:** `crates/ingot-server/src/ipc_command.rs`

Add dispatch arms for each new command. Each calls the corresponding `StorageReader` method and returns the appropriate `IpcCommandResponse`.

### Task 2.10.3: Add CLI REPL commands

**File:** `crates/ingot-cli/src/commands.rs`

Add to `ReplCommand` enum:

```rust
/// Query audit log
Audit {
    #[arg(long)]
    r#type: Option<String>,
    #[arg(long)]
    since: Option<String>,     // ISO 8601 date
    #[arg(long, default_value = "50")]
    limit: u16,
    #[command(subcommand)]
    subcommand: Option<AuditSubcommand>,
},

/// Query order history
History {
    #[arg(long)]
    symbol: Option<String>,
    #[arg(long)]
    since: Option<String>,
    #[arg(long, default_value = "50")]
    limit: u16,
    #[command(subcommand)]
    subcommand: Option<HistorySubcommand>,
},

/// Ingest historical OHLCV data
Ingest {
    #[command(subcommand)]
    subcommand: IngestSubcommand,
},
```

```rust
enum AuditSubcommand {
    Verify,
    Export {
        #[arg(long)]
        format: String,        // "csv" or "json"
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        output: Option<String>,
    },
}

enum HistorySubcommand {
    Export {
        #[arg(long)]
        format: String,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        output: Option<String>,
    },
}

enum IngestSubcommand {
    Ohlcv {
        symbol: String,
        interval: String,       // "1m", "5m", "1h", etc.
        #[arg(long)]
        since: Option<String>,
    },
}
```

### Task 2.10.4: Implement CLI display formatting

**File:** `crates/ingot-cli/src/main.rs` (or new `display.rs`)

Tabular display for audit and order results:

```
> audit --limit 3
TIMESTAMP             EVENT TYPE          ACTOR    ENTITY ID
2026-02-28 14:30:01   transaction_posted  engine   a1b2c3d4-...
2026-02-28 14:30:01   order_filled        engine   e5f6a7b8-...
2026-02-28 14:29:58   order_placed        manual   e5f6a7b8-...
```

```
> history --symbol BTC-USD --limit 3
TIMESTAMP             SYMBOL    SIDE  TYPE    QTY      PRICE       STATUS
2026-02-28 14:30:01   BTC-USD   Buy   Market  0.001    67432.50    Filled
2026-02-28 14:25:00   BTC-USD   Sell  Limit   0.002    68000.00    Filled
2026-02-28 14:20:00   BTC-USD   Buy   Market  0.001    67100.00    Filled
```

```
> audit verify
Audit chain: VALID (1,245 entries verified)
```

```
> audit export --format csv
Exported 1,245 entries to audit_20260228.csv
```

```
> ingest ohlcv BTC-USD 1h --since 2025-01-01
Ingestion started for BTC-USD 1h bars from 2025-01-01.
(Progress logged server-side. Run 'audit --type ohlcv_ingested' to check status.)
```

### Step 2.10 PR Checklist

- [ ] All standard checks
- [ ] All new CLI commands parse correctly
- [ ] IPC round-trip works for each command
- [ ] Display formatting is aligned and readable

---

## Step 2.11: GUI Trade Log Panel

**Branch:** `feature/gui-trade-log`
**Depends on:** Step 2.10

### Task 2.11.1: Add trade log panel

**File:** `crates/ingot-gui/src/main.rs` (or new `panels/trade_log.rs`)

Add a scrollable table panel to the egui GUI:

**Columns:** Timestamp, Symbol, Side, Type, Quantity, Price, Status

**Features:**
- Subscribe to `SERVICE_ORDERS` IPC pub/sub for real-time updates
- Color coding: Buy rows in green tint, Sell in red tint, Rejected in gray
- Manual refresh button that sends `QueryOrders` IPC command
- Symbol filter dropdown

**Implementation:**
- Store `Vec<OrderHistoryRow>` in app state
- On each frame, check for new IPC order updates and append
- Render as `egui::Grid` or `egui_extras::Table`

### Task 2.11.2: Add audit log tab

**File:** `crates/ingot-gui/src/main.rs` (or new `panels/audit_log.rs`)

An expandable/collapsible section showing recent audit events:

- Default: collapsed
- Shows last 20 events when expanded
- Chain status indicator (green/red dot)
- Export button (triggers file save dialog)

### Task 2.11.3: Wire IPC queries for GUI

The GUI needs to send `QueryOrders` and `QueryAudit` IPC commands. Since the GUI's IPC thread already exists for subscriptions, extend it to also handle request/response queries triggered by UI actions.

### Step 2.11 PR Checklist

- [ ] All standard checks
- [ ] Trade log table renders with mock data
- [ ] Real-time updates appear from IPC subscription
- [ ] Audit tab expands/collapses
- [ ] Export button triggers file write

---

## Appendix A: New Workspace Dependencies Summary

All added to root `Cargo.toml` `[workspace.dependencies]`:

```toml
csv = { version = "1", default-features = false }
questdb-rs = { version = "4", default-features = false }
sqlx = { version = "0.8", default-features = false }
testcontainers = { version = "0.23", default-features = false }
testcontainers-modules = { version = "0.11", default-features = false }
```

## Appendix B: Modified Crate Dependencies

| Crate | New Dependencies |
|:---|:---|
| `ingot-storage` (new) | `sqlx`, `questdb-rs`, `csv`, `sha2`, `serde_json` + workspace deps |
| `ingot-engine` | `ingot-storage` (for `StorageEvent` type) |
| `ingot-server` | `ingot-storage` (for `StorageService`, `run_storage_task`) |
| `ingot-ipc` | No new external deps (new enum variants only) |
| `ingot-cli` | No new external deps (new command variants only) |
| `ingot-gui` | No new external deps (new panels only) |
| `ingot-core` | No new external deps (`OhlcvBar` uses existing `chrono`, `rust_decimal`, `serde`) |

## Appendix C: Migration Execution Order

```
001_create_accounts.sql          (Step 2.1)
002_create_transactions.sql      (Step 2.2)
003_create_entries.sql           (Step 2.2)
004_create_audit_log.sql         (Step 2.5)
005_create_order_history.sql     (Step 2.6)
```

All migrations are embedded via `sqlx::migrate!()` and run automatically on `StorageService::connect()`.

## Appendix D: Environment Variables (Complete)

| Variable | Required | Default | Step |
|:---|:---|:---|:---|
| `DATABASE_URL` | Yes (for storage) | — | 2.1 |
| `QUESTDB_ILP_ADDR` | No | `localhost:9009` | 2.7 |
| `PG_POOL_SIZE` | No | `5` | 2.1 |
| `STORAGE_BATCH_SIZE` | No | `50` | 2.4 |
| `STORAGE_FLUSH_INTERVAL_MS` | No | `100` | 2.4 |

When `DATABASE_URL` is not set, the server runs in memory-only mode (current behavior). This preserves backward compatibility.
