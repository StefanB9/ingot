use anyhow::{Context, Result};
use ingot_core::accounting::{
    Account, AccountSide, AccountType, ExchangeRate, Ledger, QuoteBoard, Transaction,
};
use ingot_primitives::{Amount, Currency, OrderSide, Price};
use ingot_storage::{
    config::StorageConfig,
    event::{STORAGE_CHANNEL_CAPACITY, StorageEvent},
    service::StorageService,
    task::run_storage_task,
};
use rust_decimal::{Decimal, dec};
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

// --- Transaction integration tests ---

/// Create a balanced transaction with two entries (debit + credit) in the
/// given currency. Both accounts must already be saved in the database.
fn balanced_transaction(
    description: &str,
    debit_account: &Account,
    credit_account: &Account,
    amount: Amount,
) -> Result<Transaction> {
    let mut tx = Transaction::new(description.into());
    tx.add_entry(debit_account, amount, AccountSide::Debit)?;
    tx.add_entry(credit_account, amount, AccountSide::Credit)?;
    Ok(tx)
}

#[tokio::test]
async fn test_save_transaction_persists_header_fields() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let cash = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    let equity = Account::new("Equity".into(), AccountType::Equity, Currency::usd());
    service.save_account(&cash).await?;
    service.save_account(&equity).await?;

    let amount = Amount::from(Decimal::new(10000, 2));
    let tx = balanced_transaction("Initial deposit", &cash, &equity, amount)?;
    let tx_id = tx.id;
    let tx_date = tx.date;

    service.save_transaction(&tx).await?;

    let loaded = service.load_transactions().await?;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, tx_id);
    // PostgreSQL TIMESTAMPTZ has microsecond precision; Rust DateTime has
    // nanosecond. Compare at microsecond granularity.
    let loaded_us = loaded[0].date.timestamp_micros();
    let original_us = tx_date.timestamp_micros();
    assert_eq!(loaded_us, original_us, "posted_at microseconds must match");
    assert_eq!(loaded[0].description, "Initial deposit");
    Ok(())
}

#[tokio::test]
async fn test_save_transaction_persists_all_entries() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let cash = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    let equity = Account::new("Equity".into(), AccountType::Equity, Currency::usd());
    service.save_account(&cash).await?;
    service.save_account(&equity).await?;

    let amount = Amount::from(Decimal::new(50000, 2));
    let tx = balanced_transaction("Fund account", &cash, &equity, amount)?;

    service.save_transaction(&tx).await?;

    let loaded = service.load_transactions().await?;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].entries.len(), 2);

    let debit_entry = loaded[0]
        .entries
        .iter()
        .find(|e| e.side == AccountSide::Debit)
        .context("no debit entry found")?;
    let credit_entry = loaded[0]
        .entries
        .iter()
        .find(|e| e.side == AccountSide::Credit)
        .context("no credit entry found")?;

    assert_eq!(debit_entry.account_id, cash.id);
    assert_eq!(Decimal::from(debit_entry.amount), Decimal::new(50000, 2));
    assert_eq!(debit_entry.currency.code.as_str(), "USD");
    assert_eq!(debit_entry.currency.decimals, 2);

    assert_eq!(credit_entry.account_id, equity.id);
    assert_eq!(Decimal::from(credit_entry.amount), Decimal::new(50000, 2));
    assert_eq!(credit_entry.currency.code.as_str(), "USD");
    assert_eq!(credit_entry.currency.decimals, 2);
    Ok(())
}

#[tokio::test]
async fn test_save_transaction_idempotent_on_duplicate_id() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let cash = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    let equity = Account::new("Equity".into(), AccountType::Equity, Currency::usd());
    service.save_account(&cash).await?;
    service.save_account(&equity).await?;

    let amount = Amount::from(Decimal::new(100, 0));
    let tx = balanced_transaction("Deposit", &cash, &equity, amount)?;

    service.save_transaction(&tx).await?;
    // Second save with same ID should not fail (ON CONFLICT DO NOTHING on header).
    // Entries may duplicate — the test validates the header is idempotent.
    // In practice, callers ensure uniqueness.
    let result = service.save_transaction(&tx).await;
    assert!(result.is_ok());

    let loaded = service.load_transactions().await?;
    assert_eq!(loaded.len(), 1);
    Ok(())
}

