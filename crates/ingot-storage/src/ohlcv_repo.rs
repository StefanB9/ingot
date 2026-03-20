use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ingot_core::OhlcvBar;
use ingot_primitives::{Price, Quantity, Symbol};
use sqlx::PgPool;

pub struct PgOhlcvRepository {
    pool: PgPool,
}

impl PgOhlcvRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert_batch(&self, bars: &[OhlcvBar]) -> Result<u64> {
        if bars.is_empty() {
            return Ok(0);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin transaction")?;
        let mut count = 0u64;

        for bar in bars {
            let symbol = bar.symbol.as_str();
            let exchange = bar.exchange.as_str();
            let interval = bar.interval.as_str();
            let open = bar.open.value();
            let high = bar.high.value();
            let low = bar.low.value();
            let close = bar.close.value();
            let volume = bar.volume.value();

            sqlx::query!(
                "INSERT INTO ohlcv (time, symbol, exchange, interval, open, high, low, close, \
                 volume, trade_count)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (time, symbol, exchange, interval) DO NOTHING",
                bar.time,
                symbol,
                exchange,
                interval,
                open,
                high,
                low,
                close,
                volume,
                bar.trade_count,
            )
            .execute(&mut *tx)
            .await
            .context("failed to insert ohlcv bar")?;

            count += 1;
        }

        tx.commit().await.context("failed to commit ohlcv batch")?;
        Ok(count)
    }

    pub async fn get_range(
        &self,
        symbol: &Symbol,
        exchange: &str,
        interval: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<OhlcvBar>> {
        let sym = symbol.as_str();
        let rows = sqlx::query!(
            "SELECT time, symbol, exchange, interval, open, high, low, close, volume, trade_count
            FROM ohlcv
            WHERE symbol = $1 AND exchange = $2 AND interval = $3 AND time >= $4 AND time < $5
            ORDER BY time ASC",
            sym,
            exchange,
            interval,
            start,
            end,
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch ohlcv range")?;

        rows.into_iter()
            .map(|r| {
                Ok(OhlcvBar {
                    time: r.time,
                    symbol: Symbol::new(&r.symbol).context("invalid symbol")?,
                    exchange: smol_str::SmolStr::new(&r.exchange),
                    interval: smol_str::SmolStr::new(&r.interval),
                    open: Price::new(r.open),
                    high: Price::new(r.high),
                    low: Price::new(r.low),
                    close: Price::new(r.close),
                    volume: Quantity::new(r.volume).context("invalid volume")?,
                    trade_count: r.trade_count,
                })
            })
            .collect()
    }
}
