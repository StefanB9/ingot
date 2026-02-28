use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use ingot_core::{
    accounting::{
        Account, AccountSide, AccountType, ExchangeRate, Ledger, QuoteBoard, Transaction,
    },
    feed::Ticker,
};
use ingot_primitives::{Amount, Currency, Money, Price};
use rust_decimal::dec;

fn make_rate(base: Currency, quote: Currency, price: rust_decimal::Decimal) -> ExchangeRate {
    match ExchangeRate::new(base, quote, Price::from(price)) {
        Ok(r) => r,
        Err(_) => unreachable!(),
    }
}

fn bench_ticker_new(c: &mut Criterion) {
    c.bench_function("Ticker::new", |b| {
        b.iter(|| Ticker::new(black_box("BTC-USD"), black_box(dec!(50000))));
    });
}

fn bench_quoteboard_convert(c: &mut Criterion) {
    let usd = Currency::usd();
    let btc = Currency::btc();

    let mut board = QuoteBoard::new();
    board.add_rate(make_rate(btc, usd, dec!(60000)));

    let one_btc = Money::new(Amount::from(dec!(1)), btc);
    let one_usd = Money::new(Amount::from(dec!(1)), usd);

    let mut group = c.benchmark_group("QuoteBoard::convert");
    group.bench_function("direct (BTC→USD)", |b| {
        b.iter(|| board.convert(black_box(&one_btc), black_box(&usd)));
    });
    group.bench_function("inverse (USD→BTC)", |b| {
        b.iter(|| board.convert(black_box(&one_usd), black_box(&btc)));
    });
    group.bench_function("identity (BTC→BTC)", |b| {
        b.iter(|| board.convert(black_box(&one_btc), black_box(&btc)));
    });
    group.finish();
}

fn bench_transaction_validate(c: &mut Criterion) {
    let usd = Currency::usd();
    let asset = Account::new("Asset".into(), AccountType::Asset, usd);
    let equity = Account::new("Equity".into(), AccountType::Equity, usd);

    let mut tx = Transaction::new("bench".into());
    match tx.add_entry(&asset, Amount::from(dec!(100)), AccountSide::Debit) {
        Ok(()) => {}
        Err(_) => unreachable!(),
    }
    match tx.add_entry(&equity, Amount::from(dec!(100)), AccountSide::Credit) {
        Ok(()) => {}
        Err(_) => unreachable!(),
    }

    c.bench_function("Transaction::validate (2 entries, balanced)", |b| {
        b.iter(|| black_box(tx.validate()));
    });
}

fn bench_ledger_net_asset_value(c: &mut Criterion) {
    let usd = Currency::usd();
    let btc = Currency::btc();

    let mut board = QuoteBoard::new();
    board.add_rate(make_rate(btc, usd, dec!(60000)));

    let mut ledger = Ledger::new();
    let equity_btc = Account::new("Eq BTC".into(), AccountType::Equity, btc);
    let equity_usd = Account::new("Eq USD".into(), AccountType::Equity, usd);
    ledger.add_account(equity_btc.clone());
    ledger.add_account(equity_usd.clone());

    let asset_accounts: Vec<Account> = (0..5)
        .map(|i| Account::new(format!("BTC Asset {i}"), AccountType::Asset, btc))
        .collect();
    let liability_accounts: Vec<Account> = (0..2)
        .map(|i| Account::new(format!("USD Loan {i}"), AccountType::Liability, usd))
        .collect();

    for acc in &asset_accounts {
        ledger.add_account(acc.clone());
        let mut tx = Transaction::new("fund".into());
        match tx.add_entry(acc, Amount::from(dec!(1)), AccountSide::Debit) {
            Ok(()) => {}
            Err(_) => unreachable!(),
        }
        match tx.add_entry(&equity_btc, Amount::from(dec!(1)), AccountSide::Credit) {
            Ok(()) => {}
            Err(_) => unreachable!(),
        }
        match ledger.prepare_transaction(tx) {
            Ok(vtx) => ledger.post_transaction(vtx),
            Err(_) => unreachable!(),
        }
    }

    for acc in &liability_accounts {
        ledger.add_account(acc.clone());
        let mut tx = Transaction::new("loan".into());
        match tx.add_entry(acc, Amount::from(dec!(10000)), AccountSide::Credit) {
            Ok(()) => {}
            Err(_) => unreachable!(),
        }
        match tx.add_entry(&equity_usd, Amount::from(dec!(10000)), AccountSide::Debit) {
            Ok(()) => {}
            Err(_) => unreachable!(),
        }
        match ledger.prepare_transaction(tx) {
            Ok(vtx) => ledger.post_transaction(vtx),
            Err(_) => unreachable!(),
        }
    }

    c.bench_function("Ledger::net_asset_value (7 accounts)", |b| {
        b.iter(|| ledger.net_asset_value(black_box(&board), black_box(&usd)));
    });
}

criterion_group!(
    benches,
    bench_ticker_new,
    bench_quoteboard_convert,
    bench_transaction_validate,
    bench_ledger_net_asset_value,
);
criterion_main!(benches);
