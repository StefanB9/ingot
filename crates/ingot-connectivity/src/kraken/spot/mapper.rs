use std::str::FromStr;

use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_core::{
    Balance, Instrument, InstrumentDetails, OhlcvBar, OpenOrder, OrderBookLevel, OrderBookSnapshot,
    OrderFill, OrderId, OrderRequest, OrderStatus, Tick, TickerSnapshot,
};
use ingot_primitives::{
    Amount, AssetClass, Currency, Exchange, OrderSide, OrderType, Price, Quantity, Symbol,
    TimeInForce,
};
use rust_decimal::Decimal;
use smol_str::SmolStr;

use super::models::{
    KrakenAssetPair, KrakenBookLevel, KrakenOhlcTuple, KrakenOpenOrder, KrakenOrderBook,
    KrakenTickerInfo, KrakenTradeHistoryEntry, KrakenTradeTuple, KrakenWsBookLevel,
    KrakenWsExecEntry, KrakenWsTickerEntry, KrakenWsTradeEntry,
};

/// Strip Kraken's legacy X/Z prefix from asset codes and convert to Currency.
///
/// Kraken prefixes some assets: `XXBT` → BTC, `ZUSD` → USD, `XETH` → ETH.
/// Newer assets like `SOL` have no prefix.
pub(crate) fn kraken_asset_to_currency(asset: &str) -> Currency {
    let stripped = match asset.len() {
        // 4-char codes with X or Z prefix are legacy Kraken notation
        4 if asset.starts_with('X') || asset.starts_with('Z') => &asset[1..],
        _ => asset,
    };
    Currency::from_str_lossy(stripped)
}

/// Convert a human-readable interval like `"1m"`, `"1h"`, `"1d"` to Kraken's
/// integer-minute format.
pub(crate) fn interval_to_kraken(interval: &str) -> anyhow::Result<String> {
    let result = match interval {
        "1m" | "1" => "1",
        "5m" | "5" => "5",
        "15m" | "15" => "15",
        "30m" | "30" => "30",
        "1h" | "60" => "60",
        "4h" | "240" => "240",
        "1d" | "1440" => "1440",
        "1w" | "10080" => "10080",
        "15d" | "21600" => "21600",
        other => {
            return Err(anyhow::anyhow!("unsupported OHLC interval: {other}"));
        }
    };
    Ok(result.to_owned())
}

/// Convert Kraken's integer-minute interval to canonical human-readable format.
pub(crate) fn interval_to_canonical(kraken_interval: &str) -> SmolStr {
    let canonical = match kraken_interval {
        "1" => "1m",
        "5" => "5m",
        "15" => "15m",
        "30" => "30m",
        "60" => "1h",
        "240" => "4h",
        "1440" => "1d",
        "10080" => "1w",
        "21600" => "15d",
        other => other,
    };
    SmolStr::new(canonical)
}

/// Map a Kraken asset pair to an `Instrument`.
pub(crate) fn map_asset_pair(
    pair_name: &str,
    pair: &KrakenAssetPair,
) -> anyhow::Result<Instrument> {
    let symbol = Symbol::new(pair_name).context("invalid pair name")?;

    let base_currency = kraken_asset_to_currency(&pair.base);
    let quote_currency = kraken_asset_to_currency(&pair.quote);

    let tick_size = if let Some(ref ts) = pair.tick_size {
        Price::new(Decimal::from_str(ts).with_context(|| format!("invalid tick_size: {ts}"))?)
    } else {
        Price::new(Decimal::new(1, u32::from(pair.pair_decimals)))
    };

    let order_min = match &pair.ordermin {
        Some(v) => {
            Quantity::new(Decimal::from_str(v).with_context(|| format!("invalid ordermin: {v}"))?)
                .context("invalid ordermin quantity")?
        }
        None => Quantity::zero(),
    };

    let cost_min = match &pair.costmin {
        Some(v) => {
            Amount::new(Decimal::from_str(v).with_context(|| format!("invalid costmin: {v}"))?)
        }
        None => Amount::new(Decimal::ZERO),
    };

    let display_name = SmolStr::new(pair.wsname.as_deref().unwrap_or(&pair.altname));

    Ok(Instrument {
        symbol,
        asset_class: AssetClass::CryptoSpot,
        exchange: Exchange::Kraken,
        base_currency,
        quote_currency,
        tick_size,
        display_name,
        details: InstrumentDetails::CryptoSpot {
            order_min,
            cost_min,
            lot_decimals: pair.lot_decimals,
            margin_eligible: !pair.leverage_buy.is_empty(),
            leverage_tiers: pair.leverage_buy.clone(),
        },
    })
}

