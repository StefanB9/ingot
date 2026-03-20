use std::{collections::HashMap, sync::Arc};

use ingot_primitives::{AssetClass, Exchange, Symbol};

use crate::instrument::Instrument;

#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
pub struct InstrumentRegistry {
    by_symbol: HashMap<Symbol, Arc<Instrument>>,
    by_asset_class: HashMap<AssetClass, Vec<Symbol>>,
    by_exchange: HashMap<Exchange, Vec<Symbol>>,
}

impl InstrumentRegistry {
    pub fn new(instruments: Vec<Instrument>) -> Self {
        let mut by_symbol = HashMap::with_capacity(instruments.len());
        let mut by_asset_class: HashMap<AssetClass, Vec<Symbol>> = HashMap::new();
        let mut by_exchange: HashMap<Exchange, Vec<Symbol>> = HashMap::new();

        for inst in instruments {
            let symbol = inst.symbol.clone();
            by_asset_class
                .entry(inst.asset_class)
                .or_default()
                .push(symbol.clone());
            by_exchange
                .entry(inst.exchange)
                .or_default()
                .push(symbol.clone());
            by_symbol.insert(symbol, Arc::new(inst));
        }

        Self {
            by_symbol,
            by_asset_class,
            by_exchange,
        }
    }

    pub fn get(&self, symbol: &Symbol) -> Option<&Arc<Instrument>> {
        self.by_symbol.get(symbol)
    }

    pub fn list_by_asset_class(&self, asset_class: AssetClass) -> &[Symbol] {
        self.by_asset_class
            .get(&asset_class)
            .map_or(&[], Vec::as_slice)
    }

    pub fn list_by_exchange(&self, exchange: Exchange) -> &[Symbol] {
        self.by_exchange.get(&exchange).map_or(&[], Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use ingot_primitives::{Currency, Price, PrimitiveError, Quantity};
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;
    use crate::instrument::{Instrument, InstrumentDetails};

    fn make_equity(symbol_str: &str, name: &str) -> Result<Instrument, PrimitiveError> {
        Ok(Instrument {
            symbol: Symbol::new(symbol_str)?,
            asset_class: AssetClass::Equity,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.01)),
            display_name: SmolStr::new(name),
            details: InstrumentDetails::Equity {
                isin: None,
                lot_size: Quantity::new(dec!(1))?,
                fractional: false,
            },
        })
    }

    fn make_crypto_spot(symbol_str: &str, name: &str) -> Result<Instrument, PrimitiveError> {
        Ok(Instrument {
            symbol: Symbol::new(symbol_str)?,
            asset_class: AssetClass::CryptoSpot,
            exchange: Exchange::Kraken,
            base_currency: Currency::BTC,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.1)),
            display_name: SmolStr::new(name),
            details: InstrumentDetails::CryptoSpot {
                order_min: Quantity::new(dec!(0.0001))?,
                cost_min: ingot_primitives::Amount::new(dec!(0.5)),
                lot_decimals: 8,
                margin_eligible: false,
                leverage_tiers: vec![],
            },
        })
    }

    #[test]
    fn test_registry_empty() {
        let reg = InstrumentRegistry::new(vec![]);
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn test_registry_get_by_symbol() -> Result<(), PrimitiveError> {
        let aapl = make_equity("AAPL", "Apple")?;
        let reg = InstrumentRegistry::new(vec![aapl]);

        let sym = Symbol::new("AAPL")?;
        let result = reg.get(&sym);
        assert!(result.is_some());
        assert_eq!(result.map(|i| i.display_name.as_str()), Some("Apple"));
        Ok(())
    }

    #[test]
    fn test_registry_get_missing() -> Result<(), PrimitiveError> {
        let reg = InstrumentRegistry::new(vec![]);
        let sym = Symbol::new("DOESNOTEXIST")?;
        assert!(reg.get(&sym).is_none());
        Ok(())
    }

    #[test]
    fn test_registry_list_by_asset_class() -> Result<(), PrimitiveError> {
        let aapl = make_equity("AAPL", "Apple")?;
        let goog = make_equity("GOOG", "Google")?;
        let btc = make_crypto_spot("XXBTZUSD", "BTC/USD")?;

        let reg = InstrumentRegistry::new(vec![aapl, goog, btc]);

        assert_eq!(reg.list_by_asset_class(AssetClass::Equity).len(), 2);
        assert_eq!(reg.list_by_asset_class(AssetClass::CryptoSpot).len(), 1);
        assert_eq!(reg.list_by_asset_class(AssetClass::Future).len(), 0);
        Ok(())
    }

    #[test]
    fn test_registry_list_by_exchange() -> Result<(), PrimitiveError> {
        let aapl = make_equity("AAPL", "Apple")?;
        let btc = make_crypto_spot("XXBTZUSD", "BTC/USD")?;

        let reg = InstrumentRegistry::new(vec![aapl, btc]);

        assert_eq!(reg.list_by_exchange(Exchange::IBKR).len(), 1);
        assert_eq!(reg.list_by_exchange(Exchange::Kraken).len(), 1);
        assert_eq!(reg.list_by_exchange(Exchange::Paper).len(), 0);
        Ok(())
    }

    #[test]
    fn test_registry_arc_sharing() -> Result<(), PrimitiveError> {
        let aapl = make_equity("AAPL", "Apple")?;
        let reg = Arc::new(InstrumentRegistry::new(vec![aapl]));

        let reg2 = Arc::clone(&reg);
        let sym = Symbol::new("AAPL")?;
        assert!(reg2.get(&sym).is_some());
        Ok(())
    }

    #[test]
    fn test_registry_len() -> Result<(), PrimitiveError> {
        let instruments = vec![
            make_equity("AAPL", "Apple")?,
            make_equity("GOOG", "Google")?,
            make_crypto_spot("XXBTZUSD", "BTC/USD")?,
        ];
        let reg = InstrumentRegistry::new(instruments);
        assert_eq!(reg.len(), 3);
        assert!(!reg.is_empty());
        Ok(())
    }
}
