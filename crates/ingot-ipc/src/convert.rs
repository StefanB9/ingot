//! Conversions between domain types and IPC message types.
//!
//! These `From`/`Into` impls form the boundary between the domain model
//! (which uses `String`, `DateTime`, `Option`) and the IPC wire format
//! (which uses `FixedId`, raw epoch seconds, `has_*` + value pairs).

use chrono::{TimeZone, Utc};
use ingot_core::{
    api::{NavUpdate, TickerUpdate},
    execution::{OrderAcknowledgment, OrderRequest},
};
use tracing::warn;

use crate::types::{FixedId, IpcNavUpdate, IpcOrderRequest, IpcOrderUpdate, IpcTickerUpdate};

// ---------------------------------------------------------------------------
// TickerUpdate <-> IpcTickerUpdate
// ---------------------------------------------------------------------------

impl From<&TickerUpdate> for IpcTickerUpdate {
    fn from(t: &TickerUpdate) -> Self {
        Self {
            symbol: t.symbol,
            price: t.price,
        }
    }
}

impl From<IpcTickerUpdate> for TickerUpdate {
    fn from(t: IpcTickerUpdate) -> Self {
        Self {
            symbol: t.symbol,
            price: t.price,
        }
    }
}

// ---------------------------------------------------------------------------
// NavUpdate <-> IpcNavUpdate
// ---------------------------------------------------------------------------

impl From<&NavUpdate> for IpcNavUpdate {
    fn from(n: &NavUpdate) -> Self {
        Self {
            amount: n.amount,
            currency: n.currency,
        }
    }
}

impl From<IpcNavUpdate> for NavUpdate {
    fn from(n: IpcNavUpdate) -> Self {
        Self {
            amount: n.amount,
            currency: n.currency,
        }
    }
}

// ---------------------------------------------------------------------------
// OrderRequest <-> IpcOrderRequest
// ---------------------------------------------------------------------------

impl From<&OrderRequest> for IpcOrderRequest {
    fn from(o: &OrderRequest) -> Self {
        let (has_price, price) = match o.price {
            Some(p) => (true, p),
            None => (
                false,
                ingot_primitives::Price::from(rust_decimal::Decimal::ZERO),
            ),
        };
        Self {
            symbol: o.symbol,
            side: o.side,
            order_type: o.order_type,
            quantity: o.quantity,
            has_price,
            price,
            validate_only: o.validate_only,
        }
    }
}

impl From<IpcOrderRequest> for OrderRequest {
    fn from(o: IpcOrderRequest) -> Self {
        let price = if o.has_price { Some(o.price) } else { None };
        Self {
            symbol: o.symbol,
            side: o.side,
            order_type: o.order_type,
            quantity: o.quantity,
            price,
            validate_only: o.validate_only,
        }
    }
}

// ---------------------------------------------------------------------------
// OrderAcknowledgment <-> IpcOrderUpdate
// ---------------------------------------------------------------------------

impl From<&OrderAcknowledgment> for IpcOrderUpdate {
    fn from(a: &OrderAcknowledgment) -> Self {
        let (has_client_id, client_id) = match &a.client_id {
            Some(id) => (true, FixedId::from_slice(id)),
            None => (false, FixedId::default()),
        };
        Self {
            exchange_id: FixedId::from_slice(&a.exchange_id),
            has_client_id,
            client_id,
            timestamp_secs: a.timestamp.timestamp(),
            timestamp_nanos: a.timestamp.timestamp_subsec_nanos(),
            status: a.status,
        }
    }
}

impl From<IpcOrderUpdate> for OrderAcknowledgment {
    fn from(u: IpcOrderUpdate) -> Self {
        let client_id = if u.has_client_id {
            Some(u.client_id.as_str().to_owned())
        } else {
            None
        };
        Self {
            exchange_id: u.exchange_id.as_str().to_owned(),
            client_id,
            timestamp: Utc
                .timestamp_opt(u.timestamp_secs, u.timestamp_nanos)
                .single()
                .unwrap_or_else(|| {
                    warn!(
                        secs = u.timestamp_secs,
                        nanos = u.timestamp_nanos,
                        "invalid IPC timestamp, falling back to Utc::now()"
                    );
                    Utc::now()
                }),
            status: u.status,
        }
    }
}

#[cfg(test)]
mod tests {
    use ingot_primitives::{
        Amount, Currency, OrderSide, OrderStatus, OrderType, Price, Quantity, Symbol,
    };
    use rust_decimal::dec;

    use super::*;

    // -- TickerUpdate round-trip ---------------------------------------------

    #[test]
    fn test_ticker_update_round_trip() {
        let original = TickerUpdate {
            symbol: Symbol::new("BTC-USD"),
            price: Price::from(dec!(50000)),
        };
        let ipc = IpcTickerUpdate::from(&original);
        let restored = TickerUpdate::from(ipc);

        assert_eq!(restored.symbol, original.symbol);
        assert_eq!(restored.price, original.price);
    }

    // -- NavUpdate round-trip ------------------------------------------------

    #[test]
    fn test_nav_update_round_trip() {
        let original = NavUpdate {
            amount: Amount::from(dec!(10000.50)),
            currency: Currency::usd(),
        };
        let ipc = IpcNavUpdate::from(&original);
        let restored = NavUpdate::from(ipc);

        assert_eq!(restored.amount, original.amount);
        assert_eq!(restored.currency, original.currency);
    }

