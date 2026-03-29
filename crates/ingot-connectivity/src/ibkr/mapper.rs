use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use ingot_core::{
    InstrumentDetails, OhlcvBar, OpenOrder, OrderBookLevel, OrderBookSnapshot, OrderId,
    OrderRequest, OrderStatus, TickerSnapshot,
};
use ingot_primitives::{
    AssetClass, Currency, Exchange, OptionRight, OptionStyle, OrderSide, OrderType, SettlementType,
    TimeInForce,
};
use rust_decimal::Decimal;
use smol_str::SmolStr;

use super::{
    contract_registry::IbkrContractRegistry,
    error::IbkrError,
    models::{
        IbkrContractDetail, IbkrHistoryBar, IbkrLiveOrder, IbkrMarketSnapshot, IbkrOrderStatus,
    },
};

// ── Asset class mapping ──

pub(crate) fn sec_type_to_asset_class(sec_type: &str) -> anyhow::Result<AssetClass> {
    match sec_type {
        "STK" => Ok(AssetClass::Equity),
        "OPT" => Ok(AssetClass::Option),
        "FUT" => Ok(AssetClass::Future),
        "CASH" => Ok(AssetClass::Forex),
        "BOND" => Ok(AssetClass::Bond),
        other => Err(IbkrError::UnsupportedSecType(other.to_string()).into()),
    }
}

// ── Currency mapping ──

pub(crate) fn parse_currency(s: &str) -> Currency {
    match s {
        "USD" => Currency::USD,
        "EUR" => Currency::EUR,
        "GBP" => Currency::GBP,
        "JPY" => Currency::JPY,
        "CHF" => Currency::CHF,
        "CAD" => Currency::CAD,
        "AUD" => Currency::AUD,
        "NZD" => Currency::NZD,
        "HKD" => Currency::HKD,
        "SGD" => Currency::SGD,
        other => Currency::Other(SmolStr::new(other)),
    }
}

// ── Contract detail → Instrument ──

pub(crate) fn contract_detail_to_instrument(
    detail: &IbkrContractDetail,
) -> anyhow::Result<ingot_core::Instrument> {
    let asset_class = sec_type_to_asset_class(&detail.sec_type)?;
    let symbol = ingot_primitives::Symbol::new(&detail.symbol).context("invalid IBKR symbol")?;
    let currency = parse_currency(&detail.currency);

    let details = match asset_class {
        AssetClass::Equity => InstrumentDetails::Equity {
            isin: None,
            lot_size: ingot_primitives::Quantity::new(Decimal::ONE).context("invalid lot size")?,
            fractional: false,
        },
        AssetClass::Future => {
            let expiry = parse_expiry(detail.expiry.as_deref()).context("future missing expiry")?;
            let multiplier = parse_decimal_opt(detail.multiplier.as_deref())
                .context("future multiplier")?
                .unwrap_or(Decimal::ONE);
            InstrumentDetails::Future {
                underlying: None,
                expiry,
                multiplier,
                settlement: SettlementType::Physical,
            }
        }
        AssetClass::Option => {
            let expiry = parse_expiry(detail.expiry.as_deref()).context("option missing expiry")?;
            let multiplier = parse_decimal_opt(detail.multiplier.as_deref())
                .context("option multiplier")?
                .unwrap_or(Decimal::from(100));
            let strike_dec = parse_decimal_opt(detail.strike.as_deref())
                .context("option strike")?
                .unwrap_or(Decimal::ZERO);
            let strike = ingot_primitives::Price::new(strike_dec);
            let right =
                parse_option_right(detail.right.as_deref()).context("option missing right")?;
            InstrumentDetails::Option {
                underlying: symbol.clone(),
                strike,
                right,
                expiry,
                multiplier,
                style: OptionStyle::American,
            }
        }
        AssetClass::Forex => InstrumentDetails::Forex {
            pip_size: ingot_primitives::Price::new(Decimal::new(1, 4)), // 0.0001
        },
        AssetClass::Bond => {
            let maturity = parse_expiry(detail.expiry.as_deref())
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(2099, 12, 31).unwrap_or_default());
            InstrumentDetails::Bond {
                face_value: ingot_primitives::Amount::new(Decimal::from(1000)),
                coupon_rate: ingot_primitives::Percentage::new(Decimal::ZERO)
                    .context("invalid coupon rate")?,
                maturity,
            }
        }
        _ => return Err(IbkrError::UnsupportedSecType(detail.sec_type.clone()).into()),
    };

    let display_name = detail.company_name.as_deref().unwrap_or(&detail.symbol);

    Ok(ingot_core::Instrument {
        symbol,
        asset_class,
        exchange: Exchange::IBKR,
        base_currency: currency.clone(),
        quote_currency: currency,
        tick_size: ingot_primitives::Price::new(Decimal::new(1, 2)), // 0.01 default
        display_name: SmolStr::new(display_name),
        details,
    })
}

