use std::{collections::HashMap, fmt};

use chrono::{DateTime, Utc};
use ingot_core::Balance;
use ingot_primitives::{Amount, Currency, Exchange};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::{Timestamp, Uuid};

use crate::{balance::CurrencyBalance, config::AccountingConfig};

fn uuid_v7_now() -> Uuid {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ts = Timestamp::from_unix(uuid::NoContext, now.as_secs(), now.subsec_nanos());
    Uuid::new_v7(ts)
}

/// Discrepancy between ledger and broker balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discrepancy {
    pub currency: Currency,
    pub ledger_balance: Amount,
    pub broker_balance: Amount,
    pub difference: Amount,
    pub severity: DiscrepancySeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiscrepancySeverity {
    None,
    Minor,
    Major,
    Critical,
}

impl fmt::Display for DiscrepancySeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Minor => f.write_str("minor"),
            Self::Major => f.write_str("major"),
            Self::Critical => f.write_str("critical"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReconciliationStatus {
    Pass,
    Fail,
}

impl fmt::Display for ReconciliationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => f.write_str("pass"),
            Self::Fail => f.write_str("fail"),
        }
    }
}

/// Result of a reconciliation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub id: Uuid,
    pub exchange: Exchange,
    pub timestamp: DateTime<Utc>,
    pub discrepancies: Vec<Discrepancy>,
    pub status: ReconciliationStatus,
}

/// Compare internal ledger balances against broker-reported balances.
///
/// Pure function — caller provides both pre-fetched inputs.
/// Ledger balances should be pre-filtered for the relevant exchange.
pub fn reconcile(
    exchange: Exchange,
    ledger_balances: &[CurrencyBalance],
    broker_balances: &[Balance],
    config: &AccountingConfig,
) -> ReconciliationResult {
    // Aggregate ledger balances by currency
    let mut ledger_by_currency: HashMap<Currency, Decimal> = HashMap::new();
    for bal in ledger_balances {
        *ledger_by_currency
            .entry(bal.currency.clone())
            .or_insert(Decimal::ZERO) += bal.balance.value();
    }

    // Collect broker balances by currency
    let mut broker_by_currency: HashMap<Currency, Decimal> = HashMap::new();
    for bal in broker_balances {
        *broker_by_currency
            .entry(bal.currency.clone())
            .or_insert(Decimal::ZERO) += bal.total.value();
    }

    // Union of all currencies
    let mut currencies: Vec<Currency> = ledger_by_currency.keys().cloned().collect();
    for k in broker_by_currency.keys() {
        if !currencies.contains(k) {
            currencies.push(k.clone());
        }
    }
    currencies.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    let mut discrepancies = Vec::new();
    let mut has_major_or_critical = false;

    for currency in currencies {
        let ledger_amt = ledger_by_currency
            .get(&currency)
            .copied()
            .unwrap_or(Decimal::ZERO);
        let broker_amt = broker_by_currency
            .get(&currency)
            .copied()
            .unwrap_or(Decimal::ZERO);
        let difference = ledger_amt - broker_amt;
        let severity = classify_severity(difference, broker_amt, config);

        if matches!(
            severity,
            DiscrepancySeverity::Major | DiscrepancySeverity::Critical
        ) {
            has_major_or_critical = true;
        }

        discrepancies.push(Discrepancy {
            currency,
            ledger_balance: Amount::new(ledger_amt),
            broker_balance: Amount::new(broker_amt),
            difference: Amount::new(difference),
            severity,
        });
    }

    let status = if has_major_or_critical {
        ReconciliationStatus::Fail
    } else {
        ReconciliationStatus::Pass
    };

    ReconciliationResult {
        id: uuid_v7_now(),
        exchange,
        timestamp: Utc::now(),
        discrepancies,
        status,
    }
}

