use anyhow::{Context, Result};
use ingot_accounting::{Discrepancy, ReconciliationResult, ReconciliationStatus};
use ingot_primitives::Exchange;
use sqlx::{PgPool, Row};
use tracing::instrument;
use uuid::Uuid;

pub struct PgReconciliationRepository {
    pool: PgPool,
}

impl PgReconciliationRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[instrument(skip(self, result), fields(exchange = %result.exchange, status = %result.status))]
    pub async fn insert_result(&self, result: &ReconciliationResult) -> Result<()> {
        let exchange = result.exchange.as_str_lowercase().to_owned();
        let status = result.status.to_string();
        let discrepancies = serde_json::to_value(&result.discrepancies)
            .context("failed to serialize discrepancies")?;

        sqlx::query(
            "INSERT INTO reconciliation_results (id, exchange, timestamp, status, discrepancies)
            VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(result.id)
        .bind(&exchange)
        .bind(result.timestamp)
        .bind(&status)
        .bind(&discrepancies)
        .execute(&self.pool)
        .await
        .context("failed to insert reconciliation result")?;

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get_latest(&self, exchange: Exchange) -> Result<Option<ReconciliationResult>> {
        let exchange_str = exchange.as_str_lowercase().to_owned();

        let row = sqlx::query(
            "SELECT id, exchange, timestamp, status, discrepancies
            FROM reconciliation_results
            WHERE exchange = $1
            ORDER BY timestamp DESC
            LIMIT 1",
        )
        .bind(&exchange_str)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch latest reconciliation result")?;

        let Some(r) = row else {
            return Ok(None);
        };

        let id: Uuid = r.get("id");
        let exchange_val: &str = r.get("exchange");
        let timestamp = r.get("timestamp");
        let status_str: &str = r.get("status");
        let discrepancies_json: serde_json::Value = r.get("discrepancies");

        let status = match status_str {
            "pass" => ReconciliationStatus::Pass,
            "fail" => ReconciliationStatus::Fail,
            other => anyhow::bail!("unknown reconciliation status: {other}"),
        };

        let discrepancies: Vec<Discrepancy> = serde_json::from_value(discrepancies_json)
            .context("failed to deserialize discrepancies")?;

        let exchange = match exchange_val {
            "kraken" => Exchange::Kraken,
            "kraken_futures" => Exchange::KrakenFutures,
            "ibkr" => Exchange::IBKR,
            "paper" => Exchange::Paper,
            other => anyhow::bail!("unknown exchange: {other}"),
        };

        Ok(Some(ReconciliationResult {
            id,
            exchange,
            timestamp,
            discrepancies,
            status,
        }))
    }
}
