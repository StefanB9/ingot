use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_core::{Instrument, OhlcvBar, OrderBookSnapshot, Tick, TickerSnapshot};
use ingot_primitives::Symbol;
use reqwest::Client;
use serde::de::DeserializeOwned;
use tracing::instrument;

use super::{
    mapper,
    models::{
        KrakenAssetPair, KrakenOhlcValue, KrakenOrderBook, KrakenResponse, KrakenTickerInfo,
        KrakenTradesValue,
    },
};
use crate::{
    config::KrakenSpotConfig, error::ConnectivityError, rate_limiter::RateLimiter,
    traits::MarketDataProvider,
};

pub struct KrakenSpotRestClient {
    http: Client,
    config: KrakenSpotConfig,
    rate_limiter: RateLimiter,
}

impl KrakenSpotRestClient {
    /// Create a new client with default rate limiter (15 tokens, ~0.33/sec).
    pub fn new(config: KrakenSpotConfig) -> anyhow::Result<Self> {
        let http = Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        let rate_limiter = RateLimiter::new(15, 1.0 / 3.0);
        Ok(Self {
            http,
            config,
            rate_limiter,
        })
    }

    /// Create a new client with a shared rate limiter.
    #[allow(dead_code)]
    pub(crate) fn with_rate_limiter(
        config: KrakenSpotConfig,
        rate_limiter: RateLimiter,
    ) -> anyhow::Result<Self> {
        let http = Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            http,
            config,
            rate_limiter,
        })
    }

    /// Send a GET request, parse the Kraken envelope, and check for errors.
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<T> {
        self.rate_limiter
            .acquire()
            .await
            .context("rate limiter failed")?;

        let url = format!("{}{path}", self.config.rest_url);
        let response = self
            .http
            .get(&url)
            .query(params)
            .send()
            .await
            .map_err(ConnectivityError::Http)
            .context("HTTP request failed")?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ConnectivityError::RateLimited {
                retry_after_ms: 5000,
            }
            .into());
        }

        let body = response
            .text()
            .await
            .map_err(ConnectivityError::Http)
            .context("failed to read response body")?;

        let envelope: KrakenResponse<T> = serde_json::from_str(&body)
            .map_err(ConnectivityError::Deserialization)
            .context("failed to deserialize Kraken response")?;

        if !envelope.error.is_empty() {
            let msg = envelope.error.join("; ");
            return Err(ConnectivityError::ApiError {
                code: "KRAKEN".into(),
                message: msg,
            }
            .into());
        }

        envelope.result.ok_or_else(|| {
            ConnectivityError::InvalidResponse(
                "Kraken response had empty error array but no result".into(),
            )
            .into()
        })
    }
}

impl MarketDataProvider for KrakenSpotRestClient {
    #[instrument(skip(self))]
    async fn fetch_instruments(&self) -> anyhow::Result<Vec<Instrument>> {
        let pairs: HashMap<String, KrakenAssetPair> = self.get("/0/public/AssetPairs", &[]).await?;

        pairs
            .iter()
            .map(|(name, pair)| mapper::map_asset_pair(name, pair))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map asset pairs")
    }

    #[instrument(skip(self))]
    async fn fetch_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<OhlcvBar>> {
        let kraken_interval = mapper::interval_to_kraken(interval).context("invalid interval")?;

        let mut params = vec![("pair", symbol.as_str()), ("interval", &kraken_interval)];
        let since_str;
        if let Some(ts) = since {
            since_str = ts.timestamp().to_string();
            params.push(("since", &since_str));
        }

        let result: HashMap<String, KrakenOhlcValue> = self.get("/0/public/OHLC", &params).await?;

        let bars = result
            .into_iter()
            .find_map(|(_, v)| match v {
                KrakenOhlcValue::Bars(bars) => Some(bars),
                KrakenOhlcValue::Last(_) => None,
            })
            .ok_or_else(|| ConnectivityError::InvalidResponse("no OHLC data in response".into()))?;

        bars.iter()
            .map(|bar| mapper::map_ohlc_bar(symbol, &kraken_interval, bar))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map OHLC bars")
    }

