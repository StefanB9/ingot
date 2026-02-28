//! Shared request/response types for the ingot HTTP + WebSocket API.
//!
//! These types are used by both `ingot-server` (the producer) and the
//! thin clients (`ingot-cli`, `ingot-gui`). Keeping them in `ingot-core`
//! avoids a dedicated crate while reusing existing serde support.

use std::fmt;

use ingot_primitives::{Amount, Currency, Price, Symbol};
use serde::{Deserialize, Serialize};

use crate::execution::OrderAcknowledgment;

// ---------------------------------------------------------------------------
// Generic API envelope
// ---------------------------------------------------------------------------

/// JSON envelope for all REST responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> ApiResponse<T> {
    /// Convenience constructor for a successful response.
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    /// Convenience constructor for an error response.
    pub fn err(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

// ---------------------------------------------------------------------------
// REST response payloads
// ---------------------------------------------------------------------------

/// Response for `GET /api/v1/nav`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavResponse {
    pub amount: Amount,
    pub currency: Currency,
}

/// Response for `GET /api/v1/ticker/{symbol}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickerResponse {
    pub symbol: Symbol,
    pub price: Price,
}

/// Response for `POST /api/v1/heartbeat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub status: ServerStatus,
    pub uptime_secs: u64,
}

/// Server lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    Running,
    ShuttingDown,
}

impl fmt::Display for ServerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::ShuttingDown => f.write_str("shutting_down"),
        }
    }
}

/// Response for `POST /api/v1/shutdown`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownResponse {
    pub acknowledged: bool,
}

/// Response for `POST /api/v1/orders`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResponse {
    pub acknowledgment: OrderAcknowledgment,
}

// ---------------------------------------------------------------------------
// WebSocket messages
// ---------------------------------------------------------------------------

/// Tagged JSON enum for bidirectional WebSocket communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum WsMessage {
    /// Server -> Client: real-time price update.
    Tick(TickerUpdate),
    /// Server -> Client: net-asset-value update.
    Nav(NavUpdate),
    /// Server -> Client: order status change.
    OrderUpdate(OrderAcknowledgment),
    /// Client -> Server: subscribe to symbols.
    Subscribe(SubscribeRequest),
    /// Server -> Client: error notification.
    Error(String),
}

/// A single price update pushed over WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickerUpdate {
    pub symbol: Symbol,
    pub price: Price,
}

/// NAV snapshot pushed over WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavUpdate {
    pub amount: Amount,
    pub currency: Currency,
}

