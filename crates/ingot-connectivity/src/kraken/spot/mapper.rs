use std::str::FromStr;

use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_core::{
    Instrument, InstrumentDetails, OhlcvBar, OrderBookLevel, OrderBookSnapshot, Tick,
    TickerSnapshot,
};
use ingot_primitives::{
    Amount, AssetClass, Currency, Exchange, OrderSide, Price, Quantity, Symbol,
};
use rust_decimal::Decimal;
use smol_str::SmolStr;

use super::models::{
    KrakenAssetPair, KrakenBookLevel, KrakenOhlcTuple, KrakenOrderBook, KrakenTickerInfo,
    KrakenTradeTuple,
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

fn parse_decimal(s: &str, field: &str) -> anyhow::Result<Decimal> {
    Decimal::from_str(s).with_context(|| format!("failed to parse {field}: {s}"))
}

#[cfg(test)]
mod tests {
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
}
