# Implementation Plan: Phase 1c.5 — Broker Reconciliation

## Context

Phases 1c.1–1c.4 delivered the accounting crate (domain types, posting engine, balance queries, FX/NAV). Phase 1c.5 adds the broker reconciliation: a pure function `reconcile()` that compares internal ledger balances against broker-reported balances per currency, classifies discrepancies by severity, and returns a `ReconciliationResult`.

All reconciliation types (`Discrepancy`, `DiscrepancySeverity`, `ReconciliationStatus`, `ReconciliationResult`) already exist with serde/Display tests. Storage (`PgReconciliationRepository`) is already implemented. This phase adds the computation logic.

## What Already Exists

- `Discrepancy`, `DiscrepancySeverity`, `ReconciliationStatus`, `ReconciliationResult` — types in `reconciliation.rs` with 4 tests
- `AccountingError::ReconciliationFailed { exchange, reason }` — error variant ready
- `AccountingConfig { minor_threshold_pct: 0.001, major_threshold_pct: 0.01 }` — threshold config
- `CurrencyBalance { account_id, currency, balance }` — ledger balance input
- `Balance { currency, total, available, held }` — broker balance from `ingot-core/src/balance.rs`
- `PgReconciliationRepository { insert_result, get_latest }` — storage ready
- Integration test `test_reconciliation_insert_and_get_latest` — storage round-trip test exists
- `ingot-core` already in `ingot-accounting` dependencies

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| `reconcile()` | Pure function, takes pre-fetched data | Matches TDD doc; caller provides both sides; no async needed |
| Ledger aggregation | Sum all `CurrencyBalance` entries by currency (caller pre-filters by exchange) | Caller responsibility to pass exchange-filtered balances |
| Broker comparison | Use `Balance.total` field | Total = available + held; represents full broker position |
| Severity classification | `classify_severity(difference, reference, config)` uses config thresholds | `difference == 0` → None; `pct < minor` → Minor; `pct < major` → Major; else → Critical |
| Reference for pct | Use broker balance as reference (denominator) | Broker is the source of truth for position sizing |
| Zero broker balance | If broker=0 and ledger≠0 → Critical | Division by zero guard; any discrepancy with zero reference is critical |
| Pass/Fail rule | Pass if all discrepancies are None or Minor; Fail if any Major or Critical | Matches TDD doc |

## New Functions

### `reconcile` in `reconciliation.rs`

```rust
pub fn reconcile(
    exchange: Exchange,
    ledger_balances: &[CurrencyBalance],
    broker_balances: &[Balance],
    config: &AccountingConfig,
) -> ReconciliationResult
```

**Algorithm:**
1. Aggregate `ledger_balances` by currency → `HashMap<Currency, Decimal>` (sum balance values)
2. Collect all broker balances by currency → `HashMap<Currency, Decimal>` (using `Balance.total`)
3. Union of all currencies from both maps
4. For each currency:
   - Get ledger amount (0 if missing)
   - Get broker amount (0 if missing)
   - Compute `difference = ledger - broker`
   - Classify severity using thresholds
   - Build `Discrepancy`
5. Status = Pass if all None/Minor, Fail if any Major/Critical
6. Return `ReconciliationResult` with UUID v7 id and current timestamp

### `classify_severity` (private helper)

```rust
fn classify_severity(
    difference: Decimal,
    reference: Decimal,
    config: &AccountingConfig,
) -> DiscrepancySeverity
```

- `difference == 0` → `None`
- `reference == 0` → `Critical` (can't compute percentage; any nonzero diff is critical)
- `pct = (difference / reference).abs()`
- `pct < minor_threshold_pct` → `Minor`
- `pct < major_threshold_pct` → `Major`
- else → `Critical`

## TDD Step Order

### Step 1: `classify_severity` tests + implementation
**Red:**
- `test_classify_severity_zero_difference` → None
- `test_classify_severity_minor` → Minor (0.05% difference)
- `test_classify_severity_major` → Major (0.5% difference)
- `test_classify_severity_critical` → Critical (2% difference)
- `test_classify_severity_zero_reference` → Critical
**Green:** Implement `classify_severity`

### Step 2: `reconcile` — exact match
**Red:** `test_reconcile_exact_match` — ledger=1000 USD, broker=1000 USD → all None, status Pass
**Green:** Implement `reconcile`

### Step 3: `reconcile` — minor drift
**Red:** `test_reconcile_minor_drift` — ledger=1000, broker=999.95 → Minor, status Pass
**Green:** Already handled

### Step 4: `reconcile` — major drift
**Red:** `test_reconcile_major_drift` — ledger=1000, broker=995 → Major, status Fail
**Green:** Already handled

### Step 5: `reconcile` — critical drift
**Red:** `test_reconcile_critical_drift` — ledger=1000, broker=980 → Critical, status Fail
**Green:** Already handled

### Step 6: `reconcile` — currency in broker but not ledger
**Red:** `test_reconcile_currency_only_in_broker` — broker has ETH, ledger doesn't → Critical (ledger=0, broker≠0)
**Green:** Already handled by union of currencies

### Step 7: `reconcile` — currency in ledger but not broker
**Red:** `test_reconcile_currency_only_in_ledger` — ledger has BTC, broker doesn't → Critical (broker=0)
**Green:** Already handled

### Step 8: `reconcile` — multi-currency
**Red:** `test_reconcile_multi_currency` — USD matches, BTC has minor drift → Pass (all None/Minor)
**Green:** Already handled

### Step 9: Proptest — identical balances always pass
**Red:** `prop_test_identical_balances_always_pass` — random amounts, broker=ledger → all None, status Pass (1000 cases)
**Green:** No new production code

### Step 10: Integration — full cycle
**Red:** `test_reconciliation_full_cycle` — post fills → get_balances_by_exchange → reconcile against mock broker → insert_result → get_latest → verify
**Green:** No new production code (validates all layers work together)

### Step 11: Verification
1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-accounting` — all tests pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run`

## Critical Files

| File | Action |
|------|--------|
| `crates/ingot-accounting/src/reconciliation.rs` | Add `reconcile()`, `classify_severity()`, ~10 unit tests + proptest |
| `crates/ingot-accounting/src/lib.rs` | Re-export `reconcile` |
| `crates/ingot-storage/tests/integration.rs` | Add full-cycle integration test |

No new dependencies needed.

## Verification

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-accounting` — all pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run`
6. Proptest: 1000 cases for identical-balances-always-pass
7. Integration: full cycle post→balance→reconcile→store→retrieve
8. No `.unwrap()`, `.expect()`, `panic!()` anywhere