/// Map a Kraken OHLC tuple to an `OhlcvBar`.
pub(crate) fn map_ohlc_bar(
    symbol: &Symbol,
    kraken_interval: &str,
    bar: &KrakenOhlcTuple,
) -> anyhow::Result<OhlcvBar> {
    let time = DateTime::from_timestamp(bar.0, 0).context("invalid OHLC timestamp")?;

    Ok(OhlcvBar {
        time,
        symbol: symbol.clone(),
        exchange: SmolStr::new("kraken"),
        interval: interval_to_canonical(kraken_interval),
        open: Price::new(parse_decimal(&bar.1, "open")?),
        high: Price::new(parse_decimal(&bar.2, "high")?),
        low: Price::new(parse_decimal(&bar.3, "low")?),
        close: Price::new(parse_decimal(&bar.4, "close")?),
        volume: Quantity::new(parse_decimal(&bar.6, "volume")?).context("invalid OHLC volume")?,
        trade_count: Some(i32::try_from(bar.7).context("trade count overflow")?),
    })
}

/// Map a Kraken trade tuple to a `Tick`.
pub(crate) fn map_trade(symbol: &Symbol, trade: &KrakenTradeTuple) -> anyhow::Result<Tick> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let time = {
        let secs = trade.2.trunc() as i64;
        let nanos = ((trade.2.fract()) * 1_000_000_000.0) as u32;
        DateTime::from_timestamp(secs, nanos).context("invalid trade timestamp")?
    };

    let side = match trade.3.as_str() {
        "b" => Some(OrderSide::Buy),
        "s" => Some(OrderSide::Sell),
        _ => None,
    };

    Ok(Tick {
        time,
        symbol: symbol.clone(),
        exchange: SmolStr::new("kraken"),
        price: Price::new(parse_decimal(&trade.0, "trade price")?),
        quantity: Quantity::new(parse_decimal(&trade.1, "trade volume")?)
            .context("invalid trade volume")?,
        side,
        trade_id: Some(SmolStr::new(trade.6.to_string())),
    })
}

/// Map Kraken ticker info to a `TickerSnapshot`.
pub(crate) fn map_ticker(
    symbol: &Symbol,
    info: &KrakenTickerInfo,
) -> anyhow::Result<TickerSnapshot> {
    let bid = Price::new(parse_decimal(
        info.b.first().context("missing bid price")?,
        "bid",
    )?);
    let ask = Price::new(parse_decimal(
        info.a.first().context("missing ask price")?,
        "ask",
    )?);
    let last = Price::new(parse_decimal(
        info.c.first().context("missing last price")?,
        "last",
    )?);
    let volume_24h = Quantity::new(parse_decimal(
        info.v.get(1).context("missing 24h volume")?,
        "volume_24h",
    )?)
    .context("invalid 24h volume")?;

    Ok(TickerSnapshot {
        symbol: symbol.clone(),
        bid,
        ask,
        last,
        volume_24h,
        timestamp: Utc::now(),
    })
}

/// Map a Kraken order book to an `OrderBookSnapshot`.
pub(crate) fn map_order_book(
    symbol: &Symbol,
    book: &KrakenOrderBook,
) -> anyhow::Result<OrderBookSnapshot> {
    let bids = book
        .bids
        .iter()
        .map(map_book_level)
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map bid levels")?;
    let asks = book
        .asks
        .iter()
        .map(map_book_level)
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map ask levels")?;

    // Use the max timestamp across all levels, or now() if empty
    let max_ts = book
        .bids
        .iter()
        .chain(book.asks.iter())
        .map(|l| l.2)
        .max()
        .unwrap_or(0);

    let timestamp = if max_ts > 0 {
        DateTime::from_timestamp(max_ts, 0).context("invalid book timestamp")?
    } else {
        Utc::now()
    };

    Ok(OrderBookSnapshot {
        symbol: symbol.clone(),
        bids,
        asks,
        timestamp,
    })
}

/// Map a single book level.
pub(crate) fn map_book_level(level: &KrakenBookLevel) -> anyhow::Result<OrderBookLevel> {
    Ok(OrderBookLevel {
        price: Price::new(parse_decimal(&level.0, "book price")?),
        quantity: Quantity::new(parse_decimal(&level.1, "book volume")?)
            .context("invalid book volume")?,
    })
}

// ---- Enum conversions for private endpoints ----

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
        TimeInForce::Day => Err(anyhow::anyhow!(
            "Kraken spot does not support Day time-in-force"
        )),
        TimeInForce::GoodTilDate(_) => Err(anyhow::anyhow!(
            "Kraken spot does not support GoodTilDate time-in-force"
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
        "limit" => Ok(OrderType::Limit),
        "stop-loss" => Ok(OrderType::StopLoss),
        "stop-loss-limit" => Ok(OrderType::StopLossLimit),
        "take-profit" => Ok(OrderType::TakeProfit),
        "take-profit-limit" => Ok(OrderType::TakeProfitLimit),
        other => Err(anyhow::anyhow!("unknown Kraken order type: {other}")),
    }
}

/// Map Kraken order status + executed volume to domain `OrderStatus`.
pub(crate) fn map_order_status(status: &str, vol_exec: &Decimal) -> anyhow::Result<OrderStatus> {
    match status {
        "pending" => Ok(OrderStatus::Pending),
        "open" => {
            if vol_exec > &Decimal::ZERO {
                Ok(OrderStatus::PartiallyFilled)
            } else {
                Ok(OrderStatus::Open)
            }
        }
        "closed" => Ok(OrderStatus::Filled),
        "canceled" | "cancelled" => Ok(OrderStatus::Cancelled),
        "expired" => Ok(OrderStatus::Expired),
        other => Err(anyhow::anyhow!("unknown Kraken order status: {other}")),
    }
}

