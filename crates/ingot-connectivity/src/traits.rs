use chrono::{DateTime, Utc};
use ingot_core::{
    Balance, Instrument, OhlcvBar, OpenOrder, OrderBookSnapshot, OrderFill, OrderId, OrderRequest,
    Position, Tick, TickerSnapshot,
};
use ingot_primitives::Symbol;
use tokio::sync::{broadcast, mpsc};

/// Fetch instrument metadata and market snapshots.
pub trait MarketDataProvider {
    fn fetch_instruments(&self) -> impl Future<Output = anyhow::Result<Vec<Instrument>>> + Send;

    fn fetch_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        since: Option<DateTime<Utc>>,
    ) -> impl Future<Output = anyhow::Result<Vec<OhlcvBar>>> + Send;

    fn fetch_trades(
        &self,
        symbol: &Symbol,
        since: Option<DateTime<Utc>>,
    ) -> impl Future<Output = anyhow::Result<(Vec<Tick>, Option<DateTime<Utc>>)>> + Send;

    fn fetch_ticker(
        &self,
        symbol: &Symbol,
    ) -> impl Future<Output = anyhow::Result<TickerSnapshot>> + Send;

    fn fetch_order_book(
        &self,
        symbol: &Symbol,
        depth: u32,
    ) -> impl Future<Output = anyhow::Result<OrderBookSnapshot>> + Send;
}

/// Place, cancel, and query orders.
pub trait OrderExecutor {
    fn place_order(
        &self,
        request: &OrderRequest,
    ) -> impl Future<Output = anyhow::Result<OrderId>> + Send;

    fn cancel_order(&self, order_id: &OrderId) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn cancel_all_orders(&self) -> impl Future<Output = anyhow::Result<u32>> + Send;

    fn get_order_status(
        &self,
        order_id: &OrderId,
    ) -> impl Future<Output = anyhow::Result<OpenOrder>> + Send;

    fn get_open_orders(&self) -> impl Future<Output = anyhow::Result<Vec<OpenOrder>>> + Send;
}

/// Account balances and positions.
pub trait AccountProvider {
    fn get_balances(&self) -> impl Future<Output = anyhow::Result<Vec<Balance>>> + Send;

    fn get_positions(&self) -> impl Future<Output = anyhow::Result<Vec<Position>>> + Send;

    fn get_trade_history(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> impl Future<Output = anyhow::Result<Vec<OrderFill>>> + Send;
}

/// Live WebSocket data streams.
pub trait StreamProvider {
    fn subscribe_trades(
        &self,
        symbols: &[Symbol],
    ) -> impl Future<Output = anyhow::Result<broadcast::Receiver<Tick>>> + Send;

    fn subscribe_ticker(
        &self,
        symbols: &[Symbol],
    ) -> impl Future<Output = anyhow::Result<broadcast::Receiver<TickerSnapshot>>> + Send;

    fn subscribe_order_book(
        &self,
        symbols: &[Symbol],
        depth: u32,
    ) -> impl Future<Output = anyhow::Result<broadcast::Receiver<OrderBookSnapshot>>> + Send;

    fn subscribe_executions(
        &self,
    ) -> impl Future<Output = anyhow::Result<mpsc::Receiver<OrderFill>>> + Send;
}
