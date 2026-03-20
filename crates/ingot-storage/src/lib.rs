pub mod instrument_repo;
pub mod ohlcv_repo;
pub mod tick_repo;

use anyhow::{Context, Result};
use sqlx::PgPool;

pub async fn create_pool(database_url: &str) -> Result<PgPool> {
    PgPool::connect(database_url)
        .await
        .context("failed to connect to database")
}

pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .context("failed to run database migrations")
}