fn classify_severity(
    difference: Decimal,
    reference: Decimal,
    config: &AccountingConfig,
) -> DiscrepancySeverity {
    if difference == Decimal::ZERO {
        return DiscrepancySeverity::None;
    }
    if reference == Decimal::ZERO {
        return DiscrepancySeverity::Critical;
    }
    let pct = (difference / reference).abs();
    if pct < config.minor_threshold_pct {
        DiscrepancySeverity::Minor
    } else if pct < config.major_threshold_pct {
        DiscrepancySeverity::Major
    } else {
        DiscrepancySeverity::Critical
    }
}

#[cfg(test)]
mod tests {
    use ingot_primitives::Amount;
    use proptest::prelude::*;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::types::{AccountId, AccountType};

    fn default_config() -> AccountingConfig {
        AccountingConfig::default()
    }

    fn make_ledger_balance(
        currency: Currency,
        amount: Decimal,
    ) -> Result<CurrencyBalance, Box<dyn std::error::Error>> {
        Ok(CurrencyBalance {
            account_id: AccountId::new(
                AccountType::Asset,
                Exchange::Kraken,
                "spot",
                currency.clone(),
            )
            .map_err(|e| format!("{e}"))?,
            currency,
            balance: Amount::new(amount),
        })
    }

    fn make_broker_balance(currency: Currency, total: Decimal) -> Balance {
        Balance {
            currency,
            total: Amount::new(total),
            available: Amount::new(total),
            held: Amount::new(Decimal::ZERO),
        }
    }

    // ── Display tests (existing) ──────────────────────────────────────

    #[test]
    fn test_discrepancy_severity_display() {
        assert_eq!(DiscrepancySeverity::None.to_string(), "none");
        assert_eq!(DiscrepancySeverity::Minor.to_string(), "minor");
        assert_eq!(DiscrepancySeverity::Major.to_string(), "major");
        assert_eq!(DiscrepancySeverity::Critical.to_string(), "critical");
    }

    #[test]
    fn test_reconciliation_status_display() {
        assert_eq!(ReconciliationStatus::Pass.to_string(), "pass");
        assert_eq!(ReconciliationStatus::Fail.to_string(), "fail");
    }

    // ── Serde tests (existing) ────────────────────────────────────────

