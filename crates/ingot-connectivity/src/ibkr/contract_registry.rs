use std::{collections::HashMap, sync::Arc};

use ingot_core::Instrument;
use ingot_primitives::Symbol;

pub struct IbkrContractRegistry {
    conid_to_symbol: HashMap<i64, Symbol>,
    symbol_to_conid: HashMap<Symbol, i64>,
    conid_to_instrument: HashMap<i64, Arc<Instrument>>,
}

impl IbkrContractRegistry {
    pub fn new() -> Self {
        Self {
            conid_to_symbol: HashMap::new(),
            symbol_to_conid: HashMap::new(),
            conid_to_instrument: HashMap::new(),
        }
    }

    pub fn register(&mut self, conid: i64, symbol: Symbol, instrument: Instrument) {
        // Remove old symbol mapping if overwriting an existing conid
        if let Some(old_symbol) = self.conid_to_symbol.get(&conid) {
            self.symbol_to_conid.remove(old_symbol);
        }
        self.conid_to_symbol.insert(conid, symbol.clone());
        self.symbol_to_conid.insert(symbol, conid);
        self.conid_to_instrument.insert(conid, Arc::new(instrument));
    }

    pub fn symbol_for_conid(&self, conid: i64) -> Option<&Symbol> {
        self.conid_to_symbol.get(&conid)
    }

    pub fn conid_for_symbol(&self, symbol: &Symbol) -> Option<i64> {
        self.symbol_to_conid.get(symbol).copied()
    }

    pub fn instrument_for_conid(&self, conid: i64) -> Option<&Arc<Instrument>> {
        self.conid_to_instrument.get(&conid)
    }

    pub fn len(&self) -> usize {
        self.conid_to_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.conid_to_symbol.is_empty()
    }

    pub fn all_instruments(&self) -> Vec<&Arc<Instrument>> {
        self.conid_to_instrument.values().collect()
    }
}

#[cfg(test)]
mod tests {
    use ingot_primitives::{AssetClass, Currency, Exchange, Price, Quantity};
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;

    fn sample_instrument(
        symbol_str: &str,
        conid_label: &str,
    ) -> Result<(Symbol, Instrument), Box<dyn std::error::Error>> {
        let symbol = Symbol::new(symbol_str)?;
        let instrument = Instrument {
            symbol: symbol.clone(),
            asset_class: AssetClass::Equity,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.01)),
            display_name: SmolStr::new(conid_label),
            details: ingot_core::InstrumentDetails::Equity {
                isin: None,
                lot_size: Quantity::new(dec!(1))?,
                fractional: false,
            },
        };
        Ok((symbol, instrument))
    }

    #[test]
    fn test_registry_new_empty() {
        let reg = IbkrContractRegistry::new();
        assert_eq!(reg.len(), 0);
        assert!(reg.is_empty());
    }

    #[test]
    fn test_registry_register_and_lookup_by_conid() -> Result<(), Box<dyn std::error::Error>> {
        let mut reg = IbkrContractRegistry::new();
        let (symbol, instrument) = sample_instrument("AAPL", "Apple Inc")?;
        reg.register(265598, symbol.clone(), instrument);

        let found = reg.symbol_for_conid(265598).ok_or("not found")?;
        assert_eq!(found, &symbol);
        Ok(())
    }

    #[test]
    fn test_registry_register_and_lookup_by_symbol() -> Result<(), Box<dyn std::error::Error>> {
        let mut reg = IbkrContractRegistry::new();
        let (symbol, instrument) = sample_instrument("AAPL", "Apple Inc")?;
        reg.register(265598, symbol.clone(), instrument);

        let conid = reg.conid_for_symbol(&symbol).ok_or("not found")?;
        assert_eq!(conid, 265598);
        Ok(())
    }

    #[test]
    fn test_registry_instrument_lookup() -> Result<(), Box<dyn std::error::Error>> {
        let mut reg = IbkrContractRegistry::new();
        let (symbol, instrument) = sample_instrument("AAPL", "Apple Inc")?;
        reg.register(265598, symbol, instrument);

        let inst = reg.instrument_for_conid(265598).ok_or("not found")?;
        assert_eq!(inst.display_name.as_str(), "Apple Inc");
        assert_eq!(inst.asset_class, AssetClass::Equity);
        Ok(())
    }

    #[test]
    fn test_registry_missing_conid_returns_none() {
        let reg = IbkrContractRegistry::new();
        assert!(reg.symbol_for_conid(999999).is_none());
        assert!(reg.instrument_for_conid(999999).is_none());
    }

    #[test]
    fn test_registry_missing_symbol_returns_none() -> Result<(), Box<dyn std::error::Error>> {
        let reg = IbkrContractRegistry::new();
        let unknown = Symbol::new("ZZZZ")?;
        assert!(reg.conid_for_symbol(&unknown).is_none());
        Ok(())
    }

    #[test]
    fn test_registry_len_tracks_registrations() -> Result<(), Box<dyn std::error::Error>> {
        let mut reg = IbkrContractRegistry::new();
        let (s1, i1) = sample_instrument("AAPL", "Apple")?;
        let (s2, i2) = sample_instrument("MSFT", "Microsoft")?;
        let (s3, i3) = sample_instrument("GOOG", "Google")?;

        reg.register(265598, s1, i1);
        reg.register(272093, s2, i2);
        reg.register(208813720, s3, i3);

        assert_eq!(reg.len(), 3);
        assert!(!reg.is_empty());
        Ok(())
    }

    #[test]
    fn test_registry_overwrite_replaces_entry() -> Result<(), Box<dyn std::error::Error>> {
        let mut reg = IbkrContractRegistry::new();
        let (s1, i1) = sample_instrument("AAPL", "Apple Old")?;
        let (s2, i2) = sample_instrument("AAPL2", "Apple New")?;

        reg.register(265598, s1.clone(), i1);
        assert_eq!(
            reg.symbol_for_conid(265598).map(Symbol::as_str),
            Some("AAPL")
        );

        // Overwrite same conid with new symbol
        reg.register(265598, s2.clone(), i2);
        assert_eq!(
            reg.symbol_for_conid(265598).map(Symbol::as_str),
            Some("AAPL2")
        );

        // Old symbol mapping should be removed
        assert!(reg.conid_for_symbol(&s1).is_none());
        // New symbol mapping should exist
        assert_eq!(reg.conid_for_symbol(&s2), Some(265598));
        // Length should still be 1
        assert_eq!(reg.len(), 1);
        Ok(())
    }
}
