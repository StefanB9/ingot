# Implementation Plan: Phase 1c.3 — Balance Queries + Account Aggregation

## Context

Phase 1c.1 delivered domain types and storage repos. Phase 1c.2 delivered the posting engine (4 functions, 16 tests). Phase 1c.3 adds balance query methods to `PgLedgerRepository` and a pure in-memory `aggregate_balances()` function in `ingot-accounting`.

The goal: provide all the query building blocks needed by downstream features (NAV calculation in 1c.4, reconciliation in 1c.5, risk dashboard).

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| `trial_balance` return | Named `TrialBalance` struct with `is_balanced()` | Self-documenting vs ambiguous tuple; aligns with newtype philosophy |
| `get_entries_since` bounds | Only `since`, no upper bound | Minimal API; bounded range trivial to add later |
| `aggregate_balances` grouping | Group by `AccountId`, sum debit−credit | Matches SQL query granularity exactly |
| `EntryId::from_uuid` | Add to `transaction.rs` | Needed to reconstruct `LedgerEntry` from DB rows |

## Prerequisite: `EntryId::from_uuid`

`crates/ingot-accounting/src/transaction.rs` — add `from_uuid` to `EntryId` (mirrors `TransactionId::from_uuid`):

```rust
pub fn from_uuid(uuid: Uuid) -> Self {
    Self(uuid)
}
```

## New Types

### `TrialBalance` in `crates/ingot-accounting/src/balance.rs`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialBalance {
    pub total_debits: Amount,
    pub total_credits: Amount,
}

impl TrialBalance {
    pub fn is_balanced(&self) -> bool {
        self.total_debits == self.total_credits
    }
}
```

Re-export from `lib.rs`: `pub use balance::TrialBalance;`

## New Functions

### `aggregate_balances` in `crates/ingot-accounting/src/balance.rs`

Pure in-memory aggregation. Groups entries by `AccountId`, sums debit−credit.

```rust
pub fn aggregate_balances(entries: &[LedgerEntry]) -> Vec<CurrencyBalance> {
    // HashMap<AccountId, Decimal> accumulator
    // For each entry: += amount if Debit, -= amount if Credit
    // Convert to Vec<CurrencyBalance>, sorted by account_id.to_string()
}
```

### New repo methods in `crates/ingot-storage/src/ledger_repo.rs`

#### `get_balances_by_exchange`

```rust
#[instrument(skip(self))]
pub async fn get_balances_by_exchange(&self, exchange: Exchange) -> Result<Vec<CurrencyBalance>>
```

SQL: Same as `get_account_balances` but with `WHERE exchange = $1`.

#### `get_entries_since`

```rust
#[instrument(skip(self))]
pub async fn get_entries_since(&self, since: DateTime<Utc>) -> Result<Vec<LedgerEntry>>
```

SQL:
```sql
SELECT id, transaction_id, account_type, exchange, venue, currency,
       side, amount, timestamp, description
FROM ledger_entries
WHERE timestamp >= $1
ORDER BY timestamp ASC
```

Requires new helper: `parse_entry_side(&str) -> Result<EntrySide>`.

#### `trial_balance`

```rust
#[instrument(skip(self))]
pub async fn trial_balance(&self) -> Result<TrialBalance>
```

SQL:
```sql
SELECT
    COALESCE(SUM(CASE WHEN side = 'debit' THEN amount ELSE 0 END), 0) as total_debits,
    COALESCE(SUM(CASE WHEN side = 'credit' THEN amount ELSE 0 END), 0) as total_credits
FROM ledger_entries
```

## TDD Step Order

### Step 1: `TrialBalance` type + `EntryId::from_uuid`
- Add `TrialBalance` struct to `balance.rs` with `is_balanced()` method
- Add `EntryId::from_uuid(uuid: Uuid) -> Self` to `transaction.rs`
- Add `pub use balance::TrialBalance;` to `lib.rs`
- `cargo check --all-targets --workspace`

### Step 2: `aggregate_balances` — basic
**Red:** `test_aggregate_balances_single_currency` — 2 debit entries + 2 credit entries for same account → single `CurrencyBalance` with correct net
**Green:** Implement `aggregate_balances`

### Step 3: `aggregate_balances` — multi-account
**Red:** `test_aggregate_balances_multi_account` — entries across different accounts (asset:BTC, asset:USD, expense:USD) → 3 separate balances
**Green:** Already handled by HashMap grouping

### Step 4: `aggregate_balances` — empty
**Red:** `test_aggregate_balances_empty` — empty slice → empty vec
**Green:** Already handled

### Step 5: `aggregate_balances` — proptest
**Red:** `prop_test_aggregate_balances_matches_manual` — random entries → aggregate result matches manual sum per account (1000 cases)
**Green:** No new production code

### Step 6: `get_balances_by_exchange` (integration)
**Red:** `test_get_balances_by_exchange` — insert transactions on Kraken and Paper exchanges → query Kraken → only Kraken accounts returned
**Green:** Implement `get_balances_by_exchange` in `PgLedgerRepository`

### Step 7: `get_entries_since` (integration)
**Red:** `test_get_entries_since` — insert transactions at t1 and t2 → query since t2 → only t2 entries returned, fully reconstructed `LedgerEntry`
**Green:** Implement `get_entries_since` + `parse_entry_side` helper

### Step 8: `trial_balance` (integration)
**Red:** `test_trial_balance` — insert multiple balanced transactions → trial_balance returns equal debits/credits
**Green:** Implement `trial_balance`

### Step 9: `trial_balance` — empty ledger (integration)
**Red:** `test_trial_balance_empty` — no transactions → debits=0, credits=0, is_balanced()=true
**Green:** Already handled by COALESCE

### Step 10: Proptest — trial balance invariant (integration)
**Red:** `prop_test_trial_balance_always_balanced` — generate N random balanced transactions via posting engine, insert all → trial_balance always balanced (100 cases — lower count because of DB round-trip cost)
**Green:** No new production code

### Step 11: Integration — multi-transaction balance verification
**Red:** `test_multi_transaction_balances` — post_fill (buy BTC) + post_funding_rate (pay) + post_transfer (spot→futures) → verify all account balances are correct
**Green:** No new production code (validates existing methods work together)

### Step 12: Verification
1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-accounting` — all unit tests pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run`

## Critical Files

| File | Action |
|------|--------|
| `crates/ingot-accounting/src/balance.rs` | Add `TrialBalance`, `aggregate_balances()`, unit tests + proptest |
| `crates/ingot-accounting/src/transaction.rs` | Add `EntryId::from_uuid()` |
| `crates/ingot-accounting/src/lib.rs` | Re-export `TrialBalance`, `aggregate_balances` |
| `crates/ingot-storage/src/ledger_repo.rs` | Add `get_balances_by_exchange`, `get_entries_since`, `trial_balance`, `parse_entry_side` |
| `crates/ingot-storage/tests/integration.rs` | Add 6 integration tests |

## Verification

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-accounting` — unit tests + proptest pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run`
6. Proptest: 1000 cases for `aggregate_balances` correctness
7. Proptest: 100 cases for trial_balance invariant (DB round-trip)
8. Integration: multi-transaction scenario verifies all query methods
9. No `.unwrap()`, `.expect()`, `panic!()` anywhere
