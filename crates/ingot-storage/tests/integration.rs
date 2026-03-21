use anyhow::Result;
use chrono::{DateTime, Utc};
use ingot_accounting::{
    AccountId, AccountType, Discrepancy, DiscrepancySeverity, EntryId, EntrySide, LedgerEntry,
    ReconciliationResult, ReconciliationStatus, Transaction, TransactionId, TransactionType,
};
use ingot_core::{Instrument, InstrumentDetails, OhlcvBar, Tick};
use ingot_primitives::{
    Amount, AssetClass, Currency, Exchange, OrderSide, Price, Quantity, Symbol,
};
use ingot_storage::{
    instrument_repo::PgInstrumentRepository, ledger_repo::PgLedgerRepository,
    ohlcv_repo::PgOhlcvRepository, reconciliation_repo::PgReconciliationRepository,
    tick_repo::PgTickRepository,
};
use rust_decimal_macros::dec;
use smol_str::SmolStr;
use sqlx::PgPool;
use testcontainers::{ContainerAsync, GenericImage, ImageExt, runners::AsyncRunner};
use uuid::Uuid;

async fn start_timescaledb() -> Result<(ContainerAsync<GenericImage>, PgPool)> {
    let image = GenericImage::new("timescale/timescaledb", "latest-pg17")
        .with_exposed_port(5432.into())
        .with_wait_for(testcontainers::core::WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ));

    let container = image
        .with_env_var("POSTGRES_DB", "ingot_test")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("failed to start container: {e}"))?;

    let port = container
        .get_host_port_ipv4(5432)
        .await
        .map_err(|e| anyhow::anyhow!("failed to get port: {e}"))?;

    let database_url = format!("postgres://postgres:postgres@localhost:{port}/ingot_test");

    // Retry connection a few times since the container may need a moment
    let pool = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match ingot_storage::create_pool(&database_url).await {
                Ok(pool) => return pool,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for database connection"))?;

    ingot_storage::run_migrations(&pool).await?;

    Ok((container, pool))
}

fn sample_crypto_spot() -> Result<Instrument> {
    Ok(Instrument {
        symbol: Symbol::new("XXBTZUSD")?,
        asset_class: AssetClass::CryptoSpot,
        exchange: Exchange::Kraken,
        base_currency: Currency::BTC,
        quote_currency: Currency::USD,
        tick_size: Price::new(dec!(0.1)),
        display_name: SmolStr::new("XBT/USD"),
        details: InstrumentDetails::CryptoSpot {
            order_min: Quantity::new(dec!(0.00005))?,
            cost_min: Amount::new(dec!(0.5)),
            lot_decimals: 8,
            margin_eligible: true,
            leverage_tiers: vec![2, 3, 4, 5],
        },
    })
}

fn sample_equity() -> Result<Instrument> {
    Ok(Instrument {
        symbol: Symbol::new("265598")?,
        asset_class: AssetClass::Equity,
        exchange: Exchange::IBKR,
        base_currency: Currency::USD,
        quote_currency: Currency::USD,
        tick_size: Price::new(dec!(0.01)),
        display_name: SmolStr::new("AAPL"),
        details: InstrumentDetails::Equity {
            isin: Some(SmolStr::new("US0378331005")),
            lot_size: Quantity::new(dec!(1))?,
            fractional: true,
        },
    })
}

// --- Instrument Repository Tests ---

#[tokio::test]
async fn test_instrument_upsert_and_get() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgInstrumentRepository::new(pool);

    let instrument = sample_crypto_spot()?;
    repo.upsert(&instrument).await?;

    let fetched = repo.get_by_symbol(&Symbol::new("XXBTZUSD")?).await?;

    assert!(fetched.is_some());
    let fetched = fetched.ok_or_else(|| anyhow::anyhow!("instrument not found"))?;
    assert_eq!(fetched.symbol, instrument.symbol);
    assert_eq!(fetched.asset_class, instrument.asset_class);
    assert_eq!(fetched.exchange, instrument.exchange);
    assert_eq!(fetched.base_currency, instrument.base_currency);
    assert_eq!(fetched.quote_currency, instrument.quote_currency);
    assert_eq!(fetched.tick_size, instrument.tick_size);
    assert_eq!(fetched.display_name, instrument.display_name);
    assert_eq!(fetched.details, instrument.details);

    Ok(())
}

