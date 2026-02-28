use anyhow::{Context, Result};
use ingot_core::accounting::{Account, AccountType};
use ingot_primitives::Currency;
use ingot_storage::{config::StorageConfig, service::StorageService};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

/// Spin up a `PostgreSQL` container, connect, and run migrations.
async fn test_storage_service() -> Result<(StorageService, testcontainers::ContainerAsync<Postgres>)>
{
    let container = Postgres::default()
        .start()
        .await
        .context("failed to start PostgreSQL testcontainer")?;
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .context("failed to get PostgreSQL port")?;
    let pg_url = format!("postgres://postgres:postgres@localhost:{port}/postgres");
    let config = StorageConfig::new(pg_url, "localhost:9009".into(), 2, 50, 100);
    let service = StorageService::connect(&config)
        .await
        .context("failed to connect StorageService")?;
    Ok((service, container))
}

#[tokio::test]
async fn test_storage_service_connect_runs_migrations() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    // Verify the accounts table exists by loading (should be empty).
    let accounts = service.load_accounts().await?;
    assert!(accounts.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_save_account_persists_all_fields() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let account = Account::new("Cold Storage".into(), AccountType::Asset, Currency::btc());
    service.save_account(&account).await?;

    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, account.id);
    assert_eq!(loaded[0].name, "Cold Storage");
    assert_eq!(loaded[0].account_type, AccountType::Asset);
    assert_eq!(loaded[0].currency.code.as_str(), "BTC");
    assert_eq!(loaded[0].currency.decimals, 8);
    Ok(())
}

#[tokio::test]
async fn test_save_account_idempotent_on_duplicate_id() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let account = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    service.save_account(&account).await?;
    service.save_account(&account).await?; // Should not fail.

    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 1);
    Ok(())
}

#[tokio::test]
async fn test_save_account_different_ids_both_persist() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let a1 = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    let a2 = Account::new("BTC Holdings".into(), AccountType::Asset, Currency::btc());
    service.save_account(&a1).await?;
    service.save_account(&a2).await?;

    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 2);
    Ok(())
}

#[tokio::test]
async fn test_load_accounts_empty_db_returns_empty_vec() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let loaded = service.load_accounts().await?;
    assert!(loaded.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_load_accounts_preserves_field_values() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let variants = [
        ("Cash", AccountType::Asset, Currency::usd()),
        ("Loan", AccountType::Liability, Currency::usd()),
        ("Capital", AccountType::Equity, Currency::btc()),
        (
            "Trading Revenue",
            AccountType::Revenue,
            Currency::new("EUR", 2),
        ),
        ("Fees", AccountType::Expense, Currency::new("ETH", 18)),
    ];

    for (name, at, currency) in &variants {
        let account = Account::new((*name).into(), *at, *currency);
        service.save_account(&account).await?;
    }

    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 5);

    // Verify all 5 account types survived the round-trip.
    let types: Vec<AccountType> = loaded.iter().map(|a| a.account_type).collect();
    assert!(types.contains(&AccountType::Asset));
    assert!(types.contains(&AccountType::Liability));
    assert!(types.contains(&AccountType::Equity));
    assert!(types.contains(&AccountType::Revenue));
    assert!(types.contains(&AccountType::Expense));
    Ok(())
}

#[tokio::test]
async fn test_load_accounts_ordered_by_created_at() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    // Insert in this order: A, B, C. created_at defaults to now() for each.
    let a = Account::new("Account A".into(), AccountType::Asset, Currency::usd());
    let b = Account::new("Account B".into(), AccountType::Liability, Currency::usd());
    let c = Account::new("Account C".into(), AccountType::Equity, Currency::usd());

    service.save_account(&a).await?;
    service.save_account(&b).await?;
    service.save_account(&c).await?;

    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].name, "Account A");
    assert_eq!(loaded[1].name, "Account B");
    assert_eq!(loaded[2].name, "Account C");
    Ok(())
}
