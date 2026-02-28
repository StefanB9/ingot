use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::CurrencyCode;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Symbol([u8; 16]);

impl Symbol {
    pub fn new(s: &str) -> Self {
        let mut bytes = [0u8; 16];
        for (dst, src) in bytes.iter_mut().zip(s.bytes()) {
            *dst = if src == b'/' {
                b'-'
            } else {
                src.to_ascii_uppercase()
            };
        }
        Self(bytes)
    }

    pub fn from_kraken(s: &str) -> Self {
        let mut bytes = [0u8; 16];
        let src = s.as_bytes();
        let mut i = 0usize;
        let mut j = 0usize;
        while i < src.len() && j < bytes.len() {
            if j + 2 < bytes.len()
                && src
                    .get(i..i + 3)
                    .is_some_and(|w| w.eq_ignore_ascii_case(b"XBT"))
            {
                bytes[j] = b'B';
                bytes[j + 1] = b'T';
                bytes[j + 2] = b'C';
                i += 3;
                j += 3;
            } else if src[i] == b'/' {
                bytes[j] = b'-';
                i += 1;
                j += 1;
            } else {
                bytes[j] = src[i].to_ascii_uppercase();
                i += 1;
                j += 1;
            }
        }
        Self(bytes)
    }

    pub fn as_str(&self) -> &str {
        let len = self.0.iter().position(|&b| b == 0).unwrap_or(16);
        // SAFETY: all constructors (new, from_kraken, from_str) only write
        // valid ASCII/UTF-8 bytes. The empty-string fallback is a defensive
        // guard against corrupt data arriving via IPC shared memory.
        std::str::from_utf8(&self.0[..len]).unwrap_or("")
    }

    pub fn parts(&self) -> Option<(CurrencyCode, CurrencyCode)> {
        let s = self.as_str();
        let (base, quote) = s.split_once('-')?;
        if base.is_empty() || quote.is_empty() || quote.contains('-') {
            return None;
        }
        Some((CurrencyCode::new(base), CurrencyCode::new(quote)))
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Symbol({:?})", self.as_str())
    }
}

impl FromStr for Symbol {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(s))
    }
}

impl Serialize for Symbol {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Symbol {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Self::new(&s))
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result};

    use super::*;

    #[test]
    fn test_symbol_new_uppercases_and_normalizes() {
        assert_eq!(Symbol::new("btc-usd").as_str(), "BTC-USD");
        assert_eq!(Symbol::new("BTC/USD").as_str(), "BTC-USD");
        assert_eq!(Symbol::new("eth-usd").as_str(), "ETH-USD");
    }

    #[test]
    fn test_symbol_parts_valid() -> Result<()> {
        let sym = Symbol::new("BTC-USD");
        let (base, quote) = sym.parts().context("valid symbol")?;
        assert_eq!(base.as_str(), "BTC");
        assert_eq!(quote.as_str(), "USD");
        Ok(())
    }

    #[test]
    fn test_symbol_parts_invalid() {
        assert!(Symbol::new("BTCUSD").parts().is_none());
        assert!(Symbol::new("-USD").parts().is_none());
        assert!(Symbol::new("BTC-").parts().is_none());
        assert!(Symbol::new("BTC-USD-EXTRA").parts().is_none());
    }

    #[test]
    fn test_symbol_display() {
        assert_eq!(Symbol::new("BTC-USD").to_string(), "BTC-USD");
    }

    #[test]
    fn test_symbol_from_str() -> Result<()> {
        let sym: Symbol = "BTC-USD".parse().context("parse failed")?;
        assert_eq!(sym.as_str(), "BTC-USD");
        Ok(())
    }

    #[test]
    fn test_symbol_serialization() -> Result<()> {
        let sym = Symbol::new("ETH-USD");
        let json = serde_json::to_string(&sym).context("serialize failed")?;
        assert_eq!(json, r#""ETH-USD""#);
        let deserialized: Symbol = serde_json::from_str(&json).context("deserialize failed")?;
        assert_eq!(sym, deserialized);
        Ok(())
    }

    #[test]
    fn test_symbol_copy() {
        let s1 = Symbol::new("BTC-USD");
        let s2 = s1;
        assert_eq!(s1, s2);
    }

    #[test]
    fn test_symbol_from_kraken_xbt_usd() {
        assert_eq!(Symbol::from_kraken("XBT/USD").as_str(), "BTC-USD");
    }

    #[test]
    fn test_symbol_from_kraken_eth_usd() {
        // No XBT substitution needed; only slash normalisation.
        assert_eq!(Symbol::from_kraken("ETH/USD").as_str(), "ETH-USD");
    }

    #[test]
    fn test_symbol_from_kraken_xbt_eur() {
        assert_eq!(Symbol::from_kraken("XBT/EUR").as_str(), "BTC-EUR");
    }

    #[test]
    fn test_symbol_from_kraken_lowercase() {
        assert_eq!(Symbol::from_kraken("xbt/usd").as_str(), "BTC-USD");
    }

    #[test]
    fn test_symbol_from_kraken_matches_new_for_non_xbt() {
        assert_eq!(Symbol::from_kraken("ETH/USD"), Symbol::new("ETH-USD"));
    }
}
