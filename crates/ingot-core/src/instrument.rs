use chrono::{DateTime, NaiveDate, Utc};
use ingot_primitives::{
    Amount, AssetClass, CryptoContractType, Currency, Exchange, OptionRight, OptionStyle,
    Percentage, Price, Quantity, SettlementType, Symbol,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instrument {
    pub symbol: Symbol,
    pub asset_class: AssetClass,
    pub exchange: Exchange,
    pub base_currency: Currency,
    pub quote_currency: Currency,
    pub tick_size: Price,
    pub display_name: SmolStr,
    pub details: InstrumentDetails,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstrumentDetails {
    Equity {
        isin: Option<SmolStr>,
        lot_size: Quantity,
        fractional: bool,
    },
    Option {
        underlying: Symbol,
        strike: Price,
        right: OptionRight,
        expiry: NaiveDate,
        multiplier: Decimal,
        style: OptionStyle,
    },
    Future {
        underlying: Option<Symbol>,
        expiry: NaiveDate,
        multiplier: Decimal,
        settlement: SettlementType,
    },
    Forex {
        pip_size: Price,
    },
    CryptoSpot {
        order_min: Quantity,
        cost_min: Amount,
        lot_decimals: u8,
        margin_eligible: bool,
        leverage_tiers: Vec<u8>,
    },
    CryptoFuture {
        contract_type: CryptoContractType,
        expiry: Option<DateTime<Utc>>,
        max_position_size: Quantity,
        initial_margin: Percentage,
        maintenance_margin: Percentage,
    },
    Bond {
        face_value: Amount,
        coupon_rate: Percentage,
        maturity: NaiveDate,
    },
}

#[cfg(test)]
mod tests {
    use ingot_primitives::PrimitiveError;
    use rust_decimal_macros::dec;

    use super::*;

    fn sample_equity() -> Result<Instrument, PrimitiveError> {
        Ok(Instrument {
            symbol: Symbol::new("265598")?,
            asset_class: AssetClass::Equity,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.01)),
            display_name: SmolStr::new("AAPL"),
            details: InstrumentDetails::Equity {
                isin: Some(SmolStr::new("US0378331005")),
                lot_size: Quantity::new(dec!(1))?,
                fractional: true,
            },
        })
    }

    fn sample_crypto_spot() -> Result<Instrument, PrimitiveError> {
        Ok(Instrument {
            symbol: Symbol::new("XXBTZUSD")?,
            asset_class: AssetClass::CryptoSpot,
            exchange: Exchange::Kraken,
            base_currency: Currency::BTC,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.1)),
            display_name: SmolStr::new("XBT/USD"),
            details: InstrumentDetails::CryptoSpot {
                order_min: Quantity::new(dec!(0.00005))?,
                cost_min: Amount::new(dec!(0.5)),
                lot_decimals: 8,
                margin_eligible: true,
                leverage_tiers: vec![2, 3, 4, 5],
            },
        })
    }

    fn sample_future() -> Result<Instrument, PrimitiveError> {
        Ok(Instrument {
            symbol: Symbol::new("495512552")?,
            asset_class: AssetClass::Future,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.25)),
            display_name: SmolStr::new("ES Mar26"),
            details: InstrumentDetails::Future {
                underlying: Some(Symbol::new("ES")?),
                expiry: NaiveDate::from_ymd_opt(2026, 3, 20).ok_or(PrimitiveError::EmptySymbol)?,
                multiplier: dec!(50),
                settlement: SettlementType::Cash,
            },
        })
    }

    fn sample_crypto_future() -> Result<Instrument, PrimitiveError> {
        Ok(Instrument {
            symbol: Symbol::new("PF_SOLUSD")?,
            asset_class: AssetClass::CryptoFuture,
            exchange: Exchange::KrakenFutures,
            base_currency: Currency::SOL,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.001)),
            display_name: SmolStr::new("SOL/USD Perp"),
            details: InstrumentDetails::CryptoFuture {
                contract_type: CryptoContractType::PerpetualLinear,
                expiry: None,
                max_position_size: Quantity::new(dec!(500000))?,
                initial_margin: Percentage::new(dec!(0.05))?,
                maintenance_margin: Percentage::new(dec!(0.025))?,
            },
        })
    }

    #[test]
    fn test_instrument_equity_construction() -> Result<(), PrimitiveError> {
        let inst = sample_equity()?;
        assert_eq!(inst.asset_class, AssetClass::Equity);
        assert_eq!(inst.exchange, Exchange::IBKR);
        assert_eq!(inst.display_name.as_str(), "AAPL");
        Ok(())
    }

    #[test]
    fn test_instrument_crypto_spot_construction() -> Result<(), PrimitiveError> {
        let inst = sample_crypto_spot()?;
        assert_eq!(inst.asset_class, AssetClass::CryptoSpot);
        assert_eq!(inst.base_currency, Currency::BTC);
        assert_eq!(inst.quote_currency, Currency::USD);
        assert!(matches!(
            &inst.details,
            InstrumentDetails::CryptoSpot {
                margin_eligible: true,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn test_instrument_future_construction() -> Result<(), PrimitiveError> {
        let inst = sample_future()?;
        assert!(matches!(
            &inst.details,
            InstrumentDetails::Future {
                settlement: SettlementType::Cash,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn test_instrument_crypto_future_perpetual() -> Result<(), PrimitiveError> {
        let inst = sample_crypto_future()?;
        assert!(matches!(
            &inst.details,
            InstrumentDetails::CryptoFuture {
                contract_type: CryptoContractType::PerpetualLinear,
                expiry: None,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn test_instrument_serde_roundtrip_equity() -> Result<(), Box<dyn std::error::Error>> {
        let inst = sample_equity()?;
        let json = serde_json::to_string(&inst)?;
        let deserialized: Instrument = serde_json::from_str(&json)?;
        assert_eq!(inst, deserialized);
        Ok(())
    }

    #[test]
    fn test_instrument_serde_roundtrip_crypto_spot() -> Result<(), Box<dyn std::error::Error>> {
        let inst = sample_crypto_spot()?;
        let json = serde_json::to_string(&inst)?;
        let deserialized: Instrument = serde_json::from_str(&json)?;
        assert_eq!(inst, deserialized);
        Ok(())
    }

    #[test]
    fn test_instrument_serde_roundtrip_future() -> Result<(), Box<dyn std::error::Error>> {
        let inst = sample_future()?;
        let json = serde_json::to_string(&inst)?;
        let deserialized: Instrument = serde_json::from_str(&json)?;
        assert_eq!(inst, deserialized);
        Ok(())
    }

    #[test]
    fn test_instrument_serde_roundtrip_crypto_future() -> Result<(), Box<dyn std::error::Error>> {
        let inst = sample_crypto_future()?;
        let json = serde_json::to_string(&inst)?;
        let deserialized: Instrument = serde_json::from_str(&json)?;
        assert_eq!(inst, deserialized);
        Ok(())
    }
}
