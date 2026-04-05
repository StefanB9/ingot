use ingot_accounting::{TransactionType, post_dividend};
use ingot_primitives::{Amount, Currency, Exchange, Symbol};
use rust_decimal_macros::dec;

// ── Test 9: corporate action dividend through accounting ──

#[test]
fn test_corporate_action_dividend_through_accounting() -> anyhow::Result<()> {
    use anyhow::Context;
    use ingot_accounting::types::EntrySide;

    let symbol = Symbol::new("AAPL").context("invalid symbol")?;
    let amount = Amount::new(dec!(125.50));
    let timestamp = chrono::Utc::now();

    let txn = post_dividend(
        Exchange::IBKR,
        "NASDAQ",
        &Currency::USD,
        &symbol,
        amount,
        timestamp,
    )
    .context("post_dividend failed")?;

    // Validate balanced
    txn.validate().context("transaction not balanced")?;

    // Should be a Dividend transaction
    assert_eq!(txn.transaction_type, TransactionType::Dividend);

    // Should have exactly 2 entries (debit + credit)
    assert_eq!(txn.entries.len(), 2);

    // One debit, one credit
    let debit_count = txn
        .entries
        .iter()
        .filter(|e| e.side == EntrySide::Debit)
        .count();
    let credit_count = txn
        .entries
        .iter()
        .filter(|e| e.side == EntrySide::Credit)
        .count();
    assert_eq!(debit_count, 1);
    assert_eq!(credit_count, 1);

    // Both entries should be in USD with the dividend amount
    for entry in &txn.entries {
        assert_eq!(entry.currency, Currency::USD);
        assert_eq!(entry.amount, amount);
    }

    Ok(())
}
