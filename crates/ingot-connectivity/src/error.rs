use ingot_primitives::Symbol;

#[derive(Debug, thiserror::Error)]
pub enum ConnectivityError {
    #[error("HTTP request failed: {0}")]
    Http(#[source] reqwest::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("authentication failed: {reason}")]
    AuthenticationFailed { reason: String },

    #[error("rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("API error [{code}]: {message}")]
    ApiError { code: String, message: String },

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("connection lost")]
    ConnectionLost,

    #[error("order rejected: {reason}")]
    OrderRejected { reason: String },

    #[error("insufficient balance: need {required}, have {available}")]
    InsufficientBalance {
        required: rust_decimal::Decimal,
        available: rust_decimal::Decimal,
    },

    #[error("symbol not found: {0}")]
    SymbolNotFound(Symbol),

    #[error("deserialization failed: {0}")]
    Deserialization(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_http() -> Result<(), Box<dyn std::error::Error>> {
        // Build a reqwest::Error by parsing an invalid URL
        let reqwest_err = reqwest::Client::new()
            .get("http://[invalid/url")
            .build()
            .err()
            .ok_or("expected reqwest build error")?;
        let err = ConnectivityError::Http(reqwest_err);
        assert!(err.to_string().starts_with("HTTP request failed:"));
        Ok(())
    }

    #[test]
    fn test_display_websocket() {
        let err = ConnectivityError::WebSocket("connection reset".into());
        assert_eq!(err.to_string(), "WebSocket error: connection reset");
    }

    #[test]
    fn test_display_authentication_failed() {
        let err = ConnectivityError::AuthenticationFailed {
            reason: "invalid key".into(),
        };
        assert_eq!(err.to_string(), "authentication failed: invalid key");
    }

    #[test]
    fn test_display_rate_limited() {
        let err = ConnectivityError::RateLimited {
            retry_after_ms: 5000,
        };
        assert_eq!(err.to_string(), "rate limited, retry after 5000ms");
    }

    #[test]
    fn test_display_api_error() {
        let err = ConnectivityError::ApiError {
            code: "EAPI:Invalid nonce".into(),
            message: "nonce too small".into(),
        };
        assert_eq!(
            err.to_string(),
            "API error [EAPI:Invalid nonce]: nonce too small"
        );
    }

    #[test]
    fn test_display_invalid_response() {
        let err = ConnectivityError::InvalidResponse("missing field 'result'".into());
        assert_eq!(err.to_string(), "invalid response: missing field 'result'");
    }

    #[test]
    fn test_display_connection_lost() {
        let err = ConnectivityError::ConnectionLost;
        assert_eq!(err.to_string(), "connection lost");
    }

    #[test]
    fn test_display_order_rejected() {
        let err = ConnectivityError::OrderRejected {
            reason: "insufficient margin".into(),
        };
        assert_eq!(err.to_string(), "order rejected: insufficient margin");
    }

    #[test]
    fn test_display_insufficient_balance() {
        let err = ConnectivityError::InsufficientBalance {
            required: rust_decimal::Decimal::new(1000, 0),
            available: rust_decimal::Decimal::new(500, 0),
        };
        assert_eq!(err.to_string(), "insufficient balance: need 1000, have 500");
    }

    #[test]
    fn test_display_symbol_not_found() -> Result<(), Box<dyn std::error::Error>> {
        let sym = Symbol::new("XXBTZUSD")?;
        let err = ConnectivityError::SymbolNotFound(sym);
        assert_eq!(err.to_string(), "symbol not found: XXBTZUSD");
        Ok(())
    }

    #[test]
    fn test_display_deserialization() -> Result<(), Box<dyn std::error::Error>> {
        let json_err = serde_json::from_str::<String>("not json")
            .err()
            .ok_or("expected serde_json error")?;
        let err = ConnectivityError::Deserialization(json_err);
        assert!(err.to_string().starts_with("deserialization failed:"));
        Ok(())
    }
}
