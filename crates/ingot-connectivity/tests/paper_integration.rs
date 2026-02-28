use std::time::Duration;

use anyhow::{Context, Result};
use ingot_connectivity::{Exchange, PaperExchange};
use ingot_core::{execution::OrderRequest, feed::Ticker};
use ingot_primitives::{OrderSide, OrderStatus, Quantity, Symbol};
use rust_decimal::dec;
use tokio::sync::mpsc;

#[tokio::test]
async fn test_paper_exchange_lifecycle() -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<Ticker>(10);

    let mut exchange = PaperExchange::new()
        .connect(tx)
        .await
        .context("Failed to connect to exchange")?;

    exchange
        .subscribe_ticker(Symbol::new("BTC-USD"))
        .await
        .context("Failed to subscribe")?;

    let tick_option = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .context("Timed out waiting for ticker data")?;

    let t = tick_option.ok_or_else(|| anyhow::anyhow!("Stream closed unexpectedly"))?;

    assert_eq!(t.symbol, Symbol::new("BTC-USD"));
    assert!(t.price > dec!(0.0));

    let order = OrderRequest::new_market(
        Symbol::new("BTC-USD"),
        OrderSide::Buy,
        Quantity::from(dec!(1.0)),
    );

    let ack = exchange
        .place_order(&order)
        .await
        .context("Failed to place order")?;

    assert_eq!(ack.exchange_id, "PAPER-ID-001");
    assert_eq!(ack.status, OrderStatus::Filled);

    Ok(())
}