/// Infer the quote currency from a Kraken pair string.
///
/// Kraken pairs are typically structured as `XXBTZUSD`, `XETHZUSD`, `SOLUSD`.
/// This extracts the quote portion and converts it via
/// `kraken_asset_to_currency`.
pub(crate) fn infer_quote_currency(pair: &str) -> Currency {
    // Common quote suffixes (4-char legacy, then 3-char)
    let quote_suffixes = ["ZUSD", "ZEUR", "ZGBP", "ZJPY", "ZCAD", "ZAUD"];
    for suffix in &quote_suffixes {
        if pair.ends_with(suffix) {
            return kraken_asset_to_currency(suffix);
        }
    }
    // Try 3-char suffix
    if pair.len() > 3 {
        return kraken_asset_to_currency(&pair[pair.len() - 3..]);
    }
    Currency::from_str_lossy(pair)
}

/// Map Kraken balance response to domain `Balance` list.
///
/// Skips zero balances. Kraken's Balance endpoint returns total only (no held
/// info), so `available = total` and `held = 0`.
pub(crate) fn map_balances(
    raw: &std::collections::HashMap<String, String>,
) -> anyhow::Result<Vec<Balance>> {
    let mut balances = Vec::new();
    for (asset, amount_str) in raw {
        let total = parse_decimal(amount_str, &format!("balance for {asset}"))?;
        if total == Decimal::ZERO {
            continue;
        }
        balances.push(Balance {
            currency: kraken_asset_to_currency(asset),
            total: Amount::new(total),
            available: Amount::new(total),
            held: Amount::new(Decimal::ZERO),
        });
    }
    Ok(balances)
}

/// Map a Kraken open order to a domain `OpenOrder`.
pub(crate) fn map_open_order(txid: &str, order: &KrakenOpenOrder) -> anyhow::Result<OpenOrder> {
    let side = map_kraken_side(&order.descr.side)?;
    let order_type = map_kraken_order_type(&order.descr.ordertype)?;
    let vol = parse_decimal(&order.vol, "vol")?;
    let vol_exec = parse_decimal(&order.vol_exec, "vol_exec")?;
    let remaining = vol - vol_exec;

    let limit_price = if order.descr.price != "0" && !order.descr.price.is_empty() {
        Some(Price::new(parse_decimal(&order.descr.price, "price")?))
    } else {
        None
    };

    let stop_price = if order.descr.price2 != "0" && !order.descr.price2.is_empty() {
        Some(Price::new(parse_decimal(&order.descr.price2, "price2")?))
    } else {
        None
    };

    let avg_fill_price = if !order.avg_price.is_empty() && order.avg_price != "0" {
        let p = parse_decimal(&order.avg_price, "avg_price")?;
        if p > Decimal::ZERO {
            Some(Price::new(p))
        } else {
            None
        }
    } else {
        None
    };

    let status = map_order_status(&order.status, &vol_exec)?;

    let symbol = Symbol::new(&order.descr.pair).context("invalid pair in open order")?;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let created_at = {
        let secs = order.opentm.trunc() as i64;
        let nanos = ((order.opentm.fract()) * 1_000_000_000.0) as u32;
        DateTime::from_timestamp(secs, nanos).context("invalid opentm timestamp")?
    };

    Ok(OpenOrder {
        order_id: OrderId::new(txid).context("invalid order txid")?,
        request: OrderRequest {
            symbol,
            side,
            order_type,
            quantity: Quantity::new(vol).context("invalid order volume")?,
            limit_price,
            stop_price,
            time_in_force: TimeInForce::GoodTilCancelled,
        },
        status,
        filled_quantity: Quantity::new(vol_exec).context("invalid vol_exec")?,
        remaining_quantity: Quantity::new(remaining).context("invalid remaining quantity")?,
        average_fill_price: avg_fill_price,
        created_at,
    })
}

/// Map a Kraken trade history entry to a domain `OrderFill`.
pub(crate) fn map_trade_history_entry(
    trade_id_str: &str,
    entry: &KrakenTradeHistoryEntry,
) -> anyhow::Result<OrderFill> {
    let side = map_kraken_side(&entry.side)?;
    let fee_currency = infer_quote_currency(&entry.pair);

    let symbol = Symbol::new(&entry.pair).context("invalid pair in trade history")?;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let timestamp = {
        let secs = entry.time.trunc() as i64;
        let nanos = ((entry.time.fract()) * 1_000_000_000.0) as u32;
        DateTime::from_timestamp(secs, nanos).context("invalid trade timestamp")?
    };

    Ok(OrderFill {
        order_id: OrderId::new(&entry.ordertxid).context("invalid order txid in trade")?,
        symbol,
        side,
        fill_price: Price::new(parse_decimal(&entry.price, "trade price")?),
        fill_quantity: Quantity::new(parse_decimal(&entry.vol, "trade vol")?)
            .context("invalid trade volume")?,
        fee: Amount::new(parse_decimal(&entry.fee, "trade fee")?),
        fee_currency,
        timestamp,
        trade_id: Some(SmolStr::new(trade_id_str)),
    })
}