    #[test]
    fn test_reconciliation_result_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let result = ReconciliationResult {
            id: Uuid::nil(),
            exchange: Exchange::Kraken,
            timestamp: Utc::now(),
            discrepancies: vec![],
            status: ReconciliationStatus::Pass,
        };
        let json = serde_json::to_string(&result)?;
        let deserialized: ReconciliationResult = serde_json::from_str(&json)?;
        assert_eq!(result, deserialized);
        Ok(())
    }

    #[test]
    fn test_discrepancy_serde_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let disc = Discrepancy {
            currency: Currency::USD,
            ledger_balance: Amount::new(dec!(1000)),
            broker_balance: Amount::new(dec!(999)),
            difference: Amount::new(dec!(1)),
            severity: DiscrepancySeverity::Minor,
        };
        let json = serde_json::to_string(&disc)?;
        let deserialized: Discrepancy = serde_json::from_str(&json)?;
        assert_eq!(disc, deserialized);
        Ok(())
    }

    // ── classify_severity ─────────────────────────────────────────────

    #[test]
    fn test_classify_severity_zero_difference() {
        let config = default_config();
        assert_eq!(
            classify_severity(dec!(0), dec!(1000), &config),
            DiscrepancySeverity::None
        );
    }

    #[test]
    fn test_classify_severity_minor() {
        let config = default_config(); // minor < 0.1%, major < 1%
        // 0.05% of 1000 = 0.5
        assert_eq!(
            classify_severity(dec!(0.5), dec!(1000), &config),
            DiscrepancySeverity::Minor
        );
    }

    #[test]
    fn test_classify_severity_major() {
        let config = default_config();
        // 0.5% of 1000 = 5
        assert_eq!(
            classify_severity(dec!(5), dec!(1000), &config),
            DiscrepancySeverity::Major
        );
    }

    #[test]
    fn test_classify_severity_critical() {
        let config = default_config();
        // 2% of 1000 = 20
        assert_eq!(
            classify_severity(dec!(20), dec!(1000), &config),
            DiscrepancySeverity::Critical
        );
    }

    #[test]
    fn test_classify_severity_zero_reference() {
        let config = default_config();
        assert_eq!(
            classify_severity(dec!(1), dec!(0), &config),
            DiscrepancySeverity::Critical
        );
    }

    #[test]
    fn test_classify_severity_negative_difference() {
        let config = default_config();
        // -0.5 / 1000 = 0.05% → Minor (uses abs)
        assert_eq!(
            classify_severity(dec!(-0.5), dec!(1000), &config),
            DiscrepancySeverity::Minor
        );
    }

    // ── reconcile ─────────────────────────────────────────────────────

    #[test]
    fn test_reconcile_exact_match() -> Result<(), Box<dyn std::error::Error>> {
        let config = default_config();
        let ledger = vec![make_ledger_balance(Currency::USD, dec!(1000))?];
        let broker = vec![make_broker_balance(Currency::USD, dec!(1000))];

        let result = reconcile(Exchange::Kraken, &ledger, &broker, &config);

        assert_eq!(result.status, ReconciliationStatus::Pass);
        assert_eq!(result.exchange, Exchange::Kraken);
        assert_eq!(result.discrepancies.len(), 1);
        assert_eq!(result.discrepancies[0].severity, DiscrepancySeverity::None);
        assert_eq!(result.discrepancies[0].difference, Amount::new(dec!(0)));

        Ok(())
    }

    #[test]
    fn test_reconcile_minor_drift() -> Result<(), Box<dyn std::error::Error>> {
        let config = default_config();
        // Diff = 0.05, broker = 1000 → 0.005% < 0.1% → Minor
        let ledger = vec![make_ledger_balance(Currency::USD, dec!(1000))?];
        let broker = vec![make_broker_balance(Currency::USD, dec!(999.95))];

        let result = reconcile(Exchange::Kraken, &ledger, &broker, &config);

        assert_eq!(result.status, ReconciliationStatus::Pass);
        assert_eq!(result.discrepancies[0].severity, DiscrepancySeverity::Minor);

        Ok(())
    }

    #[test]
    fn test_reconcile_major_drift() -> Result<(), Box<dyn std::error::Error>> {
        let config = default_config();
        // Diff = 5, broker = 1000 → 0.5% → Major
        let ledger = vec![make_ledger_balance(Currency::USD, dec!(1000))?];
        let broker = vec![make_broker_balance(Currency::USD, dec!(995))];

        let result = reconcile(Exchange::Kraken, &ledger, &broker, &config);

        assert_eq!(result.status, ReconciliationStatus::Fail);
        assert_eq!(result.discrepancies[0].severity, DiscrepancySeverity::Major);

        Ok(())
    }

    #[test]
    fn test_reconcile_critical_drift() -> Result<(), Box<dyn std::error::Error>> {
        let config = default_config();
        // Diff = 20, broker = 1000 → 2% → Critical
        let ledger = vec![make_ledger_balance(Currency::USD, dec!(1000))?];
        let broker = vec![make_broker_balance(Currency::USD, dec!(980))];

        let result = reconcile(Exchange::Kraken, &ledger, &broker, &config);

        assert_eq!(result.status, ReconciliationStatus::Fail);
        assert_eq!(
            result.discrepancies[0].severity,
            DiscrepancySeverity::Critical
        );

        Ok(())
    }

    #[test]
    fn test_reconcile_currency_only_in_broker() {
        let config = default_config();
        let ledger: Vec<CurrencyBalance> = vec![];
        let broker = vec![make_broker_balance(Currency::ETH, dec!(10))];

        let result = reconcile(Exchange::Kraken, &ledger, &broker, &config);

        assert_eq!(result.status, ReconciliationStatus::Fail);
        assert_eq!(result.discrepancies.len(), 1);
        assert_eq!(
            result.discrepancies[0].severity,
            DiscrepancySeverity::Critical
        );
        assert_eq!(result.discrepancies[0].ledger_balance, Amount::new(dec!(0)));
        assert_eq!(
            result.discrepancies[0].broker_balance,
            Amount::new(dec!(10))
        );
    }

    #[test]
    fn test_reconcile_currency_only_in_ledger() -> Result<(), Box<dyn std::error::Error>> {
        let config = default_config();
        let ledger = vec![make_ledger_balance(Currency::BTC, dec!(1))?];
        let broker: Vec<Balance> = vec![];

        let result = reconcile(Exchange::Kraken, &ledger, &broker, &config);

        assert_eq!(result.status, ReconciliationStatus::Fail);
        assert_eq!(result.discrepancies.len(), 1);
        assert_eq!(
            result.discrepancies[0].severity,
            DiscrepancySeverity::Critical
        );

        Ok(())
    }

    #[test]
    fn test_reconcile_multi_currency() -> Result<(), Box<dyn std::error::Error>> {
        let config = default_config();
        let ledger = vec![
            make_ledger_balance(Currency::USD, dec!(10000))?,
            make_ledger_balance(Currency::BTC, dec!(1.0005))?,
        ];
        let broker = vec![
            make_broker_balance(Currency::USD, dec!(10000)),
            make_broker_balance(Currency::BTC, dec!(1)), // 0.05% diff → Minor
        ];

        let result = reconcile(Exchange::Kraken, &ledger, &broker, &config);

        assert_eq!(result.status, ReconciliationStatus::Pass);
        assert_eq!(result.discrepancies.len(), 2);

        // BTC: Minor drift
        let btc_disc = result
            .discrepancies
            .iter()
            .find(|d| d.currency == Currency::BTC)
            .ok_or("BTC discrepancy not found")?;
        assert_eq!(btc_disc.severity, DiscrepancySeverity::Minor);

        // USD: exact match
        let usd_disc = result
            .discrepancies
            .iter()
            .find(|d| d.currency == Currency::USD)
            .ok_or("USD discrepancy not found")?;
        assert_eq!(usd_disc.severity, DiscrepancySeverity::None);

        Ok(())
    }

    #[test]
    fn test_reconcile_empty_both() {
        let config = default_config();
        let result = reconcile(Exchange::Kraken, &[], &[], &config);

        assert_eq!(result.status, ReconciliationStatus::Pass);
        assert!(result.discrepancies.is_empty());
    }

    // ── Proptest ──────────────────────────────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_test_identical_balances_always_pass(
            amounts in proptest::collection::vec(1i64..=1_000_000i64, 1..=5),
        ) {
            let config = default_config();

            let mut ledger = Vec::new();
            let mut broker = Vec::new();

            // Use different currencies for variety
            let currencies = [Currency::USD, Currency::BTC, Currency::ETH, Currency::EUR, Currency::GBP];
            for (i, &amt) in amounts.iter().enumerate() {
                let currency = currencies[i % currencies.len()].clone();
                let dec_amt = Decimal::from(amt);

                ledger.push(CurrencyBalance {
                    account_id: AccountId::new(
                        AccountType::Asset,
                        Exchange::Kraken,
                        "spot",
                        currency.clone(),
                    )
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                    currency: currency.clone(),
                    balance: Amount::new(dec_amt),
                });
                broker.push(Balance {
                    currency,
                    total: Amount::new(dec_amt),
                    available: Amount::new(dec_amt),
                    held: Amount::new(Decimal::ZERO),
                });
            }

            let result = reconcile(Exchange::Kraken, &ledger, &broker, &config);

            prop_assert_eq!(result.status, ReconciliationStatus::Pass);
            for disc in &result.discrepancies {
                prop_assert_eq!(disc.severity, DiscrepancySeverity::None);
            }
        }
    }
}
