use std::collections::HashMap;

use ingot_primitives::{Amount, Currency};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{
    transaction::LedgerEntry,
    types::{AccountId, EntrySide},
};

/// Aggregated balance for an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyBalance {
    pub account_id: AccountId,
    pub currency: Currency,
    pub balance: Amount,
}

/// Result of a trial balance check across all ledger entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialBalance {
    pub total_debits: Amount,
    pub total_credits: Amount,
}

impl TrialBalance {
    /// Returns true if total debits equal total credits.
    pub fn is_balanced(&self) -> bool {
        self.total_debits == self.total_credits
    }
}

/// Aggregate ledger entries into per-account balances (pure, in-memory).
///
/// Groups entries by `AccountId`, sums debit amounts as positive
/// and credit amounts as negative. Returns sorted by account string.
pub fn aggregate_balances(entries: &[LedgerEntry]) -> Vec<CurrencyBalance> {
    let mut acc: HashMap<AccountId, (Currency, Decimal)> = HashMap::new();

    for entry in entries {
        let (_, balance) = acc
            .entry(entry.account_id.clone())
            .or_insert_with(|| (entry.currency.clone(), Decimal::ZERO));
        match entry.side {
            EntrySide::Debit => *balance += entry.amount.value(),
            EntrySide::Credit => *balance -= entry.amount.value(),
        }
    }

    let mut balances: Vec<CurrencyBalance> = acc
        .into_iter()
        .map(|(account_id, (currency, balance))| CurrencyBalance {
            account_id,
            currency,
            balance: Amount::new(balance),
        })
        .collect();

    balances.sort_by_key(|b| b.account_id.to_string());
    balances
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_primitives::Exchange;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::{
        transaction::{EntryId, TransactionId},
        types::AccountType,
    };

    fn make_entry(
        account_id: AccountId,
        side: EntrySide,
        amount: Decimal,
        currency: Currency,
    ) -> LedgerEntry {
        LedgerEntry {
            id: EntryId::new(),
            transaction_id: TransactionId::new(),
            account_id,
            side,
            amount: Amount::new(amount),
            currency,
            timestamp: Utc::now(),
            description: None,
        }
    }

    fn account(
        account_type: AccountType,
        venue: &str,
        currency: Currency,
    ) -> Result<AccountId, Box<dyn std::error::Error>> {
        AccountId::new(account_type, Exchange::Kraken, venue, currency)
            .map_err(|e| format!("{e}").into())
    }

    // ── TrialBalance ──────────────────────────────────────────────────

    #[test]
    fn test_trial_balance_is_balanced() {
        let tb = TrialBalance {
            total_debits: Amount::new(dec!(100)),
            total_credits: Amount::new(dec!(100)),
        };
        assert!(tb.is_balanced());
    }

    #[test]
    fn test_trial_balance_not_balanced() {
        let tb = TrialBalance {
            total_debits: Amount::new(dec!(100)),
            total_credits: Amount::new(dec!(99)),
        };
        assert!(!tb.is_balanced());
    }

    #[test]
    fn test_trial_balance_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let tb = TrialBalance {
            total_debits: Amount::new(dec!(500)),
            total_credits: Amount::new(dec!(500)),
        };
        let json = serde_json::to_string(&tb)?;
        let deserialized: TrialBalance = serde_json::from_str(&json)?;
        assert_eq!(tb, deserialized);
        Ok(())
    }

    // ── aggregate_balances ────────────────────────────────────────────

    #[test]
    fn test_aggregate_balances_single_currency() -> Result<(), Box<dyn std::error::Error>> {
        let acc = account(AccountType::Asset, "spot", Currency::USD)?;
        let entries = vec![
            make_entry(acc.clone(), EntrySide::Debit, dec!(100), Currency::USD),
            make_entry(acc.clone(), EntrySide::Debit, dec!(50), Currency::USD),
            make_entry(acc.clone(), EntrySide::Credit, dec!(30), Currency::USD),
            make_entry(acc.clone(), EntrySide::Credit, dec!(20), Currency::USD),
        ];

        let balances = aggregate_balances(&entries);
        assert_eq!(balances.len(), 1);
        // Net = 100 + 50 - 30 - 20 = 100
        assert_eq!(balances[0].balance, Amount::new(dec!(100)));
        assert_eq!(balances[0].currency, Currency::USD);

        Ok(())
    }

    #[test]
    fn test_aggregate_balances_multi_account() -> Result<(), Box<dyn std::error::Error>> {
        let btc_asset = account(AccountType::Asset, "spot", Currency::BTC)?;
        let usd_asset = account(AccountType::Asset, "spot", Currency::USD)?;
        let usd_expense = account(AccountType::Expense, "fee", Currency::USD)?;

        let entries = vec![
            make_entry(btc_asset.clone(), EntrySide::Debit, dec!(1), Currency::BTC),
            make_entry(
                usd_asset.clone(),
                EntrySide::Credit,
                dec!(67000),
                Currency::USD,
            ),
            make_entry(
                usd_expense.clone(),
                EntrySide::Debit,
                dec!(17.42),
                Currency::USD,
            ),
            make_entry(
                usd_asset.clone(),
                EntrySide::Credit,
                dec!(17.42),
                Currency::USD,
            ),
        ];

        let balances = aggregate_balances(&entries);
        assert_eq!(balances.len(), 3);

        // Sorted by account_id.to_string():
        // "asset:kraken:spot:BTC" → +1
        // "asset:kraken:spot:USD" → -(67000 + 17.42) = -67017.42
        // "expense:kraken:fee:USD" → +17.42
        let btc = balances.iter().find(|b| b.currency == Currency::BTC);
        assert_eq!(btc.map(|b| b.balance), Some(Amount::new(dec!(1))));

        let usd_a = balances.iter().find(|b| {
            b.account_id.account_type == AccountType::Asset && b.currency == Currency::USD
        });
        assert_eq!(usd_a.map(|b| b.balance), Some(Amount::new(dec!(-67017.42))));

        let usd_e = balances
            .iter()
            .find(|b| b.account_id.account_type == AccountType::Expense);
        assert_eq!(usd_e.map(|b| b.balance), Some(Amount::new(dec!(17.42))));

        Ok(())
    }

    #[test]
    fn test_aggregate_balances_empty() {
        let balances = aggregate_balances(&[]);
        assert!(balances.is_empty());
    }

    // ── CurrencyBalance (existing tests) ──────────────────────────────

    #[test]
    fn test_currency_balance_construction() -> Result<(), Box<dyn std::error::Error>> {
        let account_id =
            AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
                .map_err(|e| format!("{e}"))?;
        let balance = CurrencyBalance {
            account_id: account_id.clone(),
            currency: Currency::USD,
            balance: Amount::new(dec!(1000)),
        };
        assert_eq!(balance.account_id, account_id);
        assert_eq!(balance.currency, Currency::USD);
        assert_eq!(balance.balance, Amount::new(dec!(1000)));
        Ok(())
    }

    #[test]
    fn test_currency_balance_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let account_id =
            AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
                .map_err(|e| format!("{e}"))?;
        let balance = CurrencyBalance {
            account_id,
            currency: Currency::USD,
            balance: Amount::new(dec!(1000)),
        };
        let json = serde_json::to_string(&balance)?;
        let deserialized: CurrencyBalance = serde_json::from_str(&json)?;
        assert_eq!(balance, deserialized);
        Ok(())
    }

    // ── Proptest ──────────────────────────────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_test_aggregate_balances_matches_manual(
            debits in proptest::collection::vec(1i64..=100_000i64, 1..=10),
            credits in proptest::collection::vec(1i64..=100_000i64, 1..=10),
        ) {
            // All entries to same account → sum debits - sum credits
            let acc = AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;

            let txn_id = TransactionId::new();
            let mut entries = Vec::new();

            let mut expected = Decimal::ZERO;
            for d in &debits {
                let val = Decimal::from(*d);
                expected += val;
                entries.push(LedgerEntry {
                    id: EntryId::new(),
                    transaction_id: txn_id.clone(),
                    account_id: acc.clone(),
                    side: EntrySide::Debit,
                    amount: Amount::new(val),
                    currency: Currency::USD,
                    timestamp: Utc::now(),
                    description: None,
                });
            }
            for c in &credits {
                let val = Decimal::from(*c);
                expected -= val;
                entries.push(LedgerEntry {
                    id: EntryId::new(),
                    transaction_id: txn_id.clone(),
                    account_id: acc.clone(),
                    side: EntrySide::Credit,
                    amount: Amount::new(val),
                    currency: Currency::USD,
                    timestamp: Utc::now(),
                    description: None,
                });
            }

            let balances = aggregate_balances(&entries);
            prop_assert_eq!(balances.len(), 1);
            prop_assert_eq!(balances[0].balance, Amount::new(expected));
        }
    }
}
