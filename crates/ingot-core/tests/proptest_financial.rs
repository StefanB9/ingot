//! Property-based tests for critical financial logic.
//!
//! Invariants verified:
//! - `ExchangeRate::convert` is exact multiplication for all positive inputs
//! - `ExchangeRate::new` rejects every non-positive price
//! - Any balanced `Transaction` validates; any unbalanced one fails
//! - `QuoteBoard` direct conversion is exact; round-trip error is < 10⁻¹⁸
//! - `Ledger::net_asset_value` is monotone: adding a positive asset never
//!   decreases NAV

use ingot_core::accounting::{
    Account, AccountSide, AccountType, ExchangeRate, Ledger, QuoteBoard, Transaction,
    ValidatedTransaction,
};
use ingot_primitives::{Amount, Currency, Money, Price};
use proptest::{prelude::*, test_runner::TestCaseError};
use rust_decimal::Decimal;
// ── Helpers
// ───────────────────────────────────────────────────────────────────

fn fail(e: impl std::fmt::Display) -> TestCaseError {
    TestCaseError::fail(e.to_string())
}

/// Positive integer amounts [1, 1 000 000].
///
/// Integer values guarantee exact `Decimal` multiplication — no rounding
/// artefacts that would confound equality assertions.
fn amount_strat() -> impl Strategy<Value = Amount> {
    (1u64..=1_000_000u64).prop_map(|n| Amount::from(Decimal::from(n)))
}

/// Positive integer prices [1, 100 000].
fn price_strat() -> impl Strategy<Value = Price> {
    (1u64..=100_000u64).prop_map(|n| Price::from(Decimal::from(n)))
}