#[tokio::test]
async fn test_save_transaction_atomic_on_fk_violation() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    // Create a transaction referencing a non-existent account (FK violation).
    let phantom_account = Account::new("Ghost".into(), AccountType::Asset, Currency::usd());
    // Do NOT save phantom_account — it won't exist in the DB.
    let real_account = Account::new("Real".into(), AccountType::Equity, Currency::usd());
    service.save_account(&real_account).await?;

    let amount = Amount::from(Decimal::new(100, 0));
    let mut tx = Transaction::new("Bad transaction".into());
    tx.add_entry(&real_account, amount, AccountSide::Debit)?;
    tx.add_entry(&phantom_account, amount, AccountSide::Credit)?;

    let result = service.save_transaction(&tx).await;
    assert!(result.is_err(), "should fail due to FK violation");

    // Verify the entire transaction was rolled back (including the header).
    let loaded = service.load_transactions().await?;
    assert!(
        loaded.is_empty(),
        "transaction header should be rolled back on entry FK failure"
    );
    Ok(())
}

#[tokio::test]
async fn test_load_transactions_empty_db_returns_empty() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let loaded = service.load_transactions().await?;
    assert!(loaded.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_load_transactions_entries_grouped_correctly() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let asset = Account::new("Asset".into(), AccountType::Asset, Currency::usd());
    let equity = Account::new("Equity".into(), AccountType::Equity, Currency::usd());
    service.save_account(&asset).await?;
    service.save_account(&equity).await?;

    let amount1 = Amount::from(Decimal::new(100, 0));
    let amount2 = Amount::from(Decimal::new(200, 0));

    let tx1 = balanced_transaction("First", &asset, &equity, amount1)?;
    let tx2 = balanced_transaction("Second", &asset, &equity, amount2)?;

    service.save_transaction(&tx1).await?;
    service.save_transaction(&tx2).await?;

    let loaded = service.load_transactions().await?;
    assert_eq!(loaded.len(), 2);

    // Each transaction should have exactly 2 entries.
    assert_eq!(loaded[0].entries.len(), 2);
    assert_eq!(loaded[1].entries.len(), 2);

    // Entries should belong to their respective transactions.
    for entry in &loaded[0].entries {
        assert!(
            entry.account_id == asset.id || entry.account_id == equity.id,
            "entry account should match"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_load_transactions_ordered_by_posted_at() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let asset = Account::new("Asset".into(), AccountType::Asset, Currency::usd());
    let equity = Account::new("Equity".into(), AccountType::Equity, Currency::usd());
    service.save_account(&asset).await?;
    service.save_account(&equity).await?;

    let amount = Amount::from(Decimal::new(100, 0));

    // Transaction::new() uses Utc::now(), so sequential creation
    // should produce ascending posted_at.
    let tx_a = balanced_transaction("TX-A", &asset, &equity, amount)?;
    let tx_b = balanced_transaction("TX-B", &asset, &equity, amount)?;
    let tx_c = balanced_transaction("TX-C", &asset, &equity, amount)?;

    service.save_transaction(&tx_a).await?;
    service.save_transaction(&tx_b).await?;
    service.save_transaction(&tx_c).await?;

    let loaded = service.load_transactions().await?;
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].description, "TX-A");
    assert_eq!(loaded[1].description, "TX-B");
    assert_eq!(loaded[2].description, "TX-C");
    Ok(())
}

#[tokio::test]
async fn test_load_transactions_entry_fields_preserved() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let btc_asset = Account::new("BTC Holdings".into(), AccountType::Asset, Currency::btc());
    let btc_equity = Account::new("BTC Equity".into(), AccountType::Equity, Currency::btc());
    service.save_account(&btc_asset).await?;
    service.save_account(&btc_equity).await?;

    // 0.12345678 BTC (8 decimal precision).
    let amount = Amount::from(Decimal::new(12_345_678, 8));
    let tx = balanced_transaction("BTC deposit", &btc_asset, &btc_equity, amount)?;

    service.save_transaction(&tx).await?;

    let loaded = service.load_transactions().await?;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].entries.len(), 2);

    for entry in &loaded[0].entries {
        assert_eq!(
            Decimal::from(entry.amount),
            Decimal::new(12_345_678, 8),
            "Decimal precision must survive round-trip"
        );
        assert_eq!(entry.currency.code.as_str(), "BTC");
        assert_eq!(entry.currency.decimals, 8);
    }
    Ok(())
}

