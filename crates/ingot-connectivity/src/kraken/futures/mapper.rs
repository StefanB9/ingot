use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_core::{
    Balance, Instrument, InstrumentDetails, OpenOrder, OrderBookLevel, OrderBookSnapshot,
    OrderFill, OrderId, OrderRequest, OrderStatus, Position, Tick, TickerSnapshot,
};
use ingot_primitives::{
    Amount, AssetClass, CryptoContractType, Currency, Exchange, OrderSide, Price, Quantity, Symbol,
    TimeInForce,
};
use rust_decimal::Decimal;
use smol_str::SmolStr;

use super::models::{
    FuturesAccounts, FuturesFill, FuturesInstrument, FuturesOpenOrder, FuturesOrderBook,
    FuturesPosition, FuturesTicker, FuturesTrade, FuturesWsBookLevel, FuturesWsFillEntry,
    FuturesWsTickerEntry, FuturesWsTradeEntry,
};
use crate::kraken::common::{map_kraken_order_type, map_kraken_side, parse_decimal};

/// Infer contract type from Kraken Futures symbol prefix.
pub(crate) fn futures_symbol_to_contract_type(symbol: &str) -> anyhow::Result<CryptoContractType> {
    if symbol.starts_with("PF_") {
        Ok(CryptoContractType::PerpetualLinear)
    } else if symbol.starts_with("PI_") {
        Ok(CryptoContractType::PerpetualInverse)
    } else if symbol.starts_with("FI_") {
        Ok(CryptoContractType::FixedInverse)
    } else if symbol.starts_with("FF_") {
        Ok(CryptoContractType::FixedLinear)
    } else {
        Err(anyhow::anyhow!(
            "unknown Kraken Futures symbol prefix: {symbol}"
        ))
    }
}

/// Parse a Kraken Futures pair string like `"xbt:usd"` into (base, quote)
/// currencies.
pub(crate) fn futures_pair_to_currencies(pair: &str) -> anyhow::Result<(Currency, Currency)> {
    let parts: Vec<&str> = pair.split(':').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!("invalid futures pair format: {pair}"));
    }
    let base = Currency::from_str_lossy(&parts[0].to_uppercase());
    let quote = Currency::from_str_lossy(&parts[1].to_uppercase());
    Ok((base, quote))
}

/// Map a Kraken Futures instrument to a domain `Instrument`.
pub(crate) fn map_futures_instrument(inst: &FuturesInstrument) -> anyhow::Result<Instrument> {
    let symbol = Symbol::new(&inst.symbol).context("invalid futures symbol")?;
    let contract_type = futures_symbol_to_contract_type(&inst.symbol)?;

    let (base_currency, quote_currency) = if let Some(ref pair) = inst.pair {
        futures_pair_to_currencies(pair)?
    } else {
        (Currency::from_str_lossy("UNKNOWN"), Currency::USD)
    };

    let tick_size = if let Some(ref ts) = inst.tick_size {
        Price::new(parse_decimal(ts, "tick_size")?)
    } else {
        Price::new(Decimal::new(1, 2)) // default 0.01
    };

    let max_position_size = if let Some(ref mps) = inst.max_position_size {
        Quantity::new(parse_decimal(mps, "max_position_size")?)
            .context("invalid max_position_size")?
    } else {
        Quantity::zero()
    };

    let initial_margin = if let Some(ref imr) = inst.initial_margin_rate {
        ingot_primitives::Percentage::new(parse_decimal(imr, "initial_margin_rate")?)
            .context("invalid initial_margin_rate")?
    } else {
        ingot_primitives::Percentage::new(Decimal::ZERO).context("zero percentage")?
    };

    let maintenance_margin = if let Some(ref mmr) = inst.maintenance_margin_rate {
        ingot_primitives::Percentage::new(parse_decimal(mmr, "maintenance_margin_rate")?)
            .context("invalid maintenance_margin_rate")?
    } else {
        ingot_primitives::Percentage::new(Decimal::ZERO).context("zero percentage")?
    };

    let expiry = if let Some(ref ltt) = inst.last_trading_time {
        ltt.parse::<DateTime<Utc>>().ok()
    } else {
        None
    };

    let display_name = SmolStr::new(&inst.symbol);

    Ok(Instrument {
        symbol,
        asset_class: AssetClass::CryptoFuture,
        exchange: Exchange::KrakenFutures,
        base_currency,
        quote_currency,
        tick_size,
        display_name,
        details: InstrumentDetails::CryptoFuture {
            contract_type,
            expiry,
            max_position_size,
            initial_margin,
            maintenance_margin,
        },
    })
}

