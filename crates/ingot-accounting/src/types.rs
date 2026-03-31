use std::fmt;

use ingot_primitives::{Currency, Exchange};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::error::AccountingError;

/// Category of account in the chart of accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccountType {
    /// Cash, crypto holdings, position value. Normal balance = Debit.
    Asset,
    /// Short margin obligations. Normal balance = Credit.
    Liability,
    /// Realized trading gains. Normal balance = Credit.
    Revenue,
    /// Trading fees, funding costs, interest paid. Normal balance = Debit.
    Expense,
}

impl fmt::Display for AccountType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asset => f.write_str("asset"),
            Self::Liability => f.write_str("liability"),
            Self::Revenue => f.write_str("revenue"),
            Self::Expense => f.write_str("expense"),
        }
    }
}

/// Side of a ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntrySide {
    Debit,
    Credit,
}

impl fmt::Display for EntrySide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Debit => f.write_str("debit"),
            Self::Credit => f.write_str("credit"),
        }
    }
}

/// Type of financial transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransactionType {
    Trade,
    Fee,
    FundingRate,
    Interest,
    Transfer,
    Adjustment,
    Dividend,
    StockSplit,
    BondCoupon,
    Merger,
    Spinoff,
}

impl fmt::Display for TransactionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Trade => f.write_str("trade"),
            Self::Fee => f.write_str("fee"),
            Self::FundingRate => f.write_str("funding_rate"),
            Self::Interest => f.write_str("interest"),
            Self::Transfer => f.write_str("transfer"),
            Self::Adjustment => f.write_str("adjustment"),
            Self::Dividend => f.write_str("dividend"),
            Self::StockSplit => f.write_str("stock_split"),
            Self::BondCoupon => f.write_str("bond_coupon"),
            Self::Merger => f.write_str("merger"),
            Self::Spinoff => f.write_str("spinoff"),
        }
    }
}

/// Structured account identifier.
/// Display format: `{account_type}:{exchange}:{venue}:{currency}`
/// Example: "asset:kraken:spot:USD"
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountId {
    pub account_type: AccountType,
    pub exchange: Exchange,
    pub venue: SmolStr,
    pub currency: Currency,
}

impl AccountId {
    pub fn new(
        account_type: AccountType,
        exchange: Exchange,
        venue: &str,
        currency: Currency,
    ) -> Result<Self, AccountingError> {
        if venue.is_empty() {
            return Err(AccountingError::InvalidAccount {
                reason: "venue cannot be empty".into(),
            });
        }
        Ok(Self {
            account_type,
            exchange,
            venue: SmolStr::new(venue),
            currency,
        })
    }

    /// Returns the normal balance side for this account type.
    pub fn normal_side(&self) -> EntrySide {
        match self.account_type {
            AccountType::Asset | AccountType::Expense => EntrySide::Debit,
            AccountType::Liability | AccountType::Revenue => EntrySide::Credit,
        }
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}",
            self.account_type,
            self.exchange.as_str_lowercase(),
            self.venue,
            self.currency
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_account_type_display() {
        assert_eq!(AccountType::Asset.to_string(), "asset");
        assert_eq!(AccountType::Liability.to_string(), "liability");
        assert_eq!(AccountType::Revenue.to_string(), "revenue");
        assert_eq!(AccountType::Expense.to_string(), "expense");
    }

    #[test]
    fn test_entry_side_display() {
        assert_eq!(EntrySide::Debit.to_string(), "debit");
        assert_eq!(EntrySide::Credit.to_string(), "credit");
    }

    #[test]
    fn test_transaction_type_display() {
        assert_eq!(TransactionType::Trade.to_string(), "trade");
        assert_eq!(TransactionType::Fee.to_string(), "fee");
        assert_eq!(TransactionType::FundingRate.to_string(), "funding_rate");
        assert_eq!(TransactionType::Interest.to_string(), "interest");
        assert_eq!(TransactionType::Transfer.to_string(), "transfer");
        assert_eq!(TransactionType::Adjustment.to_string(), "adjustment");
    }