    // -- OrderRequest round-trips --------------------------------------------

    #[test]
    fn test_order_request_market_round_trip() {
        let original = OrderRequest::new_market(
            Symbol::new("ETH-USD"),
            OrderSide::Buy,
            Quantity::from(dec!(5)),
        );
        let ipc = IpcOrderRequest::from(&original);
        let restored = OrderRequest::from(ipc);

        assert_eq!(restored.symbol, original.symbol);
        assert_eq!(restored.side, original.side);
        assert_eq!(restored.order_type, OrderType::Market);
        assert_eq!(restored.quantity, original.quantity);
        assert_eq!(restored.price, None);
        assert!(!restored.validate_only);
    }

    #[test]
    fn test_order_request_limit_round_trip() {
        let original = OrderRequest::new_limit(
            Symbol::new("SOL-USD"),
            OrderSide::Sell,
            Quantity::from(dec!(100)),
            Price::from(dec!(150)),
        );
        let ipc = IpcOrderRequest::from(&original);
        let restored = OrderRequest::from(ipc);

        assert_eq!(restored.symbol, original.symbol);
        assert_eq!(restored.side, original.side);
        assert_eq!(restored.order_type, OrderType::Limit);
        assert_eq!(restored.quantity, original.quantity);
        assert_eq!(restored.price, Some(Price::from(dec!(150))));
    }

    // -- OrderAcknowledgment round-trips -------------------------------------

    #[test]
    fn test_order_ack_with_client_id_round_trip() {
        let original = OrderAcknowledgment {
            exchange_id: "exchange-order-789".into(),
            client_id: Some("my-client-456".into()),
            timestamp: Utc::now(),
            status: OrderStatus::Filled,
        };
        let ipc = IpcOrderUpdate::from(&original);
        let restored = OrderAcknowledgment::from(ipc);

        assert_eq!(restored.exchange_id, original.exchange_id);
        assert_eq!(restored.client_id, original.client_id);
        assert_eq!(restored.status, original.status);
        // Timestamp: subsecond precision preserved within nanos
        assert_eq!(
            restored.timestamp.timestamp(),
            original.timestamp.timestamp()
        );
        assert_eq!(
            restored.timestamp.timestamp_subsec_nanos(),
            original.timestamp.timestamp_subsec_nanos()
        );
    }

    #[test]
    fn test_order_ack_without_client_id_round_trip() {
        let original = OrderAcknowledgment {
            exchange_id: "ord-001".into(),
            client_id: None,
            timestamp: Utc::now(),
            status: OrderStatus::Sent,
        };
        let ipc = IpcOrderUpdate::from(&original);
        let restored = OrderAcknowledgment::from(ipc);

        assert_eq!(restored.exchange_id, "ord-001");
        assert!(restored.client_id.is_none());
        assert_eq!(restored.status, OrderStatus::Sent);
    }

    #[test]
    fn test_order_ack_long_exchange_id_truncated() {
        let long_id = "A".repeat(100);
        let original = OrderAcknowledgment {
            exchange_id: long_id,
            client_id: None,
            timestamp: Utc::now(),
            status: OrderStatus::Rejected,
        };
        let ipc = IpcOrderUpdate::from(&original);
        let restored = OrderAcknowledgment::from(ipc);

        // FixedId truncates to 64 bytes
        assert_eq!(restored.exchange_id.len(), 64);
        assert_eq!(restored.exchange_id, "A".repeat(64));
    }

    #[test]
    fn test_order_ack_invalid_timestamp_falls_back() {
        let ipc = IpcOrderUpdate {
            exchange_id: FixedId::from_slice("test-order"),
            has_client_id: false,
            client_id: FixedId::default(),
            timestamp_secs: i64::MAX,
            timestamp_nanos: 0,
            status: OrderStatus::Filled,
        };
        let ack = OrderAcknowledgment::from(ipc);

        // Should not panic; falls back to a recent timestamp
        let now = Utc::now();
        let diff = (now - ack.timestamp).num_seconds().abs();
        assert!(
            diff < 5,
            "expected fallback timestamp near now, got diff={diff}s"
        );
        assert_eq!(ack.status, OrderStatus::Filled);
    }

    #[test]
    fn test_order_ack_validation_success_round_trip() {
        let original = OrderAcknowledgment {
            exchange_id: "val-test".into(),
            client_id: None,
            timestamp: Utc::now(),
            status: OrderStatus::ValidationSuccess,
        };
        let ipc = IpcOrderUpdate::from(&original);
        let restored = OrderAcknowledgment::from(ipc);

        assert_eq!(restored.status, OrderStatus::ValidationSuccess);
    }

    #[test]
    fn test_order_ack_empty_exchange_id() {
        let original = OrderAcknowledgment {
            exchange_id: String::new(),
            client_id: None,
            timestamp: Utc::now(),
            status: OrderStatus::Sent,
        };
        let ipc = IpcOrderUpdate::from(&original);
        let restored = OrderAcknowledgment::from(ipc);

        assert_eq!(restored.exchange_id, "");
    }
}
