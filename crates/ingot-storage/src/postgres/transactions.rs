use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use ingot_core::accounting::{AccountSide, Transaction};
use ingot_primitives::{Amount, Currency, CurrencyCode};
use rust_decimal::Decimal;
use smallvec::SmallVec;
use sqlx::PgPool;
use tracing::instrument;
use uuid::Uuid;

/// Row type mapping the `transactions` table.
#[derive(Debug, sqlx::FromRow)]
struct TransactionRow {
    id: Uuid,
    posted_at: DateTime<Utc>,
    description: String,
}

/// Row type mapping the `entries` table.
#[derive(Debug, sqlx::FromRow)]
struct EntryRow {
    transaction_id: Uuid,
    account_id: Uuid,
    side: String,
    amount: Decimal,
    currency_code: String,
    decimals: i16,
}

/// Persist a transaction and all its entries in a single database transaction.
/// The DB transaction ensures atomicity — either all entries are saved or none.
/// Idempotent on the transaction header (ON CONFLICT DO NOTHING).
#[instrument(skip(pool, tx), fields(transaction_id = %tx.id, entry_count = tx.entries.len()))]
pub async fn save_transaction(pool: &PgPool, tx: &Transaction) -> Result<()> {
    let mut db_tx = pool
        .begin()
        .await
        .context("failed to begin database transaction")?;

    sqlx::query!(
        r#"
        INSERT INTO transactions (id, posted_at, description)
        VALUES ($1, $2, $3)
        ON CONFLICT (id) DO NOTHING
        "#,
        tx.id,
        tx.date,
        tx.description,
    )
    .execute(&mut *db_tx)
    .await
    .with_context(|| format!("failed to save transaction {}", tx.id))?;

    for entry in &tx.entries {
        let amount: Decimal = entry.amount.into();
        sqlx::query!(
            r#"
            INSERT INTO entries (transaction_id, account_id, side, amount, currency_code, decimals)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
            tx.id,
            entry.account_id,
            side_str(entry.side),
            amount,
            entry.currency.code.as_str(),
            i16::from(entry.currency.decimals),
        )
        .execute(&mut *db_tx)
        .await
        .with_context(|| {
            format!(
                "failed to save entry for transaction {} account {}",
                tx.id, entry.account_id
            )
        })?;
    }

    db_tx
        .commit()
        .await
        .with_context(|| format!("failed to commit transaction {}", tx.id))?;

    Ok(())
}

/// Load all transactions with their entries, ordered by `posted_at` ASC.
/// Uses a two-query approach with single-pass merge for efficiency.
#[instrument(skip(pool))]
pub async fn load_transactions(pool: &PgPool) -> Result<Vec<Transaction>> {
    let tx_rows = sqlx::query_as!(
        TransactionRow,
        r#"
        SELECT id, posted_at, description
        FROM transactions
        ORDER BY posted_at ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to load transactions")?;

    if tx_rows.is_empty() {
        return Ok(Vec::new());
    }

    let entry_rows = sqlx::query_as!(
        EntryRow,
        r#"
        SELECT transaction_id, account_id, side, amount, currency_code, decimals
        FROM entries
        ORDER BY transaction_id, id ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to load entries")?;

    // Group entries by transaction_id via HashMap.
    // Entries are ordered by (transaction_id, id ASC), so within each group
    // the insertion order is preserved.
    let mut entries_by_tx: HashMap<Uuid, SmallVec<ingot_core::accounting::Entry, 4>> =
        HashMap::with_capacity(tx_rows.len());

    for e in entry_rows {
        let side = parse_side(&e.side)?;
        let code = CurrencyCode::new(&e.currency_code);
        let decimals = u8::try_from(e.decimals)
            .with_context(|| format!("entry decimals out of range: {}", e.decimals))?;
        let currency = Currency { code, decimals };

        entries_by_tx
            .entry(e.transaction_id)
            .or_default()
            .push(ingot_core::accounting::Entry {
                account_id: e.account_id,
                amount: Amount::from(e.amount),
                side,
                currency,
            });
    }

    let transactions = tx_rows
        .into_iter()
        .map(|row| Transaction {
            entries: entries_by_tx.remove(&row.id).unwrap_or_default(),
            id: row.id,
            date: row.posted_at,
            description: row.description,
        })
        .collect();

    Ok(transactions)
}

fn side_str(side: AccountSide) -> &'static str {
    match side {
        AccountSide::Debit => "Debit",
        AccountSide::Credit => "Credit",
    }
}

fn parse_side(s: &str) -> Result<AccountSide> {
    match s {
        "Debit" => Ok(AccountSide::Debit),
        "Credit" => Ok(AccountSide::Credit),
        other => bail!("unknown account side: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn test_side_str_all_variants() {
        assert_eq!(side_str(AccountSide::Debit), "Debit");
        assert_eq!(side_str(AccountSide::Credit), "Credit");
    }

    #[test]
    fn test_parse_side_all_variants() -> Result<()> {
        assert_eq!(parse_side("Debit")?, AccountSide::Debit);
        assert_eq!(parse_side("Credit")?, AccountSide::Credit);
        Ok(())
    }

    #[test]
    fn test_parse_side_invalid() {
        assert!(parse_side("Invalid").is_err());
        assert!(parse_side("").is_err());
        assert!(parse_side("debit").is_err());
    }

    #[test]
    fn test_side_round_trip() -> Result<()> {
        for variant in [AccountSide::Debit, AccountSide::Credit] {
            let s = side_str(variant);
            let parsed = parse_side(s)?;
            assert_eq!(variant, parsed);
        }
        Ok(())
    }

    #[test]
    fn test_entry_row_try_from_valid_debit() -> Result<()> {
        let row = EntryRow {
            transaction_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            side: "Debit".into(),
            amount: Decimal::new(10050, 2),
            currency_code: "USD".into(),
            decimals: 2,
        };
        let side = parse_side(&row.side)?;
        let amount = Amount::from(row.amount);
        assert_eq!(side, AccountSide::Debit);
        assert_eq!(Decimal::from(amount), Decimal::new(10050, 2));
        Ok(())
    }

    #[test]
    fn test_entry_row_try_from_valid_credit() -> Result<()> {
        let row = EntryRow {
            transaction_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            side: "Credit".into(),
            amount: Decimal::new(50_000_000, 8),
            currency_code: "BTC".into(),
            decimals: 8,
        };
        let side = parse_side(&row.side)?;
        let amount = Amount::from(row.amount);
        assert_eq!(side, AccountSide::Credit);
        assert_eq!(Decimal::from(amount), Decimal::new(50_000_000, 8));
        Ok(())
    }

    #[test]
    fn test_entry_row_negative_decimals_fails() {
        let decimals: i16 = -1;
        assert!(u8::try_from(decimals).is_err());
    }

    mod proptests {
        use proptest::prelude::*;

        use super::*;

        /// Strategy for generating valid `Decimal` values that survive
        /// `Amount` round-trip (mantissa + scale).
        fn decimal_strategy() -> impl Strategy<Value = Decimal> {
            // mantissa: any i64, scale: 0..=28 (Decimal max scale)
            (any::<i64>(), 0u32..=28).prop_map(|(m, s)| Decimal::new(m, s))
        }

        proptest! {
            #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1000))]

            #[test]
            fn proptest_decimal_amount_round_trip(d in decimal_strategy()) {
                let amount = Amount::from(d);
                let back = Decimal::from(amount);
                prop_assert_eq!(d, back, "Decimal -> Amount -> Decimal must be lossless");
            }
        }
    }
}
