use ingot_primitives::Symbol;

use crate::error::ConnectivityError;

#[derive(Debug, thiserror::Error)]
pub enum IbkrError {
    #[error("session expired, re-authentication required")]
    SessionExpired,

    #[error("contract not found: conid {0}")]
    ContractNotFound(i64),

    #[error("pacing violation: too many requests")]
    PacingViolation,

    #[error("order rejected by IBKR: {reason}")]
    OrderRejected { reason: String },

    #[error("TWS connection error: {0}")]
    TwsConnection(String),

    #[error("TWS message decode error: {0}")]
    TwsDecode(String),

    #[error("unsupported security type: {0}")]
    UnsupportedSecType(String),

    #[error("gateway not authenticated")]
    NotAuthenticated,

    #[error("margin data unavailable")]
    MarginUnavailable,
}

impl From<IbkrError> for ConnectivityError {
    fn from(err: IbkrError) -> Self {
        match err {
            IbkrError::SessionExpired => Self::AuthenticationFailed {
                reason: "session expired, re-authentication required".into(),
            },
            IbkrError::NotAuthenticated => Self::AuthenticationFailed {
                reason: "gateway not authenticated".into(),
            },
            IbkrError::PacingViolation => Self::RateLimited {
                retry_after_ms: 1000,
            },
            IbkrError::OrderRejected { reason } => Self::OrderRejected { reason },
            IbkrError::ContractNotFound(conid) => {
                // conid is always a valid non-empty string when stringified
                match Symbol::new(&conid.to_string()) {
                    Ok(sym) => Self::SymbolNotFound(sym),
                    Err(_) => Self::InvalidResponse(format!("contract not found: conid {conid}")),
                }
            }
            IbkrError::TwsConnection(msg) => Self::WebSocket(msg),
            IbkrError::TwsDecode(msg) => Self::InvalidResponse(msg),
            IbkrError::UnsupportedSecType(s) => {
                Self::InvalidResponse(format!("unsupported security type: {s}"))
            }
            IbkrError::MarginUnavailable => Self::InvalidResponse("margin data unavailable".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Display tests ──

    #[test]
    fn test_ibkr_error_display_session_expired() {
        let err = IbkrError::SessionExpired;
        assert_eq!(
            err.to_string(),
            "session expired, re-authentication required"
        );
    }

    #[test]
    fn test_ibkr_error_display_contract_not_found() {
        let err = IbkrError::ContractNotFound(265598);
        assert_eq!(err.to_string(), "contract not found: conid 265598");
    }

    #[test]
    fn test_ibkr_error_display_pacing_violation() {
        let err = IbkrError::PacingViolation;
        assert_eq!(err.to_string(), "pacing violation: too many requests");
    }

    #[test]
    fn test_ibkr_error_display_order_rejected() {
        let err = IbkrError::OrderRejected {
            reason: "margin exceeded".into(),
        };
        assert_eq!(err.to_string(), "order rejected by IBKR: margin exceeded");
    }

    #[test]
    fn test_ibkr_error_display_tws_connection() {
        let err = IbkrError::TwsConnection("timeout".into());
        assert_eq!(err.to_string(), "TWS connection error: timeout");
    }

    #[test]
    fn test_ibkr_error_display_tws_decode() {
        let err = IbkrError::TwsDecode("invalid length".into());
        assert_eq!(err.to_string(), "TWS message decode error: invalid length");
    }

    #[test]
    fn test_ibkr_error_display_unsupported_sec_type() {
        let err = IbkrError::UnsupportedSecType("WAR".into());
        assert_eq!(err.to_string(), "unsupported security type: WAR");
    }

    #[test]
    fn test_ibkr_error_display_not_authenticated() {
        let err = IbkrError::NotAuthenticated;
        assert_eq!(err.to_string(), "gateway not authenticated");
    }

    #[test]
    fn test_ibkr_error_display_margin_unavailable() {
        let err = IbkrError::MarginUnavailable;
        assert_eq!(err.to_string(), "margin data unavailable");
    }

    // ── From<IbkrError> for ConnectivityError tests ──

    #[test]
    fn test_from_session_expired() {
        let ce: ConnectivityError = IbkrError::SessionExpired.into();
        assert!(matches!(
            ce,
            ConnectivityError::AuthenticationFailed { ref reason } if reason.contains("session expired")
        ));
    }

    #[test]
    fn test_from_not_authenticated() {
        let ce: ConnectivityError = IbkrError::NotAuthenticated.into();
        assert!(matches!(
            ce,
            ConnectivityError::AuthenticationFailed { ref reason } if reason.contains("not authenticated")
        ));
    }

    #[test]
    fn test_from_pacing_violation() {
        let ce: ConnectivityError = IbkrError::PacingViolation.into();
        assert!(matches!(
            ce,
            ConnectivityError::RateLimited {
                retry_after_ms: 1000
            }
        ));
    }

    #[test]
    fn test_from_order_rejected() {
        let ce: ConnectivityError = IbkrError::OrderRejected {
            reason: "insufficient margin".into(),
        }
        .into();
        assert!(matches!(
            ce,
            ConnectivityError::OrderRejected { ref reason } if reason == "insufficient margin"
        ));
    }

    #[test]
    fn test_from_contract_not_found() {
        let ce: ConnectivityError = IbkrError::ContractNotFound(265598).into();
        assert!(matches!(
            ce,
            ConnectivityError::SymbolNotFound(ref sym) if sym.as_str() == "265598"
        ));
    }

    #[test]
    fn test_from_tws_connection() {
        let ce: ConnectivityError = IbkrError::TwsConnection("reset".into()).into();
        assert!(matches!(
            ce,
            ConnectivityError::WebSocket(ref msg) if msg == "reset"
        ));
    }

    #[test]
    fn test_from_tws_decode() {
        let ce: ConnectivityError = IbkrError::TwsDecode("bad frame".into()).into();
        assert!(matches!(
            ce,
            ConnectivityError::InvalidResponse(ref msg) if msg == "bad frame"
        ));
    }
}