// ---- WebSocket v2 mappers ----

/// Convert a Kraken WS v2 symbol (e.g., `"BTC/USD"`) to a `Symbol`.
///
/// Stores the WS symbol as-is — the caller/instrument registry can match
/// on either the REST pair key or the WS format.
pub(crate) fn ws_symbol_to_symbol(ws_symbol: &str) -> anyhow::Result<Symbol> {
    Symbol::new(ws_symbol).context("invalid WS symbol")
}

/// Map a WS trade entry to a domain `Tick`.
pub(crate) fn map_ws_trade(entry: &KrakenWsTradeEntry) -> anyhow::Result<Tick> {
    let symbol = ws_symbol_to_symbol(&entry.symbol)?;
    let time: DateTime<Utc> = entry
        .timestamp
        .parse()
        .context("invalid WS trade timestamp")?;
    let side = match entry.side.as_str() {
        "buy" => Some(OrderSide::Buy),
        "sell" => Some(OrderSide::Sell),
        _ => None,
    };

    Ok(Tick {
        time,
        symbol,
        exchange: SmolStr::new("kraken"),
        price: Price::new(parse_decimal(&entry.price, "ws trade price")?),
        quantity: Quantity::new(parse_decimal(&entry.qty, "ws trade qty")?)
            .context("invalid ws trade qty")?,
        side,
        trade_id: Some(SmolStr::new(entry.trade_id.to_string())),
    })
}

/// Map a WS ticker entry to a domain `TickerSnapshot`.
pub(crate) fn map_ws_ticker(entry: &KrakenWsTickerEntry) -> anyhow::Result<TickerSnapshot> {
    let symbol = ws_symbol_to_symbol(&entry.symbol)?;

    Ok(TickerSnapshot {
        symbol,
        bid: Price::new(parse_decimal(&entry.bid, "ws ticker bid")?),
        ask: Price::new(parse_decimal(&entry.ask, "ws ticker ask")?),
        last: Price::new(parse_decimal(&entry.last, "ws ticker last")?),
        volume_24h: Quantity::new(parse_decimal(&entry.volume, "ws ticker volume")?)
            .context("invalid ws ticker volume")?,
        timestamp: Utc::now(),
    })
}

/// Map a WS book level to a domain `OrderBookLevel`.
pub(crate) fn map_ws_book_level(level: &KrakenWsBookLevel) -> anyhow::Result<OrderBookLevel> {
    Ok(OrderBookLevel {
        price: Price::new(parse_decimal(&level.price, "ws book price")?),
        quantity: Quantity::new(parse_decimal(&level.qty, "ws book qty")?)
            .context("invalid ws book qty")?,
    })
}

/// Map a WS execution entry to a domain `OrderFill`.
pub(crate) fn map_ws_execution(entry: &KrakenWsExecEntry) -> anyhow::Result<OrderFill> {
    let symbol = ws_symbol_to_symbol(&entry.symbol)?;
    let side = map_kraken_side(&entry.side)?;
    let timestamp: DateTime<Utc> = entry
        .timestamp
        .parse()
        .context("invalid WS exec timestamp")?;

    let fill_price = Price::new(parse_decimal(
        entry
            .last_price
            .as_deref()
            .context("execution missing last_price")?,
        "ws exec price",
    )?);
    let fill_quantity = Quantity::new(parse_decimal(
        entry
            .last_qty
            .as_deref()
            .context("execution missing last_qty")?,
        "ws exec qty",
    )?)
    .context("invalid ws exec qty")?;
    let fee = Amount::new(parse_decimal(
        entry.fee_paid.as_deref().unwrap_or("0"),
        "ws exec fee",
    )?);
    let fee_currency = entry
        .fee_currency
        .as_deref()
        .map_or(Currency::USD, Currency::from_str_lossy);

    Ok(OrderFill {
        order_id: OrderId::new(&entry.order_id).context("invalid ws exec order_id")?,
        symbol,
        side,
        fill_price,
        fill_quantity,
        fee,
        fee_currency,
        timestamp,
        trade_id: entry.trade_id.map(|id| SmolStr::new(id.to_string())),
    })
}

fn parse_decimal(s: &str, field: &str) -> anyhow::Result<Decimal> {
    Decimal::from_str(s).with_context(|| format!("failed to parse {field}: {s}"))
}

#[cfg(test)]
mod tests {
    use anyhow::Context;
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_kraken_asset_to_currency_xxbt() {
        assert_eq!(kraken_asset_to_currency("XXBT"), Currency::BTC);
    }

    #[test]
    fn test_kraken_asset_to_currency_zusd() {
        assert_eq!(kraken_asset_to_currency("ZUSD"), Currency::USD);
    }