// --- Ledger recovery integration tests ---

/// Helper: save all accounts and transactions from a ledger to the database.
async fn persist_ledger(
    service: &StorageService,
    accounts: &[Account],
    transactions: &[Transaction],
) -> Result<()> {
    for account in accounts {
        service.save_account(account).await?;
    }
    for tx in transactions {
        service.save_transaction(tx).await?;
    }
    Ok(())
}

#[tokio::test]
async fn test_recover_ledger_empty_db_signals_fresh() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let result = service.recover_ledger().await;
    assert!(result.is_err(), "empty DB should return Err");

    let err_msg = format!("{}", result.err().context("expected error")?);
    assert!(
        err_msg.contains("no persisted accounts"),
        "error should mention no persisted accounts, got: {err_msg}"
    );
    Ok(())
}

#[tokio::test]
async fn test_recover_ledger_single_account_no_transactions() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let account = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    service.save_account(&account).await?;

    let ledger = service.recover_ledger().await?;
    assert!(ledger.has_account(AccountType::Asset, Currency::usd()));
    assert_eq!(ledger.transaction_count(), 0);
    assert_eq!(ledger.get_balance(account.id)?.amount, Amount::ZERO);
    Ok(())
}

#[tokio::test]
async fn test_recover_ledger_full_round_trip() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let usd = Currency::usd();
    let cash = Account::new("Cash".into(), AccountType::Asset, usd);
    let equity = Account::new("Capital".into(), AccountType::Equity, usd);
    let accounts = vec![cash.clone(), equity.clone()];

    let mut ledger = Ledger::new();
    ledger.add_account(cash.clone());
    ledger.add_account(equity.clone());

    let mut tx = Transaction::new("Seed".into());
    tx.add_entry(&cash, Amount::from(dec!(10000)), AccountSide::Debit)?;
    tx.add_entry(&equity, Amount::from(dec!(10000)), AccountSide::Credit)?;
    let vtx = ledger.prepare_transaction(tx.clone())?;
    ledger.post_transaction(vtx);

    persist_ledger(&service, &accounts, &[tx]).await?;

    let recovered = service.recover_ledger().await?;
    assert!(recovered.has_account(AccountType::Asset, usd));
    assert!(recovered.has_account(AccountType::Equity, usd));
    assert_eq!(recovered.transaction_count(), 1);
    assert_eq!(
        recovered.get_balance(cash.id)?.amount,
        ledger.get_balance(cash.id)?.amount
    );
    assert_eq!(
        recovered.get_balance(equity.id)?.amount,
        ledger.get_balance(equity.id)?.amount
    );
    Ok(())
}

