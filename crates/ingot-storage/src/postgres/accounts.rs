use anyhow::{Context, Result, bail};
use ingot_core::accounting::{Account, AccountType};
use ingot_primitives::{Currency, CurrencyCode};
use sqlx::PgPool;
use tracing::instrument;
use uuid::Uuid;

/// Row type mapping the `accounts` table.
#[derive(Debug, sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    name: String,
    account_type: String,
    currency_code: String,
    decimals: i16,
}

impl TryFrom<AccountRow> for Account {
    type Error = anyhow::Error;

    fn try_from(row: AccountRow) -> Result<Self> {
        let account_type = parse_account_type(&row.account_type)?;
        let code = CurrencyCode::new(&row.currency_code);
        let decimals = u8::try_from(row.decimals)
            .with_context(|| format!("decimals out of range: {}", row.decimals))?;
        let currency = Currency { code, decimals };

        Ok(Account {
            id: row.id,
            name: row.name,
            account_type,
            currency,
        })
    }
}

/// Persist an account to `PostgreSQL`. Idempotent (ON CONFLICT DO NOTHING).
#[instrument(skip(pool, account), fields(account_id = %account.id))]
pub async fn save_account(pool: &PgPool, account: &Account) -> Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO accounts (id, name, account_type, currency_code, decimals)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (id) DO NOTHING
        "#,
        account.id,
        account.name,
        account_type_str(account.account_type),
        account.currency.code.as_str(),
        i16::from(account.currency.decimals),
    )
    .execute(pool)
    .await
    .with_context(|| format!("failed to save account {}", account.id))?;

    Ok(())
}

/// Load all accounts from `PostgreSQL`, ordered by `created_at`.
#[instrument(skip(pool))]
pub async fn load_accounts(pool: &PgPool) -> Result<Vec<Account>> {
    let rows = sqlx::query_as!(
        AccountRow,
        r#"
        SELECT id, name, account_type, currency_code, decimals
        FROM accounts
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to load accounts")?;

    rows.into_iter()
        .map(Account::try_from)
        .collect::<Result<Vec<_>>>()
        .context("failed to convert account rows")
}

fn account_type_str(at: AccountType) -> &'static str {
    match at {
        AccountType::Asset => "Asset",
        AccountType::Liability => "Liability",
        AccountType::Equity => "Equity",
        AccountType::Revenue => "Revenue",
        AccountType::Expense => "Expense",
    }
}

fn parse_account_type(s: &str) -> Result<AccountType> {
    match s {
        "Asset" => Ok(AccountType::Asset),
        "Liability" => Ok(AccountType::Liability),
        "Equity" => Ok(AccountType::Equity),
        "Revenue" => Ok(AccountType::Revenue),
        "Expense" => Ok(AccountType::Expense),
        other => bail!("unknown account type: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;

    #[test]
    fn test_account_type_str_all_variants() {
        assert_eq!(account_type_str(AccountType::Asset), "Asset");
        assert_eq!(account_type_str(AccountType::Liability), "Liability");
        assert_eq!(account_type_str(AccountType::Equity), "Equity");
        assert_eq!(account_type_str(AccountType::Revenue), "Revenue");
        assert_eq!(account_type_str(AccountType::Expense), "Expense");
    }

    #[test]
    fn test_parse_account_type_all_variants() -> Result<()> {
        assert_eq!(parse_account_type("Asset")?, AccountType::Asset);
        assert_eq!(parse_account_type("Liability")?, AccountType::Liability);
        assert_eq!(parse_account_type("Equity")?, AccountType::Equity);
        assert_eq!(parse_account_type("Revenue")?, AccountType::Revenue);
        assert_eq!(parse_account_type("Expense")?, AccountType::Expense);
        Ok(())
    }

    #[test]
    fn test_parse_account_type_invalid() {
        assert!(parse_account_type("Invalid").is_err());
        assert!(parse_account_type("").is_err());
        assert!(parse_account_type("asset").is_err());
    }

    #[test]
    fn test_account_row_try_from_valid() -> Result<()> {
        let row = AccountRow {
            id: Uuid::new_v4(),
            name: "Test Account".into(),
            account_type: "Asset".into(),
            currency_code: "USD".into(),
            decimals: 2,
        };
        let account = Account::try_from(row)?;
        assert_eq!(account.name, "Test Account");
        assert_eq!(account.account_type, AccountType::Asset);
        assert_eq!(account.currency.code.as_str(), "USD");
        assert_eq!(account.currency.decimals, 2);
        Ok(())
    }

    #[test]
    fn test_account_row_try_from_invalid_type() {
        let row = AccountRow {
            id: Uuid::new_v4(),
            name: "Bad".into(),
            account_type: "NotAnAccountType".into(),
            currency_code: "USD".into(),
            decimals: 2,
        };
        assert!(Account::try_from(row).is_err());
    }

    #[test]
    fn test_account_row_try_from_negative_decimals() {
        let row = AccountRow {
            id: Uuid::new_v4(),
            name: "Negative".into(),
            account_type: "Asset".into(),
            currency_code: "BTC".into(),
            decimals: -1,
        };
        assert!(Account::try_from(row).is_err());
    }

    #[test]
    fn test_account_type_round_trip_all_variants() -> Result<()> {
        let variants = [
            AccountType::Asset,
            AccountType::Liability,
            AccountType::Equity,
            AccountType::Revenue,
            AccountType::Expense,
        ];
        for variant in variants {
            let s = account_type_str(variant);
            let parsed = parse_account_type(s)?;
            assert_eq!(variant, parsed);
        }
        Ok(())
    }
}
