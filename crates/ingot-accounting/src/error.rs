use ingot_primitives::{Amount, Currency, Exchange};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AccountingError {
    #[error("unbalanced transaction: {currency} debits={debits}, credits={credits}")]
    UnbalancedTransaction {
        currency: Currency,
        debits: Amount,
        credits: Amount,
    },

    #[error("invalid account: {reason}")]
    InvalidAccount { reason: String },

    #[error("duplicate transaction: {0}")]
    DuplicateTransaction(Uuid),

    #[error("invalid amount: {reason}")]
    InvalidAmount { reason: String },

    #[error("FX rate unavailable for {base}/{quote}")]
    FxRateUnavailable { base: Currency, quote: Currency },

    #[error("reconciliation failed for {exchange}: {reason}")]
    ReconciliationFailed { exchange: Exchange, reason: String },
}

#[cfg(test)]
mod tests {
    use ingot_primitives::Amount;
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_error_display_unbalanced_transaction() {
        let err = AccountingError::UnbalancedTransaction {
            currency: Currency::USD,
            debits: Amount::new(dec!(100)),
            credits: Amount::new(dec!(50)),
        };
        assert_eq!(
            err.to_string(),
            "unbalanced transaction: USD debits=100, credits=50"
        );
    }

    #[test]
    fn test_error_display_invalid_account() {
        let err = AccountingError::InvalidAccount {
            reason: "venue cannot be empty".into(),
        };
        assert_eq!(err.to_string(), "invalid account: venue cannot be empty");
    }

    #[test]
    fn test_error_display_duplicate_transaction() {
        let id = Uuid::nil();
        let err = AccountingError::DuplicateTransaction(id);
        assert_eq!(
            err.to_string(),
            "duplicate transaction: 00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn test_error_display_invalid_amount() {
        let err = AccountingError::InvalidAmount {
            reason: "negative not allowed".into(),
        };
        assert_eq!(err.to_string(), "invalid amount: negative not allowed");
    }

    #[test]
    fn test_error_display_fx_rate_unavailable() {
        let err = AccountingError::FxRateUnavailable {
            base: Currency::BTC,
            quote: Currency::EUR,
        };
        assert_eq!(err.to_string(), "FX rate unavailable for BTC/EUR");
    }

    #[test]
    fn test_error_display_reconciliation_failed() {
        let err = AccountingError::ReconciliationFailed {
            exchange: Exchange::Kraken,
            reason: "timeout".into(),
        };
        assert_eq!(err.to_string(), "reconciliation failed for Kraken: timeout");
    }
}
