use chrono::{DateTime, Utc};
use ingot_primitives::{Amount, Currency, CurrencyCode};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use tracing::instrument;
use uuid::Uuid;

use crate::accounting::{Account, AccountSide, AccountingError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub account_id: Uuid,
    pub amount: Amount,
    pub side: AccountSide,
    pub currency: Currency,
}

/// Proof that a [`Transaction`] has passed full validation: per-currency
/// double-entry balance and account existence. Produced by
/// [`Ledger::prepare_transaction`]; consumed by [`Ledger::post_transaction`],
/// which is infallible.
///
/// The inner [`Transaction`] is only accessible to the `accounting` module to
/// prevent bypassing the validation gate.
pub struct ValidatedTransaction(pub(crate) Transaction);

impl ValidatedTransaction {
    pub fn id(&self) -> Uuid {
        self.0.id
    }

    pub fn entry_count(&self) -> usize {
        self.0.entries.len()
    }
}

// A double-entry transaction always has 2 entries (simple) or 4–6 (multi-leg).
// Stack capacity of 4 covers the common case with zero heap allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: Uuid,
    pub date: DateTime<Utc>,
    pub description: String,
    pub entries: SmallVec<Entry, 4>,
}

impl Transaction {
    pub fn new(description: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            date: Utc::now(),
            description,
            entries: SmallVec::new(),
        }
    }

    pub fn add_entry(
        &mut self,
        account: &Account,
        amount: Amount,
        side: AccountSide,
    ) -> Result<(), AccountingError> {
        if amount.is_sign_negative() {
            return Err(AccountingError::InvalidAmount(
                "Transaction amounts must be positive",
            ));
        }

        self.entries.push(Entry {
            account_id: account.id,
            amount,
            side,
            currency: account.currency,
        });
        Ok(())
    }

    #[instrument(skip_all, level = "debug", fields(entry_count = self.entries.len()))]
    pub fn validate(&self) -> Result<(), AccountingError> {
        // Stack-allocated linear scan: (code, debit_total, credit_total).
        // O(n²) in the number of distinct currencies, but n ≤ 4 in every
        // realistic transaction, making this faster than a HashMap due to
        // zero allocation and cache locality.
        let mut totals: SmallVec<(CurrencyCode, Amount, Amount), 4> = SmallVec::new();

        for entry in &self.entries {
            let code = entry.currency.code;
            if let Some(row) = totals.iter_mut().find(|(c, _, _)| *c == code) {
                match entry.side {
                    AccountSide::Debit => row.1 += entry.amount,
                    AccountSide::Credit => row.2 += entry.amount,
                }
            } else {
                let (debit, credit) = match entry.side {
                    AccountSide::Debit => (entry.amount, Amount::ZERO),
                    AccountSide::Credit => (Amount::ZERO, entry.amount),
                };
                totals.push((code, debit, credit));
            }
        }

        for (code, debit, credit) in totals {
            if debit != credit {
                return Err(AccountingError::MultiCurrencyTransactionUnbalanced(
                    code.to_string(),
                    debit,
                    credit,
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result};
    use assert_matches::assert_matches;
    use rust_decimal::dec;

    use super::*;
    use crate::accounting::AccountType;

    fn make_account(name: &str, currency: Currency) -> Account {
        Account::new(name.into(), AccountType::Asset, currency)
    }

    #[test]
    fn test_entry_traits() {
        let acc = make_account("Test", Currency::usd());
        let entry = Entry {
            account_id: acc.id,
            amount: Amount::from(dec!(100)),
            side: AccountSide::Debit,
            currency: Currency::usd(),
        };

        let e2 = entry.clone();
        assert_eq!(entry, e2);

        let debug_str = format!("{entry:?}");
        assert!(debug_str.contains("Debit"));
        assert!(debug_str.contains("100"));
    }

    #[test]
    fn test_transaction_new() {
        let tx = Transaction::new("Payment".into());

        assert_eq!(tx.description, "Payment");
        assert!(tx.entries.is_empty());
        assert!(!tx.id.is_nil());

        let now = Utc::now();
        let diff = now - tx.date;
        assert!(diff.num_seconds() < 5);
    }

    #[test]
    fn test_add_entry_success() -> Result<()> {
        let mut tx = Transaction::new("Test".into());
        let usd = Currency::usd();
        let acc = make_account("Wallet", usd);

        tx.add_entry(&acc, Amount::from(dec!(50.0)), AccountSide::Debit)?;

        assert_eq!(tx.entries.len(), 1);
        let entry = &tx.entries[0];
        assert_eq!(entry.account_id, acc.id);
        assert_eq!(entry.amount, Amount::from(dec!(50.0)));
        assert_eq!(entry.side, AccountSide::Debit);
        assert_eq!(entry.currency, usd);

        Ok(())
    }

    #[test]
    fn test_add_entry_rejects_negative() {
        let mut tx = Transaction::new("Test".into());
        let acc = make_account("Wallet", Currency::usd());

        let err = tx.add_entry(&acc, Amount::from(dec!(-10.0)), AccountSide::Credit);

        assert!(matches!(err, Err(AccountingError::InvalidAmount(_))));
    }

    #[test]
    fn test_add_entry_allows_zero() {
        let mut tx = Transaction::new("Zero".into());
        let acc = make_account("Wallet", Currency::usd());

        assert!(tx.add_entry(&acc, Amount::ZERO, AccountSide::Debit).is_ok());
    }

    #[test]
    fn test_validate_empty_transaction() {
        let tx = Transaction::new("Empty".into());
        assert!(tx.validate().is_ok());
    }

    #[test]
    fn test_validate_simple_balanced() -> Result<()> {
        let mut tx = Transaction::new("Simple".into());
        let acc1 = make_account("A", Currency::usd());
        let acc2 = make_account("B", Currency::usd());

        tx.add_entry(&acc1, Amount::from(dec!(10)), AccountSide::Debit)?;
        tx.add_entry(&acc2, Amount::from(dec!(10)), AccountSide::Credit)?;

        assert!(tx.validate().is_ok());
        Ok(())
    }

    #[test]
    fn test_validate_simple_unbalanced() -> Result<()> {
        let mut tx = Transaction::new("Unbalanced".into());
        let acc1 = make_account("A", Currency::usd());

        tx.add_entry(&acc1, Amount::from(dec!(10)), AccountSide::Debit)?;

        assert_matches!(tx.validate(), Err(AccountingError::MultiCurrencyTransactionUnbalanced(code, deb, cred)) => {
            assert_eq!(code, "USD");
            assert_eq!(deb, Amount::from(dec!(10)));
            assert_eq!(cred, Amount::ZERO);
        });
        Ok(())
    }

    #[test]
    fn test_validate_multi_currency_success() -> Result<()> {
        let mut tx = Transaction::new("Multi".into());
        let usd = Currency::usd();
        let btc = Currency::btc();

        let u1 = make_account("U1", usd);
        let u2 = make_account("U2", usd);
        let b1 = make_account("B1", btc);
        let b2 = make_account("B2", btc);

        tx.add_entry(&u1, Amount::from(dec!(100)), AccountSide::Debit)?;
        tx.add_entry(&u2, Amount::from(dec!(100)), AccountSide::Credit)?;
        tx.add_entry(&b1, Amount::from(dec!(5)), AccountSide::Debit)?;
        tx.add_entry(&b2, Amount::from(dec!(5)), AccountSide::Credit)?;

        assert!(tx.validate().is_ok());
        Ok(())
    }

    #[test]
    fn test_validate_mixed_currency_failure() -> Result<()> {
        let mut tx = Transaction::new("Mixed".into());
        let acc_usd = make_account("USD", Currency::usd());
        let acc_btc = make_account("BTC", Currency::btc());

        tx.add_entry(&acc_usd, Amount::from(dec!(100)), AccountSide::Debit)?;
        tx.add_entry(&acc_btc, Amount::from(dec!(100)), AccountSide::Credit)?;

        let result = tx.validate();
        assert!(result.is_err());

        assert!(
            matches!(
                result,
                Err(AccountingError::MultiCurrencyTransactionUnbalanced(_, _, _))
            ),
            "Expected MultiCurrencyTransactionUnbalanced error, but got: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_transaction_serialization() -> Result<()> {
        let mut tx = Transaction::new("JSON Test".into());
        let acc = make_account("A", Currency::usd());
        tx.add_entry(&acc, Amount::from(dec!(99.99)), AccountSide::Debit)?;

        let serialized = serde_json::to_string(&tx).context("Serialize failed")?;

        assert!(serialized.contains("JSON Test"));
        assert!(serialized.contains("99.99"));
        assert!(serialized.contains("USD"));
        assert!(serialized.contains("entries"));
        assert!(serialized.contains("date"));

        let deserialized: Transaction =
            serde_json::from_str(&serialized).context("Deserialize failed")?;
        assert_eq!(tx.id, deserialized.id);
        assert_eq!(tx.description, deserialized.description);
        assert_eq!(tx.entries.len(), deserialized.entries.len());
        Ok(())
    }
}