    #[test]
    fn test_kraken_asset_to_currency_xeth() {
        assert_eq!(kraken_asset_to_currency("XETH"), Currency::ETH);
    }

    #[test]
    fn test_kraken_asset_to_currency_plain() {
        assert_eq!(kraken_asset_to_currency("SOL"), Currency::SOL);
    }

    #[test]
    fn test_kraken_asset_to_currency_matic() {
        assert_eq!(kraken_asset_to_currency("MATIC"), Currency::MATIC);
    }

    #[test]
    fn test_interval_to_kraken() -> anyhow::Result<()> {
        assert_eq!(interval_to_kraken("1m")?, "1");
        assert_eq!(interval_to_kraken("5m")?, "5");
        assert_eq!(interval_to_kraken("1h")?, "60");
        assert_eq!(interval_to_kraken("4h")?, "240");
        assert_eq!(interval_to_kraken("1d")?, "1440");
        assert_eq!(interval_to_kraken("1w")?, "10080");
        assert_eq!(interval_to_kraken("15d")?, "21600");
        Ok(())
    }

    #[test]
    fn test_interval_to_kraken_passthrough() -> anyhow::Result<()> {
        assert_eq!(interval_to_kraken("1")?, "1");
        assert_eq!(interval_to_kraken("60")?, "60");
        Ok(())
    }

    #[test]
    fn test_interval_to_kraken_invalid() {
        assert!(interval_to_kraken("2m").is_err());
    }

    #[test]
    fn test_interval_to_canonical() {
        assert_eq!(interval_to_canonical("1").as_str(), "1m");
        assert_eq!(interval_to_canonical("60").as_str(), "1h");
        assert_eq!(interval_to_canonical("1440").as_str(), "1d");
    }

    #[test]
    fn test_map_asset_pair() -> anyhow::Result<()> {
        let pair = KrakenAssetPair {
            base: "XXBT".into(),
            quote: "ZUSD".into(),
            wsname: Some("XBT/USD".into()),
            altname: "XBTUSD".into(),
            pair_decimals: 1,
            lot_decimals: 8,
            ordermin: Some("0.0001".into()),
            costmin: Some("0.5".into()),
            tick_size: Some("0.1".into()),
            leverage_buy: vec![2, 3, 4, 5],
            leverage_sell: vec![2, 3, 4, 5],
        };
        let inst = map_asset_pair("XXBTZUSD", &pair)?;

        assert_eq!(inst.symbol.as_str(), "XXBTZUSD");
        assert_eq!(inst.base_currency, Currency::BTC);
        assert_eq!(inst.quote_currency, Currency::USD);
        assert_eq!(inst.asset_class, AssetClass::CryptoSpot);
        assert_eq!(inst.exchange, Exchange::Kraken);
        assert_eq!(inst.tick_size.value(), dec!(0.1));
        assert_eq!(inst.display_name.as_str(), "XBT/USD");

        if let InstrumentDetails::CryptoSpot {
            order_min,
            cost_min,
            lot_decimals,
            margin_eligible,
            leverage_tiers,
        } = &inst.details
        {
            assert_eq!(order_min.value(), dec!(0.0001));
            assert_eq!(cost_min.value(), dec!(0.5));
            assert_eq!(*lot_decimals, 8);
            assert!(margin_eligible);
            assert_eq!(leverage_tiers.len(), 4);
        } else {
            return Err(anyhow::anyhow!("expected CryptoSpot details"));
        }

        Ok(())
    }

    #[test]
    fn test_map_asset_pair_no_wsname() -> anyhow::Result<()> {
        let pair = KrakenAssetPair {
            base: "SOL".into(),
            quote: "ZUSD".into(),
            wsname: None,
            altname: "SOLUSD".into(),
            pair_decimals: 3,
            lot_decimals: 4,
            ordermin: None,
            costmin: None,
            tick_size: None,
            leverage_buy: vec![],
            leverage_sell: vec![],
        };
        let inst = map_asset_pair("SOLUSD", &pair)?;

        assert_eq!(inst.display_name.as_str(), "SOLUSD");
        assert_eq!(inst.tick_size.value(), dec!(0.001));
        if let InstrumentDetails::CryptoSpot {
            margin_eligible, ..
        } = &inst.details
        {
            assert!(!margin_eligible);
        }
        Ok(())
    }

    #[test]
    fn test_map_ohlc_bar() -> anyhow::Result<()> {
        let symbol = Symbol::new("XXBTZUSD")?;
        let bar = KrakenOhlcTuple(
            1_616_663_400,
            "56200.0".into(),
            "56300.0".into(),
            "56100.0".into(),
            "56250.0".into(),
            "56225.5".into(),
            "12.345".into(),
            847,
        );
        let ohlcv = map_ohlc_bar(&symbol, "1", &bar)?;

        assert_eq!(ohlcv.symbol.as_str(), "XXBTZUSD");
        assert_eq!(ohlcv.exchange.as_str(), "kraken");
        assert_eq!(ohlcv.interval.as_str(), "1m");
        assert_eq!(ohlcv.open.value(), dec!(56200.0));
        assert_eq!(ohlcv.high.value(), dec!(56300.0));
        assert_eq!(ohlcv.low.value(), dec!(56100.0));
        assert_eq!(ohlcv.close.value(), dec!(56250.0));
        assert_eq!(ohlcv.volume.value(), dec!(12.345));
        assert_eq!(ohlcv.trade_count, Some(847));
        Ok(())
    }

