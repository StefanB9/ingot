use std::collections::HashMap;

use ingot_primitives::{Amount, Currency, CurrencyCode, Money, Price};

use crate::accounting::AccountingError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeRate {
    pub base: Currency,
    pub quote: Currency,
    pub rate: Price,
}

impl ExchangeRate {
    pub fn new(base: Currency, quote: Currency, rate: Price) -> Result<Self, AccountingError> {
        if !rate.is_positive() {
            return Err(AccountingError::InvalidAmount(
                "Exchange rate must be positive",
            ));
        }
        Ok(Self { base, quote, rate })
    }

    pub fn convert(&self, amount: Amount) -> Amount {
        amount * self.rate
    }
}

#[derive(Debug, Default)]
pub struct QuoteBoard {
    rates: HashMap<(CurrencyCode, CurrencyCode), ExchangeRate>,
}

impl QuoteBoard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_rate(&mut self, rate: ExchangeRate) {
        self.rates.insert((rate.base.code, rate.quote.code), rate);
    }

    pub fn convert(&self, money: &Money, target: &Currency) -> Result<Money, AccountingError> {
        if money.currency == *target {
            return Ok(*money);
        }

        if let Some(rate) = self.rates.get(&(money.currency.code, target.code)) {
            let converted_amount = rate.convert(money.amount);
            return Ok(Money::new(converted_amount, *target));
        }

        if let Some(rate) = self.rates.get(&(target.code, money.currency.code)) {
            let converted_amount = money.amount * rate.rate.inverse();
            return Ok(Money::new(converted_amount, *target));
        }

        Err(AccountingError::CurrencyMismatch(
            money.currency.code.to_string(),
            target.code.to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rust_decimal::dec;

    use super::*;

    #[test]
    fn test_exchange_rate_new_valid() {
        let usd = Currency::usd();
        let btc = Currency::btc();
        let rate = ExchangeRate::new(btc, usd, Price::from(dec!(50000.0)));
        assert!(rate.is_ok());
    }

    #[test]
    fn test_exchange_rate_rejects_zero() {
        let usd = Currency::usd();
        let btc = Currency::btc();
        let rate = ExchangeRate::new(btc, usd, Price::from(dec!(0.0)));
        assert!(matches!(rate, Err(AccountingError::InvalidAmount(_))));
    }

    #[test]
    fn test_exchange_rate_rejects_negative() {
        let usd = Currency::usd();
        let btc = Currency::btc();
        let rate = ExchangeRate::new(btc, usd, Price::from(dec!(-100.0)));
        assert!(matches!(rate, Err(AccountingError::InvalidAmount(_))));
    }

    #[test]
    fn test_exchange_rate_traits() -> Result<()> {
        let usd = Currency::usd();
        let btc = Currency::btc();
        let rate = ExchangeRate::new(btc, usd, Price::from(dec!(50000.0)))?;

        assert_eq!(rate.base, btc);
        assert_eq!(rate.quote, usd);
        assert_eq!(rate.rate, Price::from(dec!(50000.0)));

        let rate2 = rate.clone();
        assert_eq!(rate, rate2);
        Ok(())
    }

    #[test]
    fn test_exchange_rate_convert_math() -> Result<()> {
        let usd = Currency::usd();
        let btc = Currency::btc();
        let rate = ExchangeRate::new(btc, usd, Price::from(dec!(50000.0)))?;

        let result = rate.convert(Amount::from(dec!(2.0)));
        assert_eq!(result, Amount::from(dec!(100000.0)));
        Ok(())
    }

    #[test]
    fn test_quoteboard_identity_conversion() -> Result<()> {
        let board = QuoteBoard::new();
        let usd = Currency::usd();
        let money = Money::new(Amount::from(dec!(100.0)), usd);

        let result = board.convert(&money, &usd)?;
        assert_eq!(result.amount, Amount::from(dec!(100.0)));
        assert_eq!(result.currency, usd);
        Ok(())
    }

    #[test]
    fn test_quoteboard_direct_conversion() -> Result<()> {
        let mut board = QuoteBoard::new();
        let btc = Currency::btc();
        let usd = Currency::usd();

        board.add_rate(ExchangeRate::new(btc, usd, Price::from(dec!(60000.0)))?);

        let start_money = Money::new(Amount::from(dec!(0.5)), btc);
        let result = board.convert(&start_money, &usd)?;

        assert_eq!(result.amount, Amount::from(dec!(30000.0)));
        assert_eq!(result.currency, usd);
        Ok(())
    }

    #[test]
    fn test_quoteboard_inverse_conversion() -> Result<()> {
        let mut board = QuoteBoard::new();
        let btc = Currency::btc();
        let usd = Currency::usd();

        board.add_rate(ExchangeRate::new(btc, usd, Price::from(dec!(50000.0)))?);

        let start_money = Money::new(Amount::from(dec!(100000.0)), usd);
        let result = board.convert(&start_money, &btc)?;

        assert_eq!(result.amount, Amount::from(dec!(2.0)));
        assert_eq!(result.currency, btc);
        Ok(())
    }

    #[test]
    fn test_quoteboard_missing_pair() {
        let board = QuoteBoard::new();
        let usd = Currency::usd();
        let btc = Currency::btc();

        let money = Money::new(Amount::from(dec!(100.0)), usd);
        let err = board.convert(&money, &btc);
        assert!(matches!(err, Err(AccountingError::CurrencyMismatch(_, _))));
    }
}