    #[instrument(skip(self))]
    async fn fetch_trades(
        &self,
        symbol: &Symbol,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<(Vec<Tick>, Option<DateTime<Utc>>)> {
        let mut params: Vec<(&str, &str)> = vec![("pair", symbol.as_str())];
        let since_str;
        if let Some(ts) = since {
            since_str = ts
                .timestamp_nanos_opt()
                .context("timestamp out of range")?
                .to_string();
            params.push(("since", &since_str));
        }

        let result: HashMap<String, KrakenTradesValue> =
            self.get("/0/public/Trades", &params).await?;

        let mut trades_raw = None;
        let mut last_cursor = None;

        for (_key, value) in result {
            match value {
                KrakenTradesValue::Trades(t) => trades_raw = Some(t),
                KrakenTradesValue::Last(cursor) => {
                    if let Ok(nanos) = cursor.parse::<i64>() {
                        #[allow(clippy::cast_sign_loss)]
                        let sub_nanos = (nanos % 1_000_000_000) as u32;
                        last_cursor = DateTime::from_timestamp(nanos / 1_000_000_000, sub_nanos);
                    }
                }
            }
        }

        let raw = trades_raw.ok_or_else(|| {
            ConnectivityError::InvalidResponse("no trade data in response".into())
        })?;

        let ticks = raw
            .iter()
            .map(|t| mapper::map_trade(symbol, t))
            .collect::<anyhow::Result<Vec<_>>>()
            .context("failed to map trades")?;

        Ok((ticks, last_cursor))
    }

    #[instrument(skip(self))]
    async fn fetch_ticker(&self, symbol: &Symbol) -> anyhow::Result<TickerSnapshot> {
        let result: HashMap<String, KrakenTickerInfo> = self
            .get("/0/public/Ticker", &[("pair", symbol.as_str())])
            .await?;

        let (_name, info) = result
            .into_iter()
            .next()
            .ok_or_else(|| ConnectivityError::InvalidResponse("empty ticker response".into()))?;

        mapper::map_ticker(symbol, &info).context("failed to map ticker")
    }

    #[instrument(skip(self))]
    async fn fetch_order_book(
        &self,
        symbol: &Symbol,
        depth: u32,
    ) -> anyhow::Result<OrderBookSnapshot> {
        let depth_str = depth.to_string();
        let params = [("pair", symbol.as_str()), ("count", &depth_str)];

        let result: HashMap<String, KrakenOrderBook> = self.get("/0/public/Depth", &params).await?;

        let (_name, book) = result.into_iter().next().ok_or_else(|| {
            ConnectivityError::InvalidResponse("empty order book response".into())
        })?;

        mapper::map_order_book(symbol, &book).context("failed to map order book")
    }
}

#[cfg(test)]
#[allow(clippy::unreadable_literal)]
mod tests {
    use rust_decimal_macros::dec;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    use super::*;

    async fn setup() -> anyhow::Result<(MockServer, KrakenSpotRestClient)> {
        let server = MockServer::start().await;
        let config = KrakenSpotConfig {
            api_key: String::new(),
            api_secret: String::new(),
            rest_url: server.uri(),
            ws_url: String::new(),
            ws_auth_url: String::new(),
        };
        let client = KrakenSpotRestClient::new(config)?;
        Ok((server, client))
    }

    // ---- Envelope / error handling ----

