use anyhow::{Context, Result, bail};
use ingot_core::accounting::Ledger;
use sqlx::PgPool;
use tracing::{info, instrument};

use super::{accounts, transactions};

/// Rebuild a complete [`Ledger`] from persisted state.
///
/// 1. Load all accounts from the database.
/// 2. If no accounts found, return an error (caller should bootstrap).
/// 3. Add each account to the ledger.
/// 4. Load all transactions (ordered by `posted_at`).
/// 5. Replay each transaction through `prepare_transaction` +
///    `post_transaction`.
/// 6. Return the fully reconstructed ledger.
#[instrument(skip(pool))]
pub async fn recover_ledger(pool: &PgPool) -> Result<Ledger> {
    let accounts = accounts::load_accounts(pool)
        .await
        .context("failed to load accounts during recovery")?;

    if accounts.is_empty() {
        bail!("no persisted accounts — caller should bootstrap a fresh ledger");
    }

    let mut ledger = Ledger::new();

    for account in &accounts {
        ledger.add_account(account.clone());
    }

    let transactions = transactions::load_transactions(pool)
        .await
        .context("failed to load transactions during recovery")?;

    let tx_count = transactions.len();

    for tx in transactions {
        let vtx = ledger
            .prepare_transaction(tx)
            .context("failed to validate recovered transaction")?;
        ledger.post_transaction(vtx);
    }

    info!(
        accounts = accounts.len(),
        transactions = tx_count,
        "Ledger recovered from database"
    );

    Ok(ledger)
}
