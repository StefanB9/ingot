use ingot_primitives::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KrakenSpotConfig {
    pub api_key: String,
    pub api_secret: String,
    #[serde(default = "default_kraken_spot_rest_url")]
    pub rest_url: String,
    #[serde(default = "default_kraken_spot_ws_url")]
    pub ws_url: String,
    #[serde(default = "default_kraken_spot_ws_auth_url")]
    pub ws_auth_url: String,
}

fn default_kraken_spot_rest_url() -> String {
    "https://api.kraken.com".into()
}

fn default_kraken_spot_ws_url() -> String {
    "wss://ws.kraken.com/v2".into()
}

fn default_kraken_spot_ws_auth_url() -> String {
    "wss://ws-auth.kraken.com/v2".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KrakenFuturesConfig {
    pub api_key: String,
    pub api_secret: String,
    #[serde(default = "default_kraken_futures_rest_url")]
    pub rest_url: String,
    #[serde(default = "default_kraken_futures_ws_url")]
    pub ws_url: String,
}

fn default_kraken_futures_rest_url() -> String {
    "https://futures.kraken.com/derivatives/api/v3".into()
}

fn default_kraken_futures_ws_url() -> String {
    "wss://futures.kraken.com/ws/v1".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperExchangeConfig {
    pub initial_balances: Vec<(Currency, Decimal)>,
    pub slippage_bps: Decimal,
    pub latency_ms: u64,
    pub partial_fill_probability: Decimal,
    pub maker_fee_bps: Decimal,
    pub taker_fee_bps: Decimal,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_kraken_spot_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = KrakenSpotConfig {
            api_key: "test-key".into(),
            api_secret: "test-secret".into(),
            rest_url: default_kraken_spot_rest_url(),
            ws_url: default_kraken_spot_ws_url(),
            ws_auth_url: default_kraken_spot_ws_auth_url(),
        };
        let json = serde_json::to_string(&config)?;
        let deserialized: KrakenSpotConfig = serde_json::from_str(&json)?;
        assert_eq!(config.api_key, deserialized.api_key);
        assert_eq!(config.rest_url, deserialized.rest_url);
        Ok(())
    }

    #[test]
    fn test_kraken_spot_config_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"api_key":"k","api_secret":"s"}"#;
        let config: KrakenSpotConfig = serde_json::from_str(json)?;
        assert_eq!(config.rest_url, "https://api.kraken.com");
        assert_eq!(config.ws_url, "wss://ws.kraken.com/v2");
        assert_eq!(config.ws_auth_url, "wss://ws-auth.kraken.com/v2");
        Ok(())
    }

    #[test]
    fn test_kraken_futures_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = KrakenFuturesConfig {
            api_key: "test-key".into(),
            api_secret: "test-secret".into(),
            rest_url: default_kraken_futures_rest_url(),
            ws_url: default_kraken_futures_ws_url(),
        };
        let json = serde_json::to_string(&config)?;
        let deserialized: KrakenFuturesConfig = serde_json::from_str(&json)?;
        assert_eq!(config.api_key, deserialized.api_key);
        assert_eq!(config.rest_url, deserialized.rest_url);
        Ok(())
    }

    #[test]
    fn test_kraken_futures_config_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{"api_key":"k","api_secret":"s"}"#;
        let config: KrakenFuturesConfig = serde_json::from_str(json)?;
        assert_eq!(
            config.rest_url,
            "https://futures.kraken.com/derivatives/api/v3"
        );
        assert_eq!(config.ws_url, "wss://futures.kraken.com/ws/v1");
        Ok(())
    }

    #[test]
    fn test_paper_exchange_config_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let config = PaperExchangeConfig {
            initial_balances: vec![(Currency::USD, dec!(10000.0)), (Currency::BTC, dec!(1.0))],
            slippage_bps: dec!(5),
            latency_ms: 50,
            partial_fill_probability: dec!(0.1),
            maker_fee_bps: dec!(16),
            taker_fee_bps: dec!(26),
        };
        let json = serde_json::to_string(&config)?;
        let deserialized: PaperExchangeConfig = serde_json::from_str(&json)?;
        assert_eq!(
            config.initial_balances.len(),
            deserialized.initial_balances.len()
        );
        assert_eq!(config.slippage_bps, deserialized.slippage_bps);
        assert_eq!(config.latency_ms, deserialized.latency_ms);
        Ok(())
    }
}