    #[test]
    fn test_map_trade_buy() -> anyhow::Result<()> {
        let symbol = Symbol::new("XXBTZUSD")?;
        let trade = KrakenTradeTuple(
            "56200.10000".into(),
            "0.00100000".into(),
            1_616_663_594.200_9,
            "b".into(),
            "m".into(),
            String::new(),
            12345,
        );
        let tick = map_trade(&symbol, &trade)?;

        assert_eq!(tick.symbol.as_str(), "XXBTZUSD");
        assert_eq!(tick.exchange.as_str(), "kraken");
        assert_eq!(tick.price.value(), dec!(56200.10000));
        assert_eq!(tick.quantity.value(), dec!(0.00100000));
        assert_eq!(tick.side, Some(OrderSide::Buy));
        assert_eq!(tick.trade_id.as_deref(), Some("12345"));
        Ok(())
    }

    #[test]
    fn test_map_trade_sell() -> anyhow::Result<()> {
        let symbol = Symbol::new("XXBTZUSD")?;
        let trade = KrakenTradeTuple(
            "56200.0".into(),
            "1.5".into(),
            1_616_663_594.0,
            "s".into(),
            "l".into(),
            String::new(),
            67890,
        );
        let tick = map_trade(&symbol, &trade)?;
        assert_eq!(tick.side, Some(OrderSide::Sell));
        Ok(())
    }

    #[test]
    fn test_map_ticker() -> anyhow::Result<()> {
        let symbol = Symbol::new("XXBTZUSD")?;
        let info = KrakenTickerInfo {
            a: vec!["67010.00000".into(), "1".into(), "1.000".into()],
            b: vec!["67000.00000".into(), "2".into(), "2.000".into()],
            c: vec!["67005.00000".into(), "0.001".into()],
            v: vec!["1000.0".into(), "5000.0".into()],
        };
        let snap = map_ticker(&symbol, &info)?;

        assert_eq!(snap.symbol.as_str(), "XXBTZUSD");
        assert_eq!(snap.bid.value(), dec!(67000.00000));
        assert_eq!(snap.ask.value(), dec!(67010.00000));
        assert_eq!(snap.last.value(), dec!(67005.00000));
        assert_eq!(snap.volume_24h.value(), dec!(5000.0));
        Ok(())
    }

    #[test]
    fn test_map_order_book() -> anyhow::Result<()> {
        use super::super::models::KrakenOrderBook;

        let symbol = Symbol::new("XXBTZUSD")?;
        let book = KrakenOrderBook {
            bids: vec![
                KrakenBookLevel("67000.0".into(), "3.0".into(), 1_616_663_400),
                KrakenBookLevel("66990.0".into(), "1.5".into(), 1_616_663_401),
            ],
            asks: vec![KrakenBookLevel(
                "67010.0".into(),
                "2.0".into(),
                1_616_663_400,
            )],
        };
        let snap = map_order_book(&symbol, &book)?;

        assert_eq!(snap.bids.len(), 2);
        assert_eq!(snap.asks.len(), 1);
        assert_eq!(snap.bids[0].price.value(), dec!(67000.0));
        assert_eq!(snap.bids[0].quantity.value(), dec!(3.0));
        assert_eq!(snap.asks[0].price.value(), dec!(67010.0));
        Ok(())
    }

    #[test]
    fn test_map_book_level() -> anyhow::Result<()> {
        let level = KrakenBookLevel("67010.50".into(), "2.5".into(), 1_616_663_400);
        let mapped = map_book_level(&level)?;
        assert_eq!(mapped.price.value(), dec!(67010.50));
        assert_eq!(mapped.quantity.value(), dec!(2.5));
        Ok(())
    }

    // ---- Private endpoint mapper tests ----

    #[test]
    fn test_order_side_to_kraken() {
        assert_eq!(order_side_to_kraken(OrderSide::Buy), "buy");
        assert_eq!(order_side_to_kraken(OrderSide::Sell), "sell");
    }

    #[test]
    fn test_order_type_to_kraken() {
        assert_eq!(order_type_to_kraken(OrderType::Market), "market");
        assert_eq!(order_type_to_kraken(OrderType::Limit), "limit");
        assert_eq!(order_type_to_kraken(OrderType::StopLoss), "stop-loss");
        assert_eq!(
            order_type_to_kraken(OrderType::StopLossLimit),
            "stop-loss-limit"
        );
        assert_eq!(order_type_to_kraken(OrderType::TakeProfit), "take-profit");
        assert_eq!(
            order_type_to_kraken(OrderType::TakeProfitLimit),
            "take-profit-limit"
        );
    }