    #[tokio::test]
    async fn test_get_handles_api_error() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Ticker"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": ["EGeneral:Invalid arguments"],
                "result": null
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("BAD")?;
        let result = client.fetch_ticker(&symbol).await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("EGeneral:Invalid arguments"),
            "unexpected error: {err_msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_get_handles_429_rate_limit() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Ticker"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let result = client.fetch_ticker(&symbol).await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("rate limited"),
            "unexpected error: {err_msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_get_handles_invalid_json() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Ticker"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not valid json"))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let result = client.fetch_ticker(&symbol).await;
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().context("expected error")?);
        assert!(
            err_msg.contains("deserialize"),
            "unexpected error: {err_msg}"
        );
        Ok(())
    }

    // ---- fetch_instruments ----

    #[tokio::test]
    async fn test_fetch_instruments_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/AssetPairs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": {
                        "base": "XXBT",
                        "quote": "ZUSD",
                        "wsname": "XBT/USD",
                        "altname": "XBTUSD",
                        "pair_decimals": 1,
                        "lot_decimals": 8,
                        "ordermin": "0.0001",
                        "costmin": "0.5",
                        "tick_size": "0.1",
                        "leverage_buy": [2, 3],
                        "leverage_sell": [2, 3]
                    },
                    "XETHZUSD": {
                        "base": "XETH",
                        "quote": "ZUSD",
                        "wsname": "ETH/USD",
                        "altname": "ETHUSD",
                        "pair_decimals": 2,
                        "lot_decimals": 8,
                        "ordermin": "0.01",
                        "costmin": "0.5",
                        "tick_size": "0.01",
                        "leverage_buy": [],
                        "leverage_sell": []
                    }
                }
            })))
            .mount(&server)
            .await;

        let instruments = client.fetch_instruments().await?;
        assert_eq!(instruments.len(), 2);

        let btc = instruments
            .iter()
            .find(|i| i.symbol.as_str() == "XXBTZUSD")
            .context("missing XXBTZUSD")?;
        assert_eq!(btc.base_currency, ingot_primitives::Currency::BTC);
        assert_eq!(btc.quote_currency, ingot_primitives::Currency::USD);
        assert_eq!(btc.tick_size.value(), dec!(0.1));
        assert_eq!(btc.display_name.as_str(), "XBT/USD");

        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_instruments_api_error() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/AssetPairs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": ["EGeneral:Unknown method"],
                "result": null
            })))
            .mount(&server)
            .await;

        let result = client.fetch_instruments().await;
        assert!(result.is_err());
        Ok(())
    }

    // ---- fetch_ohlcv ----

    #[tokio::test]
    async fn test_fetch_ohlcv_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/OHLC"))
            .and(query_param("pair", "XXBTZUSD"))
            .and(query_param("interval", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": [
                        [1616663400, "56200.0", "56300.0", "56100.0", "56250.0", "56225.5", "12.345", 847],
                        [1616663460, "56250.0", "56350.0", "56200.0", "56300.0", "56275.0", "8.5", 520]
                    ],
                    "last": 1616663460
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let bars = client.fetch_ohlcv(&symbol, "1m", None).await?;

        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].open.value(), dec!(56200.0));
        assert_eq!(bars[0].interval.as_str(), "1m");
        assert_eq!(bars[1].close.value(), dec!(56300.0));
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_ohlcv_with_since() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/OHLC"))
            .and(query_param("since", "1616663400"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": [
                        [1616663460, "56250.0", "56350.0", "56200.0", "56300.0", "56275.0", "8.5", 520]
                    ],
                    "last": 1616663460
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let since = DateTime::from_timestamp(1_616_663_400, 0).context("bad ts")?;
        let bars = client.fetch_ohlcv(&symbol, "1m", Some(since)).await?;

        assert_eq!(bars.len(), 1);
        Ok(())
    }

    // ---- fetch_trades ----

    #[tokio::test]
    async fn test_fetch_trades_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Trades"))
            .and(query_param("pair", "XXBTZUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": [
                        ["56200.10000", "0.00100000", 1616663594.2009, "b", "m", "", 12345],
                        ["56210.00000", "0.50000000", 1616663595.0, "s", "l", "", 12346]
                    ],
                    "last": "1616663595000000000"
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let (ticks, cursor) = client.fetch_trades(&symbol, None).await?;

        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[0].side, Some(ingot_primitives::OrderSide::Buy));
        assert_eq!(ticks[1].side, Some(ingot_primitives::OrderSide::Sell));
        assert!(cursor.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_trades_buy_sell_mapping() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Trades"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": [
                        ["56200.0", "1.0", 1616663594.0, "b", "m", "", 1],
                        ["56210.0", "2.0", 1616663595.0, "s", "l", "", 2]
                    ],
                    "last": "1616663595000000000"
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let (ticks, _) = client.fetch_trades(&symbol, None).await?;

        assert_eq!(ticks[0].side, Some(ingot_primitives::OrderSide::Buy));
        assert_eq!(ticks[1].side, Some(ingot_primitives::OrderSide::Sell));
        Ok(())
    }

    // ---- fetch_ticker ----

    #[tokio::test]
    async fn test_fetch_ticker_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Ticker"))
            .and(query_param("pair", "XXBTZUSD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": {
                        "a": ["67010.00000", "1", "1.000"],
                        "b": ["67000.00000", "2", "2.000"],
                        "c": ["67005.00000", "0.001"],
                        "v": ["1000.0", "5000.0"],
                        "p": ["67005.0", "67000.0"],
                        "t": [100, 500],
                        "l": ["66900.0", "66800.0"],
                        "h": ["67100.0", "67200.0"],
                        "o": "66950.0"
                    }
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let ticker = client.fetch_ticker(&symbol).await?;

        assert_eq!(ticker.bid.value(), dec!(67000.00000));
        assert_eq!(ticker.ask.value(), dec!(67010.00000));
        assert_eq!(ticker.last.value(), dec!(67005.00000));
        assert_eq!(ticker.volume_24h.value(), dec!(5000.0));
        Ok(())
    }

    // ---- fetch_order_book ----

    #[tokio::test]
    async fn test_fetch_order_book_happy_path() -> anyhow::Result<()> {
        let (server, client) = setup().await?;

        Mock::given(method("GET"))
            .and(path("/0/public/Depth"))
            .and(query_param("pair", "XXBTZUSD"))
            .and(query_param("count", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "XXBTZUSD": {
                        "asks": [
                            ["67010.0", "1.5", 1616663400],
                            ["67020.0", "2.0", 1616663401]
                        ],
                        "bids": [
                            ["67000.0", "3.0", 1616663400],
                            ["66990.0", "1.0", 1616663399]
                        ]
                    }
                }
            })))
            .mount(&server)
            .await;

        let symbol = Symbol::new("XXBTZUSD")?;
        let book = client.fetch_order_book(&symbol, 2).await?;

        assert_eq!(book.asks.len(), 2);
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.asks[0].price.value(), dec!(67010.0));
        assert_eq!(book.bids[0].price.value(), dec!(67000.0));
        assert_eq!(book.bids[0].quantity.value(), dec!(3.0));
        Ok(())
    }
}