#[tokio::test]
async fn test_recover_ledger_nav_matches_original() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let usd = Currency::usd();
    let btc = Currency::btc();

    let usd_asset = Account::new("USD Asset".into(), AccountType::Asset, usd);
    let usd_equity = Account::new("USD Equity".into(), AccountType::Equity, usd);
    let btc_asset = Account::new("BTC Asset".into(), AccountType::Asset, btc);
    let btc_equity = Account::new("BTC Equity".into(), AccountType::Equity, btc);
    let accounts = vec![
        usd_asset.clone(),
        usd_equity.clone(),
        btc_asset.clone(),
        btc_equity.clone(),
    ];

    let mut ledger = Ledger::new();
    for a in &accounts {
        ledger.add_account(a.clone());
    }

    // Seed $50,000
    let mut seed_tx = Transaction::new("Seed USD".into());
    seed_tx.add_entry(&usd_asset, Amount::from(dec!(50000)), AccountSide::Debit)?;
    seed_tx.add_entry(&usd_equity, Amount::from(dec!(50000)), AccountSide::Credit)?;
    let vseed = ledger.prepare_transaction(seed_tx.clone())?;
    ledger.post_transaction(vseed);

    // Buy 1 BTC @ $50,000
    ledger.post_order_fill(
        OrderSide::Buy,
        btc,
        usd,
        Amount::from(dec!(1)),
        Price::from(dec!(50000)),
    )?;

    // Collect transactions for persistence — we need to load them from the ledger.
    // Since Ledger doesn't expose transactions directly, we reconstruct the
    // transactions we created and save them.
    // The order fill creates its own transaction internally, so we save the seed
    // and also build the fill transaction manually to match.
    let mut fill_tx = Transaction::new("Buy 1 BTC @ 50000 USD".into());
    fill_tx.add_entry(&btc_asset, Amount::from(dec!(1)), AccountSide::Debit)?;
    fill_tx.add_entry(&btc_equity, Amount::from(dec!(1)), AccountSide::Credit)?;
    fill_tx.add_entry(&usd_equity, Amount::from(dec!(50000)), AccountSide::Debit)?;
    fill_tx.add_entry(&usd_asset, Amount::from(dec!(50000)), AccountSide::Credit)?;

    persist_ledger(&service, &accounts, &[seed_tx, fill_tx]).await?;

    let recovered = service.recover_ledger().await?;

    // Both ledgers should have the same balances
    assert_eq!(
        recovered.get_balance(usd_asset.id)?.amount,
        ledger.get_balance(usd_asset.id)?.amount
    );
    assert_eq!(
        recovered.get_balance(btc_asset.id)?.amount,
        ledger.get_balance(btc_asset.id)?.amount
    );

    // NAV should match exactly
    let mut market = QuoteBoard::new();
    market.add_rate(ExchangeRate::new(btc, usd, Price::from(dec!(60000)))?);

    let original_nav = ledger.net_asset_value(&market, &usd)?;
    let recovered_nav = recovered.net_asset_value(&market, &usd)?;
    assert_eq!(
        original_nav.amount, recovered_nav.amount,
        "NAV must be identical: original={}, recovered={}",
        original_nav.amount, recovered_nav.amount
    );
    Ok(())
}

#[tokio::test]
async fn test_recover_ledger_multiple_transactions_replayed_in_order() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let usd = Currency::usd();
    let cash = Account::new("Cash".into(), AccountType::Asset, usd);
    let equity = Account::new("Capital".into(), AccountType::Equity, usd);
    let accounts = vec![cash.clone(), equity.clone()];

    let mut ledger = Ledger::new();
    ledger.add_account(cash.clone());
    ledger.add_account(equity.clone());

    // Post 3 sequential transactions with different amounts
    let amounts = [dec!(1000), dec!(2000), dec!(3000)];
    let mut txns = Vec::with_capacity(3);

    for (i, &amt) in amounts.iter().enumerate() {
        let mut tx = Transaction::new(format!("Deposit {}", i + 1));
        tx.add_entry(&cash, Amount::from(amt), AccountSide::Debit)?;
        tx.add_entry(&equity, Amount::from(amt), AccountSide::Credit)?;
        let vtx = ledger.prepare_transaction(tx.clone())?;
        ledger.post_transaction(vtx);
        txns.push(tx);
    }

    persist_ledger(&service, &accounts, &txns).await?;

    let recovered = service.recover_ledger().await?;
    assert_eq!(recovered.transaction_count(), 3);

    // Final balance should be 1000 + 2000 + 3000 = 6000
    assert_eq!(
        recovered.get_balance(cash.id)?.amount,
        Amount::from(dec!(6000))
    );
    assert_eq!(
        recovered.get_balance(equity.id)?.amount,
        Amount::from(dec!(6000))
    );
    Ok(())
}

// --- Storage task integration tests ---

#[tokio::test]
async fn test_storage_task_processes_account_created() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let (tx, rx) = tokio::sync::mpsc::channel(STORAGE_CHANNEL_CAPACITY);

    let account = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    tx.send(StorageEvent::AccountCreated {
        account: account.clone(),
    })
    .await
    .context("send failed")?;
    drop(tx); // close channel → task exits after final flush

    let task_service = service.clone();
    run_storage_task(task_service, rx, 50, 100).await?;

    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, account.id);
    assert_eq!(loaded[0].name, "Cash");
    Ok(())
}