    #[test]
    fn test_time_in_force_to_kraken() -> anyhow::Result<()> {
        assert_eq!(
            time_in_force_to_kraken(TimeInForce::GoodTilCancelled)?,
            None
        );
        assert_eq!(
            time_in_force_to_kraken(TimeInForce::ImmediateOrCancel)?,
            Some("IOC")
        );
        assert_eq!(
            time_in_force_to_kraken(TimeInForce::FillOrKill)?,
            Some("FOK")
        );
        assert!(time_in_force_to_kraken(TimeInForce::Day).is_err());
        Ok(())
    }

    #[test]
    fn test_map_kraken_side() -> anyhow::Result<()> {
        assert_eq!(map_kraken_side("buy")?, OrderSide::Buy);
        assert_eq!(map_kraken_side("sell")?, OrderSide::Sell);
        assert!(map_kraken_side("unknown").is_err());
        Ok(())
    }

    #[test]
    fn test_map_kraken_order_type() -> anyhow::Result<()> {
        assert_eq!(map_kraken_order_type("market")?, OrderType::Market);
        assert_eq!(map_kraken_order_type("limit")?, OrderType::Limit);
        assert_eq!(map_kraken_order_type("stop-loss")?, OrderType::StopLoss);
        assert_eq!(
            map_kraken_order_type("stop-loss-limit")?,
            OrderType::StopLossLimit
        );
        assert!(map_kraken_order_type("unknown").is_err());
        Ok(())
    }

    #[test]
    fn test_map_order_status() -> anyhow::Result<()> {
        assert_eq!(
            map_order_status("pending", &Decimal::ZERO)?,
            OrderStatus::Pending
        );
        assert_eq!(map_order_status("open", &Decimal::ZERO)?, OrderStatus::Open);
        assert_eq!(
            map_order_status("open", &dec!(0.5))?,
            OrderStatus::PartiallyFilled
        );
        assert_eq!(
            map_order_status("closed", &Decimal::ZERO)?,
            OrderStatus::Filled
        );
        assert_eq!(
            map_order_status("canceled", &Decimal::ZERO)?,
            OrderStatus::Cancelled
        );
        assert_eq!(
            map_order_status("expired", &Decimal::ZERO)?,
            OrderStatus::Expired
        );
        assert!(map_order_status("unknown", &Decimal::ZERO).is_err());
        Ok(())
    }

    #[test]
    fn test_infer_quote_currency() {
        assert_eq!(infer_quote_currency("XXBTZUSD"), Currency::USD);
        assert_eq!(infer_quote_currency("XETHZEUR"), Currency::EUR);
        assert_eq!(infer_quote_currency("SOLUSD"), Currency::USD);
        assert_eq!(infer_quote_currency("DOTEUR"), Currency::EUR);
    }

    #[test]
    fn test_map_balances() -> anyhow::Result<()> {
        let mut raw = std::collections::HashMap::new();
        raw.insert("XXBT".to_owned(), "1.5000".to_owned());
        raw.insert("ZUSD".to_owned(), "10000.00".to_owned());
        raw.insert("XETH".to_owned(), "0.0000".to_owned()); // zero, should be skipped

        let balances = map_balances(&raw)?;
        assert_eq!(balances.len(), 2);

        let btc = balances.iter().find(|b| b.currency == Currency::BTC);
        assert!(btc.is_some());
        let btc = btc.context("missing BTC balance")?;
        assert_eq!(btc.total.value(), dec!(1.5));
        assert_eq!(btc.available.value(), dec!(1.5));
        assert_eq!(btc.held.value(), Decimal::ZERO);

        Ok(())
    }

    #[test]
    fn test_map_balances_empty() -> anyhow::Result<()> {
        let raw = std::collections::HashMap::new();
        let balances = map_balances(&raw)?;
        assert!(balances.is_empty());
        Ok(())
    }

    #[test]
    fn test_map_open_order() -> anyhow::Result<()> {
        let order = KrakenOpenOrder {
            status: "open".into(),
            descr: super::super::models::KrakenOpenOrderDescr {
                pair: "XXBTZUSD".into(),
                side: "buy".into(),
                ordertype: "limit".into(),
                price: "65000.0".into(),
                price2: "0".into(),
            },
            vol: "0.001".into(),
            vol_exec: "0.0005".into(),
            cost: "32.50".into(),
            fee: "0.05".into(),
            avg_price: "65000.0".into(),
            opentm: 1_616_663_594.0,
        };

        let mapped = map_open_order("OABCDE-12345-FGHIJ", &order)?;
        assert_eq!(mapped.order_id.as_str(), "OABCDE-12345-FGHIJ");
        assert_eq!(mapped.request.side, OrderSide::Buy);
        assert_eq!(mapped.request.order_type, OrderType::Limit);
        assert_eq!(mapped.status, OrderStatus::PartiallyFilled);
        assert_eq!(mapped.filled_quantity.value(), dec!(0.0005));
        assert_eq!(mapped.remaining_quantity.value(), dec!(0.0005));
        assert!(mapped.average_fill_price.is_some());
        assert_eq!(
            mapped.request.limit_price.map(Price::value),
            Some(dec!(65000.0))
        );
        Ok(())
    }