#[tokio::test]
async fn test_instrument_upsert_updates_existing() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgInstrumentRepository::new(pool);

    let mut instrument = sample_crypto_spot()?;
    repo.upsert(&instrument).await?;

    instrument.display_name = SmolStr::new("XBT/USD Updated");
    repo.upsert(&instrument).await?;

    let fetched = repo
        .get_by_symbol(&Symbol::new("XXBTZUSD")?)
        .await?
        .ok_or_else(|| anyhow::anyhow!("instrument not found"))?;

    assert_eq!(fetched.display_name.as_str(), "XBT/USD Updated");

    Ok(())
}

#[tokio::test]
async fn test_instrument_get_nonexistent_returns_none() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgInstrumentRepository::new(pool);

    let result = repo.get_by_symbol(&Symbol::new("DOESNOTEXIST")?).await?;

    assert!(result.is_none());

    Ok(())
}

#[tokio::test]
async fn test_instrument_list_by_exchange() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgInstrumentRepository::new(pool);

    let crypto = sample_crypto_spot()?;
    let equity = sample_equity()?;
    repo.upsert(&crypto).await?;
    repo.upsert(&equity).await?;

    let kraken_instruments = repo.list_by_exchange(Exchange::Kraken).await?;
    assert_eq!(kraken_instruments.len(), 1);
    assert_eq!(kraken_instruments[0].symbol.as_str(), "XXBTZUSD");

    let ibkr_instruments = repo.list_by_exchange(Exchange::IBKR).await?;
    assert_eq!(ibkr_instruments.len(), 1);
    assert_eq!(ibkr_instruments[0].symbol.as_str(), "265598");

    let paper_instruments = repo.list_by_exchange(Exchange::Paper).await?;
    assert!(paper_instruments.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_instrument_list_by_asset_class() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgInstrumentRepository::new(pool);

    let crypto = sample_crypto_spot()?;
    let equity = sample_equity()?;
    repo.upsert(&crypto).await?;
    repo.upsert(&equity).await?;

    let crypto_list = repo.list_by_asset_class(AssetClass::CryptoSpot).await?;
    assert_eq!(crypto_list.len(), 1);

    let equity_list = repo.list_by_asset_class(AssetClass::Equity).await?;
    assert_eq!(equity_list.len(), 1);

    let forex_list = repo.list_by_asset_class(AssetClass::Forex).await?;
    assert!(forex_list.is_empty());

    Ok(())
}

// --- OHLCV Repository Tests ---

#[tokio::test]
async fn test_ohlcv_insert_and_get_range() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgOhlcvRepository::new(pool);

    let bars = vec![
        OhlcvBar {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            interval: SmolStr::new("1m"),
            open: Price::new(dec!(67000.0)),
            high: Price::new(dec!(67150.5)),
            low: Price::new(dec!(66980.0)),
            close: Price::new(dec!(67100.0)),
            volume: Quantity::new(dec!(12.345))?,
            trade_count: Some(847),
        },
        OhlcvBar {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:01:00Z")?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            interval: SmolStr::new("1m"),
            open: Price::new(dec!(67100.0)),
            high: Price::new(dec!(67200.0)),
            low: Price::new(dec!(67050.0)),
            close: Price::new(dec!(67180.0)),
            volume: Quantity::new(dec!(8.5))?,
            trade_count: None,
        },
    ];

    let count = repo.insert_batch(&bars).await?;
    assert_eq!(count, 2);

    let start = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
    let end = DateTime::parse_from_rfc3339("2026-03-20T11:00:00Z")?.to_utc();

    let fetched = repo
        .get_range(&Symbol::new("XXBTZUSD")?, "kraken", "1m", start, end)
        .await?;

    assert_eq!(fetched.len(), 2);
    assert_eq!(fetched[0].open, Price::new(dec!(67000.0)));
    assert_eq!(fetched[0].trade_count, Some(847));
    assert_eq!(fetched[1].open, Price::new(dec!(67100.0)));
    assert!(fetched[1].trade_count.is_none());

    Ok(())
}

