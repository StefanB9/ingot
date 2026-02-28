use anyhow::Result;
use ingot_core::accounting::{
    Account, AccountSide, AccountType, ExchangeRate, Ledger, QuoteBoard, Transaction,
};
use ingot_primitives::{Amount, Currency, Price};
use rust_decimal::dec;

/// Simulates a simple "Trading Day" lifecycle.
/// Verifies that the public API is cohesive and the "Ledger of Truth"
/// functions correctly from an external caller's perspective.
#[test]
fn simulation_trading_day_cycle() -> Result<()> {
    let mut ledger = Ledger::new();
    let mut market = QuoteBoard::new();

    let usd = Currency::usd();
    let btc = Currency::btc();
    let eth = Currency::new("ETH", 18);

    let vault_btc = Account::new("Cold Storage BTC".into(), AccountType::Asset, btc);
    let vault_eth = Account::new("Cold Storage ETH".into(), AccountType::Asset, eth);
    let margin_acct = Account::new("Prime Broker Margin".into(), AccountType::Liability, usd);
    let equity_usd = Account::new("Capital USD".into(), AccountType::Equity, usd);
    let equity_btc = Account::new("Capital BTC".into(), AccountType::Equity, btc);

    ledger.add_account(vault_btc.clone());
    ledger.add_account(vault_eth.clone());
    ledger.add_account(margin_acct.clone());
    ledger.add_account(equity_usd.clone());
    ledger.add_account(equity_btc.clone());

    let mut funding_tx = Transaction::new("Initial Funding".into());
    funding_tx.add_entry(
        &margin_acct,
        Amount::from(dec!(1000000.0)),
        AccountSide::Credit,
    )?;
    funding_tx.add_entry(
        &equity_usd,
        Amount::from(dec!(1000000.0)),
        AccountSide::Debit,
    )?;
    let vfunding = ledger.prepare_transaction(funding_tx)?;
    ledger.post_transaction(vfunding);

    let mut deposit_btc = Transaction::new("Deposit BTC".into());
    deposit_btc.add_entry(&vault_btc, Amount::from(dec!(10.0)), AccountSide::Debit)?;
    deposit_btc.add_entry(&equity_btc, Amount::from(dec!(10.0)), AccountSide::Credit)?;
    let vdeposit = ledger.prepare_transaction(deposit_btc)?;
    ledger.post_transaction(vdeposit);

    market.add_rate(ExchangeRate::new(btc, usd, Price::from(dec!(60000.0)))?);
    market.add_rate(ExchangeRate::new(eth, usd, Price::from(dec!(3000.0)))?);

    // Assets: 10 BTC * $60k = $600,000
    // Liabs:  $1,000,000
    // NAV:    $600,000 - $1,000,000 = -$400,000
    let nav = ledger.net_asset_value(&market, &usd)?;

    assert_eq!(nav.amount, Amount::from(dec!(-400000.0)));

    Ok(())
}
