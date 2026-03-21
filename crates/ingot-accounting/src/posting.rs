use chrono::{DateTime, Utc};
use ingot_core::OrderFill;
use ingot_primitives::{Amount, Currency, Exchange, OrderSide};
use smol_str::SmolStr;

use crate::{
    error::AccountingError,
    transaction::{EntryId, LedgerEntry, Transaction, TransactionId},
    types::{AccountId, AccountType, EntrySide, TransactionType},
};

fn make_entry(
    txn_id: &TransactionId,
    account_id: AccountId,
    side: EntrySide,
    amount: Amount,
    currency: Currency,
    timestamp: DateTime<Utc>,
    description: Option<&str>,
) -> LedgerEntry {
    LedgerEntry {
        id: EntryId::new(),
        transaction_id: txn_id.clone(),
        account_id,
        side,
        amount,
        currency,
        timestamp,
        description: description.map(SmolStr::new),
    }
}

/// Convert an order fill into a balanced double-entry trade transaction.
pub fn post_fill(
    fill: &OrderFill,
    exchange: Exchange,
    venue: &str,
    base_currency: &Currency,
    quote_currency: &Currency,
    is_short: bool,
) -> Result<Transaction, AccountingError> {
    let txn_id = TransactionId::new();
    let quote_amount = fill.fill_price * fill.fill_quantity;
    let base_amount = Amount::new(fill.fill_quantity.value());

    let reference_id = fill
        .trade_id
        .clone()
        .unwrap_or_else(|| SmolStr::new(fill.order_id.as_str()));

    let metadata = serde_json::json!({"fill_price": fill.fill_price.value().to_string()});

    let mut entries = Vec::new();

    match fill.side {
        OrderSide::Buy => {
            // Debit base asset (receive BTC)
            entries.push(make_entry(
                &txn_id,
                AccountId::new(AccountType::Asset, exchange, venue, base_currency.clone())?,
                EntrySide::Debit,
                base_amount,
                base_currency.clone(),
                fill.timestamp,
                None,
            ));
            // Credit quote asset (pay USD)
            entries.push(make_entry(
                &txn_id,
                AccountId::new(AccountType::Asset, exchange, venue, quote_currency.clone())?,
                EntrySide::Credit,
                quote_amount,
                quote_currency.clone(),
                fill.timestamp,
                None,
            ));
        }
        OrderSide::Sell => {
            // Debit quote asset (receive USD)
            entries.push(make_entry(
                &txn_id,
                AccountId::new(AccountType::Asset, exchange, venue, quote_currency.clone())?,
                EntrySide::Debit,
                quote_amount,
                quote_currency.clone(),
                fill.timestamp,
                None,
            ));
            // Credit base: liability if short, asset otherwise
            let account_type = if is_short {
                AccountType::Liability
            } else {
                AccountType::Asset
            };
            entries.push(make_entry(
                &txn_id,
                AccountId::new(account_type, exchange, venue, base_currency.clone())?,
                EntrySide::Credit,
                base_amount,
                base_currency.clone(),
                fill.timestamp,
                None,
            ));
        }
    }

    // Fee entries (skip when fee == 0)
    if fill.fee.value() != rust_decimal::Decimal::ZERO {
        entries.push(make_entry(
            &txn_id,
            AccountId::new(
                AccountType::Expense,
                exchange,
                "fee",
                fill.fee_currency.clone(),
            )?,
            EntrySide::Debit,
            fill.fee,
            fill.fee_currency.clone(),
            fill.timestamp,
            None,
        ));
        entries.push(make_entry(
            &txn_id,
            AccountId::new(
                AccountType::Asset,
                exchange,
                venue,
                fill.fee_currency.clone(),
            )?,
            EntrySide::Credit,
            fill.fee,
            fill.fee_currency.clone(),
            fill.timestamp,
            None,
        ));
    }

    Ok(Transaction {
        id: txn_id,
        transaction_type: TransactionType::Trade,
        entries,
        timestamp: fill.timestamp,
        reference_id: Some(reference_id),
        metadata: Some(metadata),
    })
}