/// Client request to subscribe to one or more symbols.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeRequest {
    pub symbols: Vec<Symbol>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use ingot_primitives::{CurrencyCode, OrderStatus};
    use rust_decimal::dec;

    use super::*;

    #[test]
    fn test_api_response_ok_serialization() -> anyhow::Result<()> {
        let resp = ApiResponse::ok(NavResponse {
            amount: Amount::from(dec!(10000)),
            currency: Currency::usd(),
        });
        let json = serde_json::to_string(&resp)?;

        assert!(json.contains(r#""success":true"#));
        assert!(json.contains(r#""amount""#));
        assert!(!json.contains(r#""error""#)); // skip_serializing_if None
        Ok(())
    }

    #[test]
    fn test_api_response_err_serialization() -> anyhow::Result<()> {
        let resp: ApiResponse<NavResponse> = ApiResponse::err("not found".into());
        let json = serde_json::to_string(&resp)?;

        assert!(json.contains(r#""success":false"#));
        assert!(json.contains(r#""error":"not found""#));
        assert!(!json.contains(r#""data""#));
        Ok(())
    }

    #[test]
    fn test_api_response_round_trip() -> anyhow::Result<()> {
        let original = ApiResponse::ok(HeartbeatResponse {
            status: ServerStatus::Running,
            uptime_secs: 42,
        });
        let json = serde_json::to_string(&original)?;
        let decoded: ApiResponse<HeartbeatResponse> = serde_json::from_str(&json)?;

        assert!(decoded.success);
        let data = decoded
            .data
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing data"))?;
        assert_eq!(data.status, ServerStatus::Running);
        assert_eq!(data.uptime_secs, 42);
        Ok(())
    }

    #[test]
    fn test_nav_response_round_trip() -> anyhow::Result<()> {
        let original = NavResponse {
            amount: Amount::from(dec!(9999.50)),
            currency: Currency::usd(),
        };
        let json = serde_json::to_string(&original)?;
        let decoded: NavResponse = serde_json::from_str(&json)?;
        assert_eq!(decoded.amount, original.amount);
        assert_eq!(decoded.currency.code, CurrencyCode::new("USD"));
        Ok(())
    }

    #[test]
    fn test_ticker_response_round_trip() -> anyhow::Result<()> {
        let original = TickerResponse {
            symbol: Symbol::new("BTC-USD"),
            price: Price::from(dec!(55000)),
        };
        let json = serde_json::to_string(&original)?;
        let decoded: TickerResponse = serde_json::from_str(&json)?;
        assert_eq!(decoded.symbol, original.symbol);
        assert_eq!(decoded.price, original.price);
        Ok(())
    }

    #[test]
    fn test_server_status_display() {
        assert_eq!(ServerStatus::Running.to_string(), "running");
        assert_eq!(ServerStatus::ShuttingDown.to_string(), "shutting_down");
    }

    #[test]
    fn test_server_status_serialization() -> anyhow::Result<()> {
        let json = serde_json::to_string(&ServerStatus::Running)?;
        assert_eq!(json, r#""running""#);
        let json = serde_json::to_string(&ServerStatus::ShuttingDown)?;
        assert_eq!(json, r#""shutting_down""#);
        Ok(())
    }

    #[test]
    fn test_ws_message_tick_tagged_format() -> anyhow::Result<()> {
        let msg = WsMessage::Tick(TickerUpdate {
            symbol: Symbol::new("BTC-USD"),
            price: Price::from(dec!(50000)),
        });
        let json = serde_json::to_string(&msg)?;
        // Verify tagged enum format: {"type":"Tick","payload":{...}}
        assert!(json.contains(r#""type":"Tick""#));
        assert!(json.contains(r#""payload""#));

        let decoded: WsMessage = serde_json::from_str(&json)?;
        match decoded {
            WsMessage::Tick(update) => {
                assert_eq!(update.symbol, Symbol::new("BTC-USD"));
                assert_eq!(update.price, Price::from(dec!(50000)));
            }
            other => anyhow::bail!("expected Tick, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn test_ws_message_subscribe_round_trip() -> anyhow::Result<()> {
        let msg = WsMessage::Subscribe(SubscribeRequest {
            symbols: vec![Symbol::new("BTC-USD"), Symbol::new("ETH-USD")],
        });
        let json = serde_json::to_string(&msg)?;
        assert!(json.contains(r#""type":"Subscribe""#));

        let decoded: WsMessage = serde_json::from_str(&json)?;
        match decoded {
            WsMessage::Subscribe(req) => {
                assert_eq!(req.symbols.len(), 2);
                assert_eq!(req.symbols[0], Symbol::new("BTC-USD"));
                assert_eq!(req.symbols[1], Symbol::new("ETH-USD"));
            }
            other => anyhow::bail!("expected Subscribe, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn test_ws_message_order_update_round_trip() -> anyhow::Result<()> {
        let ack = OrderAcknowledgment {
            exchange_id: "test-123".into(),
            client_id: None,
            timestamp: chrono::Utc::now(),
            status: OrderStatus::Filled,
        };
        let msg = WsMessage::OrderUpdate(ack);
        let json = serde_json::to_string(&msg)?;
        assert!(json.contains(r#""type":"OrderUpdate""#));

        let decoded: WsMessage = serde_json::from_str(&json)?;
        match decoded {
            WsMessage::OrderUpdate(a) => {
                assert_eq!(a.exchange_id, "test-123");
                assert_eq!(a.status, OrderStatus::Filled);
            }
            other => anyhow::bail!("expected OrderUpdate, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn test_ws_message_error_round_trip() -> anyhow::Result<()> {
        let msg = WsMessage::Error("something went wrong".into());
        let json = serde_json::to_string(&msg)?;
        let decoded: WsMessage = serde_json::from_str(&json)?;
        match decoded {
            WsMessage::Error(e) => assert_eq!(e, "something went wrong"),
            other => anyhow::bail!("expected Error, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn test_shutdown_response_round_trip() -> anyhow::Result<()> {
        let resp = ShutdownResponse { acknowledged: true };
        let json = serde_json::to_string(&resp)?;
        let decoded: ShutdownResponse = serde_json::from_str(&json)?;
        assert!(decoded.acknowledged);
        Ok(())
    }

    #[test]
    fn test_order_response_round_trip() -> anyhow::Result<()> {
        let resp = OrderResponse {
            acknowledgment: OrderAcknowledgment {
                exchange_id: "ord-456".into(),
                client_id: Some("client-1".into()),
                timestamp: chrono::Utc::now(),
                status: OrderStatus::Sent,
            },
        };
        let json = serde_json::to_string(&resp)?;
        let decoded: OrderResponse = serde_json::from_str(&json)?;
        assert_eq!(decoded.acknowledgment.exchange_id, "ord-456");
        assert_eq!(
            decoded.acknowledgment.client_id.as_deref(),
            Some("client-1")
        );
        Ok(())
    }
}
