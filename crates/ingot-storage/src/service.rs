use anyhow::{Context, Result};
use ingot_core::accounting::{Account, Transaction};
use sqlx::postgres::{PgPool, PgPoolOptions};
use tracing::instrument;

use crate::{
    config::StorageConfig,
    postgres::{accounts, transactions},
};

/// Unified storage facade. Holds connection pools for `PostgreSQL` and
/// `QuestDB`.
#[derive(Clone)]
pub struct StorageService {
    pg: PgPool,
}

impl StorageService {
    /// Connect to `PostgreSQL` and run migrations.
    /// `QuestDB` connection deferred to Step 2.7.
    #[instrument(skip(config), fields(pg_pool_size = config.pg_pool_size))]
    pub async fn connect(config: &StorageConfig) -> Result<Self> {
        let pg = PgPoolOptions::new()
            .max_connections(config.pg_pool_size)
            .connect(&config.pg_url)
            .await
            .context("failed to connect to PostgreSQL")?;

        sqlx::migrate!("./migrations")
            .run(&pg)
            .await
            .context("failed to run PostgreSQL migrations")?;

        Ok(Self { pg })
    }

    /// Insert an account. Idempotent (ON CONFLICT DO NOTHING).
    pub async fn save_account(&self, account: &Account) -> Result<()> {
        accounts::save_account(&self.pg, account).await
    }

    /// Load all accounts, ordered by `created_at`.
    pub async fn load_accounts(&self) -> Result<Vec<Account>> {
        accounts::load_accounts(&self.pg).await
    }

    /// Persist a transaction and its entries atomically.
    pub async fn save_transaction(&self, tx: &Transaction) -> Result<()> {
        transactions::save_transaction(&self.pg, tx).await
    }

    /// Load all transactions with entries, ordered by `posted_at`.
    pub async fn load_transactions(&self) -> Result<Vec<Transaction>> {
        transactions::load_transactions(&self.pg).await
    }

    /// Expose the underlying pool for direct query access.
    pub fn pool(&self) -> &PgPool {
        &self.pg
    }
}
