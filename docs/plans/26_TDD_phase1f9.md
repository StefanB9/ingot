# Phase 1f.9: Corporate Actions — Accounting Extensions

## Overview

Add corporate action transaction types and posting functions to `ingot-accounting`:
Dividend, StockSplit, BondCoupon, Merger, Spinoff.

## Files Modified

| File | Change |
|------|--------|
| `crates/ingot-accounting/src/error.rs` | Add `InvalidCorporateAction` variant |
| `crates/ingot-accounting/src/types.rs` | Add 5 `TransactionType` variants |
| `crates/ingot-accounting/src/transaction.rs` | `validate()` skips balance check for `Merger` |
| `crates/ingot-accounting/src/posting.rs` | 5 new posting functions |
| `crates/ingot-accounting/src/lib.rs` | Re-export new posting functions |

## Design Decisions

### Stock Splits
Zero-amount entries (Debit asset, Credit revenue:split). Split details (old_qty, new_qty, ratio) stored in metadata. `0 == 0` passes balance validation.

### Mergers
Skip balance check like `Trade` — mergers are fundamentally asset exchanges that may cross currencies. Change `validate()` to `matches!(type, Trade | Merger)`.

### Revenue Sub-Accounts
Use venue field per existing convention: `"dividend"`, `"split"`, `"coupon"`, `"spinoff"`.

## Function Signatures

```rust
pub fn post_dividend(exchange: Exchange, venue: &str, currency: &Currency,
    symbol: &Symbol, amount: Amount, timestamp: DateTime<Utc>)
    -> Result<Transaction, AccountingError>

pub fn post_bond_coupon(exchange: Exchange, venue: &str, currency: &Currency,
    symbol: &Symbol, amount: Amount, timestamp: DateTime<Utc>)
    -> Result<Transaction, AccountingError>

pub fn post_stock_split(exchange: Exchange, venue: &str, symbol: &Symbol,
    currency: &Currency, old_qty: Quantity, new_qty: Quantity,
    timestamp: DateTime<Utc>) -> Result<Transaction, AccountingError>

pub fn post_merger(exchange: Exchange, venue: &str, old_symbol: &Symbol,
    old_currency: &Currency, old_qty: Quantity, new_symbol: &Symbol,
    new_currency: &Currency, new_qty: Quantity,
    cash_consideration: Option<Amount>, timestamp: DateTime<Utc>)
    -> Result<Transaction, AccountingError>

pub fn post_spinoff(exchange: Exchange, venue: &str, parent_symbol: &Symbol,
    new_symbol: &Symbol, new_currency: &Currency, new_qty: Quantity,
    cost_basis_allocation: Option<Amount>, timestamp: DateTime<Utc>)
    -> Result<Transaction, AccountingError>
```

## Account Structures

### Dividend / Bond Coupon
| Entry | Account | Side | Amount |
|-------|---------|------|--------|
| 1 | `asset:{exchange}:{venue}:{currency}` | Debit | amount |
| 2 | `revenue:{exchange}:{dividend\|coupon}:{currency}` | Credit | amount |

### Stock Split
| Entry | Account | Side | Amount |
|-------|---------|------|--------|
| 1 | `asset:{exchange}:{venue}:{currency}` | Debit | 0 |
| 2 | `revenue:{exchange}:split:{currency}` | Credit | 0 |

### Merger (without cash)
| Entry | Account | Side | Amount |
|-------|---------|------|--------|
| 1 | `asset:{exchange}:{venue}:{new_currency}` | Debit | new_qty |
| 2 | `asset:{exchange}:{venue}:{old_currency}` | Credit | old_qty |

### Merger (with cash consideration)
| Entry | Account | Side | Amount |
|-------|---------|------|--------|
| 1 | `asset:{exchange}:{venue}:{new_currency}` | Debit | new_qty |
| 2 | `asset:{exchange}:{venue}:{old_currency}` | Credit | old_qty |
| 3 | `asset:{exchange}:{venue}:{old_currency}` | Debit | cash |

### Spinoff (with cost basis)
| Entry | Account | Side | Amount |
|-------|---------|------|--------|
| 1 | `asset:{exchange}:{venue}:{new_currency}` | Debit | cost_basis |
| 2 | `asset:{exchange}:{venue}:{new_currency}` | Credit | cost_basis |

### Spinoff (without cost basis)
| Entry | Account | Side | Amount |
|-------|---------|------|--------|
| 1 | `asset:{exchange}:{venue}:{new_currency}` | Debit | 0 |
| 2 | `revenue:{exchange}:spinoff:{new_currency}` | Credit | 0 |

## TDD Test Plan (28 tests)

### Cycle 0: error.rs (1 test)
- `test_error_display_invalid_corporate_action`

### Cycle 1: types.rs (2 tests)
- `test_transaction_type_display_corporate_actions`
- `test_transaction_type_serde_roundtrip_corporate_actions`

### Cycle 2: transaction.rs (2 tests)
- `test_transaction_validate_merger_cross_currency`
- `test_transaction_validate_merger_same_currency_unbalanced`

### Cycle 3: post_dividend (4 tests)
- `test_post_dividend_basic`
- `test_post_dividend_zero_amount_rejected`
- `test_post_dividend_negative_amount_rejected`
- `prop_test_post_dividend_always_validates`

### Cycle 4: post_bond_coupon (4 tests)
- `test_post_bond_coupon_basic`
- `test_post_bond_coupon_zero_amount_rejected`
- `test_post_bond_coupon_negative_amount_rejected`
- `prop_test_post_bond_coupon_always_validates`

### Cycle 5: post_stock_split (4 tests)
- `test_post_stock_split_basic`
- `test_post_stock_split_reverse_split`
- `test_post_stock_split_same_quantity_rejected`
- `test_post_stock_split_zero_old_qty_rejected`

### Cycle 6: post_merger (8 tests)
- `test_post_merger_same_currency_no_cash`
- `test_post_merger_same_currency_with_cash`
- `test_post_merger_cross_currency`
- `test_post_merger_cross_currency_with_cash`
- `test_post_merger_zero_old_qty_rejected`
- `test_post_merger_zero_new_qty_rejected`
- `test_post_merger_zero_cash_rejected`
- `test_post_merger_negative_cash_rejected`

### Cycle 7: post_spinoff (5 tests)
- `test_post_spinoff_with_cost_basis`
- `test_post_spinoff_without_cost_basis`
- `test_post_spinoff_zero_new_qty_rejected`
- `test_post_spinoff_zero_cost_basis_rejected`
- `test_post_spinoff_negative_cost_basis_rejected`

### Cycle 8: lib.rs re-exports

### Cycle 9: Refactor — extract shared `post_income_receipt` helper

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -p ingot-accounting
cargo nextest run -p ingot-accounting
cargo nextest run --workspace --exclude ingot-storage
```
