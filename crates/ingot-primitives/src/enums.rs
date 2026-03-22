use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetClass {
    Equity,
    Option,
    Future,
    Forex,
    CryptoSpot,
    CryptoFuture,
    Bond,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Exchange {
    Kraken,
    KrakenFutures,
    IBKR,
    Paper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    StopLoss,
    StopLossLimit,
    TakeProfit,
    TakeProfitLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimeInForce {
    GoodTilCancelled,
    ImmediateOrCancel,
    FillOrKill,
    Day,
    GoodTilDate(DateTime<Utc>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OptionRight {
    Call,
    Put,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OptionStyle {
    American,
    European,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SettlementType {
    Cash,
    Physical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CryptoContractType {
    PerpetualLinear,
    PerpetualInverse,
    FixedLinear,
    FixedInverse,
}

impl Exchange {
    pub fn as_str_lowercase(&self) -> &'static str {
        match self {
            Self::Kraken => "kraken",
            Self::KrakenFutures => "kraken_futures",
            Self::IBKR => "ibkr",
            Self::Paper => "paper",
        }
    }
}

impl fmt::Display for AssetClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Equity => f.write_str("Equity"),
            Self::Option => f.write_str("Option"),
            Self::Future => f.write_str("Future"),
            Self::Forex => f.write_str("Forex"),
            Self::CryptoSpot => f.write_str("CryptoSpot"),
            Self::CryptoFuture => f.write_str("CryptoFuture"),
            Self::Bond => f.write_str("Bond"),
        }
    }
}

impl fmt::Display for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kraken => f.write_str("Kraken"),
            Self::KrakenFutures => f.write_str("KrakenFutures"),
            Self::IBKR => f.write_str("IBKR"),
            Self::Paper => f.write_str("Paper"),
        }
    }
}

impl OrderSide {
    /// Returns the opposite side: Buy → Sell, Sell → Buy.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
}

impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buy => f.write_str("Buy"),
            Self::Sell => f.write_str("Sell"),
        }
    }
}

impl fmt::Display for OrderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Market => f.write_str("Market"),
            Self::Limit => f.write_str("Limit"),
            Self::StopLoss => f.write_str("StopLoss"),
            Self::StopLossLimit => f.write_str("StopLossLimit"),
            Self::TakeProfit => f.write_str("TakeProfit"),
            Self::TakeProfitLimit => f.write_str("TakeProfitLimit"),
        }
    }
}

impl fmt::Display for OptionRight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Call => f.write_str("Call"),
            Self::Put => f.write_str("Put"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asset_class_display() {
        assert_eq!(AssetClass::Equity.to_string(), "Equity");
        assert_eq!(AssetClass::CryptoFuture.to_string(), "CryptoFuture");
    }

    #[test]
    fn test_exchange_display() {
        assert_eq!(Exchange::Kraken.to_string(), "Kraken");
        assert_eq!(Exchange::IBKR.to_string(), "IBKR");
        assert_eq!(Exchange::Paper.to_string(), "Paper");
    }

    #[test]
    fn test_order_side_display() {
        assert_eq!(OrderSide::Buy.to_string(), "Buy");
        assert_eq!(OrderSide::Sell.to_string(), "Sell");
    }

    #[test]
    fn test_order_type_display() {
        assert_eq!(OrderType::Market.to_string(), "Market");
        assert_eq!(OrderType::StopLossLimit.to_string(), "StopLossLimit");
    }

    #[test]
    fn test_option_right_display() {
        assert_eq!(OptionRight::Call.to_string(), "Call");
        assert_eq!(OptionRight::Put.to_string(), "Put");
    }

    #[test]
    fn test_asset_class_copy() {
        let a = AssetClass::Equity;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn test_order_side_copy() {
        let a = OrderSide::Buy;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn test_enum_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let side = OrderSide::Buy;
        let json = serde_json::to_string(&side)?;
        let deserialized: OrderSide = serde_json::from_str(&json)?;
        assert_eq!(side, deserialized);

        let asset = AssetClass::CryptoSpot;
        let json = serde_json::to_string(&asset)?;
        let deserialized: AssetClass = serde_json::from_str(&json)?;
        assert_eq!(asset, deserialized);

        let exchange = Exchange::KrakenFutures;
        let json = serde_json::to_string(&exchange)?;
        let deserialized: Exchange = serde_json::from_str(&json)?;
        assert_eq!(exchange, deserialized);

        Ok(())
    }

    #[test]
    fn test_exchange_as_str_lowercase() {
        assert_eq!(Exchange::Kraken.as_str_lowercase(), "kraken");
        assert_eq!(Exchange::KrakenFutures.as_str_lowercase(), "kraken_futures");
        assert_eq!(Exchange::IBKR.as_str_lowercase(), "ibkr");
        assert_eq!(Exchange::Paper.as_str_lowercase(), "paper");
    }

    #[test]
    fn test_order_side_opposite() {
        assert_eq!(OrderSide::Buy.opposite(), OrderSide::Sell);
        assert_eq!(OrderSide::Sell.opposite(), OrderSide::Buy);
    }

    #[test]
    fn test_crypto_contract_type_serde() -> Result<(), Box<dyn std::error::Error>> {
        let ct = CryptoContractType::PerpetualInverse;
        let json = serde_json::to_string(&ct)?;
        let deserialized: CryptoContractType = serde_json::from_str(&json)?;
        assert_eq!(ct, deserialized);
        Ok(())
    }
}
