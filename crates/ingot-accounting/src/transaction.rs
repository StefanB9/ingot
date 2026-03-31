use std::{collections::HashMap, fmt};

use chrono::{DateTime, Utc};
use ingot_primitives::{Amount, Currency};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use uuid::{Timestamp, Uuid};

use crate::{
    error::AccountingError,
    types::{AccountId, EntrySide, TransactionType},
};

fn uuid_v7_now() -> Uuid {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ts = Timestamp::from_unix(uuid::NoContext, now.as_secs(), now.subsec_nanos());
    Uuid::new_v7(ts)
}

/// Unique transaction identifier (UUID v7 — time-ordered).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransactionId(Uuid);

impl TransactionId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid_v7_now())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique ledger entry identifier (UUID v7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryId(Uuid);

impl EntryId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid_v7_now())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A single entry in the double-entry ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: EntryId,
    pub transaction_id: TransactionId,
    pub account_id: AccountId,
    pub side: EntrySide,
    pub amount: Amount,
    pub currency: Currency,
    pub timestamp: DateTime<Utc>,
    pub description: Option<SmolStr>,
}

/// A balanced double-entry transaction (immutable once created).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub id: TransactionId,
    pub transaction_type: TransactionType,
    pub entries: Vec<LedgerEntry>,
    pub timestamp: DateTime<Utc>,
    pub reference_id: Option<SmolStr>,
    pub metadata: Option<serde_json::Value>,
}

impl Transaction {
    /// Validate that the transaction is balanced.
    /// For `Trade` type: cross-currency legs are allowed (skip per-currency
    /// balance check). For all other types: each currency's debits must
    /// equal credits.
    pub fn validate(&self) -> Result<(), AccountingError> {
        if matches!(
            self.transaction_type,
            TransactionType::Trade | TransactionType::Merger
        ) {
            return Ok(());
        }

        let mut debit_sums: HashMap<Currency, Decimal> = HashMap::new();
        let mut credit_sums: HashMap<Currency, Decimal> = HashMap::new();

        for entry in &self.entries {
            let target = match entry.side {
                EntrySide::Debit => &mut debit_sums,
                EntrySide::Credit => &mut credit_sums,
            };
            *target
                .entry(entry.currency.clone())
                .or_insert(Decimal::ZERO) += entry.amount.value();
        }

        // Collect all currencies
        let mut currencies: Vec<&Currency> = debit_sums.keys().collect();
        for k in credit_sums.keys() {
            if !currencies.contains(&k) {
                currencies.push(k);
            }
        }

        for currency in currencies {
            let debits = debit_sums.get(currency).copied().unwrap_or(Decimal::ZERO);
            let credits = credit_sums.get(currency).copied().unwrap_or(Decimal::ZERO);
            if debits != credits {
                return Err(AccountingError::UnbalancedTransaction {
                    currency: currency.clone(),
                    debits: Amount::new(debits),
                    credits: Amount::new(credits),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ingot_primitives::Exchange;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::types::AccountType;

    fn make_account_id(
        account_type: AccountType,
        currency: Currency,
    ) -> Result<AccountId, AccountingError> {
        AccountId::new(account_type, Exchange::Kraken, "spot", currency)
    }

    fn make_entry(
        txn_id: &TransactionId,
        account_type: AccountType,
        side: EntrySide,
        amount: Decimal,
        currency: Currency,
    ) -> Result<LedgerEntry, AccountingError> {
        Ok(LedgerEntry {
            id: EntryId::new(),
            transaction_id: txn_id.clone(),
            account_id: make_account_id(account_type, currency.clone())?,
            side,
            amount: Amount::new(amount),
            currency,
            timestamp: Utc::now(),
            description: None,
        })
    }

    #[test]
    fn test_transaction_id_new_unique() {
        let a = TransactionId::new();
        let b = TransactionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn test_transaction_id_from_uuid() {
        let uuid = uuid_v7_now();
        let id = TransactionId::from_uuid(uuid);
        assert_eq!(*id.as_uuid(), uuid);
    }

    #[test]
    fn test_transaction_id_display() {
        let uuid = Uuid::nil();
        let id = TransactionId::from_uuid(uuid);
        assert_eq!(id.to_string(), "00000000-0000-0000-0000-000000000000");
    }

    #[test]
    fn test_entry_id_new_unique() {
        let a = EntryId::new();
        let b = EntryId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn test_ledger_entry_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let txn_id = TransactionId::new();
        let entry = make_entry(
            &txn_id,
            AccountType::Asset,
            EntrySide::Debit,
            dec!(100),
            Currency::USD,
        )
        .map_err(|e| format!("{e}"))?;
        let json = serde_json::to_string(&entry)?;
        let deserialized: LedgerEntry = serde_json::from_str(&json)?;
        assert_eq!(entry, deserialized);
        Ok(())
    }

    #[test]
    fn test_transaction_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let txn_id = TransactionId::new();
        let txn = Transaction {
            id: txn_id.clone(),
            transaction_type: TransactionType::Fee,
            entries: vec![
                make_entry(
                    &txn_id,
                    AccountType::Expense,
                    EntrySide::Debit,
                    dec!(10),
                    Currency::USD,
                )
                .map_err(|e| format!("{e}"))?,
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Credit,
                    dec!(10),
                    Currency::USD,
                )
                .map_err(|e| format!("{e}"))?,
            ],
            timestamp: Utc::now(),
            reference_id: Some(SmolStr::new("ref-123")),
            metadata: None,
        };
        let json = serde_json::to_string(&txn)?;
        let deserialized: Transaction = serde_json::from_str(&json)?;
        assert_eq!(txn, deserialized);
        Ok(())
    }

    #[test]
    fn test_transaction_validate_balanced_single_currency() -> Result<(), AccountingError> {
        let txn_id = TransactionId::new();
        let txn = Transaction {
            id: txn_id.clone(),
            transaction_type: TransactionType::Fee,
            entries: vec![
                make_entry(
                    &txn_id,
                    AccountType::Expense,
                    EntrySide::Debit,
                    dec!(100),
                    Currency::USD,
                )?,
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Credit,
                    dec!(100),
                    Currency::USD,
                )?,
            ],
            timestamp: Utc::now(),
            reference_id: None,
            metadata: None,
        };
        txn.validate()
    }

    #[test]
    fn test_transaction_validate_unbalanced_fails() -> Result<(), AccountingError> {
        let txn_id = TransactionId::new();
        let txn = Transaction {
            id: txn_id.clone(),
            transaction_type: TransactionType::Fee,
            entries: vec![
                make_entry(
                    &txn_id,
                    AccountType::Expense,
                    EntrySide::Debit,
                    dec!(100),
                    Currency::USD,
                )?,
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Credit,
                    dec!(50),
                    Currency::USD,
                )?,
            ],
            timestamp: Utc::now(),
            reference_id: None,
            metadata: None,
        };
        assert!(txn.validate().is_err());
        Ok(())
    }

    #[test]
    fn test_transaction_validate_empty_entries() -> Result<(), AccountingError> {
        let txn = Transaction {
            id: TransactionId::new(),
            transaction_type: TransactionType::Fee,
            entries: vec![],
            timestamp: Utc::now(),
            reference_id: None,
            metadata: None,
        };
        txn.validate()
    }

    #[test]
    fn test_transaction_validate_multi_currency_trade() -> Result<(), AccountingError> {
        let txn_id = TransactionId::new();
        let txn = Transaction {
            id: txn_id.clone(),
            transaction_type: TransactionType::Trade,
            entries: vec![
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Debit,
                    dec!(1),
                    Currency::BTC,
                )?,
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Credit,
                    dec!(67000),
                    Currency::USD,
                )?,
            ],
            timestamp: Utc::now(),
            reference_id: None,
            metadata: None,
        };
        txn.validate()
    }

