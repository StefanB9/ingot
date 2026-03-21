use std::str::FromStr;

use anyhow::Context;
use ingot_primitives::{OrderSide, OrderType, TimeInForce};
use rust_decimal::Decimal;

/// Convert domain `OrderSide` to Kraken string.
pub(crate) fn order_side_to_kraken(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "buy",
        OrderSide::Sell => "sell",
    }
}

/// Convert domain `OrderType` to Kraken string.
pub(crate) fn order_type_to_kraken(ot: OrderType) -> &'static str {
    match ot {
        OrderType::Market => "market",
        OrderType::Limit => "limit",
        OrderType::StopLoss => "stop-loss",
        OrderType::StopLossLimit => "stop-loss-limit",
        OrderType::TakeProfit => "take-profit",
        OrderType::TakeProfitLimit => "take-profit-limit",
    }
}

/// Convert domain `TimeInForce` to Kraken's optional `timeinforce` parameter.
///
/// Returns `Ok(None)` for GTC (Kraken default), `Ok(Some(...))` for supported
/// values, and `Err(...)` for unsupported values like `Day`.
pub(crate) fn time_in_force_to_kraken(tif: TimeInForce) -> anyhow::Result<Option<&'static str>> {
    match tif {
        TimeInForce::GoodTilCancelled => Ok(None),
        TimeInForce::ImmediateOrCancel => Ok(Some("IOC")),
        TimeInForce::FillOrKill => Ok(Some("FOK")),
        TimeInForce::Day => Err(anyhow::anyhow!("Kraken does not support Day time-in-force")),
        TimeInForce::GoodTilDate(_) => Err(anyhow::anyhow!(
            "Kraken does not support GoodTilDate time-in-force"
        )),
    }
}

/// Parse Kraken side string to domain `OrderSide`.
pub(crate) fn map_kraken_side(s: &str) -> anyhow::Result<OrderSide> {
    match s {
        "buy" => Ok(OrderSide::Buy),
        "sell" => Ok(OrderSide::Sell),
        other => Err(anyhow::anyhow!("unknown Kraken order side: {other}")),
    }
}

/// Parse Kraken order type string to domain `OrderType`.
pub(crate) fn map_kraken_order_type(s: &str) -> anyhow::Result<OrderType> {
    match s {
        "market" => Ok(OrderType::Market),
        "limit" | "lmt" => Ok(OrderType::Limit),
        "stop-loss" | "stp" => Ok(OrderType::StopLoss),
        "stop-loss-limit" => Ok(OrderType::StopLossLimit),
        "take-profit" | "take_profit" => Ok(OrderType::TakeProfit),
        "take-profit-limit" => Ok(OrderType::TakeProfitLimit),
        other => Err(anyhow::anyhow!("unknown Kraken order type: {other}")),
    }
}

/// Parse a decimal string with context on error.
pub(crate) fn parse_decimal(s: &str, field: &str) -> anyhow::Result<Decimal> {
    Decimal::from_str(s).with_context(|| format!("failed to parse {field}: {s}"))
}