// ── Market data conversions ──

pub(crate) fn market_snapshot_to_ticker(
    symbol: ingot_primitives::Symbol,
    snap: &IbkrMarketSnapshot,
) -> anyhow::Result<TickerSnapshot> {
    let last = parse_snapshot_field(snap.last_price.as_deref(), "last_price")?;
    let bid = parse_snapshot_field(snap.bid.as_deref(), "bid")?;
    let ask = parse_snapshot_field(snap.ask.as_deref(), "ask")?;
    let volume = snap
        .volume
        .as_deref()
        .and_then(|s| s.parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO);

    Ok(TickerSnapshot {
        symbol,
        bid: ingot_primitives::Price::new(bid),
        ask: ingot_primitives::Price::new(ask),
        last: ingot_primitives::Price::new(last),
        volume_24h: ingot_primitives::Quantity::new(volume)
            .context("invalid volume in snapshot")?,
        timestamp: Utc::now(),
    })
}

pub(crate) fn snapshot_to_order_book(
    symbol: ingot_primitives::Symbol,
    snap: &IbkrMarketSnapshot,
) -> anyhow::Result<OrderBookSnapshot> {
    let mut bids = Vec::new();
    let mut asks = Vec::new();

    if let Some(bid_str) = snap.bid.as_deref() {
        let bid_dec: Decimal = bid_str.parse().context("invalid bid price")?;
        bids.push(OrderBookLevel {
            price: ingot_primitives::Price::new(bid_dec),
            quantity: ingot_primitives::Quantity::new(Decimal::ZERO).context("invalid bid qty")?,
        });
    }

    if let Some(ask_str) = snap.ask.as_deref() {
        let ask_dec: Decimal = ask_str.parse().context("invalid ask price")?;
        asks.push(OrderBookLevel {
            price: ingot_primitives::Price::new(ask_dec),
            quantity: ingot_primitives::Quantity::new(Decimal::ZERO).context("invalid ask qty")?,
        });
    }

    Ok(OrderBookSnapshot {
        symbol,
        bids,
        asks,
        timestamp: Utc::now(),
    })
}

pub(crate) fn history_bar_to_ohlcv(
    symbol: ingot_primitives::Symbol,
    interval: &str,
    bar: &IbkrHistoryBar,
) -> anyhow::Result<OhlcvBar> {
    let time = DateTime::from_timestamp(bar.timestamp, 0).context("invalid bar timestamp")?;
    let open_dec = Decimal::try_from(bar.open).context("invalid open")?;
    let high_dec = Decimal::try_from(bar.high).context("invalid high")?;
    let low_dec = Decimal::try_from(bar.low).context("invalid low")?;
    let close_dec = Decimal::try_from(bar.close).context("invalid close")?;
    let vol_dec = Decimal::try_from(bar.volume).context("invalid volume")?;

    Ok(OhlcvBar {
        time,
        symbol,
        exchange: SmolStr::new("IBKR"),
        interval: SmolStr::new(interval),
        open: ingot_primitives::Price::new(open_dec),
        high: ingot_primitives::Price::new(high_dec),
        low: ingot_primitives::Price::new(low_dec),
        close: ingot_primitives::Price::new(close_dec),
        volume: ingot_primitives::Quantity::new(vol_dec).context("invalid bar volume")?,
        trade_count: None,
    })
}

// ── Order-related mappings (for 1f.4) ──

pub(crate) fn order_side_to_ibkr(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "BUY",
        OrderSide::Sell => "SELL",
    }
}

pub(crate) fn order_type_to_ibkr(order_type: OrderType) -> anyhow::Result<&'static str> {
    match order_type {
        OrderType::Market => Ok("MKT"),
        OrderType::Limit => Ok("LMT"),
        OrderType::StopLoss => Ok("STP"),
        OrderType::StopLossLimit => Ok("STP LMT"),
        OrderType::TakeProfit | OrderType::TakeProfitLimit => {
            anyhow::bail!("IBKR does not support TakeProfit order types directly")
        }
    }
}