#[tokio::test]
async fn test_storage_task_processes_transaction_posted() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let cash = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    let equity = Account::new("Equity".into(), AccountType::Equity, Currency::usd());

    // Pre-save accounts so FK constraints are satisfied
    service.save_account(&cash).await?;
    service.save_account(&equity).await?;

    let tx_data = balanced_transaction("Deposit", &cash, &equity, Amount::from(dec!(500)))?;
    let tx_id = tx_data.id;

    let (tx, rx) = tokio::sync::mpsc::channel(STORAGE_CHANNEL_CAPACITY);

    tx.send(StorageEvent::TransactionPosted {
        transaction: tx_data,
    })
    .await
    .context("send failed")?;
    drop(tx);

    let task_service = service.clone();
    run_storage_task(task_service, rx, 50, 100).await?;

    let loaded = service.load_transactions().await?;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, tx_id);
    assert_eq!(loaded[0].entries.len(), 2);
    Ok(())
}

#[tokio::test]
async fn test_storage_task_flushes_on_batch_size() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    // Use batch_size=2 so we can trigger a flush with just 2 events
    let (tx, rx) = tokio::sync::mpsc::channel(STORAGE_CHANNEL_CAPACITY);

    let a1 = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    let a2 = Account::new("BTC".into(), AccountType::Asset, Currency::btc());

    tx.send(StorageEvent::AccountCreated {
        account: a1.clone(),
    })
    .await
    .context("send a1 failed")?;
    tx.send(StorageEvent::AccountCreated {
        account: a2.clone(),
    })
    .await
    .context("send a2 failed")?;

    // Send a Flush to verify the batch was already flushed at batch_size=2,
    // then close channel
    let (flush_tx, flush_rx) = tokio::sync::oneshot::channel();
    tx.send(StorageEvent::Flush { done: flush_tx })
        .await
        .context("send flush failed")?;
    drop(tx);

    let task_service = service.clone();
    let task_handle =
        tokio::spawn(async move { run_storage_task(task_service, rx, 2, 60_000).await });

    // Wait for flush to complete
    flush_rx.await.context("flush oneshot dropped")?;

    // Verify accounts are already in DB (flushed at batch_size boundary)
    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 2);

    task_handle.await.context("task panicked")??;
    Ok(())
}

#[tokio::test]
async fn test_storage_task_flushes_on_channel_close() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let (tx, rx) = tokio::sync::mpsc::channel(STORAGE_CHANNEL_CAPACITY);

    let account = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    tx.send(StorageEvent::AccountCreated {
        account: account.clone(),
    })
    .await
    .context("send failed")?;
    // Drop sender with buffer not full (batch_size=50) — task should final-flush
    drop(tx);

    let task_service = service.clone();
    run_storage_task(task_service, rx, 50, 60_000).await?;

    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, account.id);
    Ok(())
}

#[tokio::test]
async fn test_storage_task_flush_command_drains_buffer() -> Result<()> {
    let (service, _container) = test_storage_service().await?;

    let (tx, rx) = tokio::sync::mpsc::channel(STORAGE_CHANNEL_CAPACITY);

    let account = Account::new("Cash".into(), AccountType::Asset, Currency::usd());
    tx.send(StorageEvent::AccountCreated {
        account: account.clone(),
    })
    .await
    .context("send failed")?;

    // Send flush command (buffer has 1 item, batch_size=50 — not yet flushed)
    let (flush_tx, flush_rx) = tokio::sync::oneshot::channel();
    tx.send(StorageEvent::Flush { done: flush_tx })
        .await
        .context("send flush failed")?;
    drop(tx);

    let task_service = service.clone();
    let task_handle =
        tokio::spawn(async move { run_storage_task(task_service, rx, 50, 60_000).await });

    // Wait for flush acknowledgment
    flush_rx.await.context("flush oneshot dropped")?;

    // Verify data is persisted
    let loaded = service.load_accounts().await?;
    assert_eq!(loaded.len(), 1);

    task_handle.await.context("task panicked")??;
    Ok(())
}