/// Map a Kraken Futures ticker to a domain `TickerSnapshot`.
pub(crate) fn map_futures_ticker(
    symbol: &Symbol,
    ticker: &FuturesTicker,
) -> anyhow::Result<TickerSnapshot> {
    let bid = Price::new(parse_decimal(ticker.bid.as_deref().unwrap_or("0"), "bid")?);
    let ask = Price::new(parse_decimal(ticker.ask.as_deref().unwrap_or("0"), "ask")?);
    let last = Price::new(parse_decimal(
        ticker.last.as_deref().unwrap_or("0"),
        "last",
    )?);
    let volume_24h = Quantity::new(parse_decimal(
        ticker.vol24h.as_deref().unwrap_or("0"),
        "vol24h",
    )?)
    .context("invalid vol24h")?;

    Ok(TickerSnapshot {
        symbol: symbol.clone(),
        bid,
        ask,
        last,
        volume_24h,
        timestamp: Utc::now(),
    })
}

/// Map a Kraken Futures order book to an `OrderBookSnapshot`.
pub(crate) fn map_futures_order_book(
    symbol: &Symbol,
    book: &FuturesOrderBook,
) -> anyhow::Result<OrderBookSnapshot> {
    let bids = book
        .bids
        .iter()
        .map(|level| {
            #[allow(clippy::cast_possible_truncation)]
            let price = Decimal::try_from(level.0).context("invalid bid price")?;
            #[allow(clippy::cast_possible_truncation)]
            let qty = Decimal::try_from(level.1).context("invalid bid qty")?;
            Ok(OrderBookLevel {
                price: Price::new(price),
                quantity: Quantity::new(qty).context("invalid bid quantity")?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map bid levels")?;

    let asks = book
        .asks
        .iter()
        .map(|level| {
            #[allow(clippy::cast_possible_truncation)]
            let price = Decimal::try_from(level.0).context("invalid ask price")?;
            #[allow(clippy::cast_possible_truncation)]
            let qty = Decimal::try_from(level.1).context("invalid ask qty")?;
            Ok(OrderBookLevel {
                price: Price::new(price),
                quantity: Quantity::new(qty).context("invalid ask quantity")?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map ask levels")?;

    Ok(OrderBookSnapshot {
        symbol: symbol.clone(),
        bids,
        asks,
        timestamp: Utc::now(),
    })
}

/// Map a Kraken Futures public trade to a domain `Tick`.
pub(crate) fn map_futures_trade(symbol: &Symbol, trade: &FuturesTrade) -> anyhow::Result<Tick> {
    let time: DateTime<Utc> = trade.time.parse().context("invalid trade timestamp")?;
    let side = match trade.side.as_str() {
        "buy" => Some(OrderSide::Buy),
        "sell" => Some(OrderSide::Sell),
        _ => None,
    };

    Ok(Tick {
        time,
        symbol: symbol.clone(),
        exchange: SmolStr::new("kraken_futures"),
        price: Price::new(parse_decimal(&trade.price, "trade price")?),
        quantity: Quantity::new(parse_decimal(&trade.size, "trade size")?)
            .context("invalid trade size")?,
        side,
        trade_id: trade.uid.as_deref().map(SmolStr::new),
    })
}

/// Map a Kraken Futures open order to a domain `OpenOrder`.
pub(crate) fn map_futures_open_order(order: &FuturesOpenOrder) -> anyhow::Result<OpenOrder> {
    let side = map_kraken_side(&order.side)?;
    let order_type = map_kraken_order_type(&order.order_type)?;
    let qty = parse_decimal(&order.quantity, "quantity")?;
    let filled = parse_decimal(&order.filled_quantity, "filled_quantity")?;
    let remaining = qty - filled;

    let limit_price = if let Some(ref lp) = order.limit_price {
        Some(Price::new(parse_decimal(lp, "limit_price")?))
    } else {
        None
    };

    let status = match order.status.as_str() {
        "untouched" => OrderStatus::Open,
        "partiallyFilled" => OrderStatus::PartiallyFilled,
        "filled" => OrderStatus::Filled,
        "cancelled" => OrderStatus::Cancelled,
        other => {
            return Err(anyhow::anyhow!(
                "unknown Kraken Futures order status: {other}"
            ));
        }
    };

    let symbol = Symbol::new(&order.symbol).context("invalid symbol in open order")?;

    let created_at = if let Some(ref rt) = order.received_time {
        rt.parse::<DateTime<Utc>>()
            .context("invalid receivedTime")?
    } else {
        Utc::now()
    };

    Ok(OpenOrder {
        order_id: OrderId::new(&order.order_id).context("invalid order_id")?,
        request: OrderRequest {
            symbol,
            side,
            order_type,
            quantity: Quantity::new(qty).context("invalid order quantity")?,
            limit_price,
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        },
        status,
        filled_quantity: Quantity::new(filled).context("invalid filled_quantity")?,
        remaining_quantity: Quantity::new(remaining).context("invalid remaining_quantity")?,
        average_fill_price: None,
        created_at,
    })
}

/// Map a Kraken Futures position to a domain `Position`.
pub(crate) fn map_futures_position(pos: &FuturesPosition) -> anyhow::Result<Position> {
    let side = match pos.side.as_str() {
        "long" => OrderSide::Buy,
        "short" => OrderSide::Sell,
        other => return Err(anyhow::anyhow!("unknown position side: {other}")),
    };

    let unrealized_pnl = if let Some(ref uf) = pos.unrealized_funding {
        Some(Amount::new(parse_decimal(uf, "unrealized_funding")?))
    } else {
        None
    };

    Ok(Position {
        symbol: Symbol::new(&pos.symbol).context("invalid position symbol")?,
        side,
        quantity: Quantity::new(parse_decimal(&pos.size, "position size")?)
            .context("invalid position size")?,
        average_entry_price: Price::new(parse_decimal(&pos.price, "position price")?),
        unrealized_pnl,
        liquidation_price: None,
    })
}

/// Map Kraken Futures accounts to a list of `Balance`.
pub(crate) fn map_futures_balances(accounts: &FuturesAccounts) -> anyhow::Result<Vec<Balance>> {
    let Some(flex) = &accounts.flex else {
        return Ok(vec![]);
    };

    let Some(balances_map) = &flex.balances else {
        return Ok(vec![]);
    };

    let mut balances = Vec::new();
    for (currency_str, amount_str) in balances_map {
        let total = parse_decimal(amount_str, &format!("balance for {currency_str}"))?;
        if total == Decimal::ZERO {
            continue;
        }
        balances.push(Balance {
            currency: Currency::from_str_lossy(&currency_str.to_uppercase()),
            total: Amount::new(total),
            available: Amount::new(total),
            held: Amount::new(Decimal::ZERO),
        });
    }
    Ok(balances)
}

/// Map a Kraken Futures fill to a domain `OrderFill`.
pub(crate) fn map_futures_fill(fill: &FuturesFill) -> anyhow::Result<OrderFill> {
    let side = map_kraken_side(&fill.side)?;
    let timestamp: DateTime<Utc> = fill.fill_time.parse().context("invalid fill_time")?;

    Ok(OrderFill {
        order_id: OrderId::new(&fill.order_id).context("invalid fill order_id")?,
        symbol: Symbol::new(&fill.symbol).context("invalid fill symbol")?,
        side,
        fill_price: Price::new(parse_decimal(&fill.price, "fill price")?),
        fill_quantity: Quantity::new(parse_decimal(&fill.size, "fill size")?)
            .context("invalid fill size")?,
        fee: Amount::new(parse_decimal(
            fill.fee.as_deref().unwrap_or("0"),
            "fill fee",
        )?),
        fee_currency: Currency::USD,
        timestamp,
        trade_id: Some(SmolStr::new(&fill.fill_id)),
    })
}

// ---- WebSocket mappers ----

/// Map a WS trade entry to a domain `Tick`.
pub(crate) fn map_ws_futures_trade(
    product_id: &str,
    entry: &FuturesWsTradeEntry,
) -> anyhow::Result<Tick> {
    let symbol = Symbol::new(product_id).context("invalid ws trade product_id")?;
    let time = DateTime::from_timestamp_millis(entry.time).context("invalid ws trade timestamp")?;
    let side = match entry.side.as_str() {
        "buy" => Some(OrderSide::Buy),
        "sell" => Some(OrderSide::Sell),
        _ => None,
    };

    Ok(Tick {
        time,
        symbol,
        exchange: SmolStr::new("kraken_futures"),
        price: Price::new(parse_decimal(&entry.price, "ws trade price")?),
        quantity: Quantity::new(parse_decimal(&entry.qty, "ws trade qty")?)
            .context("invalid ws trade qty")?,
        side,
        trade_id: entry.uid.as_deref().map(SmolStr::new),
    })
}

/// Map a WS ticker entry to a domain `TickerSnapshot`.
pub(crate) fn map_ws_futures_ticker(
    entry: &FuturesWsTickerEntry,
) -> anyhow::Result<TickerSnapshot> {
    let symbol = Symbol::new(&entry.product_id).context("invalid ws ticker product_id")?;

    Ok(TickerSnapshot {
        symbol,
        bid: Price::new(parse_decimal(
            entry.bid.as_deref().unwrap_or("0"),
            "ws ticker bid",
        )?),
        ask: Price::new(parse_decimal(
            entry.ask.as_deref().unwrap_or("0"),
            "ws ticker ask",
        )?),
        last: Price::new(parse_decimal(
            entry.last.as_deref().unwrap_or("0"),
            "ws ticker last",
        )?),
        volume_24h: Quantity::new(parse_decimal(
            entry.volume.as_deref().unwrap_or("0"),
            "ws ticker volume",
        )?)
        .context("invalid ws ticker volume")?,
        timestamp: Utc::now(),
    })
}

/// Map a WS book level to a domain `OrderBookLevel`.
pub(crate) fn map_ws_futures_book_level(
    level: &FuturesWsBookLevel,
) -> anyhow::Result<OrderBookLevel> {
    Ok(OrderBookLevel {
        price: Price::new(parse_decimal(&level.price, "ws book price")?),
        quantity: Quantity::new(parse_decimal(&level.qty, "ws book qty")?)
            .context("invalid ws book qty")?,
    })
}

/// Map a WS fill entry to a domain `OrderFill`.
pub(crate) fn map_ws_futures_fill(entry: &FuturesWsFillEntry) -> anyhow::Result<OrderFill> {
    let symbol = Symbol::new(&entry.instrument).context("invalid ws fill instrument")?;
    let side = map_kraken_side(&entry.side)?;
    let timestamp =
        DateTime::from_timestamp_millis(entry.time).context("invalid ws fill timestamp")?;
    let fee_currency = entry.fee_currency.as_deref().map_or(Currency::USD, |s| {
        Currency::from_str_lossy(&s.to_uppercase())
    });

    Ok(OrderFill {
        order_id: OrderId::new(&entry.order_id).context("invalid ws fill order_id")?,
        symbol,
        side,
        fill_price: Price::new(parse_decimal(&entry.price, "ws fill price")?),
        fill_quantity: Quantity::new(parse_decimal(&entry.qty, "ws fill qty")?)
            .context("invalid ws fill qty")?,
        fee: Amount::new(parse_decimal(
            entry.fee_paid.as_deref().unwrap_or("0"),
            "ws fill fee",
        )?),
        fee_currency,
        timestamp,
        trade_id: Some(SmolStr::new(&entry.fill_id)),
    })
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;
    use crate::kraken::futures::models::*;

    #[test]
    fn test_futures_symbol_to_contract_type_perpetual_linear() -> anyhow::Result<()> {
        assert_eq!(
            futures_symbol_to_contract_type("PF_XBTUSD")?,
            CryptoContractType::PerpetualLinear
        );
        Ok(())
    }

    #[test]
    fn test_futures_symbol_to_contract_type_perpetual_inverse() -> anyhow::Result<()> {
        assert_eq!(
            futures_symbol_to_contract_type("PI_XBTUSD")?,
            CryptoContractType::PerpetualInverse
        );
        Ok(())
    }

    #[test]
    fn test_futures_symbol_to_contract_type_fixed() -> anyhow::Result<()> {
        assert_eq!(
            futures_symbol_to_contract_type("FI_XBTUSD_250328")?,
            CryptoContractType::FixedInverse
        );
        assert_eq!(
            futures_symbol_to_contract_type("FF_ETHUSD_250328")?,
            CryptoContractType::FixedLinear
        );
        Ok(())
    }

    #[test]
    fn test_futures_symbol_to_contract_type_unknown() {
        assert!(futures_symbol_to_contract_type("XBTUSD").is_err());
    }

    #[test]
    fn test_futures_pair_to_currencies() -> anyhow::Result<()> {
        let (base, quote) = futures_pair_to_currencies("xbt:usd")?;
        assert_eq!(base, Currency::from_str_lossy("XBT"));
        assert_eq!(quote, Currency::USD);
        Ok(())
    }

    #[test]
    fn test_futures_pair_to_currencies_invalid() {
        assert!(futures_pair_to_currencies("xbtusd").is_err());
    }

    #[test]
    fn test_map_futures_instrument() -> anyhow::Result<()> {
        let inst = FuturesInstrument {
            symbol: "PF_XBTUSD".into(),
            instrument_type: "perpetual".into(),
            underlying: Some("rr_xbtusd".into()),
            tick_size: Some("0.5".into()),
            contract_size: Some("1".into()),
            max_position_size: Some("500000".into()),
            initial_margin_rate: Some("0.02".into()),
            maintenance_margin_rate: Some("0.01".into()),
            last_trading_time: None,
            pair: Some("xbt:usd".into()),
        };
        let mapped = map_futures_instrument(&inst)?;

        assert_eq!(mapped.symbol.as_str(), "PF_XBTUSD");
        assert_eq!(mapped.asset_class, AssetClass::CryptoFuture);
        assert_eq!(mapped.exchange, Exchange::KrakenFutures);
        assert_eq!(mapped.tick_size.value(), dec!(0.5));
        if let InstrumentDetails::CryptoFuture {
            contract_type,
            max_position_size,
            ..
        } = &mapped.details
        {
            assert_eq!(*contract_type, CryptoContractType::PerpetualLinear);
            assert_eq!(max_position_size.value(), dec!(500000));
        } else {
            return Err(anyhow::anyhow!("expected CryptoFuture details"));
        }
        Ok(())
    }

    #[test]
    fn test_map_futures_ticker() -> anyhow::Result<()> {
        let symbol = Symbol::new("PF_XBTUSD")?;
        let ticker = FuturesTicker {
            symbol: "PF_XBTUSD".into(),
            bid: Some("67000.0".into()),
            ask: Some("67010.0".into()),
            last: Some("67005.0".into()),
            vol24h: Some("15000.5".into()),
            mark_price: Some("67002.0".into()),
            open_interest: None,
        };
        let snap = map_futures_ticker(&symbol, &ticker)?;
        assert_eq!(snap.bid.value(), dec!(67000.0));
        assert_eq!(snap.ask.value(), dec!(67010.0));
        assert_eq!(snap.volume_24h.value(), dec!(15000.5));
        Ok(())
    }

    #[test]
    fn test_map_futures_order_book() -> anyhow::Result<()> {
        let symbol = Symbol::new("PF_XBTUSD")?;
        let book = FuturesOrderBook {
            bids: vec![FuturesBookLevel(67000.0, 3.5)],
            asks: vec![FuturesBookLevel(67010.0, 2.0)],
        };
        let snap = map_futures_order_book(&symbol, &book)?;
        assert_eq!(snap.bids.len(), 1);
        assert_eq!(snap.asks.len(), 1);
        Ok(())
    }

    #[test]
    fn test_map_futures_trade() -> anyhow::Result<()> {
        let symbol = Symbol::new("PF_XBTUSD")?;
        let trade = FuturesTrade {
            uid: Some("trade-1".into()),
            side: "buy".into(),
            symbol: "PF_XBTUSD".into(),
            price: "67000.5".into(),
            size: "0.01".into(),
            time: "2024-01-15T10:30:00Z".into(),
        };
        let tick = map_futures_trade(&symbol, &trade)?;
        assert_eq!(tick.price.value(), dec!(67000.5));
        assert_eq!(tick.side, Some(OrderSide::Buy));
        assert_eq!(tick.trade_id.as_deref(), Some("trade-1"));
        Ok(())
    }

    #[test]
    fn test_map_futures_open_order() -> anyhow::Result<()> {
        let order = FuturesOpenOrder {
            order_id: "ord-123".into(),
            symbol: "PF_XBTUSD".into(),
            side: "buy".into(),
            order_type: "lmt".into(),
            quantity: "10".into(),
            filled_quantity: "5".into(),
            limit_price: Some("65000.0".into()),
            status: "partiallyFilled".into(),
            received_time: Some("2024-01-15T10:30:00Z".into()),
        };
        let mapped = map_futures_open_order(&order)?;
        assert_eq!(mapped.order_id.as_str(), "ord-123");
        assert_eq!(mapped.status, OrderStatus::PartiallyFilled);
        assert_eq!(mapped.filled_quantity.value(), dec!(5));
        assert_eq!(mapped.remaining_quantity.value(), dec!(5));
        Ok(())
    }

    #[test]
    fn test_map_futures_position() -> anyhow::Result<()> {
        let pos = FuturesPosition {
            symbol: "PF_XBTUSD".into(),
            side: "long".into(),
            size: "100".into(),
            price: "67000.0".into(),
            unrealized_funding: Some("12.50".into()),
        };
        let mapped = map_futures_position(&pos)?;
        assert_eq!(mapped.side, OrderSide::Buy);
        assert_eq!(mapped.quantity.value(), dec!(100));
        assert!(mapped.unrealized_pnl.is_some());
        Ok(())
    }

    #[test]
    fn test_map_futures_balances() -> anyhow::Result<()> {
        let accounts = FuturesAccounts {
            flex: Some(FuturesFlexAccount {
                available_margin: Some("5000.00".into()),
                portfolio_value: Some("15000.00".into()),
                balances: Some(
                    [
                        ("xbt".into(), "0.5".into()),
                        ("usd".into(), "10000.00".into()),
                    ]
                    .into_iter()
                    .collect(),
                ),
            }),
        };
        let balances = map_futures_balances(&accounts)?;
        assert_eq!(balances.len(), 2);
        Ok(())
    }

    #[test]
    fn test_map_futures_fill() -> anyhow::Result<()> {
        let fill = FuturesFill {
            fill_id: "fill-1".into(),
            order_id: "ord-1".into(),
            symbol: "PF_XBTUSD".into(),
            side: "buy".into(),
            price: "67000.0".into(),
            size: "0.01".into(),
            fee: Some("0.05".into()),
            fill_time: "2024-01-15T10:30:00Z".into(),
        };
        let mapped = map_futures_fill(&fill)?;
        assert_eq!(mapped.fill_price.value(), dec!(67000.0));
        assert_eq!(mapped.fee.value(), dec!(0.05));
        assert_eq!(mapped.trade_id.as_deref(), Some("fill-1"));
        Ok(())
    }

    #[test]
    fn test_map_ws_futures_trade() -> anyhow::Result<()> {
        let entry = FuturesWsTradeEntry {
            side: "buy".into(),
            price: "67000.5".into(),
            qty: "0.01".into(),
            time: 1_705_312_200_000,
            uid: Some("uid-1".into()),
        };
        let tick = map_ws_futures_trade("PF_XBTUSD", &entry)?;
        assert_eq!(tick.symbol.as_str(), "PF_XBTUSD");
        assert_eq!(tick.side, Some(OrderSide::Buy));
        assert!(tick.trade_id.is_some());
        Ok(())
    }

    #[test]
    fn test_map_ws_futures_ticker() -> anyhow::Result<()> {
        let entry = FuturesWsTickerEntry {
            product_id: "PF_XBTUSD".into(),
            bid: Some("67000.0".into()),
            ask: Some("67010.0".into()),
            last: Some("67005.0".into()),
            volume: Some("5000.0".into()),
            mark_price: None,
        };
        let snap = map_ws_futures_ticker(&entry)?;
        assert_eq!(snap.symbol.as_str(), "PF_XBTUSD");
        assert_eq!(snap.bid.value(), dec!(67000.0));
        Ok(())
    }

    #[test]
    fn test_map_ws_futures_book_level() -> anyhow::Result<()> {
        let level = FuturesWsBookLevel {
            price: "67010.50".into(),
            qty: "2.5".into(),
        };
        let mapped = map_ws_futures_book_level(&level)?;
        assert_eq!(mapped.price.value(), dec!(67010.50));
        assert_eq!(mapped.quantity.value(), dec!(2.5));
        Ok(())
    }

    #[test]
    fn test_map_ws_futures_fill() -> anyhow::Result<()> {
        let entry = FuturesWsFillEntry {
            instrument: "PF_XBTUSD".into(),
            side: "buy".into(),
            price: "67000.0".into(),
            qty: "0.01".into(),
            order_id: "ord-1".into(),
            fill_id: "fill-1".into(),
            fee_paid: Some("0.05".into()),
            fee_currency: Some("USD".into()),
            time: 1_705_312_200_000,
        };
        let fill = map_ws_futures_fill(&entry)?;
        assert_eq!(fill.symbol.as_str(), "PF_XBTUSD");
        assert_eq!(fill.fee.value(), dec!(0.05));
        assert_eq!(fill.fee_currency, Currency::USD);
        Ok(())
    }
}
