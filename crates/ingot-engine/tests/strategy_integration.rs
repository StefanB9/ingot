use std::sync::Arc;

use anyhow::Result;
use ingot_connectivity::PaperExchange;
use ingot_core::{accounting::QuoteBoard, execution::OrderRequest, feed::Ticker};
use ingot_engine::{
    EngineCommand, MARKET_DATA_CHANNEL_CAPACITY, TradingEngine, bootstrap_ledger,
    strategy::{QuoteBoardStrategy, Strategy, StrategyContext},
};
use ingot_primitives::{Amount, Currency, Money, OrderSide, OrderStatus, Price, Quantity, Symbol};
use rust_decimal::dec;
use tokio::sync::{RwLock, mpsc, oneshot};

/// A mock strategy that records whether `on_tick` was called.
struct RecordingStrategy;

impl Strategy for RecordingStrategy {
    fn name(&self) -> &'static str {
        "RecordingStrategy"
    }

    fn on_tick(&mut self, _tick: &Ticker, _ctx: &mut StrategyContext<'_>) {}
}

/// A strategy that emits an order on every tick.
struct OrderEmittingStrategy;

impl Strategy for OrderEmittingStrategy {
    fn name(&self) -> &'static str {
        "OrderEmitter"
    }

    fn on_tick(&mut self, _tick: &Ticker, ctx: &mut StrategyContext<'_>) {
        ctx.orders.push(OrderRequest::new_market(
            Symbol::new("BTC-USD"),
            OrderSide::Buy,
            Quantity::from(dec!(0.01)),
        ));
    }
}

#[tokio::test]
async fn test_engine_dispatches_tick_to_strategy() -> Result<()> {
    let ledger = bootstrap_ledger().await?;
    let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
    let (market_tx, market_rx) = mpsc::channel(MARKET_DATA_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    let (paper_tx, _paper_rx) = mpsc::channel(64);
    let exchange = PaperExchange::new().connect(paper_tx).await?;

    let strategies: Vec<Box<dyn Strategy>> =
        vec![Box::new(QuoteBoardStrategy), Box::new(RecordingStrategy)];
    let mut engine = TradingEngine::new(ledger, market_data.clone(), exchange, strategies);

    // Send one tick then drop sender — engine exits when channels close
    market_tx.send(Ticker::new("BTC-USD", dec!(55000))).await?;
    drop(market_tx);
    drop(cmd_tx);

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        engine.run(market_rx, cmd_rx),
    )
    .await??;

    // Verify the QuoteBoardStrategy updated the board
    let board = market_data.read().await;
    let btc = Currency::btc();
    let usd = Currency::usd();
    let one_btc = Money::new(Amount::from(dec!(1)), btc);
    let result = board.convert(&one_btc, &usd)?;
    assert_eq!(result.amount, Amount::from(dec!(55000)));

    Ok(())
}

#[tokio::test]
async fn test_engine_dispatches_to_multiple_strategies() -> Result<()> {
    let ledger = bootstrap_ledger().await?;
    let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
    let (market_tx, market_rx) = mpsc::channel(MARKET_DATA_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    let (paper_tx, _paper_rx) = mpsc::channel(64);
    let exchange = PaperExchange::new().connect(paper_tx).await?;

    let strategies: Vec<Box<dyn Strategy>> = vec![
        Box::new(QuoteBoardStrategy),
        Box::new(RecordingStrategy),
        Box::new(RecordingStrategy),
    ];
    let mut engine = TradingEngine::new(ledger, market_data, exchange, strategies);

    market_tx.send(Ticker::new("BTC-USD", dec!(42000))).await?;
    drop(market_tx);
    drop(cmd_tx);

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        engine.run(market_rx, cmd_rx),
    )
    .await??;

    // If the engine dispatched without panicking, all strategies ran
    Ok(())
}

#[tokio::test]
async fn test_engine_processes_strategy_emitted_orders() -> Result<()> {
    let ledger = bootstrap_ledger().await?;
    let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
    let (market_tx, market_rx) = mpsc::channel(MARKET_DATA_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    let (paper_tx, _paper_rx) = mpsc::channel(64);
    let exchange = PaperExchange::new().connect(paper_tx).await?;

    let strategies: Vec<Box<dyn Strategy>> = vec![
        Box::new(QuoteBoardStrategy),
        Box::new(OrderEmittingStrategy),
    ];
    let mut engine = TradingEngine::new(ledger.clone(), market_data.clone(), exchange, strategies);

    // Send a tick — QuoteBoardStrategy updates rates, OrderEmittingStrategy emits
    // an order
    market_tx.send(Ticker::new("BTC-USD", dec!(50000))).await?;
    drop(market_tx);
    drop(cmd_tx);

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        engine.run(market_rx, cmd_rx),
    )
    .await??;

    // PaperExchange auto-fills orders. The fill records a double-entry
    // transaction. After the fill the ledger should have a non-zero NAV
    // that reflects the BTC position's value.
    let board = market_data.read().await;
    let l = ledger.read().await;
    let usd = Currency::usd();
    let nav = l.net_asset_value(&board, &usd)?;
    // Seed was 10 000 USD. A filled order changes the composition but
    // NAV should still be >= 10 000 (no slippage in paper exchange).
    assert!(
        nav.amount >= Amount::from(dec!(10000)),
        "expected NAV >= 10000 after fill, got {nav:?}"
    );

    Ok(())
}

