use rust_decimal::Decimal;

#[derive(Debug, thiserror::Error)]
pub enum PrimitiveError {
    #[error("invalid quantity: {0} (must be non-negative)")]
    InvalidQuantity(Decimal),

    #[error("invalid percentage: {0} (must be in [0, 1])")]
    InvalidPercentage(Decimal),

    #[error("unknown currency: {0}")]
    UnknownCurrency(String),

    #[error("symbol cannot be empty")]
    EmptySymbol,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_error_display_invalid_quantity() {
        let err = PrimitiveError::InvalidQuantity(dec!(-1.5));
        assert_eq!(
            err.to_string(),
            "invalid quantity: -1.5 (must be non-negative)"
        );
    }

    #[test]
    fn test_error_display_invalid_percentage() {
        let err = PrimitiveError::InvalidPercentage(dec!(1.5));
        assert_eq!(
            err.to_string(),
            "invalid percentage: 1.5 (must be in [0, 1])"
        );
    }

    #[test]
    fn test_error_display_unknown_currency() {
        let err = PrimitiveError::UnknownCurrency("XYZ".to_string());
        assert_eq!(err.to_string(), "unknown currency: XYZ");
    }

    #[test]
    fn test_error_display_empty_symbol() {
        let err = PrimitiveError::EmptySymbol;
        assert_eq!(err.to_string(), "symbol cannot be empty");
    }
}
