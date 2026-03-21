use std::fmt;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Currency {
    // Fiat
    USD,
    EUR,
    GBP,
    JPY,
    CHF,
    CAD,
    AUD,
    NZD,
    HKD,
    SGD,
    // Crypto
    BTC,
    ETH,
    SOL,
    XRP,
    ADA,
    DOT,
    AVAX,
    MATIC,
    LINK,
    // Escape hatch
    Other(SmolStr),
}

impl Currency {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "USD" => Self::USD,
            "EUR" => Self::EUR,
            "GBP" => Self::GBP,
            "JPY" => Self::JPY,
            "CHF" => Self::CHF,
            "CAD" => Self::CAD,
            "AUD" => Self::AUD,
            "NZD" => Self::NZD,
            "HKD" => Self::HKD,
            "SGD" => Self::SGD,
            "BTC" | "XBT" => Self::BTC,
            "ETH" => Self::ETH,
            "SOL" => Self::SOL,
            "XRP" => Self::XRP,
            "ADA" => Self::ADA,
            "DOT" => Self::DOT,
            "AVAX" => Self::AVAX,
            "MATIC" => Self::MATIC,
            "LINK" => Self::LINK,
            other => Self::Other(SmolStr::new(other)),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::USD => "USD",
            Self::EUR => "EUR",
            Self::GBP => "GBP",
            Self::JPY => "JPY",
            Self::CHF => "CHF",
            Self::CAD => "CAD",
            Self::AUD => "AUD",
            Self::NZD => "NZD",
            Self::HKD => "HKD",
            Self::SGD => "SGD",
            Self::BTC => "BTC",
            Self::ETH => "ETH",
            Self::SOL => "SOL",
            Self::XRP => "XRP",
            Self::ADA => "ADA",
            Self::DOT => "DOT",
            Self::AVAX => "AVAX",
            Self::MATIC => "MATIC",
            Self::LINK => "LINK",
            Self::Other(s) => s.as_str(),
        }
    }

    pub fn is_fiat(&self) -> bool {
        matches!(
            self,
            Self::USD
                | Self::EUR
                | Self::GBP
                | Self::JPY
                | Self::CHF
                | Self::CAD
                | Self::AUD
                | Self::NZD
                | Self::HKD
                | Self::SGD
        )
    }

    pub fn is_crypto(&self) -> bool {
        matches!(
            self,
            Self::BTC
                | Self::ETH
                | Self::SOL
                | Self::XRP
                | Self::ADA
                | Self::DOT
                | Self::AVAX
                | Self::MATIC
                | Self::LINK
        )
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_currency_from_str_lossy_known_fiat() {
        assert_eq!(Currency::from_str_lossy("USD"), Currency::USD);
        assert_eq!(Currency::from_str_lossy("usd"), Currency::USD);
        assert_eq!(Currency::from_str_lossy("eur"), Currency::EUR);
    }

    #[test]
    fn test_currency_from_str_lossy_known_crypto() {
        assert_eq!(Currency::from_str_lossy("BTC"), Currency::BTC);
        assert_eq!(Currency::from_str_lossy("ETH"), Currency::ETH);
    }

    #[test]
    fn test_currency_xbt_maps_to_btc() {
        assert_eq!(Currency::from_str_lossy("XBT"), Currency::BTC);
        assert_eq!(Currency::from_str_lossy("xbt"), Currency::BTC);
    }

    #[test]
    fn test_currency_unknown_falls_through() {
        let c = Currency::from_str_lossy("DOGE");
        assert_eq!(c, Currency::Other(SmolStr::new("DOGE")));
        assert_eq!(c.as_str(), "DOGE");
    }

    #[test]
    fn test_currency_is_fiat() {
        assert!(Currency::USD.is_fiat());
        assert!(Currency::EUR.is_fiat());
        assert!(!Currency::BTC.is_fiat());
        assert!(!Currency::Other(SmolStr::new("DOGE")).is_fiat());
    }

    #[test]
    fn test_currency_is_crypto() {
        assert!(Currency::BTC.is_crypto());
        assert!(Currency::ETH.is_crypto());
        assert!(!Currency::USD.is_crypto());
        assert!(!Currency::Other(SmolStr::new("DOGE")).is_crypto());
    }

    #[test]
    fn test_currency_display() {
        assert_eq!(Currency::USD.to_string(), "USD");
        assert_eq!(Currency::BTC.to_string(), "BTC");
        assert_eq!(Currency::Other(SmolStr::new("DOGE")).to_string(), "DOGE");
    }

    #[test]
    fn test_currency_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let currencies = vec![
            Currency::USD,
            Currency::BTC,
            Currency::Other(SmolStr::new("DOGE")),
        ];
        for c in currencies {
            let json = serde_json::to_string(&c)?;
            let deserialized: Currency = serde_json::from_str(&json)?;
            assert_eq!(c, deserialized);
        }
        Ok(())
    }
}
