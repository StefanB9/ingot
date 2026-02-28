mod rest;
mod types;
mod ws;

use std::{borrow::Cow, env};

use anyhow::{Context, Result, anyhow};
use ingot_core::execution::{OrderAcknowledgment, OrderRequest};
use ingot_primitives::Symbol;
use reqwest::Client;
use secrecy::SecretString;
use tokio::sync::mpsc;
use tracing::{info, instrument, warn};
use types::{KrakenPair, KrakenSubscribeRequest, KrakenSubscription};

use crate::Exchange;

const KRAKEN_WS_URL: &str = "wss://ws.kraken.com";
const KRAKEN_REST_URL: &str = "https://api.kraken.com";

pub struct KrakenExchange {
    ws_sender: Option<mpsc::Sender<String>>,
    client: Client,
    api_key: SecretString,
    api_secret: SecretString,
    base_rest_url: Cow<'static, str>,
    base_ws_url: Cow<'static, str>,
}

impl KrakenExchange {
    pub fn new() -> Self {
        let api_key = env::var("KRAKEN_API_KEY").unwrap_or_default();
        let api_secret = env::var("KRAKEN_API_SECRET").unwrap_or_default();

        if api_key.is_empty() || api_secret.is_empty() {
            warn!("KRAKEN_API_KEY or KRAKEN_API_SECRET not set");
        }

        Self {
            ws_sender: None,
            client: Client::new(),
            api_key: SecretString::new(Box::from(api_key)),
            api_secret: SecretString::new(Box::from(api_secret)),
            base_rest_url: Cow::Borrowed(KRAKEN_REST_URL),
            base_ws_url: Cow::Borrowed(KRAKEN_WS_URL),
        }
    }

    /// Create a `KrakenExchange` from environment variables, failing early
    /// if credentials are missing.
    ///
    /// Required env vars: `KRAKEN_API_KEY`, `KRAKEN_API_SECRET`.
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("KRAKEN_API_KEY").context("KRAKEN_API_KEY not set")?;
        let api_secret = env::var("KRAKEN_API_SECRET").context("KRAKEN_API_SECRET not set")?;
        Ok(Self {
            ws_sender: None,
            client: Client::new(),
            api_key: SecretString::new(Box::from(api_key)),
            api_secret: SecretString::new(Box::from(api_secret)),
            base_rest_url: Cow::Borrowed(KRAKEN_REST_URL),
            base_ws_url: Cow::Borrowed(KRAKEN_WS_URL),
        })
    }

    pub fn new_with_keys(key: String, secret: String) -> Self {
        Self {
            ws_sender: None,
            client: Client::new(),
            api_key: SecretString::new(Box::from(key)),
            api_secret: SecretString::new(Box::from(secret)),
            base_rest_url: Cow::Borrowed(KRAKEN_REST_URL),
            base_ws_url: Cow::Borrowed(KRAKEN_WS_URL),
        }
    }

    pub fn set_rest_url(&mut self, url: &str) {
        self.base_rest_url = Cow::Owned(url.to_string());
    }
}

impl Default for KrakenExchange {
    fn default() -> Self {
        Self::new()
    }
}

impl Exchange for KrakenExchange {
    fn id(&self) -> &'static str {
        "kraken"
    }

    #[instrument(skip(self), fields(symbol = %symbol))]
    async fn subscribe_ticker(&mut self, symbol: Symbol) -> Result<()> {
        let sender = self.ws_sender.as_ref().ok_or(anyhow!("Not connected"))?;

        let pair = KrakenPair::from_symbol(&symbol);
        let req = KrakenSubscribeRequest {
            event: "subscribe",
            pair: [pair],
            subscription: KrakenSubscription { name: "ticker" },
        };
        let msg = serde_json::to_string(&req)?;

        info!(pair = %req.pair[0].as_str(), "sending subscription");
        sender.send(msg).await?;

        Ok(())
    }

    #[instrument(skip(self, order), fields(side = ?order.side, symbol = %order.symbol, quantity = %order.quantity))]
    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAcknowledgment> {
        self.rest_place_order(order).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kraken_new_with_keys_creates_instance() {
        let exchange = KrakenExchange::new_with_keys("test-key".into(), "test-secret".into());
        assert_eq!(exchange.id(), "kraken");
    }

    #[test]
    fn test_kraken_default_creates_instance() {
        let exchange = KrakenExchange::default();
        assert_eq!(exchange.id(), "kraken");
    }

    #[test]
    fn test_kraken_set_rest_url() {
        let mut exchange = KrakenExchange::default();
        exchange.set_rest_url("http://localhost:8080");
        assert_eq!(exchange.base_rest_url.as_ref(), "http://localhost:8080");
    }
}
