//! A cloneable handle to a running [`TradingEngine`](crate::TradingEngine).
//!
//! `EngineHandle` wraps the command channel and shared state, providing
//! a clean async API for server handlers. All fields are `Arc` or
//! `mpsc::Sender` — cloning is cheap.

use std::sync::Arc;

use anyhow::{Context, Result};
use ingot_core::{
    accounting::{Ledger, QuoteBoard},
    api::WsMessage,
};
use ingot_primitives::{Amount, Currency, Money, Price, Symbol};
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::instrument;

use crate::EngineCommand;

/// Thread-safe, cheaply cloneable handle to a running engine.
#[derive(Clone)]
pub struct EngineHandle {
    cmd_tx: mpsc::Sender<EngineCommand>,
    ledger: Arc<RwLock<Ledger>>,
    market_data: Arc<RwLock<QuoteBoard>>,
    event_tx: Option<broadcast::Sender<WsMessage>>,
}

impl EngineHandle {
    /// Create a new handle from the engine's command sender and shared state.
    pub fn new(
        cmd_tx: mpsc::Sender<EngineCommand>,
        ledger: Arc<RwLock<Ledger>>,
        market_data: Arc<RwLock<QuoteBoard>>,
    ) -> Self {
        Self {
            cmd_tx,
            ledger,
            market_data,
            event_tx: None,
        }
    }

    /// Create a handle with a broadcast sender for engine events.
    pub fn with_broadcast(
        cmd_tx: mpsc::Sender<EngineCommand>,
        ledger: Arc<RwLock<Ledger>>,
        market_data: Arc<RwLock<QuoteBoard>>,
        event_tx: broadcast::Sender<WsMessage>,
    ) -> Self {
        Self {
            cmd_tx,
            ledger,
            market_data,
            event_tx: Some(event_tx),
        }
    }

    /// Subscribe to engine events (ticks, fills).
    ///
    /// Returns `None` if the engine was started without a broadcast channel.
    pub fn subscribe(&self) -> Option<broadcast::Receiver<WsMessage>> {
        self.event_tx.as_ref().map(broadcast::Sender::subscribe)
    }

    /// Send a heartbeat to the engine.
    #[instrument(skip(self))]
    pub async fn heartbeat(&self) -> Result<()> {
        self.cmd_tx
            .send(EngineCommand::Heartbeat)
            .await
            .context("engine channel closed (heartbeat)")
    }

    /// Request a graceful engine shutdown.
    #[instrument(skip(self))]
    pub async fn shutdown(&self) -> Result<()> {
        self.cmd_tx
            .send(EngineCommand::Shutdown)
            .await
            .context("engine channel closed (shutdown)")
    }

    /// Place an order through the engine without waiting for acknowledgment.
    #[instrument(skip(self, order), fields(symbol = %order.symbol, side = %order.side))]
    pub async fn place_order(&self, order: ingot_core::execution::OrderRequest) -> Result<()> {
        self.cmd_tx
            .send(EngineCommand::PlaceOrder(order, None))
            .await
            .context("engine channel closed (place_order)")
    }