pub(crate) fn tif_to_ibkr(tif: TimeInForce) -> anyhow::Result<&'static str> {
    match tif {
        TimeInForce::GoodTilCancelled => Ok("GTC"),
        TimeInForce::ImmediateOrCancel => Ok("IOC"),
        TimeInForce::FillOrKill => Ok("FOK"),
        TimeInForce::Day => Ok("DAY"),
        TimeInForce::GoodTilDate(_) => {
            anyhow::bail!("IBKR GoodTilDate requires special handling via expiry field")
        }
    }
}

// ── IBKR status → OrderStatus ──

pub(crate) fn ibkr_status_to_order_status(status: &str) -> anyhow::Result<OrderStatus> {
    match status {
        "Submitted" => Ok(OrderStatus::Open),
        "Filled" => Ok(OrderStatus::Filled),
        "Cancelled" => Ok(OrderStatus::Cancelled),
        "PreSubmitted" => Ok(OrderStatus::Pending),
        "Inactive" => Ok(OrderStatus::Rejected),
        other => anyhow::bail!("unknown IBKR order status: {other}"),
    }
}

// ── Asset class → IBKR sec_type ──

pub(crate) fn asset_class_to_sec_type(asset_class: AssetClass) -> &'static str {
    match asset_class {
        AssetClass::Equity => "STK",
        AssetClass::Option => "OPT",
        AssetClass::Future => "FUT",
        AssetClass::Forex => "CASH",
        AssetClass::Bond => "BOND",
        AssetClass::CryptoSpot | AssetClass::CryptoFuture => "STK",
    }
}

// ── Reverse mappers (IBKR string → domain type) ──

pub(crate) fn ibkr_side_to_order_side(side: &str) -> anyhow::Result<OrderSide> {
    match side {
        "BUY" | "B" => Ok(OrderSide::Buy),
        "SELL" | "S" => Ok(OrderSide::Sell),
        other => anyhow::bail!("unknown IBKR order side: {other}"),
    }
}

pub(crate) fn ibkr_order_type_from_str(s: &str) -> anyhow::Result<OrderType> {
    match s {
        "MKT" => Ok(OrderType::Market),
        "LMT" => Ok(OrderType::Limit),
        "STP" => Ok(OrderType::StopLoss),
        "STP LMT" => Ok(OrderType::StopLossLimit),
        other => anyhow::bail!("unknown IBKR order type: {other}"),
    }
}

pub(crate) fn ibkr_tif_from_str(s: &str) -> anyhow::Result<TimeInForce> {
    match s {
        "GTC" => Ok(TimeInForce::GoodTilCancelled),
        "IOC" => Ok(TimeInForce::ImmediateOrCancel),
        "FOK" => Ok(TimeInForce::FillOrKill),
        "DAY" => Ok(TimeInForce::Day),
        other => anyhow::bail!("unknown IBKR time-in-force: {other}"),
    }
}

// ── Order conversion: rich endpoint (get_open_orders) ──

pub(crate) fn ibkr_live_order_to_open_order(
    live: &IbkrLiveOrder,
    registry: &IbkrContractRegistry,
) -> anyhow::Result<OpenOrder> {
    let symbol = registry
        .symbol_for_conid(live.conid)
        .with_context(|| format!("conid {} not found in registry", live.conid))?
        .clone();

    let side = ibkr_side_to_order_side(&live.side)?;
    let order_type = ibkr_order_type_from_str(&live.order_type)?;
    let tif = match live.time_in_force.as_deref() {
        Some(s) => ibkr_tif_from_str(s)?,
        None => TimeInForce::Day,
    };

    let limit_price = match order_type {
        OrderType::Limit | OrderType::StopLossLimit => live
            .price
            .map(|p| {
                let d = Decimal::try_from(p).context("invalid limit price")?;
                Ok::<_, anyhow::Error>(ingot_primitives::Price::new(d))
            })
            .transpose()?,
        _ => None,
    };

    let stop_price = match order_type {
        OrderType::StopLoss | OrderType::StopLossLimit => live
            .aux_price
            .map(|p| {
                let d = Decimal::try_from(p).context("invalid stop price")?;
                Ok::<_, anyhow::Error>(ingot_primitives::Price::new(d))
            })
            .transpose()?,
        _ => None,
    };

    let quantity = ingot_primitives::Quantity::new(
        Decimal::try_from(live.quantity).context("invalid quantity")?,
    )
    .context("invalid quantity value")?;

    let filled_quantity = ingot_primitives::Quantity::new(
        Decimal::try_from(live.filled_quantity).context("invalid filled quantity")?,
    )
    .context("invalid filled quantity value")?;

    let remaining_quantity = ingot_primitives::Quantity::new(
        Decimal::try_from(live.remaining_quantity).context("invalid remaining quantity")?,
    )
    .context("invalid remaining quantity value")?;

    let status = ibkr_status_to_order_status(&live.status)?;

    Ok(OpenOrder {
        order_id: OrderId::new(&live.order_id).context("invalid order id")?,
        request: OrderRequest {
            symbol,
            side,
            order_type,
            quantity,
            limit_price,
            stop_price,
            time_in_force: tif,
        },
        status,
        filled_quantity,
        remaining_quantity,
        average_fill_price: None,
        created_at: Utc::now(),
    })
}

