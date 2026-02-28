pub mod kraken;

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use chrono::Utc;
use ingot_core::{
    execution::{OrderAcknowledgment, OrderRequest},
    feed::Ticker,
};
use ingot_primitives::{OrderStatus, Symbol};
use rust_decimal::dec;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{info, instrument, warn};

/// Abstracts over market-data exchanges that are already connected and
/// ready for use.
///
/// Connection and authentication are lifecycle concerns that vary
/// significantly between exchange implementations; they are intentionally
/// excluded from this trait. Each concrete type exposes its own `connect`
/// method. The engine receives an exchange value that has already completed
/// setup, so every method on this trait can assume an active connection.
// `async_fn_in_trait`: this trait is used exclusively via monomorphic generics
// (`TradingEngine<E: Exchange>`) — never as `dyn Exchange`. The compiler
// verifies `Send` bounds on returned futures at each `tokio::spawn` call site,
// so the inability to express them in the trait definition is not a problem.
#[allow(async_fn_in_trait)]
pub trait Exchange: Send + Sync {
    fn id(&self) -> &'static str;

    async fn subscribe_ticker(&mut self, symbol: Symbol) -> Result<()>;
    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAcknowledgment>;
}

/// Marker type for a [`PaperExchange`] that has not yet been connected.
pub struct Disconnected;

/// Live state carried by a connected [`PaperExchange`].
pub struct Connected {
    feed_sender: mpsc::Sender<Ticker>,
    /// Active simulation tasks, keyed by symbol. At most one task per symbol.
    subscriptions: HashMap<Symbol, JoinHandle<()>>,
}

/// A paper-trading exchange whose connection state is tracked at compile time.
///
/// Call [`PaperExchange::connect`] on `PaperExchange<Disconnected>` to obtain
/// a `PaperExchange<Connected>`, which implements [`Exchange`] and can be
/// passed to the engine.
pub struct PaperExchange<S> {
    id: &'static str,
    state: S,
}

impl PaperExchange<Disconnected> {
    pub fn new() -> Self {
        Self {
            id: "paper-exchange",
            state: Disconnected,
        }
    }

    #[instrument(skip(self, data_feed), fields(exchange = %self.id))]
    pub async fn connect(
        self,
        data_feed: mpsc::Sender<Ticker>,
    ) -> Result<PaperExchange<Connected>> {
        info!(exchange = %self.id, "connecting");
        Ok(PaperExchange {
            id: self.id,
            state: Connected {
                feed_sender: data_feed,
                subscriptions: HashMap::new(),
            },
        })
    }
}

impl Default for PaperExchange<Disconnected> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AnyExchange — runtime exchange dispatch via enum (zero-cost match)
// ---------------------------------------------------------------------------

/// Runtime dispatch between exchange backends.
///
/// Monomorphic enum adapter so that `TradingEngine<AnyExchange>` can work
/// with either Paper or Kraken without `Box<dyn Exchange>`.
pub enum AnyExchange {
    /// Paper-trading simulator.
    Paper(PaperExchange<Connected>),
    /// Live Kraken exchange.
    Kraken(kraken::KrakenExchange),
}

impl Exchange for AnyExchange {
    fn id(&self) -> &'static str {
        match self {
            Self::Paper(e) => e.id(),
            Self::Kraken(e) => e.id(),
        }
    }

    async fn subscribe_ticker(&mut self, symbol: Symbol) -> Result<()> {
        match self {
            Self::Paper(e) => e.subscribe_ticker(symbol).await,
            Self::Kraken(e) => e.subscribe_ticker(symbol).await,
        }
    }

    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAcknowledgment> {
        match self {
            Self::Paper(e) => e.place_order(order).await,
            Self::Kraken(e) => e.place_order(order).await,
        }
    }
}

impl Exchange for PaperExchange<Connected> {
    fn id(&self) -> &'static str {
        self.id
    }

    #[instrument(skip(self), fields(symbol = %symbol))]
    async fn subscribe_ticker(&mut self, symbol: Symbol) -> Result<()> {
        if let Some(existing) = self.state.subscriptions.remove(&symbol) {
            existing.abort();
            info!(symbol = %symbol, exchange = self.id, "replaced existing subscription");
        } else {
            info!(symbol = %symbol, exchange = self.id, "subscribed to ticker");
        }

        let sender = self.state.feed_sender.clone();
        let handle = tokio::spawn(async move {
            let mut price = dec!(50000.0);
            loop {
                price += dec!(10.0);
                let tick = Ticker::new(symbol.as_str(), price);

                if sender.send(tick).await.is_err() {
                    warn!(symbol = %symbol, "feed receiver dropped, stopping stream");
                    break;
                }

                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
        self.state.subscriptions.insert(symbol, handle);

        Ok(())
    }

    #[instrument(skip(self, order))]
    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAcknowledgment> {
        info!(
            side = ?order.side,
            quantity = %order.quantity,
            symbol = %order.symbol,
            "[Paper] Order Placed"
        );

        Ok(OrderAcknowledgment {
            exchange_id: "PAPER-ID-001".to_string(),
            client_id: None,
            timestamp: Utc::now(),
            status: OrderStatus::Filled,
        })
    }
}

#[cfg(test)]
mod tests {
    use ingot_core::execution::OrderRequest;
    use ingot_primitives::{OrderSide, Quantity};
    use rust_decimal::dec;

    use super::*;

    async fn make_paper() -> Result<PaperExchange<Connected>> {
        let (tx, _rx) = mpsc::channel(16);
        PaperExchange::new().connect(tx).await
    }

    #[tokio::test]
    async fn test_any_exchange_paper_id() -> Result<()> {
        let paper = make_paper().await?;
        let any = AnyExchange::Paper(paper);
        assert_eq!(any.id(), "paper-exchange");
        Ok(())
    }

    #[tokio::test]
    async fn test_any_exchange_paper_subscribe() -> Result<()> {
        let paper = make_paper().await?;
        let mut any = AnyExchange::Paper(paper);
        any.subscribe_ticker(Symbol::new("BTC-USD")).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_any_exchange_paper_place_order() -> Result<()> {
        let paper = make_paper().await?;
        let any = AnyExchange::Paper(paper);
        let order = OrderRequest::new_market(
            Symbol::new("BTC-USD"),
            OrderSide::Buy,
            Quantity::from(dec!(1)),
        );
        let ack = any.place_order(&order).await?;
        assert_eq!(ack.status, OrderStatus::Filled);
        Ok(())
    }

    #[tokio::test]
    async fn test_any_exchange_kraken_id() -> Result<()> {
        let kraken = kraken::KrakenExchange::new();
        let any = AnyExchange::Kraken(kraken);
        assert_eq!(any.id(), "kraken");
        Ok(())
    }
}
