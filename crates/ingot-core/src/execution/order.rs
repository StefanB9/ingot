use std::{fmt, fmt::Display};

use chrono::{DateTime, Utc};
use ingot_primitives::{OrderSide, OrderStatus, OrderType, Price, Quantity, Symbol};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OrderRequest {
    pub symbol: Symbol,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: Quantity,
    pub price: Option<Price>,
    pub validate_only: bool,
}

impl OrderRequest {
    pub fn new_market(symbol: Symbol, side: OrderSide, quantity: Quantity) -> Self {
        Self {
            symbol,
            side,
            order_type: OrderType::Market,
            quantity,
            price: None,
            validate_only: false,
        }
    }

    pub fn new_limit(symbol: Symbol, side: OrderSide, quantity: Quantity, price: Price) -> Self {
        Self {
            symbol,
            side,
            order_type: OrderType::Limit,
            quantity,
            price: Some(price),
            validate_only: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAcknowledgment {
    pub exchange_id: String,
    pub client_id: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub status: OrderStatus,
}

impl Display for OrderAcknowledgment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[OrderAck] ID: {} | Status: {} | Time: {}",
            self.exchange_id,
            self.status,
            self.timestamp.format("%H:%M:%S")
        )
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rust_decimal::dec;

    use super::*;

    #[test]
    fn test_order_request_constructors() {
        let market = OrderRequest::new_market(
            Symbol::new("BTC-USD"),
            OrderSide::Buy,
            Quantity::from(dec!(1.5)),
        );
        assert_eq!(market.symbol, Symbol::new("BTC-USD"));
        assert_eq!(market.side, OrderSide::Buy);
        assert_eq!(market.order_type, OrderType::Market);
        assert_eq!(market.quantity, Quantity::from(dec!(1.5)));
        assert_eq!(market.price, None);
        assert!(!market.validate_only);

        let limit = OrderRequest::new_limit(
            Symbol::new("ETH-USD"),
            OrderSide::Sell,
            Quantity::from(dec!(10.0)),
            Price::from(dec!(3000.0)),
        );
        assert_eq!(limit.order_type, OrderType::Limit);
        assert_eq!(limit.price, Some(Price::from(dec!(3000.0))));
    }

    #[test]
    fn test_display_trait_formatting() {
        assert_eq!(OrderSide::Buy.to_string(), "buy");
        assert_eq!(OrderSide::Sell.to_string(), "sell");
        assert_eq!(OrderType::Market.to_string(), "market");
        assert_eq!(OrderType::Limit.to_string(), "limit");
        assert_eq!(OrderStatus::Sent.to_string(), "sent");
        assert_eq!(OrderStatus::Filled.to_string(), "filled");
        assert_eq!(OrderStatus::Rejected.to_string(), "rejected");
        assert_eq!(
            OrderStatus::ValidationSuccess.to_string(),
            "validation_success"
        );
    }

    #[test]
    fn test_serialization() -> Result<()> {
        let order = OrderRequest::new_limit(
            Symbol::new("SOL-USD"),
            OrderSide::Buy,
            Quantity::from(dec!(100)),
            Price::from(dec!(50)),
        );
        let json = serde_json::to_string(&order)?;

        assert!(json.contains("SOL-USD"));
        assert!(json.contains("Buy"));
        assert!(json.contains("Limit"));
        assert!(json.contains("100"));

        Ok(())
    }

    #[test]
    fn test_order_status_serialization() -> Result<()> {
        let json = serde_json::to_string(&OrderStatus::Sent)?;
        assert_eq!(json, r#""sent""#);
        let deserialized: OrderStatus = serde_json::from_str(&json)?;
        assert_eq!(deserialized, OrderStatus::Sent);

        let json = serde_json::to_string(&OrderStatus::ValidationSuccess)?;
        assert_eq!(json, r#""validation_success""#);

        Ok(())
    }
}