    #[test]
    fn test_account_id_new_valid() -> Result<(), AccountingError> {
        let id = AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)?;
        assert_eq!(id.account_type, AccountType::Asset);
        assert_eq!(id.exchange, Exchange::Kraken);
        assert_eq!(id.venue.as_str(), "spot");
        assert_eq!(id.currency, Currency::USD);
        Ok(())
    }

    #[test]
    fn test_account_id_new_empty_venue_rejected() {
        let result = AccountId::new(AccountType::Asset, Exchange::Kraken, "", Currency::USD);
        assert!(result.is_err());
    }

    #[test]
    fn test_account_id_display() -> Result<(), AccountingError> {
        let id = AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)?;
        assert_eq!(id.to_string(), "asset:kraken:spot:USD");
        Ok(())
    }

    #[test]
    fn test_account_id_normal_side() -> Result<(), AccountingError> {
        let asset = AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)?;
        assert_eq!(asset.normal_side(), EntrySide::Debit);

        let expense = AccountId::new(
            AccountType::Expense,
            Exchange::Kraken,
            "spot",
            Currency::USD,
        )?;
        assert_eq!(expense.normal_side(), EntrySide::Debit);

        let liability = AccountId::new(
            AccountType::Liability,
            Exchange::Kraken,
            "spot",
            Currency::USD,
        )?;
        assert_eq!(liability.normal_side(), EntrySide::Credit);

        let revenue = AccountId::new(
            AccountType::Revenue,
            Exchange::Kraken,
            "spot",
            Currency::USD,
        )?;
        assert_eq!(revenue.normal_side(), EntrySide::Credit);

        Ok(())
    }

    #[test]
    fn test_account_type_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        for variant in [
            AccountType::Asset,
            AccountType::Liability,
            AccountType::Revenue,
            AccountType::Expense,
        ] {
            let json = serde_json::to_string(&variant)?;
            let deserialized: AccountType = serde_json::from_str(&json)?;
            assert_eq!(variant, deserialized);
        }
        Ok(())
    }

    #[test]
    fn test_entry_side_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        for variant in [EntrySide::Debit, EntrySide::Credit] {
            let json = serde_json::to_string(&variant)?;
            let deserialized: EntrySide = serde_json::from_str(&json)?;
            assert_eq!(variant, deserialized);
        }
        Ok(())
    }

    #[test]
    fn test_transaction_type_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        for variant in [
            TransactionType::Trade,
            TransactionType::Fee,
            TransactionType::FundingRate,
            TransactionType::Interest,
            TransactionType::Transfer,
            TransactionType::Adjustment,
        ] {
            let json = serde_json::to_string(&variant)?;
            let deserialized: TransactionType = serde_json::from_str(&json)?;
            assert_eq!(variant, deserialized);
        }
        Ok(())
    }

    #[test]
    fn test_transaction_type_display_corporate_actions() {
        assert_eq!(TransactionType::Dividend.to_string(), "dividend");
        assert_eq!(TransactionType::StockSplit.to_string(), "stock_split");
        assert_eq!(TransactionType::BondCoupon.to_string(), "bond_coupon");
        assert_eq!(TransactionType::Merger.to_string(), "merger");
        assert_eq!(TransactionType::Spinoff.to_string(), "spinoff");
    }

    #[test]
    fn test_transaction_type_serde_roundtrip_corporate_actions()
    -> Result<(), Box<dyn std::error::Error>> {
        for variant in [
            TransactionType::Dividend,
            TransactionType::StockSplit,
            TransactionType::BondCoupon,
            TransactionType::Merger,
            TransactionType::Spinoff,
        ] {
            let json = serde_json::to_string(&variant)?;
            let deserialized: TransactionType = serde_json::from_str(&json)?;
            assert_eq!(variant, deserialized);
        }
        Ok(())
    }

    #[test]
    fn test_account_id_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let id = AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
            .map_err(|e| format!("{e}"))?;
        let json = serde_json::to_string(&id)?;
        let deserialized: AccountId = serde_json::from_str(&json)?;
        assert_eq!(id, deserialized);
        Ok(())
    }
}