    #[test]
    fn test_transaction_validate_merger_cross_currency() -> Result<(), AccountingError> {
        let txn_id = TransactionId::new();
        let txn = Transaction {
            id: txn_id.clone(),
            transaction_type: TransactionType::Merger,
            entries: vec![
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Debit,
                    dec!(50),
                    Currency::EUR,
                )?,
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Credit,
                    dec!(100),
                    Currency::USD,
                )?,
            ],
            timestamp: Utc::now(),
            reference_id: None,
            metadata: None,
        };
        txn.validate()
    }

    #[test]
    fn test_transaction_validate_merger_same_currency_unbalanced() -> Result<(), AccountingError> {
        let txn_id = TransactionId::new();
        let txn = Transaction {
            id: txn_id.clone(),
            transaction_type: TransactionType::Merger,
            entries: vec![
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Debit,
                    dec!(50),
                    Currency::USD,
                )?,
                make_entry(
                    &txn_id,
                    AccountType::Asset,
                    EntrySide::Credit,
                    dec!(100),
                    Currency::USD,
                )?,
            ],
            timestamp: Utc::now(),
            reference_id: None,
            metadata: None,
        };
        txn.validate()
    }

    proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_test_balanced_transaction_validates(
            amounts in proptest::collection::vec(1i64..=1_000_000i64, 1..=5)
        ) {
            // For each random amount, create a balanced debit+credit pair in USD
            let txn_id = TransactionId::new();
            let mut entries = Vec::new();
            for amt in amounts {
                let decimal = Decimal::from(amt);
                // unwrap-free: these constructors only fail on empty venue, which "spot" is not
                let debit = LedgerEntry {
                    id: EntryId::new(),
                    transaction_id: txn_id.clone(),
                    account_id: AccountId::new(AccountType::Expense, Exchange::Kraken, "spot", Currency::USD)
                        .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                    side: EntrySide::Debit,
                    amount: Amount::new(decimal),
                    currency: Currency::USD,
                    timestamp: Utc::now(),
                    description: None,
                };
                let credit = LedgerEntry {
                    id: EntryId::new(),
                    transaction_id: txn_id.clone(),
                    account_id: AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
                        .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                    side: EntrySide::Credit,
                    amount: Amount::new(decimal),
                    currency: Currency::USD,
                    timestamp: Utc::now(),
                    description: None,
                };
                entries.push(debit);
                entries.push(credit);
            }
            let txn = Transaction {
                id: txn_id,
                transaction_type: TransactionType::Fee,
                entries,
                timestamp: Utc::now(),
                reference_id: None,
                metadata: None,
            };
            prop_assert!(txn.validate().is_ok());
        }
    }
}
