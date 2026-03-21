use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_primitives::Symbol;
use ingot_storage::{ohlcv_repo::PgOhlcvRepository, tick_repo::PgTickRepository};
use tokio::sync::watch;
use tracing::{info, instrument};

use crate::traits::MarketDataProvider;

/// Paginates through historical OHLCV bars and trade ticks via a
/// [`MarketDataProvider`], persisting them to the database.
pub struct BackfillWorker<M> {
    market_data: M,
    ohlcv_repo: PgOhlcvRepository,
    tick_repo: PgTickRepository,
}

impl<M: MarketDataProvider + Send + Sync> BackfillWorker<M> {
    pub fn new(market_data: M, ohlcv_repo: PgOhlcvRepository, tick_repo: PgTickRepository) -> Self {
        Self {
            market_data,
            ohlcv_repo,
            tick_repo,
        }
    }

    /// Fetch and store OHLCV bars, paginating from `since` to now.
    /// Returns total number of bars stored.
    #[instrument(skip(self, shutdown), fields(%symbol, %interval))]
    pub async fn backfill_ohlcv(
        &self,
        symbol: &Symbol,
        interval: &str,
        since: DateTime<Utc>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> anyhow::Result<u64> {
        let mut cursor = since;
        let mut total_stored = 0u64;

        loop {
            if *shutdown.borrow() {
                info!("OHLCV backfill shutdown requested, stopping early");
                break;
            }

            let bars = self
                .market_data
                .fetch_ohlcv(symbol, interval, Some(cursor))
                .await
                .context("failed to fetch OHLCV page")?;

            if bars.is_empty() {
                break;
            }

            let stored = self
                .ohlcv_repo
                .insert_batch(&bars)
                .await
                .context("failed to store OHLCV batch")?;

            total_stored += stored;

            let last_time = bars
                .last()
                .map(|b| b.time)
                .ok_or_else(|| anyhow::anyhow!("bars was non-empty but last() returned None"))?;

            if last_time <= cursor {
                break;
            }

            cursor = last_time;

            info!("OHLCV backfill {symbol}/{interval}: stored {stored} bars, cursor now {cursor}");
        }

        Ok(total_stored)
    }

    /// Fetch and store trade ticks, paginating via cursor from `since` to now.
    /// Returns total number of ticks stored.
    #[instrument(skip(self, shutdown), fields(%symbol))]
    pub async fn backfill_ticks(
        &self,
        symbol: &Symbol,
        since: DateTime<Utc>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> anyhow::Result<u64> {
        let mut cursor: Option<DateTime<Utc>> = Some(since);
        let mut total_stored = 0u64;

        loop {
            if *shutdown.borrow() {
                info!("Tick backfill shutdown requested, stopping early");
                break;
            }

            let (ticks, next_cursor) = self
                .market_data
                .fetch_trades(symbol, cursor)
                .await
                .context("failed to fetch trades page")?;

            if ticks.is_empty() {
                break;
            }

            let stored = self
                .tick_repo
                .insert_batch(&ticks)
                .await
                .context("failed to store tick batch")?;

            total_stored += stored;

            match next_cursor {
                Some(nc) if nc > cursor.unwrap_or(since) => {
                    cursor = Some(nc);
                }
                _ => break,
            }

            info!("Tick backfill {symbol}: stored {stored} ticks, cursor now {cursor:?}");
        }

        Ok(total_stored)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anyhow::Result;
    use chrono::DateTime;
    use ingot_core::{
        Balance, Instrument, OhlcvBar, OpenOrder, OrderBookSnapshot, OrderFill, OrderId,
        OrderRequest, Position, Tick, TickerSnapshot,
    };
    use ingot_primitives::{OrderSide, Price, Quantity, Symbol};
    use ingot_storage::{ohlcv_repo::PgOhlcvRepository, tick_repo::PgTickRepository};
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;
    use sqlx::PgPool;
    use testcontainers::{ContainerAsync, GenericImage, ImageExt, runners::AsyncRunner};
    use tokio::sync::{Mutex, broadcast, mpsc, watch};

    use super::*;
    use crate::traits::{AccountProvider, MarketDataProvider, OrderExecutor, StreamProvider};

    // ---- Mock MarketDataProvider ----

    type TradePage = (Vec<Tick>, Option<DateTime<Utc>>);

    struct MockMarketData {
        ohlcv_pages: Mutex<Vec<Vec<OhlcvBar>>>,
        trade_pages: Mutex<Vec<TradePage>>,
        ohlcv_call_count: AtomicUsize,
        trade_call_count: AtomicUsize,
    }

    impl MockMarketData {
        fn new(
            ohlcv_pages: Vec<Vec<OhlcvBar>>,
            trade_pages: Vec<(Vec<Tick>, Option<DateTime<Utc>>)>,
        ) -> Self {
            Self {
                ohlcv_pages: Mutex::new(ohlcv_pages),
                trade_pages: Mutex::new(trade_pages),
                ohlcv_call_count: AtomicUsize::new(0),
                trade_call_count: AtomicUsize::new(0),
            }
        }
    }

    impl MarketDataProvider for MockMarketData {
        async fn fetch_instruments(&self) -> Result<Vec<Instrument>> {
            Ok(vec![])
        }

        async fn fetch_ohlcv(
            &self,
            _symbol: &Symbol,
            _interval: &str,
            _since: Option<DateTime<Utc>>,
        ) -> Result<Vec<OhlcvBar>> {
            self.ohlcv_call_count.fetch_add(1, Ordering::SeqCst);
            let mut pages = self.ohlcv_pages.lock().await;
            if pages.is_empty() {
                return Ok(vec![]);
            }
            Ok(pages.remove(0))
        }

        async fn fetch_trades(
            &self,
            _symbol: &Symbol,
            _since: Option<DateTime<Utc>>,
        ) -> Result<(Vec<Tick>, Option<DateTime<Utc>>)> {
            self.trade_call_count.fetch_add(1, Ordering::SeqCst);
            let mut pages = self.trade_pages.lock().await;
            if pages.is_empty() {
                return Ok((vec![], None));
            }
            Ok(pages.remove(0))
        }

        async fn fetch_ticker(&self, _symbol: &Symbol) -> Result<TickerSnapshot> {
            anyhow::bail!("not implemented for mock")
        }

        async fn fetch_order_book(
            &self,
            _symbol: &Symbol,
            _depth: u32,
        ) -> Result<OrderBookSnapshot> {
            anyhow::bail!("not implemented for mock")
        }
    }

    impl OrderExecutor for MockMarketData {
        async fn place_order(&self, _request: &OrderRequest) -> Result<OrderId> {
            anyhow::bail!("not implemented for mock")
        }

        async fn cancel_order(&self, _order_id: &OrderId) -> Result<()> {
            anyhow::bail!("not implemented for mock")
        }

        async fn cancel_all_orders(&self) -> Result<u32> {
            anyhow::bail!("not implemented for mock")
        }

        async fn get_order_status(&self, _order_id: &OrderId) -> Result<OpenOrder> {
            anyhow::bail!("not implemented for mock")
        }

        async fn get_open_orders(&self) -> Result<Vec<OpenOrder>> {
            anyhow::bail!("not implemented for mock")
        }
    }

    impl AccountProvider for MockMarketData {
        async fn get_balances(&self) -> Result<Vec<Balance>> {
            anyhow::bail!("not implemented for mock")
        }

        async fn get_positions(&self) -> Result<Vec<Position>> {
            anyhow::bail!("not implemented for mock")
        }

        async fn get_trade_history(&self, _since: Option<DateTime<Utc>>) -> Result<Vec<OrderFill>> {
            anyhow::bail!("not implemented for mock")
        }
    }

    impl StreamProvider for MockMarketData {
        async fn subscribe_trades(&self, _symbols: &[Symbol]) -> Result<broadcast::Receiver<Tick>> {
            anyhow::bail!("not implemented for mock")
        }

        async fn subscribe_ticker(
            &self,
            _symbols: &[Symbol],
        ) -> Result<broadcast::Receiver<TickerSnapshot>> {
            anyhow::bail!("not implemented for mock")
        }

        async fn subscribe_order_book(
            &self,
            _symbols: &[Symbol],
            _depth: u32,
        ) -> Result<broadcast::Receiver<OrderBookSnapshot>> {
            anyhow::bail!("not implemented for mock")
        }

        async fn subscribe_executions(&self) -> Result<mpsc::Receiver<OrderFill>> {
            anyhow::bail!("not implemented for mock")
        }
    }

    // ---- Test Helpers ----

    async fn start_timescaledb() -> Result<(ContainerAsync<GenericImage>, PgPool)> {
        let image = GenericImage::new("timescale/timescaledb", "latest-pg17")
            .with_exposed_port(5432.into())
            .with_wait_for(testcontainers::core::WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ));

        let container = image
            .with_env_var("POSTGRES_DB", "ingot_test")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .start()
            .await
            .map_err(|e| anyhow::anyhow!("failed to start container: {e}"))?;

        let port = container
            .get_host_port_ipv4(5432)
            .await
            .map_err(|e| anyhow::anyhow!("failed to get port: {e}"))?;

        let database_url = format!("postgres://postgres:postgres@localhost:{port}/ingot_test");

        let pool = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                match ingot_storage::create_pool(&database_url).await {
                    Ok(pool) => return pool,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for database connection"))?;

        ingot_storage::run_migrations(&pool).await?;

        Ok((container, pool))
    }

    fn make_bar(time_str: &str) -> Result<OhlcvBar> {
        Ok(OhlcvBar {
            time: DateTime::parse_from_rfc3339(time_str)?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            interval: SmolStr::new("1m"),
            open: Price::new(dec!(67000.0)),
            high: Price::new(dec!(67150.0)),
            low: Price::new(dec!(66980.0)),
            close: Price::new(dec!(67100.0)),
            volume: Quantity::new(dec!(12.0))?,
            trade_count: Some(100),
        })
    }

    fn make_tick(time_str: &str, trade_id: &str) -> Result<Tick> {
        Ok(Tick {
            time: DateTime::parse_from_rfc3339(time_str)?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            price: Price::new(dec!(67100.50)),
            quantity: Quantity::new(dec!(0.5))?,
            side: Some(OrderSide::Buy),
            trade_id: Some(SmolStr::new(trade_id)),
        })
    }

    fn no_shutdown() -> watch::Receiver<bool> {
        let (_tx, rx) = watch::channel(false);
        rx
    }

    // ---- OHLCV Backfill Tests ----

    #[tokio::test]
    async fn test_backfill_ohlcv_single_page() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        let bars = vec![
            make_bar("2026-03-20T10:00:00Z")?,
            make_bar("2026-03-20T10:01:00Z")?,
        ];

        let mock = MockMarketData::new(vec![bars], vec![]);
        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
        let mut shutdown = no_shutdown();

        let count = worker
            .backfill_ohlcv(&Symbol::new("XXBTZUSD")?, "1m", since, &mut shutdown)
            .await?;

        assert_eq!(count, 2);
        assert_eq!(
            worker.market_data.ohlcv_call_count.load(Ordering::SeqCst),
            2
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_backfill_ohlcv_pagination() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        let page1 = vec![
            make_bar("2026-03-20T10:00:00Z")?,
            make_bar("2026-03-20T10:01:00Z")?,
        ];
        let page2 = vec![
            make_bar("2026-03-20T10:02:00Z")?,
            make_bar("2026-03-20T10:03:00Z")?,
        ];

        let mock = MockMarketData::new(vec![page1, page2], vec![]);
        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
        let mut shutdown = no_shutdown();

        let count = worker
            .backfill_ohlcv(&Symbol::new("XXBTZUSD")?, "1m", since, &mut shutdown)
            .await?;

        assert_eq!(count, 4);
        // 2 pages + 1 empty response to terminate
        assert_eq!(
            worker.market_data.ohlcv_call_count.load(Ordering::SeqCst),
            3
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_backfill_ohlcv_empty_response() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        let mock = MockMarketData::new(vec![], vec![]);
        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
        let mut shutdown = no_shutdown();

        let count = worker
            .backfill_ohlcv(&Symbol::new("XXBTZUSD")?, "1m", since, &mut shutdown)
            .await?;

        assert_eq!(count, 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_backfill_ohlcv_no_progress_terminates() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        // Return bars where last timestamp == since (no progress)
        let bars = vec![make_bar("2026-03-20T09:00:00Z")?];

        let mock = MockMarketData::new(vec![bars], vec![]);
        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
        let mut shutdown = no_shutdown();

        let count = worker
            .backfill_ohlcv(&Symbol::new("XXBTZUSD")?, "1m", since, &mut shutdown)
            .await?;

        // Bars are stored but loop terminates due to no progress
        assert_eq!(count, 1);
        assert_eq!(
            worker.market_data.ohlcv_call_count.load(Ordering::SeqCst),
            1
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_backfill_ohlcv_shutdown_stops_early() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        let page1 = vec![make_bar("2026-03-20T10:00:00Z")?];
        let page2 = vec![make_bar("2026-03-20T10:01:00Z")?];

        let mock = MockMarketData::new(vec![page1, page2], vec![]);

        // Signal shutdown immediately
        let (tx, mut shutdown) = watch::channel(true);
        let _tx = tx; // keep alive

        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();

        let count = worker
            .backfill_ohlcv(&Symbol::new("XXBTZUSD")?, "1m", since, &mut shutdown)
            .await?;

        assert_eq!(count, 0);
        assert_eq!(
            worker.market_data.ohlcv_call_count.load(Ordering::SeqCst),
            0
        );

        Ok(())
    }

    // ---- Tick Backfill Tests ----

    #[tokio::test]
    async fn test_backfill_ticks_single_page() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        let cursor_time = DateTime::parse_from_rfc3339("2026-03-20T10:01:00Z")?.to_utc();
        let ticks = vec![
            make_tick("2026-03-20T10:00:00.100Z", "t-001")?,
            make_tick("2026-03-20T10:00:00.200Z", "t-002")?,
        ];

        let mock = MockMarketData::new(vec![], vec![(ticks, Some(cursor_time))]);
        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
        let mut shutdown = no_shutdown();

        let count = worker
            .backfill_ticks(&Symbol::new("XXBTZUSD")?, since, &mut shutdown)
            .await?;

        assert_eq!(count, 2);

        Ok(())
    }

    #[tokio::test]
    async fn test_backfill_ticks_pagination() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        let cursor1 = DateTime::parse_from_rfc3339("2026-03-20T10:01:00Z")?.to_utc();
        let cursor2 = DateTime::parse_from_rfc3339("2026-03-20T10:02:00Z")?.to_utc();

        let page1 = (
            vec![make_tick("2026-03-20T10:00:00.100Z", "t-001")?],
            Some(cursor1),
        );
        let page2 = (
            vec![make_tick("2026-03-20T10:01:00.100Z", "t-002")?],
            Some(cursor2),
        );

        let mock = MockMarketData::new(vec![], vec![page1, page2]);
        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
        let mut shutdown = no_shutdown();

        let count = worker
            .backfill_ticks(&Symbol::new("XXBTZUSD")?, since, &mut shutdown)
            .await?;

        assert_eq!(count, 2);
        assert_eq!(
            worker.market_data.trade_call_count.load(Ordering::SeqCst),
            3
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_backfill_ticks_no_cursor_terminates() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        let ticks = vec![make_tick("2026-03-20T10:00:00.100Z", "t-001")?];

        // Return None cursor → should stop
        let mock = MockMarketData::new(vec![], vec![(ticks, None)]);
        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
        let mut shutdown = no_shutdown();

        let count = worker
            .backfill_ticks(&Symbol::new("XXBTZUSD")?, since, &mut shutdown)
            .await?;

        assert_eq!(count, 1);
        assert_eq!(
            worker.market_data.trade_call_count.load(Ordering::SeqCst),
            1
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_backfill_ticks_shutdown_stops_early() -> Result<()> {
        let (_container, pool) = start_timescaledb().await?;
        let ohlcv_repo = PgOhlcvRepository::new(pool.clone());
        let tick_repo = PgTickRepository::new(pool);

        let cursor_time = DateTime::parse_from_rfc3339("2026-03-20T10:01:00Z")?.to_utc();
        let ticks = vec![make_tick("2026-03-20T10:00:00.100Z", "t-001")?];

        let mock = MockMarketData::new(vec![], vec![(ticks, Some(cursor_time))]);

        let (tx, mut shutdown) = watch::channel(true);
        let _tx = tx;

        let worker = BackfillWorker::new(mock, ohlcv_repo, tick_repo);

        let since = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();

        let count = worker
            .backfill_ticks(&Symbol::new("XXBTZUSD")?, since, &mut shutdown)
            .await?;

        assert_eq!(count, 0);
        assert_eq!(
            worker.market_data.trade_call_count.load(Ordering::SeqCst),
            0
        );

        Ok(())
    }
}
