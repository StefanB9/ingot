use std::collections::HashMap;

use ingot_primitives::{Amount, Currency, CurrencyCode, Money, OrderSide, Price};
use smallvec::SmallVec;
use tracing::instrument;
use uuid::Uuid;

use crate::accounting::{
    Account, AccountSide, AccountType, AccountingError, QuoteBoard, Transaction,
    ValidatedTransaction,
};

#[derive(Debug, Default)]
pub struct Ledger {
    accounts: HashMap<Uuid, Account>,
    transactions: Vec<Transaction>,
    balances: HashMap<Uuid, Amount>,
}

impl Ledger {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            transactions: Vec::new(),
            balances: HashMap::new(),
        }
    }

    pub fn add_account(&mut self, account: Account) {
        self.accounts.insert(account.id, account);
    }

    /// Validates a transaction against this ledger in a single pass and returns
    /// a [`ValidatedTransaction`] — a typed proof that:
    ///
    /// 1. Every entry's account exists in this ledger.
    /// 2. Per-currency debits equal credits (double-entry balance).
    ///
    /// The returned token can be passed to [`Self::post_transaction`], which is
    /// infallible. Separating validation from application allows the borrow
    /// checker to enforce the correct sequencing without holding a mutable
    /// borrow during the read-only validity check.
    #[instrument(skip_all, fields(tx_id = %tx.id, entry_count = tx.entries.len()))]
    pub fn prepare_transaction(
        &self,
        tx: Transaction,
    ) -> Result<ValidatedTransaction, AccountingError> {
        // Stack-allocated accumulator — O(n) in entries, zero heap allocation
        // for the common ≤4-currency case.
        let mut totals: SmallVec<(CurrencyCode, Amount, Amount), 4> = SmallVec::new();

        for entry in &tx.entries {
            if !self.accounts.contains_key(&entry.account_id) {
                return Err(AccountingError::AccountNotFound(entry.account_id));
            }

            let code = entry.currency.code;
            if let Some(row) = totals.iter_mut().find(|(c, _, _)| *c == code) {
                match entry.side {
                    AccountSide::Debit => row.1 += entry.amount,
                    AccountSide::Credit => row.2 += entry.amount,
                }
            } else {
                let (debit, credit) = match entry.side {
                    AccountSide::Debit => (entry.amount, Amount::ZERO),
                    AccountSide::Credit => (Amount::ZERO, entry.amount),
                };
                totals.push((code, debit, credit));
            }
        }

        for (code, debit, credit) in totals {
            if debit != credit {
                return Err(AccountingError::MultiCurrencyTransactionUnbalanced(
                    code.to_string(),
                    debit,
                    credit,
                ));
            }
        }

        Ok(ValidatedTransaction(tx))
    }

    /// Applies a pre-validated transaction to the ledger's balances.
    ///
    /// This method is **infallible**: the [`ValidatedTransaction`] token
    /// guarantees that all accounts exist and all entries are balanced.
    /// Obtain the token via [`Self::prepare_transaction`].
    #[instrument(skip_all, fields(tx_id = %transaction.id(), entry_count = transaction.entry_count()))]
    pub fn post_transaction(&mut self, transaction: ValidatedTransaction) {
        let tx = transaction.0;
        for entry in &tx.entries {
            // SAFETY: ValidatedTransaction guarantees every account_id exists
            // in this ledger. The None arm is structurally unreachable.
            let Some(account) = self.accounts.get(&entry.account_id) else {
                continue;
            };
            let balance = self
                .balances
                .entry(entry.account_id)
                .or_insert(Amount::ZERO);
            if entry.side == account.normal_balance() {
                *balance += entry.amount;
            } else {
                *balance -= entry.amount;
            }
        }
        self.transactions.push(tx);
    }

    #[instrument(skip_all, level = "debug", fields(account_id = %account_id))]
    pub fn get_balance(&self, account_id: Uuid) -> Result<Money, AccountingError> {
        let account = self
            .accounts
            .get(&account_id)
            .ok_or(AccountingError::AccountNotFound(account_id))?;

        let amount = self
            .balances
            .get(&account_id)
            .copied()
            .unwrap_or(Amount::ZERO);

        Ok(Money::new(amount, account.currency))
    }

    #[instrument(skip_all, fields(denomination = %denomination.code))]
    pub fn net_asset_value(
        &self,
        market: &QuoteBoard,
        denomination: &Currency,
    ) -> Result<Money, AccountingError> {
        let mut total_equity = Amount::ZERO;

        for account in self.accounts.values() {
            match account.account_type {
                AccountType::Asset => {
                    let balance = self.get_balance(account.id)?;
                    total_equity += market.convert(&balance, denomination)?.amount;
                }
                AccountType::Liability => {
                    let balance = self.get_balance(account.id)?;
                    total_equity -= market.convert(&balance, denomination)?.amount;
                }
                _ => {}
            }
        }

        Ok(Money::new(total_equity, *denomination))
    }

    /// Number of posted transactions.
    pub fn transaction_count(&self) -> usize {
        self.transactions.len()
    }

    /// Iterate over all accounts in the ledger.
    pub fn accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts.values()
    }

    /// Slice of all posted transactions, in posting order.
    pub fn transactions(&self) -> &[Transaction] {
        &self.transactions
    }

    /// Returns `true` if the ledger contains an account with the given type
    /// and currency.
    pub fn has_account(&self, account_type: AccountType, currency: Currency) -> bool {
        self.accounts
            .values()
            .any(|a| a.account_type == account_type && a.currency == currency)
    }

    fn find_account(
        &self,
        account_type: AccountType,
        currency: Currency,
    ) -> Result<Account, AccountingError> {
        self.accounts
            .values()
            .find(|a| a.account_type == account_type && a.currency == currency)
            .cloned()
            .ok_or(AccountingError::NoCurrencyAccount(currency.code))
    }

    /// Records a completed spot-trade fill using a 4-entry double-entry
    /// transaction.
    ///
    /// Each currency leg is independently balanced: the asset account is offset
    /// by its corresponding equity account so that per-currency debit ==
    /// credit.
    #[instrument(skip_all, fields(
        side = %side,
        base = %base,
        quote = %quote,
        qty = %base_qty,
        price = %fill_price
    ))]
    pub fn post_order_fill(
        &mut self,
        side: OrderSide,
        base: Currency,
        quote: Currency,
        base_qty: Amount,
        fill_price: Price,
    ) -> Result<Transaction, AccountingError> {
        let quote_amount = base_qty * fill_price;

        let base_asset = self.find_account(AccountType::Asset, base)?;
        let base_equity = self.find_account(AccountType::Equity, base)?;
        let quote_asset = self.find_account(AccountType::Asset, quote)?;
        let quote_equity = self.find_account(AccountType::Equity, quote)?;

        let mut tx = Transaction::new(format!("{side} {base_qty} {base} @ {fill_price} {quote}"));

        match side {
            OrderSide::Buy => {
                // Gain base: debit base asset, credit base equity
                tx.add_entry(&base_asset, base_qty, AccountSide::Debit)?;
                tx.add_entry(&base_equity, base_qty, AccountSide::Credit)?;
                // Spend quote: debit quote equity, credit quote asset
                tx.add_entry(&quote_equity, quote_amount, AccountSide::Debit)?;
                tx.add_entry(&quote_asset, quote_amount, AccountSide::Credit)?;
            }
            OrderSide::Sell => {
                // Lose base: debit base equity, credit base asset
                tx.add_entry(&base_equity, base_qty, AccountSide::Debit)?;
                tx.add_entry(&base_asset, base_qty, AccountSide::Credit)?;
                // Receive quote: debit quote asset, credit quote equity
                tx.add_entry(&quote_asset, quote_amount, AccountSide::Debit)?;
                tx.add_entry(&quote_equity, quote_amount, AccountSide::Credit)?;
            }
        }

        let saved = tx.clone();
        let vtx = self.prepare_transaction(tx)?;
        self.post_transaction(vtx);
        Ok(saved)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use ingot_primitives::{Amount, Currency, Price};
    use rust_decimal::dec;

    use super::*;
    use crate::accounting::{AccountSide, ExchangeRate};

    fn setup_ledger() -> (Ledger, Currency, Currency) {
        (Ledger::new(), Currency::usd(), Currency::btc())
    }

    fn make_account(name: &str, type_: AccountType, currency: Currency) -> Account {
        Account::new(name.into(), type_, currency)
    }

    /// Four-account spot-trading ledger: USD asset/equity + BTC asset/equity.
    fn make_spot_ledger() -> (Ledger, Account, Account, Account, Account) {
        let usd = Currency::usd();
        let btc = Currency::btc();
        let usd_asset = Account::new("USD Asset".into(), AccountType::Asset, usd);
        let usd_equity = Account::new("USD Equity".into(), AccountType::Equity, usd);
        let btc_asset = Account::new("BTC Asset".into(), AccountType::Asset, btc);
        let btc_equity = Account::new("BTC Equity".into(), AccountType::Equity, btc);
        let mut ledger = Ledger::new();
        ledger.add_account(usd_asset.clone());
        ledger.add_account(usd_equity.clone());
        ledger.add_account(btc_asset.clone());
        ledger.add_account(btc_equity.clone());
        (ledger, usd_asset, usd_equity, btc_asset, btc_equity)
    }

    #[test]
    fn test_add_account() {
        let (mut ledger, usd, _) = setup_ledger();
        let acc = make_account("Cash", AccountType::Asset, usd);

        ledger.add_account(acc.clone());

        assert!(ledger.get_balance(acc.id).is_ok());
    }

    #[test]
    fn test_get_balance_initial_is_zero() -> Result<()> {
        let (mut ledger, usd, _) = setup_ledger();
        let acc = make_account("Cash", AccountType::Asset, usd);
        ledger.add_account(acc.clone());

        let balance = ledger.get_balance(acc.id)?;
        assert_eq!(balance.amount, Amount::ZERO);
        assert_eq!(balance.currency, usd);

        Ok(())
    }

    #[test]
    fn test_post_transaction_updates_balance() -> Result<()> {
        let (mut ledger, usd, _) = setup_ledger();
        let wallet = make_account("Wallet", AccountType::Asset, usd);
        let equity = make_account("Equity", AccountType::Equity, usd);

        ledger.add_account(wallet.clone());
        ledger.add_account(equity.clone());

        let mut tx = Transaction::new("Deposit".into());
        tx.add_entry(&wallet, Amount::from(dec!(100)), AccountSide::Debit)?;
        tx.add_entry(&equity, Amount::from(dec!(100)), AccountSide::Credit)?;

        let vtx = ledger.prepare_transaction(tx)?;
        ledger.post_transaction(vtx);

        let bal = ledger.get_balance(wallet.id)?;
        assert_eq!(bal.amount, Amount::from(dec!(100)));

        Ok(())
    }

    #[test]
    fn test_post_transaction_fails_if_account_missing() -> Result<()> {
        let (ledger, usd, _) = setup_ledger();
        let wallet = make_account("Wallet", AccountType::Asset, usd);

        let mut tx = Transaction::new("Bad Tx".into());
        tx.add_entry(&wallet, Amount::from(dec!(100)), AccountSide::Debit)?;
        tx.add_entry(&wallet, Amount::from(dec!(100)), AccountSide::Credit)?;

        let err = ledger.prepare_transaction(tx);
        assert!(matches!(err, Err(AccountingError::AccountNotFound(_))));

        Ok(())
    }

    #[test]
    fn test_balance_mechanics_asset_vs_liability() -> Result<()> {
        let (mut ledger, usd, _) = setup_ledger();

        let asset = make_account("Asset", AccountType::Asset, usd);
        let liability = make_account("Loan", AccountType::Liability, usd);

        ledger.add_account(asset.clone());
        ledger.add_account(liability.clone());

        let mut tx1 = Transaction::new("Borrow".into());
        tx1.add_entry(&asset, Amount::from(dec!(100)), AccountSide::Debit)?;
        tx1.add_entry(&liability, Amount::from(dec!(100)), AccountSide::Credit)?;
        let vtx1 = ledger.prepare_transaction(tx1)?;
        ledger.post_transaction(vtx1);

        assert_eq!(
            ledger.get_balance(asset.id)?.amount,
            Amount::from(dec!(100))
        );
        assert_eq!(
            ledger.get_balance(liability.id)?.amount,
            Amount::from(dec!(100))
        );

        let mut tx2 = Transaction::new("Repay".into());
        tx2.add_entry(&asset, Amount::from(dec!(20)), AccountSide::Credit)?;
        tx2.add_entry(&liability, Amount::from(dec!(20)), AccountSide::Debit)?;
        let vtx2 = ledger.prepare_transaction(tx2)?;
        ledger.post_transaction(vtx2);

        assert_eq!(ledger.get_balance(asset.id)?.amount, Amount::from(dec!(80)));
        assert_eq!(
            ledger.get_balance(liability.id)?.amount,
            Amount::from(dec!(80))
        );

        Ok(())
    }

    #[test]
    fn test_net_asset_value_calculation() -> Result<()> {
        let (mut ledger, usd, btc) = setup_ledger();

        let btc_wallet = make_account("BTC Wallet", AccountType::Asset, btc);
        let usd_loan = make_account("USD Loan", AccountType::Liability, usd);
        let equity_btc = make_account("Eq BTC", AccountType::Equity, btc);
        let equity_usd = make_account("Eq USD", AccountType::Equity, usd);

        ledger.add_account(btc_wallet.clone());
        ledger.add_account(usd_loan.clone());
        ledger.add_account(equity_btc.clone());
        ledger.add_account(equity_usd.clone());

        let mut tx1 = Transaction::new("Get BTC".into());
        tx1.add_entry(&btc_wallet, Amount::from(dec!(2.0)), AccountSide::Debit)?;
        tx1.add_entry(&equity_btc, Amount::from(dec!(2.0)), AccountSide::Credit)?;
        let vtx1 = ledger.prepare_transaction(tx1)?;
        ledger.post_transaction(vtx1);

        let mut tx2 = Transaction::new("Get Loan".into());
        tx2.add_entry(&usd_loan, Amount::from(dec!(50000)), AccountSide::Credit)?;
        tx2.add_entry(&equity_usd, Amount::from(dec!(50000)), AccountSide::Debit)?;
        let vtx2 = ledger.prepare_transaction(tx2)?;
        ledger.post_transaction(vtx2);

        let mut market = QuoteBoard::new();
        market.add_rate(ExchangeRate::new(btc, usd, Price::from(dec!(60000)))?);

        let nav = ledger.net_asset_value(&market, &usd)?;

        assert_eq!(nav.amount, Amount::from(dec!(70000)));
        assert_eq!(nav.currency, usd);

        Ok(())
    }

    #[test]
    fn test_net_asset_value_missing_rate() -> Result<()> {
        let (mut ledger, usd, btc) = setup_ledger();
        let btc_wallet = make_account("BTC", AccountType::Asset, btc);
        let equity = make_account("Eq", AccountType::Equity, btc);

        ledger.add_account(btc_wallet.clone());
        ledger.add_account(equity.clone());

        let mut tx = Transaction::new("Add BTC".into());
        tx.add_entry(&btc_wallet, Amount::from(dec!(1)), AccountSide::Debit)?;
        tx.add_entry(&equity, Amount::from(dec!(1)), AccountSide::Credit)?;
        let vtx = ledger.prepare_transaction(tx)?;
        ledger.post_transaction(vtx);

        let market = QuoteBoard::new();

        let result = ledger.net_asset_value(&market, &usd);
        assert!(matches!(
            result,
            Err(AccountingError::CurrencyMismatch(_, _))
        ));

        Ok(())
    }

    #[test]
    fn test_post_order_fill_buy_reduces_usd_increases_btc() -> Result<()> {
        let (mut ledger, usd_asset, usd_equity, btc_asset, _btc_equity) = make_spot_ledger();

        let mut seed = Transaction::new("Seed USD".into());
        seed.add_entry(&usd_asset, Amount::from(dec!(50000)), AccountSide::Debit)?;
        seed.add_entry(&usd_equity, Amount::from(dec!(50000)), AccountSide::Credit)?;
        let vseed = ledger.prepare_transaction(seed)?;
        ledger.post_transaction(vseed);

        ledger.post_order_fill(
            OrderSide::Buy,
            Currency::btc(),
            Currency::usd(),
            Amount::from(dec!(1)),
            Price::from(dec!(50000)),
        )?;

        assert_eq!(ledger.get_balance(usd_asset.id)?.amount, Amount::ZERO);
        assert_eq!(
            ledger.get_balance(btc_asset.id)?.amount,
            Amount::from(dec!(1))
        );
        Ok(())
    }

    #[test]
    fn test_post_order_fill_sell_increases_usd_reduces_btc() -> Result<()> {
        let (mut ledger, usd_asset, _usd_equity, btc_asset, btc_equity) = make_spot_ledger();

        let mut seed = Transaction::new("Seed BTC".into());
        seed.add_entry(&btc_asset, Amount::from(dec!(1)), AccountSide::Debit)?;
        seed.add_entry(&btc_equity, Amount::from(dec!(1)), AccountSide::Credit)?;
        let vseed = ledger.prepare_transaction(seed)?;
        ledger.post_transaction(vseed);

        ledger.post_order_fill(
            OrderSide::Sell,
            Currency::btc(),
            Currency::usd(),
            Amount::from(dec!(1)),
            Price::from(dec!(50000)),
        )?;

        assert_eq!(
            ledger.get_balance(usd_asset.id)?.amount,
            Amount::from(dec!(50000))
        );
        assert_eq!(ledger.get_balance(btc_asset.id)?.amount, Amount::ZERO);
        Ok(())
    }

    #[test]
    fn test_accounts_empty() {
        let ledger = Ledger::new();
        assert_eq!(ledger.accounts().count(), 0);
    }

    #[test]
    fn test_accounts_returns_added() {
        let (mut ledger, usd, _) = setup_ledger();
        let a1 = make_account("Cash", AccountType::Asset, usd);
        let a2 = make_account("Equity", AccountType::Equity, usd);
        ledger.add_account(a1.clone());
        ledger.add_account(a2.clone());

        let ids: Vec<Uuid> = ledger.accounts().map(|a| a.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a1.id));
        assert!(ids.contains(&a2.id));
    }

    #[test]
    fn test_transactions_empty() {
        let ledger = Ledger::new();
        assert!(ledger.transactions().is_empty());
    }

    #[test]
    fn test_transactions_returns_posted() -> Result<()> {
        let (mut ledger, usd, _) = setup_ledger();
        let wallet = make_account("Wallet", AccountType::Asset, usd);
        let equity = make_account("Equity", AccountType::Equity, usd);
        ledger.add_account(wallet.clone());
        ledger.add_account(equity.clone());

        let mut tx = Transaction::new("Deposit".into());
        tx.add_entry(&wallet, Amount::from(dec!(100)), AccountSide::Debit)?;
        tx.add_entry(&equity, Amount::from(dec!(100)), AccountSide::Credit)?;
        let tx_id = tx.id;
        let vtx = ledger.prepare_transaction(tx)?;
        ledger.post_transaction(vtx);

        let txns = ledger.transactions();
        assert_eq!(txns.len(), 1);
        assert_eq!(txns[0].id, tx_id);

        Ok(())
    }

    #[test]
    fn test_post_order_fill_returns_transaction() -> Result<()> {
        let (mut ledger, usd_asset, usd_equity, _btc_asset, _btc_equity) = make_spot_ledger();

        let mut seed = Transaction::new("Seed USD".into());
        seed.add_entry(&usd_asset, Amount::from(dec!(50000)), AccountSide::Debit)?;
        seed.add_entry(&usd_equity, Amount::from(dec!(50000)), AccountSide::Credit)?;
        let vseed = ledger.prepare_transaction(seed)?;
        ledger.post_transaction(vseed);

        let fill_tx = ledger.post_order_fill(
            OrderSide::Buy,
            Currency::btc(),
            Currency::usd(),
            Amount::from(dec!(1)),
            Price::from(dec!(50000)),
        )?;

        // The returned transaction should have 4 entries (spot trade)
        assert_eq!(fill_tx.entries.len(), 4);
        assert!(fill_tx.description.contains("buy"));

        Ok(())
    }

    #[test]
    fn test_transaction_count_empty() {
        let ledger = Ledger::new();
        assert_eq!(ledger.transaction_count(), 0);
    }

    #[test]
    fn test_transaction_count_after_posts() -> Result<()> {
        let (mut ledger, usd, _) = setup_ledger();
        let wallet = make_account("Wallet", AccountType::Asset, usd);
        let equity = make_account("Equity", AccountType::Equity, usd);
        ledger.add_account(wallet.clone());
        ledger.add_account(equity.clone());

        let mut tx1 = Transaction::new("Deposit 1".into());
        tx1.add_entry(&wallet, Amount::from(dec!(100)), AccountSide::Debit)?;
        tx1.add_entry(&equity, Amount::from(dec!(100)), AccountSide::Credit)?;
        let vtx1 = ledger.prepare_transaction(tx1)?;
        ledger.post_transaction(vtx1);
        assert_eq!(ledger.transaction_count(), 1);

        let mut tx2 = Transaction::new("Deposit 2".into());
        tx2.add_entry(&wallet, Amount::from(dec!(50)), AccountSide::Debit)?;
        tx2.add_entry(&equity, Amount::from(dec!(50)), AccountSide::Credit)?;
        let vtx2 = ledger.prepare_transaction(tx2)?;
        ledger.post_transaction(vtx2);
        assert_eq!(ledger.transaction_count(), 2);

        Ok(())
    }

    #[test]
    fn test_has_account_true() {
        let (mut ledger, usd, _) = setup_ledger();
        let acc = make_account("Cash", AccountType::Asset, usd);
        ledger.add_account(acc);
        assert!(ledger.has_account(AccountType::Asset, usd));
    }

    #[test]
    fn test_has_account_false() {
        let (ledger, usd, _) = setup_ledger();
        assert!(!ledger.has_account(AccountType::Asset, usd));
    }

    #[test]
    fn test_has_account_wrong_type() {
        let (mut ledger, usd, _) = setup_ledger();
        let acc = make_account("Cash", AccountType::Asset, usd);
        ledger.add_account(acc);
        assert!(!ledger.has_account(AccountType::Equity, usd));
    }

    #[test]
    fn test_post_order_fill_err_missing_account() {
        let mut ledger = Ledger::new();
        // Only USD accounts — no BTC accounts registered
        let usd_asset = Account::new("USD Asset".into(), AccountType::Asset, Currency::usd());
        let usd_equity = Account::new("USD Equity".into(), AccountType::Equity, Currency::usd());
        ledger.add_account(usd_asset);
        ledger.add_account(usd_equity);

        let result = ledger.post_order_fill(
            OrderSide::Buy,
            Currency::btc(),
            Currency::usd(),
            Amount::from(dec!(1)),
            Price::from(dec!(50000)),
        );

        assert!(matches!(result, Err(AccountingError::NoCurrencyAccount(_))));
    }
}