#[tokio::test]
async fn test_ohlcv_empty_batch() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgOhlcvRepository::new(pool);

    let count = repo.insert_batch(&[]).await?;
    assert_eq!(count, 0);

    Ok(())
}

#[tokio::test]
async fn test_ohlcv_range_filtering() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgOhlcvRepository::new(pool);

    let bars = vec![
        OhlcvBar {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            interval: SmolStr::new("1m"),
            open: Price::new(dec!(67000.0)),
            high: Price::new(dec!(67150.0)),
            low: Price::new(dec!(66980.0)),
            close: Price::new(dec!(67100.0)),
            volume: Quantity::new(dec!(12.0))?,
            trade_count: None,
        },
        OhlcvBar {
            time: DateTime::parse_from_rfc3339("2026-03-20T11:00:00Z")?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            interval: SmolStr::new("1m"),
            open: Price::new(dec!(67100.0)),
            high: Price::new(dec!(67200.0)),
            low: Price::new(dec!(67050.0)),
            close: Price::new(dec!(67180.0)),
            volume: Quantity::new(dec!(8.0))?,
            trade_count: None,
        },
    ];

    repo.insert_batch(&bars).await?;

    // Query range that only includes the first bar
    let start = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
    let end = DateTime::parse_from_rfc3339("2026-03-20T10:30:00Z")?.to_utc();

    let fetched = repo
        .get_range(&Symbol::new("XXBTZUSD")?, "kraken", "1m", start, end)
        .await?;

    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].open, Price::new(dec!(67000.0)));

    Ok(())
}

#[tokio::test]
async fn test_ohlcv_duplicate_insert_ignored() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgOhlcvRepository::new(pool);

    let bar = OhlcvBar {
        time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
        symbol: Symbol::new("XXBTZUSD")?,
        exchange: SmolStr::new("kraken"),
        interval: SmolStr::new("1m"),
        open: Price::new(dec!(67000.0)),
        high: Price::new(dec!(67150.0)),
        low: Price::new(dec!(66980.0)),
        close: Price::new(dec!(67100.0)),
        volume: Quantity::new(dec!(12.0))?,
        trade_count: None,
    };

    repo.insert_batch(std::slice::from_ref(&bar)).await?;
    // Second insert should not error (ON CONFLICT DO NOTHING)
    repo.insert_batch(std::slice::from_ref(&bar)).await?;

    let start = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
    let end = DateTime::parse_from_rfc3339("2026-03-20T11:00:00Z")?.to_utc();
    let fetched = repo
        .get_range(&Symbol::new("XXBTZUSD")?, "kraken", "1m", start, end)
        .await?;

    assert_eq!(fetched.len(), 1);

    Ok(())
}

// --- Tick Repository Tests ---

#[tokio::test]
async fn test_tick_insert_and_get_range() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgTickRepository::new(pool);

    let ticks = vec![
        Tick {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00.100Z")?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            price: Price::new(dec!(67100.50)),
            quantity: Quantity::new(dec!(0.5))?,
            side: Some(OrderSide::Buy),
            trade_id: Some(SmolStr::new("t-001")),
        },
        Tick {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00.200Z")?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            price: Price::new(dec!(67098.00)),
            quantity: Quantity::new(dec!(1.2))?,
            side: Some(OrderSide::Sell),
            trade_id: Some(SmolStr::new("t-002")),
        },
    ];

    let count = repo.insert_batch(&ticks).await?;
    assert_eq!(count, 2);

    let start = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
    let end = DateTime::parse_from_rfc3339("2026-03-20T11:00:00Z")?.to_utc();

    let fetched = repo
        .get_range(&Symbol::new("XXBTZUSD")?, "kraken", start, end)
        .await?;

    assert_eq!(fetched.len(), 2);
    assert_eq!(fetched[0].price, Price::new(dec!(67100.50)));
    assert_eq!(fetched[0].side, Some(OrderSide::Buy));
    assert_eq!(fetched[0].trade_id.as_deref(), Some("t-001"));
    assert_eq!(fetched[1].side, Some(OrderSide::Sell));

    Ok(())
}