/// Convert a funding rate payment/receipt into a balanced transaction.
pub fn post_funding_rate(
    exchange: Exchange,
    venue: &str,
    currency: &Currency,
    amount: Amount,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError> {
    if amount.value() == rust_decimal::Decimal::ZERO {
        return Err(AccountingError::InvalidAmount {
            reason: "funding rate amount cannot be zero".into(),
        });
    }

    let txn_id = TransactionId::new();
    let abs_amount = Amount::new(amount.value().abs());
    let mut entries = Vec::new();

    if amount.value().is_sign_positive() {
        // Pay funding: debit expense, credit asset
        entries.push(make_entry(
            &txn_id,
            AccountId::new(AccountType::Expense, exchange, "funding", currency.clone())?,
            EntrySide::Debit,
            abs_amount,
            currency.clone(),
            timestamp,
            None,
        ));
        entries.push(make_entry(
            &txn_id,
            AccountId::new(AccountType::Asset, exchange, venue, currency.clone())?,
            EntrySide::Credit,
            abs_amount,
            currency.clone(),
            timestamp,
            None,
        ));
    } else {
        // Receive funding: debit asset, credit revenue
        entries.push(make_entry(
            &txn_id,
            AccountId::new(AccountType::Asset, exchange, venue, currency.clone())?,
            EntrySide::Debit,
            abs_amount,
            currency.clone(),
            timestamp,
            None,
        ));
        entries.push(make_entry(
            &txn_id,
            AccountId::new(AccountType::Revenue, exchange, "funding", currency.clone())?,
            EntrySide::Credit,
            abs_amount,
            currency.clone(),
            timestamp,
            None,
        ));
    }

    Ok(Transaction {
        id: txn_id,
        transaction_type: TransactionType::FundingRate,
        entries,
        timestamp,
        reference_id: None,
        metadata: None,
    })
}

/// Convert an asset transfer between venues into a balanced transaction.
pub fn post_transfer(
    from_exchange: Exchange,
    from_venue: &str,
    to_exchange: Exchange,
    to_venue: &str,
    currency: &Currency,
    amount: Amount,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError> {
    if amount.value() <= rust_decimal::Decimal::ZERO {
        return Err(AccountingError::InvalidAmount {
            reason: "transfer amount must be positive".into(),
        });
    }

    let txn_id = TransactionId::new();

    let entries = vec![
        make_entry(
            &txn_id,
            AccountId::new(AccountType::Asset, to_exchange, to_venue, currency.clone())?,
            EntrySide::Debit,
            amount,
            currency.clone(),
            timestamp,
            None,
        ),
        make_entry(
            &txn_id,
            AccountId::new(
                AccountType::Asset,
                from_exchange,
                from_venue,
                currency.clone(),
            )?,
            EntrySide::Credit,
            amount,
            currency.clone(),
            timestamp,
            None,
        ),
    ];

    Ok(Transaction {
        id: txn_id,
        transaction_type: TransactionType::Transfer,
        entries,
        timestamp,
        reference_id: None,
        metadata: None,
    })
}

/// Convert a manual balance adjustment into a balanced transaction.
pub fn post_adjustment(
    account_id: &AccountId,
    amount: Amount,
    timestamp: DateTime<Utc>,
    reason: &str,
) -> Result<Transaction, AccountingError> {
    if amount.value() == rust_decimal::Decimal::ZERO {
        return Err(AccountingError::InvalidAmount {
            reason: "adjustment amount cannot be zero".into(),
        });
    }

    let txn_id = TransactionId::new();
    let abs_amount = Amount::new(amount.value().abs());
    let contra = AccountId::new(
        AccountType::Revenue,
        account_id.exchange,
        "adjustment",
        account_id.currency.clone(),
    )?;

    let metadata = serde_json::json!({"reason": reason});

    let entries = if amount.value().is_sign_positive() {
        // Positive: debit target, credit revenue:adjustment
        vec![
            make_entry(
                &txn_id,
                account_id.clone(),
                EntrySide::Debit,
                abs_amount,
                account_id.currency.clone(),
                timestamp,
                None,
            ),
            make_entry(
                &txn_id,
                contra,
                EntrySide::Credit,
                abs_amount,
                account_id.currency.clone(),
                timestamp,
                None,
            ),
        ]
    } else {
        // Negative: debit revenue:adjustment, credit target
        vec![
            make_entry(
                &txn_id,
                contra,
                EntrySide::Debit,
                abs_amount,
                account_id.currency.clone(),
                timestamp,
                None,
            ),
            make_entry(
                &txn_id,
                account_id.clone(),
                EntrySide::Credit,
                abs_amount,
                account_id.currency.clone(),
                timestamp,
                None,
            ),
        ]
    };

    Ok(Transaction {
        id: txn_id,
        transaction_type: TransactionType::Adjustment,
        entries,
        timestamp,
        reference_id: None,
        metadata: Some(metadata),
    })
}

#[cfg(test)]
mod tests {
    use ingot_core::OrderId;
    use ingot_primitives::{Price, Quantity};
    use proptest::prelude::*;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::*;

    fn make_fill(
        side: OrderSide,
        price: Decimal,
        quantity: Decimal,
        fee: Decimal,
    ) -> Result<OrderFill, Box<dyn std::error::Error>> {
        Ok(OrderFill {
            order_id: OrderId::new("order-1").map_err(|e| format!("{e}"))?,
            symbol: ingot_primitives::Symbol::new("BTCUSD").map_err(|e| format!("{e}"))?,
            side,
            fill_price: Price::new(price),
            fill_quantity: Quantity::new(quantity).map_err(|e| format!("{e}"))?,
            fee: Amount::new(fee),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: Some(SmolStr::new("trade-42")),
        })
    }

    // ── post_fill: Buy trade ──────────────────────────────────────────

    #[test]
    fn test_post_fill_buy_trade() -> Result<(), Box<dyn std::error::Error>> {
        let fill = make_fill(OrderSide::Buy, dec!(67000), dec!(1), dec!(17.42))?;
        let txn = post_fill(
            &fill,
            Exchange::Kraken,
            "spot",
            &Currency::BTC,
            &Currency::USD,
            false,
        )
        .map_err(|e| format!("{e}"))?;

        // 4 entries: base debit, quote credit, fee debit, fee credit
        assert_eq!(txn.entries.len(), 4);
        assert_eq!(txn.transaction_type, TransactionType::Trade);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Entry 0: debit asset:kraken:spot:BTC 1.0
        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:kraken:spot:BTC"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(1)));
        assert_eq!(txn.entries[0].currency, Currency::BTC);

        // Entry 1: credit asset:kraken:spot:USD 67000
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "asset:kraken:spot:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(67000)));
        assert_eq!(txn.entries[1].currency, Currency::USD);

        // Entry 2: debit expense:kraken:fee:USD 17.42
        assert_eq!(
            txn.entries[2].account_id.to_string(),
            "expense:kraken:fee:USD"
        );
        assert_eq!(txn.entries[2].side, EntrySide::Debit);
        assert_eq!(txn.entries[2].amount, Amount::new(dec!(17.42)));

        // Entry 3: credit asset:kraken:spot:USD 17.42
        assert_eq!(
            txn.entries[3].account_id.to_string(),
            "asset:kraken:spot:USD"
        );
        assert_eq!(txn.entries[3].side, EntrySide::Credit);
        assert_eq!(txn.entries[3].amount, Amount::new(dec!(17.42)));

        // reference_id = trade_id
        assert_eq!(txn.reference_id.as_deref(), Some("trade-42"));

        // metadata has fill_price
        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert_eq!(meta["fill_price"], "67000");

        Ok(())
    }

    // ── post_fill: Sell trade ─────────────────────────────────────────

    #[test]
    fn test_post_fill_sell_trade() -> Result<(), Box<dyn std::error::Error>> {
        let fill = make_fill(OrderSide::Sell, dec!(68000), dec!(1), dec!(17.68))?;
        let txn = post_fill(
            &fill,
            Exchange::Kraken,
            "spot",
            &Currency::BTC,
            &Currency::USD,
            false,
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 4);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Entry 0: debit quote asset (receive USD)
        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:kraken:spot:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(68000)));

        // Entry 1: credit base asset (deliver BTC)
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "asset:kraken:spot:BTC"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(1)));

        Ok(())
    }

    // ── post_fill: Short sell ─────────────────────────────────────────

    #[test]
    fn test_post_fill_short_sell() -> Result<(), Box<dyn std::error::Error>> {
        let fill = make_fill(OrderSide::Sell, dec!(68000), dec!(1), dec!(0))?;
        let txn = post_fill(
            &fill,
            Exchange::Kraken,
            "spot",
            &Currency::BTC,
            &Currency::USD,
            true,
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Entry 1: credit liability (not asset)
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "liability:kraken:spot:BTC"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);

        Ok(())
    }

    // ── post_fill: Zero fee ───────────────────────────────────────────

    #[test]
    fn test_post_fill_zero_fee() -> Result<(), Box<dyn std::error::Error>> {
        let fill = make_fill(OrderSide::Buy, dec!(67000), dec!(1), dec!(0))?;
        let txn = post_fill(
            &fill,
            Exchange::Kraken,
            "spot",
            &Currency::BTC,
            &Currency::USD,
            false,
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        txn.validate().map_err(|e| format!("{e}"))?;

        Ok(())
    }

    // ── post_fill: reference_id falls back to order_id ────────────────

    #[test]
    fn test_post_fill_reference_id_fallback() -> Result<(), Box<dyn std::error::Error>> {
        let fill = OrderFill {
            order_id: OrderId::new("order-99").map_err(|e| format!("{e}"))?,
            symbol: ingot_primitives::Symbol::new("BTCUSD").map_err(|e| format!("{e}"))?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67000)),
            fill_quantity: Quantity::new(dec!(1)).map_err(|e| format!("{e}"))?,
            fee: Amount::new(dec!(0)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };

        let txn = post_fill(
            &fill,
            Exchange::Kraken,
            "spot",
            &Currency::BTC,
            &Currency::USD,
            false,
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.reference_id.as_deref(), Some("order-99"));

        Ok(())
    }

    // ── post_fill: Proptest ───────────────────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_test_post_fill_always_validates(
            price in 1i64..=1_000_000i64,
            qty in 1i64..=1_000_000i64,
            fee in 0i64..=10_000i64,
            is_buy in proptest::bool::ANY,
            is_short in proptest::bool::ANY,
        ) {
            let side = if is_buy { OrderSide::Buy } else { OrderSide::Sell };
            let fill = OrderFill {
                order_id: OrderId::new("prop-order")
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                symbol: ingot_primitives::Symbol::new("BTCUSD")
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                side,
                fill_price: Price::new(Decimal::from(price)),
                fill_quantity: Quantity::new(Decimal::from(qty))
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                fee: Amount::new(Decimal::from(fee)),
                fee_currency: Currency::USD,
                timestamp: Utc::now(),
                trade_id: Some(SmolStr::new("prop-trade")),
            };

            let txn = post_fill(
                &fill,
                Exchange::Kraken,
                "spot",
                &Currency::BTC,
                &Currency::USD,
                is_short,
            )
            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;

            txn.validate()
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
        }
    }

    // ── post_funding_rate: Pay ────────────────────────────────────────

    #[test]
    fn test_post_funding_rate_pay() -> Result<(), Box<dyn std::error::Error>> {
        let txn = post_funding_rate(
            Exchange::Kraken,
            "futures",
            &Currency::USD,
            Amount::new(dec!(6.70)),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.transaction_type, TransactionType::FundingRate);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Debit expense:kraken:funding:USD
        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "expense:kraken:funding:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(6.70)));

        // Credit asset:kraken:futures:USD
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "asset:kraken:futures:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(6.70)));

        Ok(())
    }

    // ── post_funding_rate: Receive ────────────────────────────────────

    #[test]
    fn test_post_funding_rate_receive() -> Result<(), Box<dyn std::error::Error>> {
        let txn = post_funding_rate(
            Exchange::Kraken,
            "futures",
            &Currency::USD,
            Amount::new(dec!(-25)),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Debit asset:kraken:futures:USD
        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:kraken:futures:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(25)));

        // Credit revenue:kraken:funding:USD
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "revenue:kraken:funding:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(25)));

        Ok(())
    }

    // ── post_funding_rate: Zero rejected ──────────────────────────────

    #[test]
    fn test_post_funding_rate_zero_rejected() {
        let result = post_funding_rate(
            Exchange::Kraken,
            "futures",
            &Currency::USD,
            Amount::new(dec!(0)),
            Utc::now(),
        );
        assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
    }

    // ── post_transfer ─────────────────────────────────────────────────

    #[test]
    fn test_post_transfer_same_exchange() -> Result<(), Box<dyn std::error::Error>> {
        let txn = post_transfer(
            Exchange::Kraken,
            "spot",
            Exchange::Kraken,
            "futures",
            &Currency::USD,
            Amount::new(dec!(10000)),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.transaction_type, TransactionType::Transfer);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Debit destination
        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:kraken:futures:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);

        // Credit source
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "asset:kraken:spot:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);

        Ok(())
    }

    #[test]
    fn test_post_transfer_cross_exchange() -> Result<(), Box<dyn std::error::Error>> {
        let txn = post_transfer(
            Exchange::Kraken,
            "spot",
            Exchange::Paper,
            "spot",
            &Currency::USD,
            Amount::new(dec!(5000)),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        txn.validate().map_err(|e| format!("{e}"))?;

        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:paper:spot:USD"
        );
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "asset:kraken:spot:USD"
        );

        Ok(())
    }

    #[test]
    fn test_post_transfer_zero_amount_rejected() {
        let result = post_transfer(
            Exchange::Kraken,
            "spot",
            Exchange::Kraken,
            "futures",
            &Currency::USD,
            Amount::new(dec!(0)),
            Utc::now(),
        );
        assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
    }

    #[test]
    fn test_post_transfer_negative_amount_rejected() {
        let result = post_transfer(
            Exchange::Kraken,
            "spot",
            Exchange::Kraken,
            "futures",
            &Currency::USD,
            Amount::new(dec!(-100)),
            Utc::now(),
        );
        assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
    }

    // ── post_adjustment ───────────────────────────────────────────────

    #[test]
    fn test_post_adjustment_positive() -> Result<(), Box<dyn std::error::Error>> {
        let account = AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
            .map_err(|e| format!("{e}"))?;

        let txn = post_adjustment(&account, Amount::new(dec!(500)), Utc::now(), "bonus credit")
            .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.transaction_type, TransactionType::Adjustment);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Debit target
        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:kraken:spot:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(500)));

        // Credit revenue:adjustment
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "revenue:kraken:adjustment:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(500)));

        // Metadata has reason
        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert_eq!(meta["reason"], "bonus credit");

        Ok(())
    }

    #[test]
    fn test_post_adjustment_negative() -> Result<(), Box<dyn std::error::Error>> {
        let account = AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD)
            .map_err(|e| format!("{e}"))?;

        let txn = post_adjustment(&account, Amount::new(dec!(-200)), Utc::now(), "correction")
            .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Debit revenue:adjustment (reversed)
        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "revenue:kraken:adjustment:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(200)));

        // Credit target
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "asset:kraken:spot:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(200)));

        Ok(())
    }

    #[test]
    fn test_post_adjustment_zero_rejected() {
        let account = AccountId::new(AccountType::Asset, Exchange::Kraken, "spot", Currency::USD);
        // AccountId::new could fail, but "spot" is valid
        if let Ok(acc) = account {
            let result = post_adjustment(&acc, Amount::new(dec!(0)), Utc::now(), "noop");
            assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        }
    }
}