// ── Order conversion: sparse endpoint (get_order_status) ──

pub(crate) fn ibkr_order_status_to_open_order(
    status: &IbkrOrderStatus,
    registry: &IbkrContractRegistry,
) -> anyhow::Result<OpenOrder> {
    let symbol = registry
        .symbol_for_conid(status.conid)
        .with_context(|| format!("conid {} not found in registry", status.conid))?
        .clone();

    let side = ibkr_side_to_order_side(&status.side)?;
    let order_status = ibkr_status_to_order_status(&status.status)?;

    let filled_quantity = ingot_primitives::Quantity::new(
        Decimal::try_from(status.filled_quantity).context("invalid filled quantity")?,
    )
    .context("invalid filled quantity value")?;

    let remaining_quantity = ingot_primitives::Quantity::new(
        Decimal::try_from(status.remaining_quantity).context("invalid remaining quantity")?,
    )
    .context("invalid remaining quantity value")?;

    let total_qty_f64 = status.filled_quantity + status.remaining_quantity;
    let total_quantity = ingot_primitives::Quantity::new(
        Decimal::try_from(total_qty_f64).context("invalid total quantity")?,
    )
    .context("invalid total quantity value")?;

    let average_fill_price = if status.avg_price > 0.0 && status.filled_quantity > 0.0 {
        let d = Decimal::try_from(status.avg_price).context("invalid avg price")?;
        Some(ingot_primitives::Price::new(d))
    } else {
        None
    };

    Ok(OpenOrder {
        order_id: OrderId::new(&status.order_id).context("invalid order id")?,
        request: OrderRequest {
            symbol,
            side,
            order_type: OrderType::Market,
            quantity: total_quantity,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::Day,
        },
        status: order_status,
        filled_quantity,
        remaining_quantity,
        average_fill_price,
        created_at: Utc::now(),
    })
}

// ── Interval mapping ──

pub(crate) fn ibkr_interval(interval: &str) -> anyhow::Result<&'static str> {
    match interval {
        "1m" => Ok("1min"),
        "5m" => Ok("5mins"),
        "15m" => Ok("15mins"),
        "30m" => Ok("30mins"),
        "1h" => Ok("1h"),
        "4h" => Ok("4h"),
        "1d" => Ok("1d"),
        "1w" => Ok("1w"),
        other => anyhow::bail!("unsupported IBKR interval: {other}"),
    }
}

// ── Helpers ──

fn parse_expiry(s: Option<&str>) -> anyhow::Result<NaiveDate> {
    let s = s.context("missing expiry date")?;
    NaiveDate::parse_from_str(s, "%Y%m%d")
        .with_context(|| format!("invalid expiry date format: {s}"))
}

fn parse_decimal_opt(s: Option<&str>) -> anyhow::Result<Option<Decimal>> {
    match s {
        Some(val) => {
            let d: Decimal = val
                .parse()
                .with_context(|| format!("invalid decimal: {val}"))?;
            Ok(Some(d))
        }
        None => Ok(None),
    }
}

fn parse_option_right(s: Option<&str>) -> anyhow::Result<OptionRight> {
    match s {
        Some("C") => Ok(OptionRight::Call),
        Some("P") => Ok(OptionRight::Put),
        Some(other) => anyhow::bail!("unknown option right: {other}"),
        None => anyhow::bail!("missing option right"),
    }
}

