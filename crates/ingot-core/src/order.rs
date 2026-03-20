use std::fmt;

use chrono::{DateTime, Utc};
use ingot_primitives::{
    Amount, Currency, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce,
};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::error::CoreError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(SmolStr);

impl OrderId {
    pub fn new(id: &str) -> Result<Self, CoreError> {
        if id.is_empty() {
            return Err(CoreError::EmptyOrderId);
        }
        Ok(Self(SmolStr::new(id)))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRequest {
    pub symbol: Symbol,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: Quantity,
    pub limit_price: Option<Price>,
    pub stop_price: Option<Price>,
    pub time_in_force: TimeInForce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
    Expired,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::Open => f.write_str("Open"),
            Self::PartiallyFilled => f.write_str("PartiallyFilled"),
            Self::Filled => f.write_str("Filled"),
            Self::Cancelled => f.write_str("Cancelled"),
            Self::Rejected => f.write_str("Rejected"),
            Self::Expired => f.write_str("Expired"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderFill {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub fill_price: Price,
    pub fill_quantity: Quantity,
    pub fee: Amount,
    pub fee_currency: Currency,
    pub timestamp: DateTime<Utc>,
    pub trade_id: Option<SmolStr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenOrder {
    pub order_id: OrderId,
    pub request: OrderRequest,
    pub status: OrderStatus,
    pub filled_quantity: Quantity,
    pub remaining_quantity: Quantity,
    pub average_fill_price: Option<Price>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_order_id_new() -> Result<(), CoreError> {
        let id = OrderId::new("OABCDE-12345-FGHIJ")?;
        assert_eq!(id.as_str(), "OABCDE-12345-FGHIJ");
        Ok(())
    }

    #[test]
    fn test_order_id_empty_rejected() {
        let result = OrderId::new("");
        assert!(result.is_err());
    }

    #[test]
    fn test_order_id_display() -> Result<(), CoreError> {
        let id = OrderId::new("ORD-001")?;
        assert_eq!(id.to_string(), "ORD-001");
        Ok(())
    }

    #[test]
    fn test_order_status_display() {
        assert_eq!(OrderStatus::Pending.to_string(), "Pending");
        assert_eq!(OrderStatus::Open.to_string(), "Open");
        assert_eq!(OrderStatus::PartiallyFilled.to_string(), "PartiallyFilled");
        assert_eq!(OrderStatus::Filled.to_string(), "Filled");
        assert_eq!(OrderStatus::Cancelled.to_string(), "Cancelled");
        assert_eq!(OrderStatus::Rejected.to_string(), "Rejected");
        assert_eq!(OrderStatus::Expired.to_string(), "Expired");
    }

    #[test]
    fn test_order_status_copy() {
        let a = OrderStatus::Open;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn test_order_request_construction() -> Result<(), Box<dyn std::error::Error>> {
        let req = OrderRequest {
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(0.5))?,
            limit_price: Some(Price::new(dec!(67000.0))),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        };
        assert_eq!(req.symbol.as_str(), "XXBTZUSD");
        assert_eq!(req.side, OrderSide::Buy);
        Ok(())
    }

    #[test]
    fn test_order_request_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let req = OrderRequest {
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(1.0))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };
        let json = serde_json::to_string(&req)?;
        let deserialized: OrderRequest = serde_json::from_str(&json)?;
        assert_eq!(req, deserialized);
        Ok(())
    }

    #[test]
    fn test_order_fill_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let fill = OrderFill {
            order_id: OrderId::new("ORD-001")?,
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67100.50)),
            fill_quantity: Quantity::new(dec!(0.5))?,
            fee: Amount::new(dec!(1.25)),
            fee_currency: Currency::USD,
            timestamp: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
            trade_id: Some(SmolStr::new("t-12345")),
        };
        let json = serde_json::to_string(&fill)?;
        let deserialized: OrderFill = serde_json::from_str(&json)?;
        assert_eq!(fill, deserialized);
        Ok(())
    }

    #[test]
    fn test_order_id_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let id = OrderId::new("OABCDE-12345")?;
        let json = serde_json::to_string(&id)?;
        let deserialized: OrderId = serde_json::from_str(&json)?;
        assert_eq!(id, deserialized);
        Ok(())
    }

    #[test]
    fn test_order_status_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let status = OrderStatus::PartiallyFilled;
        let json = serde_json::to_string(&status)?;
        let deserialized: OrderStatus = serde_json::from_str(&json)?;
        assert_eq!(status, deserialized);
        Ok(())
    }

    #[test]
    fn test_open_order_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let order = OpenOrder {
            order_id: OrderId::new("ORD-002")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Sell,
                order_type: OrderType::Limit,
                quantity: Quantity::new(dec!(1.0))?,
                limit_price: Some(Price::new(dec!(70000.0))),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            status: OrderStatus::Open,
            filled_quantity: Quantity::zero(),
            remaining_quantity: Quantity::new(dec!(1.0))?,
            average_fill_price: None,
            created_at: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
        };
        let json = serde_json::to_string(&order)?;
        let deserialized: OpenOrder = serde_json::from_str(&json)?;
        assert_eq!(order, deserialized);
        Ok(())
    }
}