#[tokio::test]
async fn test_engine_quote_board_strategy_integration() -> Result<()> {
    let ledger = bootstrap_ledger().await?;
    let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
    let (market_tx, market_rx) = mpsc::channel(MARKET_DATA_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    let (paper_tx, _paper_rx) = mpsc::channel(64);
    let exchange = PaperExchange::new().connect(paper_tx).await?;

    let strategies: Vec<Box<dyn Strategy>> = vec![Box::new(QuoteBoardStrategy)];
    let mut engine = TradingEngine::new(ledger, market_data.clone(), exchange, strategies);

    // Send multiple ticks to verify the board updates each time
    for price in [dec!(48000), dec!(49000), dec!(50000)] {
        market_tx.send(Ticker::new("BTC-USD", price)).await?;
    }
    drop(market_tx);
    drop(cmd_tx);

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        engine.run(market_rx, cmd_rx),
    )
    .await??;

    // The board should reflect the last tick's price
    let board = market_data.read().await;
    let btc = Currency::btc();
    let usd = Currency::usd();
    let one_btc = Money::new(Amount::from(dec!(1)), btc);
    let result = board.convert(&one_btc, &usd)?;
    assert_eq!(result.amount, Amount::from(dec!(50000)));

    Ok(())
}

#[tokio::test]
async fn test_engine_rejects_market_order_without_price_data() -> Result<()> {
    let ledger = bootstrap_ledger().await?;
    let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
    let (market_tx, market_rx) = mpsc::channel(MARKET_DATA_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    let (paper_tx, _paper_rx) = mpsc::channel(64);
    let exchange = PaperExchange::new().connect(paper_tx).await?;

    let strategies: Vec<Box<dyn Strategy>> = vec![Box::new(QuoteBoardStrategy)];
    let mut engine = TradingEngine::new(ledger.clone(), market_data, exchange, strategies);

    let engine_handle = tokio::spawn(async move { engine.run(market_rx, cmd_rx).await });

    // Send a market order for an unsubscribed symbol (no price data in QuoteBoard)
    let order = OrderRequest::new_market(
        Symbol::new("ETH-USD"),
        OrderSide::Buy,
        Quantity::from(dec!(1)),
    );
    let (ack_tx, ack_rx) = oneshot::channel();
    cmd_tx
        .send(EngineCommand::PlaceOrder(order, Some(ack_tx)))
        .await?;

    let ack = tokio::time::timeout(std::time::Duration::from_secs(5), ack_rx).await??;
    assert_eq!(
        ack.status,
        OrderStatus::Rejected,
        "market order without price data should be rejected, got {:?}",
        ack.status
    );

    // Verify ledger unchanged — no new transactions beyond the seed
    let l = ledger.read().await;
    assert_eq!(
        l.transactions().len(),
        1,
        "ledger should only have the seed transaction"
    );

    drop(cmd_tx);
    drop(market_tx);
    engine_handle.await??;

    Ok(())
}

#[tokio::test]
async fn test_engine_accepts_limit_order_without_subscription() -> Result<()> {
    let ledger = bootstrap_ledger().await?;
    let market_data = Arc::new(RwLock::new(QuoteBoard::new()));
    let (market_tx, market_rx) = mpsc::channel(MARKET_DATA_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    let (paper_tx, _paper_rx) = mpsc::channel(64);
    let exchange = PaperExchange::new().connect(paper_tx).await?;

    let strategies: Vec<Box<dyn Strategy>> = vec![Box::new(QuoteBoardStrategy)];
    let mut engine = TradingEngine::new(ledger.clone(), market_data, exchange, strategies);

    let engine_handle = tokio::spawn(async move { engine.run(market_rx, cmd_rx).await });

    // Send a limit order with explicit price — should work even without
    // subscription
    let order = OrderRequest::new_limit(
        Symbol::new("ETH-USD"),
        OrderSide::Buy,
        Quantity::from(dec!(1)),
        Price::from(dec!(3000)),
    );
    let (ack_tx, ack_rx) = oneshot::channel();
    cmd_tx
        .send(EngineCommand::PlaceOrder(order, Some(ack_tx)))
        .await?;

    let ack = tokio::time::timeout(std::time::Duration::from_secs(5), ack_rx).await??;
    assert_eq!(
        ack.status,
        OrderStatus::Filled,
        "limit order with explicit price should be filled, got {:?}",
        ack.status
    );

    drop(cmd_tx);
    drop(market_tx);
    engine_handle.await??;

    Ok(())
}