#[tokio::test]
async fn test_tick_no_side_no_trade_id() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgTickRepository::new(pool);

    let tick = Tick {
        time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
        symbol: Symbol::new("AAPL")?,
        exchange: SmolStr::new("ibkr"),
        price: Price::new(dec!(175.50)),
        quantity: Quantity::new(dec!(100))?,
        side: None,
        trade_id: None,
    };

    repo.insert_batch(&[tick]).await?;

    let start = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
    let end = DateTime::parse_from_rfc3339("2026-03-20T11:00:00Z")?.to_utc();

    let fetched = repo
        .get_range(&Symbol::new("AAPL")?, "ibkr", start, end)
        .await?;

    assert_eq!(fetched.len(), 1);
    assert!(fetched[0].side.is_none());
    assert!(fetched[0].trade_id.is_none());

    Ok(())
}

#[tokio::test]
async fn test_tick_empty_batch() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgTickRepository::new(pool);

    let count = repo.insert_batch(&[]).await?;
    assert_eq!(count, 0);

    Ok(())
}

#[tokio::test]
async fn test_tick_range_filtering() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgTickRepository::new(pool);

    let ticks = vec![
        Tick {
            time: DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            price: Price::new(dec!(67000.0)),
            quantity: Quantity::new(dec!(1.0))?,
            side: Some(OrderSide::Buy),
            trade_id: None,
        },
        Tick {
            time: DateTime::parse_from_rfc3339("2026-03-20T12:00:00Z")?.to_utc(),
            symbol: Symbol::new("XXBTZUSD")?,
            exchange: SmolStr::new("kraken"),
            price: Price::new(dec!(67500.0)),
            quantity: Quantity::new(dec!(2.0))?,
            side: Some(OrderSide::Sell),
            trade_id: None,
        },
    ];

    repo.insert_batch(&ticks).await?;

    // Range that excludes the second tick
    let start = DateTime::parse_from_rfc3339("2026-03-20T09:00:00Z")?.to_utc();
    let end = DateTime::parse_from_rfc3339("2026-03-20T11:00:00Z")?.to_utc();

    let fetched = repo
        .get_range(&Symbol::new("XXBTZUSD")?, "kraken", start, end)
        .await?;

    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].price, Price::new(dec!(67000.0)));

    Ok(())
}

// --- Ledger Repository Tests ---

fn make_balanced_fee_transaction() -> Result<Transaction> {
    let txn_id = TransactionId::new();
    let now = Utc::now();

    let debit_account = AccountId::new(
        AccountType::Expense,
        Exchange::Kraken,
        "spot",
        Currency::USD,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let credit_account =
        AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

    let entries = vec![
        LedgerEntry {
            id: EntryId::new(),
            transaction_id: txn_id.clone(),
            account_id: debit_account,
            side: EntrySide::Debit,
            amount: Amount::new(dec!(17.42)),
            currency: Currency::USD,
            timestamp: now,
            description: Some(SmolStr::new("trading fee")),
        },
        LedgerEntry {
            id: EntryId::new(),
            transaction_id: txn_id.clone(),
            account_id: credit_account,
            side: EntrySide::Credit,
            amount: Amount::new(dec!(17.42)),
            currency: Currency::USD,
            timestamp: now,
            description: Some(SmolStr::new("trading fee")),
        },
    ];

    Ok(Transaction {
        id: txn_id,
        transaction_type: TransactionType::Fee,
        entries,
        timestamp: now,
        reference_id: Some(SmolStr::new("order-123")),
        metadata: None,
    })
}

#[tokio::test]
async fn test_ledger_insert_and_get_balances() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgLedgerRepository::new(pool);

    let txn = make_balanced_fee_transaction()?;
    repo.insert_transaction(&txn).await?;

    let balances = repo.get_account_balances().await?;
    assert_eq!(balances.len(), 2);

    // Find expense account (debit side → positive balance)
    let expense = balances
        .iter()
        .find(|b| b.account_id.account_type == AccountType::Expense)
        .ok_or_else(|| anyhow::anyhow!("expense balance not found"))?;
    assert_eq!(expense.balance, Amount::new(dec!(17.42)));

    // Find asset account (credit side → negative balance since debit-based)
    let asset = balances
        .iter()
        .find(|b| b.account_id.account_type == AccountType::Asset)
        .ok_or_else(|| anyhow::anyhow!("asset balance not found"))?;
    assert_eq!(asset.balance, Amount::new(dec!(-17.42)));

    Ok(())
}

