use chrono::{DateTime, Utc};
use ingot_primitives::Symbol;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Ticker {
    pub symbol: Symbol,
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
}

impl Ticker {
    pub fn new(symbol: &str, price: Decimal) -> Self {
        Self {
            symbol: Symbol::new(symbol),
            price,
            timestamp: Utc::now(),
        }
    }
}