    #[test]
    fn test_map_trade_history_entry() -> anyhow::Result<()> {
        let entry = KrakenTradeHistoryEntry {
            ordertxid: "OABCDE-12345-FGHIJ".into(),
            pair: "XXBTZUSD".into(),
            side: "buy".into(),
            ordertype: "market".into(),
            price: "67000.50".into(),
            vol: "0.001".into(),
            cost: "67.0005".into(),
            fee: "0.10".into(),
            time: 1_616_663_594.0,
            trade_id: Some(99999),
        };

        let fill = map_trade_history_entry("TABC-DEF-GHIJ", &entry)?;
        assert_eq!(fill.order_id.as_str(), "OABCDE-12345-FGHIJ");
        assert_eq!(fill.side, OrderSide::Buy);
        assert_eq!(fill.fill_price.value(), dec!(67000.50));
        assert_eq!(fill.fill_quantity.value(), dec!(0.001));
        assert_eq!(fill.fee.value(), dec!(0.10));
        assert_eq!(fill.fee_currency, Currency::USD);
        assert_eq!(fill.trade_id.as_deref(), Some("TABC-DEF-GHIJ"));
        Ok(())
    }

    // ---- WebSocket v2 mapper tests ----

    #[test]
    fn test_ws_symbol_to_symbol() -> anyhow::Result<()> {
        let sym = ws_symbol_to_symbol("BTC/USD")?;
        assert_eq!(sym.as_str(), "BTC/USD");
        Ok(())
    }

    #[test]
    fn test_ws_symbol_to_symbol_empty_fails() {
        assert!(ws_symbol_to_symbol("").is_err());
    }

    #[test]
    fn test_map_ws_trade() -> anyhow::Result<()> {
        let entry = super::super::models::KrakenWsTradeEntry {
            symbol: "BTC/USD".into(),
            price: "67000.50".into(),
            qty: "0.001".into(),
            side: "buy".into(),
            timestamp: "2024-01-15T10:30:00Z".into(),
            trade_id: 12345,
        };
        let tick = map_ws_trade(&entry)?;

        assert_eq!(tick.symbol.as_str(), "BTC/USD");
        assert_eq!(tick.exchange.as_str(), "kraken");
        assert_eq!(tick.price.value(), dec!(67000.50));
        assert_eq!(tick.quantity.value(), dec!(0.001));
        assert_eq!(tick.side, Some(OrderSide::Buy));
        assert_eq!(tick.trade_id.as_deref(), Some("12345"));
        Ok(())
    }

    #[test]
    fn test_map_ws_ticker() -> anyhow::Result<()> {
        let entry = super::super::models::KrakenWsTickerEntry {
            symbol: "BTC/USD".into(),
            bid: "67000.00".into(),
            ask: "67010.00".into(),
            last: "67005.00".into(),
            volume: "5000.0".into(),
        };
        let snap = map_ws_ticker(&entry)?;

        assert_eq!(snap.symbol.as_str(), "BTC/USD");
        assert_eq!(snap.bid.value(), dec!(67000.00));
        assert_eq!(snap.ask.value(), dec!(67010.00));
        assert_eq!(snap.last.value(), dec!(67005.00));
        assert_eq!(snap.volume_24h.value(), dec!(5000.0));
        Ok(())
    }

    #[test]
    fn test_map_ws_book_level() -> anyhow::Result<()> {
        let level = super::super::models::KrakenWsBookLevel {
            price: "67010.50".into(),
            qty: "2.5".into(),
        };
        let mapped = map_ws_book_level(&level)?;
        assert_eq!(mapped.price.value(), dec!(67010.50));
        assert_eq!(mapped.quantity.value(), dec!(2.5));
        Ok(())
    }

    #[test]
    fn test_map_ws_execution() -> anyhow::Result<()> {
        let entry = super::super::models::KrakenWsExecEntry {
            exec_type: "filled".into(),
            order_id: "OABCDE-12345-FGHIJ".into(),
            symbol: "BTC/USD".into(),
            side: "buy".into(),
            last_price: Some("67000.50".into()),
            last_qty: Some("0.001".into()),
            fee_paid: Some("0.10".into()),
            fee_currency: Some("USD".into()),
            timestamp: "2024-01-15T10:30:00Z".into(),
            trade_id: Some(99999),
        };
        let fill = map_ws_execution(&entry)?;

        assert_eq!(fill.order_id.as_str(), "OABCDE-12345-FGHIJ");
        assert_eq!(fill.symbol.as_str(), "BTC/USD");
        assert_eq!(fill.side, OrderSide::Buy);
        assert_eq!(fill.fill_price.value(), dec!(67000.50));
        assert_eq!(fill.fill_quantity.value(), dec!(0.001));
        assert_eq!(fill.fee.value(), dec!(0.10));
        assert_eq!(fill.fee_currency, Currency::USD);
        assert_eq!(fill.trade_id.as_deref(), Some("99999"));
        Ok(())
    }
}
