# Step 2.2: Transaction + Entry Persistence

**Project:** Ingot — Portfolio Management & Systematic Trading Workstation
**Scope:** Phase 2, Step 2.2 — Persistent Storage & Audit Trail
**Branch:** `feature/transaction-persist`
**Depends on:** Step 2.1 (Account CRUD)
**Date:** February 28, 2026

---

## Overview

Add persistence for `Transaction` and `Entry` types — the core double-entry accounting records. Two new PostgreSQL tables (`transactions`, `entries`) with atomic save, two-query load with single-pass merge, and full round-trip fidelity for `Decimal` amounts.

## Key Type Mappings

| Rust Field | DB Column | Type |
|:---|:---|:---|
| `Transaction.id` | `transactions.id` | `UUID` |
| `Transaction.date` | `transactions.posted_at` | `TIMESTAMPTZ` |
| `Transaction.description` | `transactions.description` | `TEXT` |
| `Entry.account_id` | `entries.account_id` | `UUID` (FK → accounts) |
| `Entry.amount` → `Decimal` | `entries.amount` | `NUMERIC` |
| `Entry.side` (`AccountSide`) | `entries.side` | `TEXT` CHECK (`Debit`, `Credit`) |
| `Entry.currency.code` | `entries.currency_code` | `TEXT` |
| `Entry.currency.decimals` | `entries.decimals` | `SMALLINT` |

## Tasks

### Task 2.2.1: Transaction Migration

**File:** `crates/ingot-storage/migrations/<ts>_002_create_transactions.{up,down}.sql`

```sql
-- UP
CREATE TABLE IF NOT EXISTS transactions (
    id          UUID        PRIMARY KEY,
    posted_at   TIMESTAMPTZ NOT NULL,
    description TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_transactions_posted_at ON transactions (posted_at);

-- DOWN
DROP TABLE IF EXISTS transactions;
```

### Task 2.2.2: Entries Migration

**File:** `crates/ingot-storage/migrations/<ts>_003_create_entries.{up,down}.sql`

```sql
-- UP
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

-- DOWN
DROP TABLE IF EXISTS entries;
```

### Task 2.2.3: Implement `save_transaction()`

**File:** `crates/ingot-storage/src/postgres/transactions.rs`

```rust
pub async fn save_transaction(pool: &PgPool, tx: &Transaction) -> Result<()>
```

Atomic: `pool.begin()` → INSERT transaction → INSERT entries → `commit()`. ON CONFLICT DO NOTHING for idempotency.

**Helpers:** `side_str(AccountSide) -> &'static str`, `parse_side(&str) -> Result<AccountSide>`

### Task 2.2.4: Implement `load_transactions()`

**File:** `crates/ingot-storage/src/postgres/transactions.rs`

```rust
pub async fn load_transactions(pool: &PgPool) -> Result<Vec<Transaction>>
```

Two-query approach:
1. SELECT transactions ORDER BY posted_at ASC
2. SELECT entries ORDER BY transaction_id, id ASC
3. Single-pass merge grouping entries into parent transactions

### Task 2.2.5: StorageService Integration

**Files:** `service.rs`, `postgres/mod.rs`

Add `save_transaction()` and `load_transactions()` delegate methods.

### Task 2.2.6: Tests

**Unit tests** (in `transactions.rs`):
- Side str/parse helpers (all variants, invalid, round-trip)
- Proptest: `Decimal` round-trip through `NUMERIC` (1000 cases)

**Integration tests** (in `tests/pg_integration.rs`):
- Save: header fields, all entries, idempotent, atomic rollback on FK violation
- Load: empty DB, entries grouped, ordered by posted_at, field values preserved

### Task 2.2.7: Regenerate sqlx Offline Cache

```bash
cargo sqlx migrate run
cargo sqlx prepare --workspace
```

## Acceptance Criteria

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --workspace` — zero warnings
- [ ] `cargo nextest run --workspace` — all tests pass
- [ ] `cargo check --all-targets --workspace`
- [ ] `cargo bench --no-run`
- [ ] `.sqlx/` updated
- [ ] Proptest decimal fidelity passes (1000 cases)
- [ ] Atomicity test verifies rollback on partial failure
