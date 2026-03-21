use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ingot_accounting::{
    AccountId, AccountType, CurrencyBalance, EntryId, EntrySide, LedgerEntry, Transaction,
    TransactionId, TrialBalance,
};
use ingot_primitives::{Amount, Currency, Exchange};
use rust_decimal::Decimal;
use smol_str::SmolStr;
use sqlx::{PgPool, Row};
use tracing::instrument;
use uuid::Uuid;

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

    #[instrument(skip(self), fields(exchange = %exchange))]
    pub async fn get_balances_by_exchange(
        &self,
        exchange: Exchange,
    ) -> Result<Vec<CurrencyBalance>> {
        let exchange_str = exchange.as_str_lowercase();

        let rows = sqlx::query(
            "SELECT account_type, exchange, venue, currency,
                SUM(CASE WHEN side = 'debit' THEN amount ELSE -amount END) as balance
            FROM ledger_entries
            WHERE exchange = $1
            GROUP BY account_type, exchange, venue, currency
            ORDER BY account_type, exchange, venue, currency",
        )
        .bind(exchange_str)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch balances by exchange")?;

        rows.into_iter()
            .map(|r| {
                let account_type_str: &str = r.get("account_type");
                let exchange_str: &str = r.get("exchange");
                let venue: &str = r.get("venue");
                let currency_str: &str = r.get("currency");
                let balance: Option<Decimal> = r.get("balance");

                let account_type = parse_account_type(account_type_str)?;
                let ex = parse_exchange(exchange_str)?;
                let currency = Currency::from_str_lossy(currency_str);

                let account_id = AccountId::new(account_type, ex, venue, currency.clone())
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

    #[instrument(skip(self))]
    pub async fn get_entries_since(&self, since: DateTime<Utc>) -> Result<Vec<LedgerEntry>> {
        let rows = sqlx::query(
            "SELECT id, transaction_id, account_type, exchange, venue, currency,
                    side, amount, timestamp, description
            FROM ledger_entries
            WHERE timestamp >= $1
            ORDER BY timestamp ASC",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch entries since timestamp")?;

        rows.into_iter()
            .map(|r| {
                let id: Uuid = r.get("id");
                let transaction_id: Uuid = r.get("transaction_id");
                let account_type_str: &str = r.get("account_type");
                let exchange_str: &str = r.get("exchange");
                let venue: &str = r.get("venue");
                let currency_str: &str = r.get("currency");
                let side_str: &str = r.get("side");
                let amount: Decimal = r.get("amount");
                let timestamp: DateTime<Utc> = r.get("timestamp");
                let description: Option<String> = r.get("description");

                let account_type = parse_account_type(account_type_str)?;
                let exchange = parse_exchange(exchange_str)?;
                let side = parse_entry_side(side_str)?;
                let currency = Currency::from_str_lossy(currency_str);

                let account_id = AccountId::new(account_type, exchange, venue, currency.clone())
                    .map_err(|e| anyhow::anyhow!("{e}"))
                    .context("invalid account id from database")?;

                Ok(LedgerEntry {
                    id: EntryId::from_uuid(id),
                    transaction_id: TransactionId::from_uuid(transaction_id),
                    account_id,
                    side,
                    amount: Amount::new(amount),
                    currency,
                    timestamp,
                    description: description.map(SmolStr::from),
                })
            })
            .collect()
    }

    #[instrument(skip(self))]
    pub async fn trial_balance(&self) -> Result<TrialBalance> {
        let row = sqlx::query(
            "SELECT
                COALESCE(SUM(CASE WHEN side = 'debit' THEN amount ELSE 0 END), 0) as total_debits,
                COALESCE(SUM(CASE WHEN side = 'credit' THEN amount ELSE 0 END), 0) as total_credits
            FROM ledger_entries",
        )
        .fetch_one(&self.pool)
        .await
        .context("failed to compute trial balance")?;

        let total_debits: Decimal = row.get("total_debits");
        let total_credits: Decimal = row.get("total_credits");

        Ok(TrialBalance {
            total_debits: Amount::new(total_debits),
            total_credits: Amount::new(total_credits),
        })
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

fn parse_entry_side(s: &str) -> Result<EntrySide> {
    match s {
        "debit" => Ok(EntrySide::Debit),
        "credit" => Ok(EntrySide::Credit),
        other => anyhow::bail!("unknown entry side: {other}"),
    }
}
