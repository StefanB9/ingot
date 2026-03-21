use std::fmt;

#[derive(Debug)]
pub enum CoreError {
    EmptyOrderId,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOrderId => f.write_str("order ID cannot be empty"),
        }
    }
}

impl std::error::Error for CoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_error_display() {
        assert_eq!(
            CoreError::EmptyOrderId.to_string(),
            "order ID cannot be empty"
        );
    }
}
