use std::{collections::HashMap, future::Future};

use chrono::{DateTime, Utc};
use ingot_primitives::{Amount, Currency, Price};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{balance::CurrencyBalance, error::AccountingError};

/// Point-in-time NAV calculation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavSnapshot {
    pub timestamp: DateTime<Utc>,
    pub base_currency: Currency,
    pub total_nav: Amount,
    pub breakdown: Vec<NavBreakdownEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavBreakdownEntry {
    pub currency: Currency,
    pub native_balance: Amount,
    pub fx_rate: Price,
    pub base_currency_value: Amount,
}

/// Provides FX rates for currency conversion.
pub trait FxRateProvider {
    fn get_rate(
        &self,
        base: &Currency,
        quote: &Currency,
    ) -> impl Future<Output = Result<Price, AccountingError>> + Send;
}

/// Static FX rate provider for testing and offline use.
pub struct StaticFxRateProvider {
    rates: HashMap<(Currency, Currency), Price>,
}

impl StaticFxRateProvider {
    pub fn new(rates: Vec<(Currency, Currency, Price)>) -> Self {
        let map = rates.into_iter().map(|(b, q, p)| ((b, q), p)).collect();
        Self { rates: map }
    }
}

impl FxRateProvider for StaticFxRateProvider {
    async fn get_rate(&self, base: &Currency, quote: &Currency) -> Result<Price, AccountingError> {
        // Identity: same currency → 1
        if base == quote {
            return Ok(Price::new(Decimal::ONE));
        }

        // Direct lookup
        if let Some(&rate) = self.rates.get(&(base.clone(), quote.clone())) {
            return Ok(rate);
        }

        // Inverse lookup
        if let Some(&rate) = self.rates.get(&(quote.clone(), base.clone())) {
            return Ok(Price::new(Decimal::ONE / rate.value()));
        }

        Err(AccountingError::FxRateUnavailable {
            base: base.clone(),
            quote: quote.clone(),
        })
    }
}

/// Calculates portfolio NAV by converting multi-currency balances to a base
/// currency.
pub struct NavCalculator<F> {
    fx_provider: F,
}

impl<F: FxRateProvider> NavCalculator<F> {
    pub fn new(fx_provider: F) -> Self {
        Self { fx_provider }
    }

    pub async fn calculate_nav(
        &self,
        base_currency: &Currency,
        balances: &[CurrencyBalance],
    ) -> Result<NavSnapshot, AccountingError> {
        // Group balances by currency
        let mut by_currency: HashMap<Currency, Decimal> = HashMap::new();
        for bal in balances {
            *by_currency
                .entry(bal.currency.clone())
                .or_insert(Decimal::ZERO) += bal.balance.value();
        }

        let mut breakdown = Vec::new();
        let mut total_nav = Decimal::ZERO;

        // Sort currencies for deterministic output
        let mut currencies: Vec<Currency> = by_currency.keys().cloned().collect();
        currencies.sort_by(|a, b| a.as_str().cmp(b.as_str()));

        for currency in currencies {
            let native_balance_dec = by_currency.get(&currency).copied().unwrap_or(Decimal::ZERO);
            let fx_rate = self.fx_provider.get_rate(&currency, base_currency).await?;
            let base_value = native_balance_dec * fx_rate.value();

            breakdown.push(NavBreakdownEntry {
                currency,
                native_balance: Amount::new(native_balance_dec),
                fx_rate,
                base_currency_value: Amount::new(base_value),
            });

            total_nav += base_value;
        }

        Ok(NavSnapshot {
            timestamp: Utc::now(),
            base_currency: base_currency.clone(),
            total_nav: Amount::new(total_nav),
            breakdown,
        })
    }
}

#[cfg(test)]
mod tests {
    use ingot_primitives::Exchange;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::types::{AccountId, AccountType};

    fn usd_balance(venue: &str, amount: Decimal) -> Result<CurrencyBalance, AccountingError> {
        Ok(CurrencyBalance {
            account_id: AccountId::new(AccountType::Asset, Exchange::Kraken, venue, Currency::USD)?,
            currency: Currency::USD,
            balance: Amount::new(amount),
        })
    }

