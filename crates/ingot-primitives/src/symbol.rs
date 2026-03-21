use std::fmt;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::error::PrimitiveError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Symbol(SmolStr);

impl Symbol {
    pub fn new(s: &str) -> Result<Self, PrimitiveError> {
        if s.is_empty() {
            return Err(PrimitiveError::EmptySymbol);
        }
        Ok(Self(SmolStr::new(s)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_new_valid() -> Result<(), PrimitiveError> {
        let s = Symbol::new("XXBTZUSD")?;
        assert_eq!(s.as_str(), "XXBTZUSD");
        Ok(())
    }

    #[test]
    fn test_symbol_new_empty_rejected() {
        let result = Symbol::new("");
        assert!(result.is_err());
    }

    #[test]
    fn test_symbol_display() -> Result<(), PrimitiveError> {
        let s = Symbol::new("PF_SOLUSD")?;
        assert_eq!(s.to_string(), "PF_SOLUSD");
        Ok(())
    }

    #[test]
    fn test_symbol_equality() -> Result<(), PrimitiveError> {
        let a = Symbol::new("AAPL")?;
        let b = Symbol::new("AAPL")?;
        assert_eq!(a, b);
        Ok(())
    }

    #[test]
    fn test_symbol_inequality() -> Result<(), PrimitiveError> {
        let a = Symbol::new("AAPL")?;
        let b = Symbol::new("GOOG")?;
        assert_ne!(a, b);
        Ok(())
    }

    #[test]
    fn test_symbol_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let sym = Symbol::new("ES_20260320")?;
        let json = serde_json::to_string(&sym)?;
        let deserialized: Symbol = serde_json::from_str(&json)?;
        assert_eq!(sym, deserialized);
        Ok(())
    }

    #[test]
    fn test_symbol_hash_consistency() -> Result<(), PrimitiveError> {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        let sym = Symbol::new("BTC")?;
        map.insert(sym.clone(), 42);
        assert_eq!(map.get(&Symbol::new("BTC")?), Some(&42));
        Ok(())
    }

    #[test]
    fn test_symbol_inline_storage() -> Result<(), PrimitiveError> {
        // SmolStr inlines strings <= 23 bytes. All ticker symbols should be inline.
        let short = Symbol::new("AAPL")?;
        assert_eq!(short.as_str().len(), 4);

        let long_but_inline = Symbol::new("FI_XBTUSD_260327")?;
        assert!(long_but_inline.as_str().len() <= 23);
        Ok(())
    }
}
