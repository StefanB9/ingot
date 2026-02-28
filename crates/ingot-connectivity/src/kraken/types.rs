// Wire-format types for the Kraken WebSocket and REST APIs.

use ingot_primitives::Symbol;
use serde::{Deserialize, Serialize};

/// Kraken wire-format pair string, stored in a fixed-size stack buffer.
/// Constructed from an internal [`Symbol`] via a zero-allocation byte
/// transform: `BTC` -> `XBT`, `'-'` -> `'/'`.
pub(super) struct KrakenPair {
    bytes: [u8; 16],
    len: usize,
}

impl KrakenPair {
    pub(super) fn from_symbol(sym: &Symbol) -> Self {
        let src = sym.as_str().as_bytes();
        let mut bytes = [0u8; 16];
        let mut i = 0usize;
        let mut j = 0usize;
        while i < src.len() && j < bytes.len() {
            if j + 2 < bytes.len() && src.get(i..i + 3).is_some_and(|w| w == b"BTC") {
                bytes[j] = b'X';
                bytes[j + 1] = b'B';
                bytes[j + 2] = b'T';
                i += 3;
                j += 3;
            } else if src[i] == b'-' {
                bytes[j] = b'/';
                i += 1;
                j += 1;
            } else {
                bytes[j] = src[i]; // Symbol bytes are already ASCII uppercase
                i += 1;
                j += 1;
            }
        }
        Self { bytes, len: j }
    }

    /// Like [`from_symbol`] but omits the separator — for Kraken's REST API
    /// pair format (e.g. `"BTC-USD"` -> `"XBTUSD"`).
    pub(super) fn from_symbol_no_sep(sym: &Symbol) -> Self {
        let src = sym.as_str().as_bytes();
        let mut bytes = [0u8; 16];
        let mut i = 0usize;
        let mut j = 0usize;
        while i < src.len() && j < bytes.len() {
            if j + 2 < bytes.len() && src.get(i..i + 3).is_some_and(|w| w == b"BTC") {
                bytes[j] = b'X';
                bytes[j + 1] = b'B';
                bytes[j + 2] = b'T';
                i += 3;
                j += 3;
            } else if src[i] == b'-' {
                i += 1; // skip separator
            } else {
                bytes[j] = src[i];
                i += 1;
                j += 1;
            }
        }
        Self { bytes, len: j }
    }

    pub(super) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl Serialize for KrakenPair {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Serialize)]
pub(super) struct KrakenSubscribeRequest {
    pub(super) event: &'static str,
    pub(super) pair: [KrakenPair; 1],
    pub(super) subscription: KrakenSubscription,
}

#[derive(Serialize)]
pub(super) struct KrakenSubscription {
    pub(super) name: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum KrakenMessage {
    Event(KrakenEvent),
    Unknown(serde_json::Value),
}

// Zero-copy parse of a Kraken ticker array: [channelID, data, channelName,
// pair]
#[derive(Debug, Deserialize)]
pub(super) struct KrakenTickerMessage<'a>(
    #[allow(dead_code)] pub(super) u64,
    pub(super) KrakenTickerData<'a>,
    #[serde(borrow)]
    #[allow(dead_code)]
    pub(super) &'a str,
    #[serde(borrow)] pub(super) &'a str,
);

#[derive(Debug, Deserialize)]
#[serde(tag = "event")]
#[serde(rename_all = "camelCase")]
pub(super) enum KrakenEvent {
    Heartbeat,
    SystemStatus {
        status: String,
        #[serde(default, rename = "connectionID")]
        _connection_id: Option<u64>,
    },
    SubscriptionStatus {
        status: String,
        #[serde(default)]
        pair: Option<String>,
        #[serde(default, rename = "errorMessage")]
        error_message: Option<String>,
    },
    Pong,
}

#[derive(Debug, Deserialize)]
pub(super) struct KrakenTickerData<'a> {
    #[serde(borrow)]
    pub(super) c: (&'a str, &'a str),
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result};

    use super::*;

    #[test]
    fn test_deserialize_ticker_message() -> Result<()> {
        // Real sample from Kraken WS (Ticker Data)
        // Format: [ChannelID, Data, ChannelName, Pair]
        let msg = r#"[345,{"c":["50000.00000","0.0023"]},"ticker","XBT/USD"]"#;

        let KrakenTickerMessage(channel_id, data, channel_name, pair) =
            serde_json::from_str::<KrakenTickerMessage<'_>>(msg)
                .context("Failed to deserialize Ticker")?;

        assert_eq!(channel_id, 345);
        assert_eq!(channel_name, "ticker");
        assert_eq!(pair, "XBT/USD");
        assert_eq!(data.c.0, "50000.00000");

        Ok(())
    }

    #[test]
    fn test_deserialize_heartbeat() -> Result<()> {
        let msg = r#"{"event":"heartbeat"}"#;
        let parsed: KrakenMessage = serde_json::from_str(msg)?;

        match parsed {
            KrakenMessage::Event(KrakenEvent::Heartbeat) => { /* Success */ }
            _ => anyhow::bail!("Expected Heartbeat event, got {parsed:?}"),
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_system_status() -> Result<()> {
        let msg = r#"{"connectionID":18398533478347834,"event":"systemStatus","status":"online","version":"1.9.0"}"#;
        let parsed: KrakenMessage = serde_json::from_str(msg)?;

        match parsed {
            KrakenMessage::Event(KrakenEvent::SystemStatus { status, .. }) => {
                assert_eq!(status, "online");
            }
            _ => anyhow::bail!("Expected SystemStatus event, got {parsed:?}"),
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_subscription_status_success() -> Result<()> {
        let msg = r#"{"channelID":345,"channelName":"ticker","event":"subscriptionStatus","pair":"XBT/USD","status":"subscribed","subscription":{"name":"ticker"}}"#;
        let parsed: KrakenMessage = serde_json::from_str(msg)?;

        match parsed {
            KrakenMessage::Event(KrakenEvent::SubscriptionStatus { status, pair, .. }) => {
                assert_eq!(status, "subscribed");
                assert_eq!(pair.as_deref(), Some("XBT/USD"));
            }
            _ => anyhow::bail!("Expected SubscriptionStatus event, got {parsed:?}"),
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_subscription_status_error() -> Result<()> {
        let msg = r#"{"errorMessage":"Subscription depth not supported","event":"subscriptionStatus","pair":"XBT/USD","status":"error","subscription":{"name":"ticker"}}"#;
        let parsed: KrakenMessage = serde_json::from_str(msg)?;

        match parsed {
            KrakenMessage::Event(KrakenEvent::SubscriptionStatus {
                status,
                error_message,
                ..
            }) => {
                assert_eq!(status, "error");
                match error_message {
                    Some(emsg) => assert!(emsg.contains("not supported")),
                    None => {
                        anyhow::bail!("Expected error message in subscription status, found None")
                    }
                }
            }
            _ => anyhow::bail!("Expected SubscriptionStatus error, got {parsed:?}"),
        }
        Ok(())
    }

    #[test]
    fn test_deserialize_unknown_format_fallback() -> Result<()> {
        // If Kraken sends something totally new, it should fall back to Unknown rather
        // than erroring
        let msg = r#"{"event":"newFancyEvent","someField":123}"#;
        let parsed: KrakenMessage = serde_json::from_str(msg)?;

        match parsed {
            KrakenMessage::Unknown(val) => {
                assert_eq!(val["event"], "newFancyEvent");
            }
            KrakenMessage::Event(_) => anyhow::bail!("Expected Unknown variant, got {parsed:?}"),
        }
        Ok(())
    }
}