    fn balance_for(
        currency: Currency,
        amount: Decimal,
    ) -> Result<CurrencyBalance, AccountingError> {
        Ok(CurrencyBalance {
            account_id: AccountId::new(
                AccountType::Asset,
                Exchange::Kraken,
                "spot",
                currency.clone(),
            )?,
            currency,
            balance: Amount::new(amount),
        })
    }

    // ── Serde tests (existing) ────────────────────────────────────────

    #[test]
    fn test_nav_snapshot_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = NavSnapshot {
            timestamp: Utc::now(),
            base_currency: Currency::USD,
            total_nav: Amount::new(dec!(100000)),
            breakdown: vec![NavBreakdownEntry {
                currency: Currency::BTC,
                native_balance: Amount::new(dec!(1)),
                fx_rate: Price::new(dec!(67000)),
                base_currency_value: Amount::new(dec!(67000)),
            }],
        };
        let json = serde_json::to_string(&snapshot)?;
        let deserialized: NavSnapshot = serde_json::from_str(&json)?;
        assert_eq!(snapshot, deserialized);
        Ok(())
    }

    #[test]
    fn test_nav_breakdown_entry_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let entry = NavBreakdownEntry {
            currency: Currency::ETH,
            native_balance: Amount::new(dec!(10)),
            fx_rate: Price::new(dec!(3500)),
            base_currency_value: Amount::new(dec!(35000)),
        };
        let json = serde_json::to_string(&entry)?;
        let deserialized: NavBreakdownEntry = serde_json::from_str(&json)?;
        assert_eq!(entry, deserialized);
        Ok(())
    }

    // ── StaticFxRateProvider ──────────────────────────────────────────

    #[tokio::test]
    async fn test_static_fx_rate_identity() -> Result<(), AccountingError> {
        let provider = StaticFxRateProvider::new(vec![]);
        let rate = provider.get_rate(&Currency::USD, &Currency::USD).await?;
        assert_eq!(rate, Price::new(Decimal::ONE));
        Ok(())
    }

    #[tokio::test]
    async fn test_static_fx_rate_direct() -> Result<(), AccountingError> {
        let provider = StaticFxRateProvider::new(vec![(
            Currency::BTC,
            Currency::USD,
            Price::new(dec!(67000)),
        )]);
        let rate = provider.get_rate(&Currency::BTC, &Currency::USD).await?;
        assert_eq!(rate, Price::new(dec!(67000)));
        Ok(())
    }

    #[tokio::test]
    async fn test_static_fx_rate_inverse() -> Result<(), AccountingError> {
        let provider = StaticFxRateProvider::new(vec![(
            Currency::BTC,
            Currency::USD,
            Price::new(dec!(67000)),
        )]);
        let rate = provider.get_rate(&Currency::USD, &Currency::BTC).await?;
        // 1 / 67000
        assert_eq!(rate, Price::new(Decimal::ONE / dec!(67000)));
        Ok(())
    }

    #[tokio::test]
    async fn test_static_fx_rate_missing() {
        let provider = StaticFxRateProvider::new(vec![]);
        let result = provider.get_rate(&Currency::ETH, &Currency::EUR).await;
        assert!(matches!(
            result,
            Err(AccountingError::FxRateUnavailable { .. })
        ));
    }

    // ── NavCalculator ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_nav_single_currency() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticFxRateProvider::new(vec![]);
        let calc = NavCalculator::new(provider);

        let balances = vec![
            usd_balance("spot", dec!(50000)).map_err(|e| format!("{e}"))?,
            usd_balance("futures", dec!(10000)).map_err(|e| format!("{e}"))?,
        ];

        let nav = calc
            .calculate_nav(&Currency::USD, &balances)
            .await
            .map_err(|e| format!("{e}"))?;

        assert_eq!(nav.total_nav, Amount::new(dec!(60000)));
        assert_eq!(nav.breakdown.len(), 1);
        assert_eq!(nav.breakdown[0].currency, Currency::USD);
        assert_eq!(nav.breakdown[0].native_balance, Amount::new(dec!(60000)));
        assert_eq!(nav.breakdown[0].fx_rate, Price::new(Decimal::ONE));
        assert_eq!(nav.base_currency, Currency::USD);

        Ok(())
    }

    #[tokio::test]
    async fn test_nav_multi_currency() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticFxRateProvider::new(vec![(
            Currency::BTC,
            Currency::USD,
            Price::new(dec!(67000)),
        )]);
        let calc = NavCalculator::new(provider);

        let balances = vec![
            balance_for(Currency::BTC, dec!(2)).map_err(|e| format!("{e}"))?,
            balance_for(Currency::USD, dec!(10000)).map_err(|e| format!("{e}"))?,
        ];

        let nav = calc
            .calculate_nav(&Currency::USD, &balances)
            .await
            .map_err(|e| format!("{e}"))?;

        // BTC: 2 * 67000 = 134000, USD: 10000 * 1 = 10000, total = 144000
        assert_eq!(nav.total_nav, Amount::new(dec!(144000)));
        assert_eq!(nav.breakdown.len(), 2);

        // Sorted by currency string: BTC < USD
        let btc_entry = nav
            .breakdown
            .iter()
            .find(|e| e.currency == Currency::BTC)
            .ok_or("BTC entry not found")?;
        assert_eq!(btc_entry.native_balance, Amount::new(dec!(2)));
        assert_eq!(btc_entry.fx_rate, Price::new(dec!(67000)));
        assert_eq!(btc_entry.base_currency_value, Amount::new(dec!(134000)));

        Ok(())
    }

    #[tokio::test]
    async fn test_nav_aggregates_same_currency() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticFxRateProvider::new(vec![]);
        let calc = NavCalculator::new(provider);

        let balances = vec![
            usd_balance("spot", dec!(30000)).map_err(|e| format!("{e}"))?,
            usd_balance("futures", dec!(20000)).map_err(|e| format!("{e}"))?,
        ];

        let nav = calc
            .calculate_nav(&Currency::USD, &balances)
            .await
            .map_err(|e| format!("{e}"))?;

        // Should be aggregated into one breakdown entry
        assert_eq!(nav.breakdown.len(), 1);
        assert_eq!(nav.breakdown[0].native_balance, Amount::new(dec!(50000)));
        assert_eq!(nav.total_nav, Amount::new(dec!(50000)));

        Ok(())
    }

    #[tokio::test]
    async fn test_nav_missing_rate_error() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticFxRateProvider::new(vec![]); // no ETH/USD rate
        let calc = NavCalculator::new(provider);

        let balances = vec![balance_for(Currency::ETH, dec!(10)).map_err(|e| format!("{e}"))?];

        let result = calc.calculate_nav(&Currency::USD, &balances).await;
        assert!(matches!(
            result,
            Err(AccountingError::FxRateUnavailable { .. })
        ));

        Ok(())
    }

    #[tokio::test]
    async fn test_nav_empty_balances() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticFxRateProvider::new(vec![]);
        let calc = NavCalculator::new(provider);

        let nav = calc
            .calculate_nav(&Currency::USD, &[])
            .await
            .map_err(|e| format!("{e}"))?;

        assert_eq!(nav.total_nav, Amount::new(dec!(0)));
        assert!(nav.breakdown.is_empty());

        Ok(())
    }

    // ── Proptest ──────────────────────────────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_test_nav_non_negative(
            usd_balance_val in 0i64..=1_000_000i64,
            btc_balance_val in 0i64..=100i64,
            btc_rate in 1i64..=200_000i64,
        ) {
            // All positive balances + positive rates → NAV ≥ 0
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;

            rt.block_on(async {
                let provider = StaticFxRateProvider::new(vec![
                    (Currency::BTC, Currency::USD, Price::new(Decimal::from(btc_rate))),
                ]);
                let calc = NavCalculator::new(provider);

                let balances = vec![
                    CurrencyBalance {
                        account_id: AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                        currency: Currency::USD,
                        balance: Amount::new(Decimal::from(usd_balance_val)),
                    },
                    CurrencyBalance {
                        account_id: AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::BTC)
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                        currency: Currency::BTC,
                        balance: Amount::new(Decimal::from(btc_balance_val)),
                    },
                ];

                let nav = calc.calculate_nav(&Currency::USD, &balances).await
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;

                prop_assert!(nav.total_nav.value() >= Decimal::ZERO);
                Ok(())
            })?;
        }
    }
}
