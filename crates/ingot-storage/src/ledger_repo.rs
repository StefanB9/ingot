use anyhow::{Context, Result};
use ingot_accounting::{AccountId, AccountType, CurrencyBalance, Transaction};
use ingot_primitives::{Amount, Currency, Exchange};
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use tracing::instrument;

pub struct PgLedgerRepository {
    pool: PgPool,
}

impl PgLedgerRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[instrument(skip(self, txn), fields(txn_id = %txn.id))]
    pub async fn insert_transaction(&self, txn: &Transaction) -> Result<()> {
        let mut db_tx = self
            .pool
            .begin()
            .await
            .context("failed to begin transaction")?;

        let txn_id = *txn.id.as_uuid();
        let txn_type = txn.transaction_type.to_string();
        let reference_id = txn.reference_id.as_deref().map(String::from);
        let metadata = txn.metadata.clone();

        sqlx::query(
            "INSERT INTO ledger_transactions (id, transaction_type, timestamp, reference_id, \
             metadata)
            VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(txn_id)
        .bind(&txn_type)
        .bind(txn.timestamp)
        .bind(&reference_id)
        .bind(&metadata)
        .execute(&mut *db_tx)
        .await
        .context("failed to insert ledger transaction")?;

        for entry in &txn.entries {
            let entry_id = *entry.id.as_uuid();
            let account_type = entry.account_id.account_type.to_string();
            let exchange = entry.account_id.exchange.as_str_lowercase().to_owned();
            let venue = entry.account_id.venue.to_string();
            let currency = entry.currency.as_str().to_owned();
            let side = entry.side.to_string();
            let amount = entry.amount.value();
            let description = entry.description.as_deref().map(String::from);

            sqlx::query(
                "INSERT INTO ledger_entries (id, transaction_id, account_type, exchange, venue, \
                 currency, side, amount, timestamp, description)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(entry_id)
            .bind(txn_id)
            .bind(&account_type)
            .bind(&exchange)
            .bind(&venue)
            .bind(&currency)
            .bind(&side)
            .bind(amount)
            .bind(entry.timestamp)
            .bind(&description)
            .execute(&mut *db_tx)
            .await
            .context("failed to insert ledger entry")?;
        }

        db_tx
            .commit()
            .await
            .context("failed to commit ledger transaction")?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get_account_balances(&self) -> Result<Vec<CurrencyBalance>> {
        let rows = sqlx::query(
            "SELECT account_type, exchange, venue, currency,
                SUM(CASE WHEN side = 'debit' THEN amount ELSE -amount END) as balance
            FROM ledger_entries
            GROUP BY account_type, exchange, venue, currency
            ORDER BY account_type, exchange, venue, currency",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch account balances")?;

        rows.into_iter()
            .map(|r| {
                let account_type_str: &str = r.get("account_type");
                let exchange_str: &str = r.get("exchange");
                let venue: &str = r.get("venue");
                let currency_str: &str = r.get("currency");
                let balance: Option<Decimal> = r.get("balance");

                let account_type = parse_account_type(account_type_str)?;
                let exchange = parse_exchange(exchange_str)?;
                let currency = Currency::from_str_lossy(currency_str);

                let account_id = AccountId::new(account_type, exchange, venue, currency.clone())
                    .map_err(|e| anyhow::anyhow!("{e}"))
                    .context("invalid account id from database")?;

                Ok(CurrencyBalance {
                    account_id,
                    currency,
                    balance: Amount::new(balance.unwrap_or(Decimal::ZERO)),
                })
            })
            .collect()
    }
}

fn parse_account_type(s: &str) -> Result<AccountType> {
    match s {
        "asset" => Ok(AccountType::Asset),
        "liability" => Ok(AccountType::Liability),
        "revenue" => Ok(AccountType::Revenue),
        "expense" => Ok(AccountType::Expense),
        other => anyhow::bail!("unknown account type: {other}"),
    }
}

fn parse_exchange(s: &str) -> Result<Exchange> {
    match s {
        "kraken" => Ok(Exchange::Kraken),
        "kraken_futures" => Ok(Exchange::KrakenFutures),
        "ibkr" => Ok(Exchange::IBKR),
        "paper" => Ok(Exchange::Paper),
        other => anyhow::bail!("unknown exchange: {other}"),
    }
}
