// Kraken WebSocket supervisor: connection, reconnection, and message dispatch.

use std::{collections::HashSet, str::FromStr, time::Duration};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use ingot_core::feed::Ticker;
use ingot_primitives::Symbol;
use rust_decimal::Decimal;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{debug, error, info, instrument, warn};
use url::Url;

use super::types::{KrakenEvent, KrakenMessage, KrakenTickerMessage};

impl super::KrakenExchange {
    #[instrument(skip(self, data_feed), fields(exchange = "kraken"))]
    pub async fn connect(&mut self, data_feed: mpsc::Sender<Ticker>) -> Result<()> {
        info!("Initializing WebSocket Supervisor...");

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<String>(32);
        self.ws_sender = Some(cmd_tx);

        let url_str = Url::parse(&self.base_ws_url)?;

        tokio::spawn(async move {
            let mut active_subscriptions: HashSet<String> = HashSet::new();

            loop {
                info!(url = %url_str, "attempting ws connection");

                match connect_async(url_str.as_str()).await {
                    Ok((ws_stream, _)) => {
                        info!("ws connected");
                        let (mut write, mut read) = ws_stream.split();

                        // 2. Resubscribe to existing topics (if any)
                        for sub_msg in &active_subscriptions {
                            info!(msg = %sub_msg, "resubscribing");
                            if let Err(e) = write.send(Message::Text(sub_msg.clone().into())).await
                            {
                                error!(error = %e, "failed to resubscribe, reconnecting");
                                break;
                            }
                        }

                        loop {
                            tokio::select! {
                                Some(msg) = cmd_rx.recv() => {
                                    // SAFETY: not cancellation-safe — this arm mutates
                                    // `active_subscriptions` before the `.await` on
                                    // `write.send()`. If the task were cancelled at that
                                    // await point the subscription would be recorded as
                                    // active but never transmitted, causing a ghost
                                    // resubscription on the next reconnect. The task is
                                    // currently spawned with no external cancellation
                                    // handle so this is a latent rather than immediate
                                    // risk. Long-term fix: stage messages into a
                                    // `VecDeque` and flush them outside the select! arm.
                                    active_subscriptions.insert(msg.clone());

                                    if let Err(e) = write.send(Message::Text(msg.into())).await {
                                        error!(error = %e, "failed to send command, reconnecting");
                                        break;
                                    }
                                }

                                maybe_msg = read.next() => {
                                    match maybe_msg {
                                        Some(Ok(Message::Text(text))) => {
                                            let text_str: &str = &text;
                                            // Kraken ticker messages are JSON arrays; events are objects.
                                            if text_str.starts_with('[') {
                                                // 1. Ticker Data (Array) — zero-copy price borrow
                                                match serde_json::from_str::<KrakenTickerMessage<'_>>(text_str) {
                                                    Ok(KrakenTickerMessage(_, data, _, pair)) => {
                                                        let price_str = data.c.0;
                                                        match Decimal::from_str(price_str) {
                                                            Ok(price) => {
                                                                let symbol = Symbol::from_kraken(pair);
                                                                let ticker = Ticker::new(symbol.as_str(), price);
                                                                if data_feed.send(ticker).await.is_err() {
                                                                    warn!("Feed receiver dropped. Stopping supervisor.");
                                                                    return;
                                                                }
                                                            }
                                                            Err(e) => error!(price = %price_str, error = %e, "failed to parse decimal price"),
                                                        }
                                                    }
                                                    Err(e) => error!(error = %e, text = %text_str, "json deserialization failed"),
                                                }
                                            } else {
                                                match serde_json::from_str::<KrakenMessage>(text_str) {
                                                    // 2. Events (Heartbeat, Status)
                                                    Ok(KrakenMessage::Event(event)) => match event {
                                                        KrakenEvent::Heartbeat => debug!("Heartbeat received"),
                                                        KrakenEvent::SystemStatus { status, .. } => debug!(status = %status, "system status"),
                                                        KrakenEvent::SubscriptionStatus { status, pair, error_message } => {
                                                            if status == "subscribed" {
                                                                info!(pair = ?pair, "subscription active");
                                                            } else {
                                                                error!(pair = ?pair, error_message = ?error_message, "subscription failed");
                                                            }
                                                        }
                                                        KrakenEvent::Pong => debug!("pong received"),
                                                    },

                                                    // 3. Fallback / Unknown
                                                    Ok(KrakenMessage::Unknown(v)) => debug!(msg = ?v, "unhandled message structure"),
                                                    Err(e) => error!(error = %e, text = %text_str, "json deserialization failed"),
                                                }
                                            }
                                        }
                                        Some(Ok(Message::Ping(_))) => debug!("Received Ping"),
                                        Some(Err(e)) => {
                                            error!(error = %e, "ws stream error");
                                            break;
                                        }
                                        None => {
                                            warn!("ws stream closed by server");
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "connection failed, retrying in 5s");
                    }
                }

                // Backoff before retrying
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        Ok(())
    }
}
