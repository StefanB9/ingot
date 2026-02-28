use ingot_primitives::Currency;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountType {
    Asset,
    Liability,
    Equity,
    Revenue,
    Expense,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountSide {
    Debit,
    Credit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    #[allow(clippy::struct_field_names)]
    pub account_type: AccountType,
    pub currency: Currency,
}

impl Account {
    pub fn new(name: String, account_type: AccountType, currency: Currency) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            account_type,
            currency,
        }
    }

    pub fn normal_balance(&self) -> AccountSide {
        match self.account_type {
            AccountType::Asset | AccountType::Expense => AccountSide::Debit,
            AccountType::Liability | AccountType::Equity | AccountType::Revenue => {
                AccountSide::Credit
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use anyhow::{Context, Result};

    use super::*;

    #[test]
    fn test_account_type_equality_and_cloning() {
        let t1 = AccountType::Asset;
        let t2 = t1; // Copy
        #[allow(clippy::clone_on_copy)]
        let t3 = t1.clone(); // Clone

        assert_eq!(t1, t2);
        assert_eq!(t1, t3);
        assert_ne!(t1, AccountType::Liability);
    }

    #[test]
    fn test_account_type_serialization() -> Result<()> {
        let t = AccountType::Revenue;
        let serialized = serde_json::to_string(&t).context("Failed to serialize AccountType")?;
        assert_eq!(serialized, "\"Revenue\"");

        let deserialized: AccountType =
            serde_json::from_str(&serialized).context("Failed to deserialize")?;
        assert_eq!(t, deserialized);
        Ok(())
    }

    #[test]
    fn test_account_side_equality() {
        assert_eq!(AccountSide::Debit, AccountSide::Debit);
        assert_ne!(AccountSide::Debit, AccountSide::Credit);
    }

    #[test]
    fn test_account_side_serialization() -> Result<()> {
        let side = AccountSide::Credit;
        let serialized = serde_json::to_string(&side).context("Failed to serialize")?;
        assert_eq!(serialized, "\"Credit\"");
        Ok(())
    }

    #[test]
    fn test_account_creation() {
        let btc = Currency::btc();
        let name = "Cold Storage";
        let account = Account::new(name.into(), AccountType::Asset, btc);

        assert_eq!(account.name, "Cold Storage");
        assert_eq!(account.account_type, AccountType::Asset);
        assert_eq!(account.currency, btc);
        // Ensure UUID is populated and not nil
        assert!(!account.id.is_nil());
    }

    #[test]
    fn test_account_ids_are_unique() {
        let btc = Currency::btc();
        let a1 = Account::new("Test".into(), AccountType::Asset, btc);
        let a2 = Account::new("Test".into(), AccountType::Asset, btc);

        // Even with identical names/types, IDs must differ
        assert_ne!(a1.id, a2.id);
        assert_ne!(a1, a2);
    }

    #[test]
    fn test_normal_balance_logic() {
        let usd = Currency::usd();

        let asset = Account::new("Cash".into(), AccountType::Asset, usd);
        assert_eq!(asset.normal_balance(), AccountSide::Debit);

        let expense = Account::new("Fees".into(), AccountType::Expense, usd);
        assert_eq!(expense.normal_balance(), AccountSide::Debit);

        let liability = Account::new("Loan".into(), AccountType::Liability, usd);
        assert_eq!(liability.normal_balance(), AccountSide::Credit);

        let equity = Account::new("Capital".into(), AccountType::Equity, usd);
        assert_eq!(equity.normal_balance(), AccountSide::Credit);

        let revenue = Account::new("Sales".into(), AccountType::Revenue, usd);
        assert_eq!(revenue.normal_balance(), AccountSide::Credit);
    }

    #[test]
    fn test_account_serialization() -> Result<()> {
        let acc = Account::new(
            "Test Account".into(),
            AccountType::Liability,
            Currency::usd(),
        );

        let serialized = serde_json::to_string(&acc).context("Failed to serialize Account")?;

        // Check structural integrity of JSON
        assert!(serialized.contains("id"));
        assert!(serialized.contains("Test Account"));
        assert!(serialized.contains("Liability"));
        assert!(serialized.contains("USD"));

        let deserialized: Account =
            serde_json::from_str(&serialized).context("Failed to deserialize")?;
        assert_eq!(acc, deserialized);
        assert_eq!(acc.id, deserialized.id);
        Ok(())
    }

    #[test]
    fn test_account_cloning() {
        let a1 = Account::new("Original".into(), AccountType::Asset, Currency::btc());
        let a2 = a1.clone();

        assert_eq!(a1, a2);
        assert_eq!(a1.id, a2.id); // Clones share the same ID
    }
}