// ── All property tests
// ────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    // ── ExchangeRate ──────────────────────────────────────────────────────────

    /// For integer-valued inputs, `convert(a)` is exactly `a * rate` —
    /// the same multiplication `Amount * Price` performs.
    #[test]
    fn exchange_rate_convert_is_multiplication(
        a in amount_strat(),
        p in price_strat(),
    ) {
        let rate = ExchangeRate::new(Currency::btc(), Currency::usd(), p).map_err(fail)?;
        prop_assert_eq!(rate.convert(a), a * p);
    }

    /// Every non-positive price (zero and all negatives) is rejected.
    #[test]
    fn exchange_rate_rejects_non_positive(raw in i64::MIN..=0_i64) {
        let price = Price::from(Decimal::from(raw));
        prop_assert!(ExchangeRate::new(Currency::btc(), Currency::usd(), price).is_err());
    }

    // ── Transaction ───────────────────────────────────────────────────────────

    /// A single-currency transaction with equal debit and credit always passes
    /// balance validation.
    #[test]
    fn balanced_transaction_validates(amount in amount_strat()) {
        let usd = Currency::usd();
        let asset  = Account::new("Asset".into(),  AccountType::Asset,  usd);
        let equity = Account::new("Equity".into(), AccountType::Equity, usd);

        let mut tx = Transaction::new("balanced".into());
        tx.add_entry(&asset,  amount, AccountSide::Debit ).map_err(fail)?;
        tx.add_entry(&equity, amount, AccountSide::Credit).map_err(fail)?;

        prop_assert!(tx.validate().is_ok());
    }

    /// A single-currency transaction with differing debit and credit totals
    /// always fails balance validation.
    #[test]
    fn unbalanced_transaction_fails(a in amount_strat(), b in amount_strat()) {
        prop_assume!(a != b);

        let usd = Currency::usd();
        let acc = Account::new("Acct".into(), AccountType::Asset, usd);

        let mut tx = Transaction::new("unbalanced".into());
        tx.add_entry(&acc, a, AccountSide::Debit ).map_err(fail)?;
        tx.add_entry(&acc, b, AccountSide::Credit).map_err(fail)?;

        prop_assert!(tx.validate().is_err());
    }

    // ── QuoteBoard ────────────────────────────────────────────────────────────

    /// Direct conversion (base → quote) is exactly `amount × rate` for
    /// integer-valued inputs.
    #[test]
    fn quoteboard_direct_conversion_exact(a in amount_strat(), r in price_strat()) {
        let btc = Currency::btc();
        let usd = Currency::usd();

        let mut board = QuoteBoard::new();
        board.add_rate(ExchangeRate::new(btc, usd, r).map_err(fail)?);

        let result = board.convert(&Money::new(a, btc), &usd).map_err(fail)?;

        prop_assert_eq!(result.amount, a * r);
        prop_assert_eq!(result.currency, usd);
    }

    /// Round-trip 1 BTC → USD → BTC stays within 10⁻¹⁸ of the starting value.
    ///
    /// The inverse path computes `amount * (1 / rate)`, which introduces at
    /// most `rate / 10^28` rounding error. For rates ≤ 100 000 the error is
    /// bounded by `10^(-23)`, well below the 10^(-18) tolerance used here.
    #[test]
    fn quoteboard_round_trip_within_tolerance(r in price_strat()) {
        let btc = Currency::btc();
        let usd = Currency::usd();

        let mut board = QuoteBoard::new();
        board.add_rate(ExchangeRate::new(btc, usd, r).map_err(fail)?);

        let one_btc  = Money::new(Amount::from(Decimal::ONE), btc);
        let in_usd   = board.convert(&one_btc, &usd).map_err(fail)?;
        let back_btc = board.convert(&in_usd,  &btc).map_err(fail)?;

        // tolerance = 10^-18
        let diff = (Decimal::from(back_btc.amount) - Decimal::ONE).abs();
        prop_assert!(
            diff <= Decimal::new(1, 18),
            "round-trip error {diff} exceeds 10^-18 tolerance"
        );
    }

    // ── Ledger ────────────────────────────────────────────────────────────────

    /// Depositing a positive amount of an asset always increases (or at worst
    /// preserves) the ledger's net asset value at any positive exchange rate.
    #[test]
    fn ledger_nav_increases_with_asset_addition(
        initial  in amount_strat(),
        addition in amount_strat(),
        rate     in price_strat(),
    ) {
        let btc = Currency::btc();
        let usd = Currency::usd();

        let asset  = Account::new("BTC Asset".into(),  AccountType::Asset,  btc);
        let equity = Account::new("BTC Equity".into(), AccountType::Equity, btc);

        let mut ledger = Ledger::new();
        ledger.add_account(asset.clone());
        ledger.add_account(equity.clone());

        // Seed the ledger with the initial BTC holding.
        let mut tx1 = Transaction::new("initial".into());
        tx1.add_entry(&asset,  initial, AccountSide::Debit ).map_err(fail)?;
        tx1.add_entry(&equity, initial, AccountSide::Credit).map_err(fail)?;
        let vtx1: ValidatedTransaction = ledger.prepare_transaction(tx1).map_err(fail)?;
        ledger.post_transaction(vtx1);

        let mut market = QuoteBoard::new();
        market.add_rate(ExchangeRate::new(btc, usd, rate).map_err(fail)?);

        let nav0 = ledger.net_asset_value(&market, &usd).map_err(fail)?;

        // Add more BTC.
        let mut tx2 = Transaction::new("addition".into());
        tx2.add_entry(&asset,  addition, AccountSide::Debit ).map_err(fail)?;
        tx2.add_entry(&equity, addition, AccountSide::Credit).map_err(fail)?;
        let vtx2: ValidatedTransaction = ledger.prepare_transaction(tx2).map_err(fail)?;
        ledger.post_transaction(vtx2);

        let nav1 = ledger.net_asset_value(&market, &usd).map_err(fail)?;

        prop_assert!(
            nav1.amount >= nav0.amount,
            "nav1 ({}) should be >= nav0 ({})",
            nav1.amount,
            nav0.amount
        );
    }
}