fn parse_snapshot_field(s: Option<&str>, field_name: &str) -> anyhow::Result<Decimal> {
    let val = s.with_context(|| format!("missing snapshot field: {field_name}"))?;
    val.parse()
        .with_context(|| format!("invalid {field_name}: {val}"))
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    // ── Tests 1-6: sec_type_to_asset_class ──

    #[test]
    fn test_sec_type_to_asset_class_stk() -> anyhow::Result<()> {
        assert_eq!(sec_type_to_asset_class("STK")?, AssetClass::Equity);
        Ok(())
    }

    #[test]
    fn test_sec_type_to_asset_class_opt() -> anyhow::Result<()> {
        assert_eq!(sec_type_to_asset_class("OPT")?, AssetClass::Option);
        Ok(())
    }

    #[test]
    fn test_sec_type_to_asset_class_fut() -> anyhow::Result<()> {
        assert_eq!(sec_type_to_asset_class("FUT")?, AssetClass::Future);
        Ok(())
    }

    #[test]
    fn test_sec_type_to_asset_class_cash() -> anyhow::Result<()> {
        assert_eq!(sec_type_to_asset_class("CASH")?, AssetClass::Forex);
        Ok(())
    }

    #[test]
    fn test_sec_type_to_asset_class_bond() -> anyhow::Result<()> {
        assert_eq!(sec_type_to_asset_class("BOND")?, AssetClass::Bond);
        Ok(())
    }

    #[test]
    fn test_sec_type_to_asset_class_unknown() {
        let result = sec_type_to_asset_class("WAR");
        assert!(result.is_err());
    }

    // ── Tests 7-11: contract_detail_to_instrument ──

    fn make_detail(sec_type: &str) -> IbkrContractDetail {
        IbkrContractDetail {
            con_id: 265598,
            symbol: "AAPL".into(),
            sec_type: sec_type.into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            local_symbol: None,
            trading_class: None,
            multiplier: None,
            expiry: None,
            strike: None,
            right: None,
            company_name: Some("Apple Inc".into()),
            valid_exchanges: None,
        }
    }

    #[test]
    fn test_contract_detail_to_instrument_equity() -> anyhow::Result<()> {
        let detail = make_detail("STK");
        let inst = contract_detail_to_instrument(&detail)?;

        assert_eq!(inst.symbol.as_str(), "AAPL");
        assert_eq!(inst.asset_class, AssetClass::Equity);
        assert_eq!(inst.exchange, Exchange::IBKR);
        assert_eq!(inst.base_currency, Currency::USD);
        assert_eq!(inst.display_name.as_str(), "Apple Inc");

        assert!(matches!(
            inst.details,
            InstrumentDetails::Equity {
                isin: None,
                fractional: false,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn test_contract_detail_to_instrument_future() -> anyhow::Result<()> {
        let mut detail = make_detail("FUT");
        detail.symbol = "ES".into();
        detail.expiry = Some("20260320".into());
        detail.multiplier = Some("50".into());
        detail.company_name = Some("E-mini S&P 500".into());

        let inst = contract_detail_to_instrument(&detail)?;
        assert_eq!(inst.asset_class, AssetClass::Future);

        assert!(matches!(
            inst.details,
            InstrumentDetails::Future {
                multiplier,
                settlement: SettlementType::Physical,
                ..
            } if multiplier == dec!(50)
        ));
        Ok(())
    }

    #[test]
    fn test_contract_detail_to_instrument_option() -> anyhow::Result<()> {
        let mut detail = make_detail("OPT");
        detail.expiry = Some("20260320".into());
        detail.multiplier = Some("100".into());
        detail.strike = Some("175.00".into());
        detail.right = Some("C".into());

        let inst = contract_detail_to_instrument(&detail)?;
        assert_eq!(inst.asset_class, AssetClass::Option);

        assert!(matches!(
            inst.details,
            InstrumentDetails::Option {
                right: OptionRight::Call,
                style: OptionStyle::American,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn test_contract_detail_to_instrument_forex() -> anyhow::Result<()> {
        let mut detail = make_detail("CASH");
        detail.symbol = "EUR".into();
        detail.currency = "USD".into();

        let inst = contract_detail_to_instrument(&detail)?;
        assert_eq!(inst.asset_class, AssetClass::Forex);
        assert!(matches!(inst.details, InstrumentDetails::Forex { .. }));
        Ok(())
    }

    #[test]
    fn test_contract_detail_to_instrument_bond() -> anyhow::Result<()> {
        let mut detail = make_detail("BOND");
        detail.symbol = "UST10Y".into();
        detail.expiry = Some("20350615".into());

        let inst = contract_detail_to_instrument(&detail)?;
        assert_eq!(inst.asset_class, AssetClass::Bond);
        assert!(matches!(inst.details, InstrumentDetails::Bond { .. }));
        Ok(())
    }

    // ── Tests 12-13: market_snapshot_to_ticker ──

    #[test]
    fn test_market_snapshot_to_ticker() -> anyhow::Result<()> {
        let snap = IbkrMarketSnapshot {
            conid: 265598,
            last_price: Some("178.50".into()),
            bid: Some("178.45".into()),
            ask: Some("178.55".into()),
            volume: Some("45000000".into()),
            open: Some("177.00".into()),
            high: Some("179.00".into()),
            low: Some("176.50".into()),
            close: Some("178.00".into()),
        };
        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let ticker = market_snapshot_to_ticker(symbol, &snap)?;

        assert_eq!(ticker.last, ingot_primitives::Price::new(dec!(178.50)));
        assert_eq!(ticker.bid, ingot_primitives::Price::new(dec!(178.45)));
        assert_eq!(ticker.ask, ingot_primitives::Price::new(dec!(178.55)));
        Ok(())
    }

    #[test]
    fn test_market_snapshot_to_ticker_partial_fields() -> anyhow::Result<()> {
        let snap = IbkrMarketSnapshot {
            conid: 265598,
            last_price: Some("178.50".into()),
            bid: Some("178.45".into()),
            ask: Some("178.55".into()),
            volume: None, // missing volume
            open: None,
            high: None,
            low: None,
            close: None,
        };
        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let ticker = market_snapshot_to_ticker(symbol, &snap)?;

        // Missing volume defaults to 0
        assert_eq!(
            ticker.volume_24h,
            ingot_primitives::Quantity::new(Decimal::ZERO)?
        );
        Ok(())
    }

    // ── Test 14: history_bar_to_ohlcv ──

    #[test]
    fn test_history_bar_to_ohlcv() -> anyhow::Result<()> {
        let bar = IbkrHistoryBar {
            timestamp: 1_711_900_800, // 2024-03-31T12:00:00Z
            open: 177.0,
            high: 179.2,
            low: 176.8,
            close: 178.5,
            volume: 45_230_100.0,
        };
        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let ohlcv = history_bar_to_ohlcv(symbol, "1d", &bar)?;

        assert_eq!(ohlcv.exchange.as_str(), "IBKR");
        assert_eq!(ohlcv.interval.as_str(), "1d");
        assert_eq!(ohlcv.time.timestamp(), 1_711_900_800);
        Ok(())
    }

    // ── Tests 15-17: order-related mappers ──

    #[test]
    fn test_order_side_to_ibkr() {
        assert_eq!(order_side_to_ibkr(OrderSide::Buy), "BUY");
        assert_eq!(order_side_to_ibkr(OrderSide::Sell), "SELL");
    }

    #[test]
    fn test_order_type_to_ibkr() -> anyhow::Result<()> {
        assert_eq!(order_type_to_ibkr(OrderType::Market)?, "MKT");
        assert_eq!(order_type_to_ibkr(OrderType::Limit)?, "LMT");
        assert_eq!(order_type_to_ibkr(OrderType::StopLoss)?, "STP");
        assert_eq!(order_type_to_ibkr(OrderType::StopLossLimit)?, "STP LMT");
        assert!(order_type_to_ibkr(OrderType::TakeProfit).is_err());
        assert!(order_type_to_ibkr(OrderType::TakeProfitLimit).is_err());
        Ok(())
    }

    #[test]
    fn test_tif_to_ibkr() -> anyhow::Result<()> {
        assert_eq!(tif_to_ibkr(TimeInForce::GoodTilCancelled)?, "GTC");
        assert_eq!(tif_to_ibkr(TimeInForce::ImmediateOrCancel)?, "IOC");
        assert_eq!(tif_to_ibkr(TimeInForce::FillOrKill)?, "FOK");
        assert_eq!(tif_to_ibkr(TimeInForce::Day)?, "DAY");
        assert!(tif_to_ibkr(TimeInForce::GoodTilDate(Utc::now())).is_err());
        Ok(())
    }

    // ── Tests 18-19: interval mapping ──

    #[test]
    fn test_ibkr_interval() -> anyhow::Result<()> {
        assert_eq!(ibkr_interval("1m")?, "1min");
        assert_eq!(ibkr_interval("5m")?, "5mins");
        assert_eq!(ibkr_interval("15m")?, "15mins");
        assert_eq!(ibkr_interval("30m")?, "30mins");
        assert_eq!(ibkr_interval("1h")?, "1h");
        assert_eq!(ibkr_interval("4h")?, "4h");
        assert_eq!(ibkr_interval("1d")?, "1d");
        assert_eq!(ibkr_interval("1w")?, "1w");
        Ok(())
    }

    #[test]
    fn test_ibkr_interval_unsupported() {
        assert!(ibkr_interval("2m").is_err());
        assert!(ibkr_interval("3h").is_err());
    }

    // ── Tests 1f.4 1-5: ibkr_status_to_order_status ──

    #[test]
    fn test_ibkr_status_to_order_status_submitted() -> anyhow::Result<()> {
        assert_eq!(ibkr_status_to_order_status("Submitted")?, OrderStatus::Open);
        Ok(())
    }

    #[test]
    fn test_ibkr_status_to_order_status_filled() -> anyhow::Result<()> {
        assert_eq!(ibkr_status_to_order_status("Filled")?, OrderStatus::Filled);
        Ok(())
    }

    #[test]
    fn test_ibkr_status_to_order_status_cancelled() -> anyhow::Result<()> {
        assert_eq!(
            ibkr_status_to_order_status("Cancelled")?,
            OrderStatus::Cancelled
        );
        Ok(())
    }

    #[test]
    fn test_ibkr_status_to_order_status_presubmitted() -> anyhow::Result<()> {
        assert_eq!(
            ibkr_status_to_order_status("PreSubmitted")?,
            OrderStatus::Pending
        );
        Ok(())
    }

    #[test]
    fn test_ibkr_status_to_order_status_inactive() -> anyhow::Result<()> {
        assert_eq!(
            ibkr_status_to_order_status("Inactive")?,
            OrderStatus::Rejected
        );
        Ok(())
    }

    // ── Test 1f.4 6: ibkr_live_order_to_open_order ──

    #[test]
    fn test_ibkr_live_order_to_open_order() -> anyhow::Result<()> {
        use ingot_core::Instrument;

        use crate::ibkr::contract_registry::IbkrContractRegistry;

        let mut registry = IbkrContractRegistry::new();
        let symbol = ingot_primitives::Symbol::new("AAPL")?;
        let instrument = Instrument {
            symbol: symbol.clone(),
            asset_class: AssetClass::Equity,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: ingot_primitives::Price::new(dec!(0.01)),
            display_name: SmolStr::new("Apple Inc"),
            details: InstrumentDetails::Equity {
                isin: None,
                lot_size: ingot_primitives::Quantity::new(dec!(1))?,
                fractional: false,
            },
        };
        registry.register(265598, symbol, instrument);

        let live = IbkrLiveOrder {
            order_id: "12345".into(),
            conid: 265598,
            order_type: "LMT".into(),
            side: "BUY".into(),
            price: Some(178.50),
            aux_price: None,
            quantity: 100.0,
            filled_quantity: 50.0,
            remaining_quantity: 50.0,
            status: "Submitted".into(),
            time_in_force: Some("GTC".into()),
            ticker: Some("AAPL".into()),
        };

        let open = ibkr_live_order_to_open_order(&live, &registry)?;

        assert_eq!(open.order_id.as_str(), "12345");
        assert_eq!(open.status, OrderStatus::Open);
        assert_eq!(open.request.symbol.as_str(), "AAPL");
        assert_eq!(open.request.side, OrderSide::Buy);
        assert_eq!(open.request.order_type, OrderType::Limit);
        assert_eq!(open.request.time_in_force, TimeInForce::GoodTilCancelled);
        assert_eq!(
            open.request.limit_price,
            Some(ingot_primitives::Price::new(dec!(178.50)))
        );
        assert_eq!(
            open.filled_quantity,
            ingot_primitives::Quantity::new(dec!(50))?
        );
        assert_eq!(
            open.remaining_quantity,
            ingot_primitives::Quantity::new(dec!(50))?
        );
        Ok(())
    }
}
