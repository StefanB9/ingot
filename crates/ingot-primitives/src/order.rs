use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderSide::Buy => write!(f, "buy"),
            OrderSide::Sell => write!(f, "sell"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub enum OrderType {
    Market,
    Limit,
}

impl fmt::Display for OrderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderType::Market => write!(f, "market"),
            OrderType::Limit => write!(f, "limit"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Sent,
    Filled,
    Rejected,
    ValidationSuccess,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderStatus::Sent => write!(f, "sent"),
            OrderStatus::Filled => write!(f, "filled"),
            OrderStatus::Rejected => write!(f, "rejected"),
            OrderStatus::ValidationSuccess => write!(f, "validation_success"),
        }
    }
}