    /// Place an order and wait for the exchange acknowledgment.
    ///
    /// Returns the `OrderAcknowledgment` once the engine has processed
    /// the order. Used by the IPC command thread for synchronous replies.
    #[instrument(skip(self, order), fields(symbol = %order.symbol, side = %order.side))]
    pub async fn place_order_with_ack(
        &self,
        order: ingot_core::execution::OrderRequest,
    ) -> Result<ingot_core::execution::OrderAcknowledgment> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::PlaceOrder(order, Some(tx)))
            .await
            .context("engine channel closed (place_order_with_ack)")?;
        rx.await
            .context("engine dropped ack sender (place_order_with_ack)")
    }

    /// Subscribe to a ticker symbol on the exchange.
    ///
    /// Sends a [`Subscribe`](EngineCommand::Subscribe) command through the
    /// engine and waits for the exchange to confirm the subscription.
    #[instrument(skip(self), fields(symbol = %symbol))]
    pub async fn subscribe_ticker(&self, symbol: Symbol) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::Subscribe(symbol, Some(tx)))
            .await
            .context("engine channel closed (subscribe_ticker)")?;
        rx.await
            .context("engine dropped subscribe ack sender")?
            .context("subscription failed on exchange")
    }

    /// Calculate the current net asset value in the given denomination.
    #[instrument(skip(self), fields(denomination = %denomination.code))]
    pub async fn nav(&self, denomination: &Currency) -> Result<Money> {
        let ledger = self.ledger.read().await;
        let board = self.market_data.read().await;
        ledger
            .net_asset_value(&board, denomination)
            .context("failed to calculate NAV")
    }

    /// Look up the current price for a symbol on the quote board.
    ///
    /// Returns `None` if no rate data is available for the symbol.
    #[instrument(skip(self))]
    pub async fn ticker(&self, symbol: Symbol) -> Result<Option<Price>> {
        let Some((base_code, quote_code)) = symbol.parts() else {
            anyhow::bail!("invalid symbol format: {symbol}");
        };

        let base = Currency::from_code(base_code);
        let quote = Currency::from_code(quote_code);

        let board = self.market_data.read().await;
        let one_base = Money::new(Amount::from(rust_decimal::Decimal::ONE), base);
        match board.convert(&one_base, &quote) {
            Ok(m) => Ok(Some(Price::from(rust_decimal::Decimal::from(m.amount)))),
            Err(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use ingot_core::{
        api::{TickerUpdate, WsMessage},
        execution::OrderRequest,
    };
    use ingot_primitives::{OrderSide, Quantity};
    use rust_decimal::dec;

    use super::*;
    use crate::bootstrap_ledger;

    async fn make_handle() -> Result<(EngineHandle, mpsc::Receiver<EngineCommand>)> {
        let ledger = bootstrap_ledger().await?;
        let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        Ok((EngineHandle::new(cmd_tx, ledger, market_data), cmd_rx))
    }

    #[tokio::test]
    async fn test_handle_heartbeat_sends_command() -> Result<()> {
        let (handle, mut rx) = make_handle().await?;
        handle.heartbeat().await?;

        let cmd = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("channel empty"))?;
        assert!(matches!(cmd, EngineCommand::Heartbeat));
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_shutdown_sends_command() -> Result<()> {
        let (handle, mut rx) = make_handle().await?;
        handle.shutdown().await?;

        let cmd = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("channel empty"))?;
        assert!(matches!(cmd, EngineCommand::Shutdown));
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_place_order_sends_command() -> Result<()> {
        let (handle, mut rx) = make_handle().await?;
        let order = OrderRequest::new_market(
            Symbol::new("BTC-USD"),
            OrderSide::Buy,
            Quantity::from(dec!(0.5)),
        );
        handle.place_order(order).await?;

        let cmd = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("channel empty"))?;
        match cmd {
            EngineCommand::PlaceOrder(o, _ack_tx) => {
                assert_eq!(o.symbol, Symbol::new("BTC-USD"));
                assert_eq!(o.side, OrderSide::Buy);
            }
            other => anyhow::bail!("expected PlaceOrder, got {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_nav_returns_seed_value() -> Result<()> {
        let ledger = bootstrap_ledger().await?;
        let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
        // bootstrap_ledger creates a BTC account — NAV needs a BTC/USD rate
        // even when the BTC balance is zero (convert is called on all accounts).
        {
            use ingot_core::accounting::ExchangeRate;
            let btc = Currency::btc();
            let usd = Currency::usd();
            let rate = ExchangeRate::new(btc, usd, Price::from(dec!(50000)))?;
            market_data.write().await.add_rate(rate);
        }
        let (cmd_tx, _rx) = mpsc::channel(32);
        let handle = EngineHandle::new(cmd_tx, ledger, market_data);
        let usd = Currency::usd();
        let nav = handle.nav(&usd).await?;
        // bootstrap_ledger seeds 10 000 USD, BTC balance is 0
        assert_eq!(nav.amount, Amount::from(dec!(10000)));
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_ticker_no_data() -> Result<()> {
        let (handle, _rx) = make_handle().await?;
        let price = handle.ticker(Symbol::new("BTC-USD")).await?;
        assert!(price.is_none(), "expected None when no rates loaded");
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_ticker_invalid_symbol() {
        let ledger = Arc::new(RwLock::new(Ledger::new()));
        let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
        let (cmd_tx, _rx) = mpsc::channel(32);
        let handle = EngineHandle::new(cmd_tx, ledger, market_data);

        let result = handle.ticker(Symbol::new("INVALID")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_subscribe_ticker_sends_command() -> Result<()> {
        let (handle, mut rx) = make_handle().await?;
        // Fire off the subscribe; we won't have an engine to ack, so just check the
        // channel
        let symbol = Symbol::new("ETH-USD");
        let handle2 = handle.clone();
        let join = tokio::spawn(async move { handle2.subscribe_ticker(symbol).await });

        let cmd = rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("channel empty"))?;
        match cmd {
            EngineCommand::Subscribe(sym, Some(ack_tx)) => {
                assert_eq!(sym, Symbol::new("ETH-USD"));
                // Send back an Ok to unblock the handle
                let _ = ack_tx.send(Ok(()));
            }
            other => anyhow::bail!("expected Subscribe, got {other:?}"),
        }

        join.await.map_err(|e| anyhow::anyhow!("join: {e}"))??;
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_subscribe_returns_none_without_broadcast() -> Result<()> {
        let (handle, _rx) = make_handle().await?;
        assert!(handle.subscribe().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_subscribe_returns_receiver_with_broadcast() -> Result<()> {
        let ledger = bootstrap_ledger().await?;
        let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
        let (cmd_tx, _cmd_rx) = mpsc::channel(32);
        let (event_tx, _) = broadcast::channel(16);
        let handle = EngineHandle::with_broadcast(cmd_tx, ledger, market_data, event_tx);
        assert!(handle.subscribe().is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_handle_multiple_subscribers_receive_events() -> Result<()> {
        let ledger = bootstrap_ledger().await?;
        let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
        let (cmd_tx, _cmd_rx) = mpsc::channel(32);
        let (event_tx, _) = broadcast::channel(16);
        let handle = EngineHandle::with_broadcast(cmd_tx, ledger, market_data, event_tx.clone());

        let mut rx1 = handle
            .subscribe()
            .ok_or_else(|| anyhow::anyhow!("expected receiver"))?;
        let mut rx2 = handle
            .subscribe()
            .ok_or_else(|| anyhow::anyhow!("expected receiver"))?;

        let msg = WsMessage::Tick(TickerUpdate {
            symbol: Symbol::new("BTC-USD"),
            price: Price::from(dec!(60000)),
        });
        event_tx
            .send(msg)
            .map_err(|_| anyhow::anyhow!("broadcast send failed"))?;

        let m1 = rx1.recv().await.map_err(|e| anyhow::anyhow!("rx1: {e}"))?;
        let m2 = rx2.recv().await.map_err(|e| anyhow::anyhow!("rx2: {e}"))?;

        match (&m1, &m2) {
            (WsMessage::Tick(t1), WsMessage::Tick(t2)) => {
                assert_eq!(t1.symbol, Symbol::new("BTC-USD"));
                assert_eq!(t2.symbol, Symbol::new("BTC-USD"));
            }
            _ => anyhow::bail!("expected Tick messages, got {m1:?} / {m2:?}"),
        }
        Ok(())
    }
}
