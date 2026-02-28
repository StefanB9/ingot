// Kraken REST API: request signing and order placement.

use std::{
    fmt::Write as FmtWrite,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use base64::{Engine, engine::general_purpose};
use chrono::Utc;
use hmac::{Hmac, Mac};
use ingot_core::execution::{OrderAcknowledgment, OrderRequest};
use ingot_primitives::OrderStatus;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use secrecy::ExposeSecret;
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512, digest::KeyInit};
use tracing::{debug, error, info};

use super::types::KrakenPair;

#[derive(Debug, Deserialize)]
struct KrakenOrderResponse {
    error: Vec<String>,
    result: Option<KrakenOrderResult>,
}

#[derive(Debug, Deserialize)]
struct KrakenOrderResult {
    txid: Option<Vec<String>>,
    #[serde(default)]
    _descr: Option<serde_json::Value>,
}

impl super::KrakenExchange {
    pub(super) fn sign_request(&self, path: &str, nonce: &str, post_data: &str) -> Result<String> {
        let mut sha256 = Sha256::new();
        sha256.update(nonce);
        sha256.update(post_data);
        let sha256_digest = sha256.finalize();

        let secret_bytes = general_purpose::STANDARD.decode(self.api_secret.expose_secret())?;

        let mut hmac = Hmac::<Sha512>::new_from_slice(&secret_bytes)?;
        hmac.update(path.as_bytes());
        hmac.update(&sha256_digest);
        let hmac_digest = hmac.finalize();

        Ok(general_purpose::STANDARD.encode(hmac_digest.into_bytes()))
    }

    pub(super) async fn rest_place_order(
        &self,
        order: &OrderRequest,
    ) -> Result<OrderAcknowledgment> {
        let path = "/0/private/AddOrder";
        let url = format!("{}{}", self.base_rest_url, path);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .to_string();

        let kraken_pair = KrakenPair::from_symbol_no_sep(&order.symbol);

        // Build the URL-encoded request body directly into a pre-allocated String,
        // avoiding the intermediate Vec + per-field String allocations of the old
        // params builder pattern.
        let mut post_data = String::with_capacity(160);
        write!(
            post_data,
            "nonce={}&ordertype={}&type={}&volume={}&pair={}",
            nonce,
            order.order_type,
            order.side,
            order.quantity,
            kraken_pair.as_str(),
        )?;
        if let Some(price) = order.price {
            write!(post_data, "&price={price}")?;
        }
        if order.validate_only {
            post_data.push_str("&validate=true");
        }

        let signature = self.sign_request(path, &nonce, &post_data)?;

        let mut headers = HeaderMap::new();
        headers.insert(
            "API-Key",
            HeaderValue::from_str(self.api_key.expose_secret())?,
        );
        headers.insert("API-Sign", HeaderValue::from_str(&signature)?);
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );

        debug!(url = %url, "sending order request");
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .body(post_data)
            .send()
            .await?;

        let resp_text = resp.text().await?;

        let response_data: KrakenOrderResponse = serde_json::from_str(&resp_text)
            .with_context(|| format!("Failed to parse Kraken API response: {resp_text}"))?;

        if !response_data.error.is_empty() {
            error!(errors = ?response_data.error, "order placement error");
            return Err(anyhow!("Kraken Error: {:?}", response_data.error));
        }

        let result = response_data
            .result
            .ok_or_else(|| anyhow!("Missing 'result' in Kraken response"))?;

        let txid = if let Some(txids) = result.txid {
            txids
                .into_iter()
                .next()
                .context("Kraken returned empty txid array")?
        } else if order.validate_only {
            "VALIDATION-SUCCESS".to_string()
        } else {
            return Err(anyhow!("Missing txid in execution response"));
        };

        info!(txid = %txid, "order placed");

        Ok(OrderAcknowledgment {
            exchange_id: txid,
            client_id: None,
            timestamp: Utc::now(),
            status: OrderStatus::Sent,
        })
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result};
    use base64::{Engine, engine::general_purpose};
    use ingot_core::execution::OrderRequest;
    use ingot_primitives::{OrderSide, OrderStatus, Quantity, Symbol};
    use rust_decimal::dec;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, header_exists, method, path},
    };

    use crate::{Exchange, kraken::KrakenExchange};

    #[test]
    fn test_signature_generation() -> Result<()> {
        // Known test vector for Kraken API signing
        let secret = "kQH5HW/8p1uGOVjbgWA7FunAmGO8lsVI0505+P6972alxtj75JSIh/5yHe/61IPo/\
                      TeJzYL+R1uP1Q+frGe0Rw==";
        let exchange = KrakenExchange::new_with_keys("fake_key".into(), secret.into());

        let path = "/0/private/AddOrder";
        let nonce = "1616492376594";
        let post_data =
            "nonce=1616492376594&ordertype=limit&pair=XBTUSD&price=37500&type=buy&volume=1.25";

        let signature = exchange
            .sign_request(path, nonce, post_data)
            .context("Signing failed")?;

        // Verify it produces a valid base64 string
        assert!(!signature.is_empty());
        assert!(general_purpose::STANDARD.decode(&signature).is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn test_place_order_empty_txid_returns_error() -> Result<()> {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/0/private/AddOrder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "descr": { "order": "buy 1.0 XBTUSD @ market" },
                    "txid": []
                }
            })))
            .mount(&mock_server)
            .await;

        let mut exchange = KrakenExchange::new_with_keys(
            "test_key".into(),
            "kQH5HW/8p1uGOVjbgWA7FunAmGO8lsVI0505+P6972alxtj75JSIh/5yHe/61IPo/\
             TeJzYL+R1uP1Q+frGe0Rw=="
                .into(),
        );
        exchange.set_rest_url(&mock_server.uri());

        let order = OrderRequest::new_market(
            Symbol::new("BTC-USD"),
            OrderSide::Buy,
            Quantity::from(dec!(1.0)),
        );

        let result = exchange.place_order(&order).await;
        assert!(result.is_err(), "expected error for empty txid array");
        let err_msg = format!("{}", result.as_ref().err().context("no error")?);
        assert!(
            err_msg.contains("empty txid"),
            "expected 'empty txid' in error, got: {err_msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_place_order_sends_correct_headers_and_body() -> Result<()> {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/0/private/AddOrder"))
            .and(header_exists("API-Key"))
            .and(header_exists("API-Sign"))
            .and(header("Content-Type", "application/x-www-form-urlencoded"))
            .and(body_string_contains("pair=XBTUSD"))
            .and(body_string_contains("volume=1.5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": [],
                "result": {
                    "descr": { "order": "buy 1.5 XBTUSD @ market" },
                    "txid": ["ODKK-4729-1992"]
                }
            })))
            .mount(&mock_server)
            .await;

        let mut exchange = KrakenExchange::new_with_keys(
            "test_key".into(),
            "kQH5HW/8p1uGOVjbgWA7FunAmGO8lsVI0505+P6972alxtj75JSIh/5yHe/61IPo/\
             TeJzYL+R1uP1Q+frGe0Rw=="
                .into(),
        );
        exchange.set_rest_url(&mock_server.uri());

        let order = OrderRequest::new_market(
            Symbol::new("BTC-USD"),
            OrderSide::Buy,
            Quantity::from(dec!(1.5)),
        );

        // Updated expectation: We get an OrderAcknowledgment struct, not a string
        let ack = exchange
            .place_order(&order)
            .await
            .context("Failed to place order with Mock Server")?;

        assert_eq!(ack.exchange_id, "ODKK-4729-1992");
        assert_eq!(ack.status, OrderStatus::Sent);
        Ok(())
    }
}
