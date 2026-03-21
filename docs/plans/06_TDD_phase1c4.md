# Implementation Plan: Phase 1c.4 — FX Rate Service + NAV Calculation

## Context

Phase 1c.1–1c.3 delivered domain types, posting engine, and balance queries. Phase 1c.4 adds the FX rate service (`FxRateProvider` trait + `StaticFxRateProvider`) and NAV calculation (`NavCalculator`). Together these allow converting multi-currency portfolio balances into a single base-currency net asset value.

`nav.rs` already has `NavSnapshot` and `NavBreakdownEntry` data types (with serde tests). This phase adds the computation logic.

## What Already Exists

- `NavSnapshot`, `NavBreakdownEntry` — data types in `nav.rs` (line 6–20)
- `AccountingError::FxRateUnavailable { base, quote }` — error variant ready to use
- `CurrencyBalance { account_id, currency, balance }` — input type for NAV calculation
- `AccountingConfig { base_currency, ... }` — base currency defaults to USD
- `Price::new(Decimal)`, `Price::value() -> Decimal` — FX rate representation
- `Amount::new(Decimal)`, `Amount::value() -> Decimal` — balance/nav values
- Async trait pattern: `impl Future<Output = ...> + Send` (used in `ingot-connectivity/src/traits.rs`)
- `tokio` already in dev-dependencies

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| `FxRateProvider` pattern | `fn get_rate() -> impl Future + Send` (RPITIT) | Matches existing codebase trait pattern in connectivity |
| Rate lookup | Identity → direct → inverse → error | Standard FX cascade; covers all derivable pairs |
| Inverse calculation | `Price::new(Decimal::ONE / rate.value())` | No division op on Price; extract Decimal, divide, rewrap |
| NAV grouping | Sum `CurrencyBalance` by currency before conversion | Multiple accounts may hold same currency |
| `StaticFxRateProvider` storage | `HashMap<(Currency, Currency), Price>` | Simple, deterministic, perfect for testing |
| Proptest strategy | Generate positive balances + positive rates → NAV ≥ 0 | Validates no sign-flip bugs in conversion math |

## New Types & Functions

### `FxRateProvider` trait

```rust
pub trait FxRateProvider {
    fn get_rate(
        &self,
        base: &Currency,
        quote: &Currency,
    ) -> impl Future<Output = Result<Price, AccountingError>> + Send;
}
```

### `StaticFxRateProvider`

```rust
pub struct StaticFxRateProvider {
    rates: HashMap<(Currency, Currency), Price>,
}
```

- `new(rates: Vec<(Currency, Currency, Price)>) -> Self`
- `get_rate`: identity check → direct lookup → inverse lookup → `FxRateUnavailable`

### `NavCalculator<F: FxRateProvider>`

```rust
pub struct NavCalculator<F> {
    fx_provider: F,
}
```

- `new(fx_provider: F) -> Self`
- `calculate_nav(&self, base_currency: &Currency, balances: &[CurrencyBalance]) -> Result<NavSnapshot, AccountingError>`

**Algorithm:**
1. Group balances by currency → `HashMap<Currency, Decimal>` (sum balance values)
2. For each currency group, get FX rate to base_currency
3. Compute `base_currency_value = native_balance * fx_rate`
4. Build `NavBreakdownEntry` per currency
5. Sum all `base_currency_value` → `total_nav`
6. Return `NavSnapshot` with timestamp, breakdown, total

## TDD Step Order

### Step 1: `FxRateProvider` trait + `StaticFxRateProvider` scaffold
- Add trait and struct to `nav.rs`
- Stub `get_rate` to return `FxRateUnavailable`
- Add re-exports to `lib.rs`
- `cargo check --all-targets --workspace`

### Step 2: Identity rate
**Red:** `test_static_fx_rate_identity` — USD/USD → Price(1)
**Green:** Add identity check in `get_rate`

### Step 3: Direct lookup
**Red:** `test_static_fx_rate_direct` — BTC/USD with rate 67000 → Price(67000)
**Green:** Add HashMap lookup

### Step 4: Inverse rate
**Red:** `test_static_fx_rate_inverse` — USD/BTC when only BTC/USD=67000 exists → Price(1/67000)
**Green:** Add inverse lookup branch

### Step 5: Missing rate
**Red:** `test_static_fx_rate_missing` — ETH/EUR with no rates → `FxRateUnavailable`
**Green:** Already handled by error fallthrough

### Step 6: `NavCalculator` — single currency
**Red:** `test_nav_single_currency` — all USD balances, base=USD → total = sum of balances, fx_rate=1 for all
**Green:** Implement `calculate_nav`

### Step 7: `NavCalculator` — multi-currency
**Red:** `test_nav_multi_currency` — BTC + USD balances, BTC/USD=67000, base=USD → BTC converted, USD at rate 1, total correct
**Green:** Already handled by grouping + conversion

### Step 8: `NavCalculator` — aggregates same-currency accounts
**Red:** `test_nav_aggregates_same_currency` — two USD accounts (spot + futures) → single breakdown entry with summed balance
**Green:** Already handled by HashMap grouping

### Step 9: `NavCalculator` — missing rate propagates error
**Red:** `test_nav_missing_rate_error` — ETH balance but no ETH/USD rate → `FxRateUnavailable`
**Green:** Already handled by `?` propagation

### Step 10: `NavCalculator` — empty balances
**Red:** `test_nav_empty_balances` — no balances → total_nav=0, empty breakdown
**Green:** Already handled

### Step 11: Proptest — NAV non-negative for non-negative balances
**Red:** `prop_test_nav_non_negative` — random positive balances + positive FX rates → total_nav ≥ 0 (1000 cases)
**Green:** No new production code

### Step 12: Verification
1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-accounting` — all tests pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run`

## Critical Files

| File | Action |
|------|--------|
| `crates/ingot-accounting/src/nav.rs` | Add `FxRateProvider`, `StaticFxRateProvider`, `NavCalculator` + ~12 tests + proptest |
| `crates/ingot-accounting/src/lib.rs` | Re-export `FxRateProvider`, `StaticFxRateProvider`, `NavCalculator` |

No new dependencies needed. No storage changes.

## Verification

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --workspace` — zero new warnings
3. `cargo nextest run -p ingot-accounting` — all pass
4. `cargo check --all-targets --workspace`
5. `cargo bench --no-run`
6. Proptest: 1000 cases for NAV non-negativity
7. No `.unwrap()`, `.expect()`, `panic!()` anywhere
