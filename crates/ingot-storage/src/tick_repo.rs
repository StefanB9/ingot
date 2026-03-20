use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ingot_core::Tick;
use ingot_primitives::{OrderSide, Price, Quantity, Symbol};
use sqlx::PgPool;
use tracing::instrument;

pub struct PgTickRepository {
    pool: PgPool,
}

impl PgTickRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[instrument(skip(self, ticks), fields(count = ticks.len()))]
    pub async fn insert_batch(&self, ticks: &[Tick]) -> Result<u64> {
        if ticks.is_empty() {
            return Ok(0);
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin transaction")?;
        let mut count = 0u64;

        for tick in ticks {
            let symbol = tick.symbol.as_str();
            let exchange = tick.exchange.as_str();
            let price = tick.price.value();
            let quantity = tick.quantity.value();
            let side_str = tick.side.as_ref().map(|s| match s {
                OrderSide::Buy => "buy",
                OrderSide::Sell => "sell",
            });
            let trade_id = tick.trade_id.as_deref();

            sqlx::query!(
                "INSERT INTO ticks (time, symbol, exchange, price, quantity, side, trade_id)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                ON CONFLICT (time, symbol, exchange) DO NOTHING",
                tick.time,
                symbol,
                exchange,
                price,
                quantity,
                side_str,
                trade_id,
            )
            .execute(&mut *tx)
            .await
            .context("failed to insert tick")?;

            count += 1;
        }

        tx.commit().await.context("failed to commit tick batch")?;
        Ok(count)
    }

    #[instrument(skip(self))]
    pub async fn get_range(
        &self,
        symbol: &Symbol,
        exchange: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Tick>> {
        let sym = symbol.as_str();
        let rows = sqlx::query!(
            "SELECT time, symbol, exchange, price, quantity, side, trade_id
            FROM ticks
            WHERE symbol = $1 AND exchange = $2 AND time >= $3 AND time < $4
            ORDER BY time ASC",
            sym,
            exchange,
            start,
            end,
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch tick range")?;

        rows.into_iter()
            .map(|r| {
                let side = r
                    .side
                    .as_deref()
                    .map(|s| match s {
                        "buy" => Ok(OrderSide::Buy),
                        "sell" => Ok(OrderSide::Sell),
                        other => anyhow::bail!("unknown order side: {other}"),
                    })
                    .transpose()?;

                Ok(Tick {
                    time: r.time,
                    symbol: Symbol::new(&r.symbol).context("invalid symbol")?,
                    exchange: smol_str::SmolStr::new(&r.exchange),
                    price: Price::new(r.price),
                    quantity: Quantity::new(r.quantity).context("invalid quantity")?,
                    side,
                    trade_id: r.trade_id.map(|s| smol_str::SmolStr::new(&s)),
                })
            })
            .collect()
    }
}