// --- Reconciliation Repository Tests ---

#[tokio::test]
async fn test_reconciliation_insert_and_get_latest() -> Result<()> {
    let (_container, pool) = start_timescaledb().await?;
    let repo = PgReconciliationRepository::new(pool);

    let result = ReconciliationResult {
        id: Uuid::now_v7(),
        exchange: Exchange::Kraken,
        timestamp: Utc::now(),
        discrepancies: vec![Discrepancy {
            currency: Currency::USD,
            ledger_balance: Amount::new(dec!(1000)),
            broker_balance: Amount::new(dec!(999.50)),
            difference: Amount::new(dec!(0.50)),
            severity: DiscrepancySeverity::Minor,
        }],
        status: ReconciliationStatus::Pass,
    };

    repo.insert_result(&result).await?;

    let latest = repo
        .get_latest(Exchange::Kraken)
        .await?
        .ok_or_else(|| anyhow::anyhow!("reconciliation result not found"))?;

    assert_eq!(latest.exchange, Exchange::Kraken);
    assert_eq!(latest.status, ReconciliationStatus::Pass);
    assert_eq!(latest.discrepancies.len(), 1);
    assert_eq!(latest.discrepancies[0].currency, Currency::USD);
    assert_eq!(latest.discrepancies[0].difference, Amount::new(dec!(0.50)));

    // Different exchange should return None
    let paper = repo.get_latest(Exchange::Paper).await?;
    assert!(paper.is_none());

    Ok(())
}

// --- Posting Engine → Storage Round-Trip ---

#[tokio::test]
async fn test_post_fill_storage_roundtrip() -> Result<()> {
    use ingot_accounting::post_fill;
    use ingot_core::OrderId;

    let (_container, pool) = start_timescaledb().await?;
    let repo = PgLedgerRepository::new(pool);

    // Buy 1 BTC @ 67,000 USD, fee 17.42 USD
    let fill = ingot_core::OrderFill {
        order_id: OrderId::new("roundtrip-order").map_err(|e| anyhow::anyhow!("{e}"))?,
        symbol: Symbol::new("BTCUSD")?,
        side: OrderSide::Buy,
        fill_price: Price::new(dec!(67000)),
        fill_quantity: Quantity::new(dec!(1))?,
        fee: Amount::new(dec!(17.42)),
        fee_currency: Currency::USD,
        timestamp: Utc::now(),
        trade_id: Some(SmolStr::new("rt-trade-1")),
    };

    let txn = post_fill(
        &fill,
        Exchange::Kraken,
        "spot",
        &Currency::BTC,
        &Currency::USD,
        false,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    repo.insert_transaction(&txn).await?;

    let balances = repo.get_account_balances().await?;

    // BTC asset balance = +1.0 (debit)
    let btc_asset = balances
        .iter()
        .find(|b| b.account_id.account_type == AccountType::Asset && b.currency == Currency::BTC)
        .ok_or_else(|| anyhow::anyhow!("BTC asset balance not found"))?;
    assert_eq!(btc_asset.balance, Amount::new(dec!(1)));

    // USD asset balance = -(67000 + 17.42) = -67017.42 (two credits)
    let usd_asset = balances
        .iter()
        .find(|b| b.account_id.account_type == AccountType::Asset && b.currency == Currency::USD)
        .ok_or_else(|| anyhow::anyhow!("USD asset balance not found"))?;
    assert_eq!(usd_asset.balance, Amount::new(dec!(-67017.42)));

    // USD expense balance = +17.42 (debit)
    let usd_expense = balances
        .iter()
        .find(|b| b.account_id.account_type == AccountType::Expense)
        .ok_or_else(|| anyhow::anyhow!("USD expense balance not found"))?;
    assert_eq!(usd_expense.balance, Amount::new(dec!(17.42)));

    Ok(())
}
