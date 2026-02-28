use ingot_primitives::{Amount, CurrencyCode};
use thiserror::Error;
use uuid::Uuid;

#[derive(Error, Debug, PartialEq)]
pub enum AccountingError {
    #[error("Currency Mismatch: Cannot operate on {0} and {1}")]
    CurrencyMismatch(String, String),

    #[error("Precision Error: {0}")]
    PrecisionError(String),

    #[error("Transaction Unbalanced: Debits {0} != Credits {1}")]
    TransactionUnbalanced(Amount, Amount),

    #[error("Multi-Currency Transaction Unbalanced for {0}: Debits {1} != Credits {2}")]
    MultiCurrencyTransactionUnbalanced(String, Amount, Amount),

    #[error("Account Not Found: {0}")]
    AccountNotFound(Uuid),

    #[error("Invalid Amount: {0}")]
    InvalidAmount(&'static str),

    #[error("No account found for currency: {0}")]
    NoCurrencyAccount(CurrencyCode),
}

#[cfg(test)]
mod tests {
    use rust_decimal::dec;

    use super::*;

    #[test]
    fn test_error_formatting_currency_mismatch() {
        let err = AccountingError::CurrencyMismatch("USD".into(), "BTC".into());
        assert_eq!(
            format!("{err}"),
            "Currency Mismatch: Cannot operate on USD and BTC"
        );
    }

    #[test]
    fn test_error_formatting_precision_error() {
        let err = AccountingError::PrecisionError("Division by zero".into());
        assert_eq!(format!("{err}"), "Precision Error: Division by zero");
    }

    #[test]
    fn test_error_formatting_transaction_unbalanced() {
        let err = AccountingError::TransactionUnbalanced(
            Amount::from(dec!(100)),
            Amount::from(dec!(101)),
        );
        assert_eq!(
            format!("{err}"),
            "Transaction Unbalanced: Debits 100 != Credits 101"
        );
    }

    #[test]
    fn test_error_formatting_multi_currency_unbalanced() {
        let err = AccountingError::MultiCurrencyTransactionUnbalanced(
            "USD".into(),
            Amount::from(dec!(50)),
            Amount::from(dec!(60)),
        );
        assert_eq!(
            format!("{err}"),
            "Multi-Currency Transaction Unbalanced for USD: Debits 50 != Credits 60"
        );
    }

    #[test]
    fn test_error_formatting_account_not_found() {
        let id = Uuid::nil();
        let err = AccountingError::AccountNotFound(id);
        assert_eq!(
            format!("{err}"),
            "Account Not Found: 00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn test_error_formatting_invalid_amount() {
        let err = AccountingError::InvalidAmount("Negative value");
        assert_eq!(format!("{err}"), "Invalid Amount: Negative value");
    }

    #[test]
    fn test_error_matchability() {
        let err = AccountingError::AccountNotFound(Uuid::nil());
        assert!(matches!(err, AccountingError::AccountNotFound(_)));
    }

    #[test]
    fn test_error_formatting_no_currency_account() {
        let err = AccountingError::NoCurrencyAccount(CurrencyCode::new("BTC"));
        assert_eq!(format!("{err}"), "No account found for currency: BTC");
    }
}
