use chrono::{DateTime, Utc};
use ingot_core::OrderFill;
use ingot_primitives::{Amount, Currency, Exchange, OrderSide, Quantity, Symbol};
use rust_decimal::Decimal;
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

/// Post income receipt (shared logic for dividend and bond coupon).
#[allow(clippy::too_many_arguments)]
fn post_income_receipt(
    exchange: Exchange,
    venue: &str,
    currency: &Currency,
    symbol: &Symbol,
    amount: Amount,
    timestamp: DateTime<Utc>,
    txn_type: TransactionType,
    revenue_venue: &str,
    income_name: &str,
) -> Result<Transaction, AccountingError> {
    if amount.value() <= Decimal::ZERO {
        return Err(AccountingError::InvalidAmount {
            reason: format!("{income_name} amount must be positive"),
        });
    }

    let txn_id = TransactionId::new();
    let metadata = serde_json::json!({"symbol": symbol.as_str()});

    let entries = vec![
        make_entry(
            &txn_id,
            AccountId::new(AccountType::Asset, exchange, venue, currency.clone())?,
            EntrySide::Debit,
            amount,
            currency.clone(),
            timestamp,
            None,
        ),
        make_entry(
            &txn_id,
            AccountId::new(
                AccountType::Revenue,
                exchange,
                revenue_venue,
                currency.clone(),
            )?,
            EntrySide::Credit,
            amount,
            currency.clone(),
            timestamp,
            None,
        ),
    ];

    let txn = Transaction {
        id: txn_id,
        transaction_type: txn_type,
        entries,
        timestamp,
        reference_id: None,
        metadata: Some(metadata),
    };
    txn.validate()?;
    Ok(txn)
}

/// Post a dividend payment as a balanced double-entry transaction.
pub fn post_dividend(
    exchange: Exchange,
    venue: &str,
    currency: &Currency,
    symbol: &Symbol,
    amount: Amount,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError> {
    post_income_receipt(
        exchange,
        venue,
        currency,
        symbol,
        amount,
        timestamp,
        TransactionType::Dividend,
        "dividend",
        "dividend",
    )
}

/// Post a bond coupon payment as a balanced double-entry transaction.
pub fn post_bond_coupon(
    exchange: Exchange,
    venue: &str,
    currency: &Currency,
    symbol: &Symbol,
    amount: Amount,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError> {
    post_income_receipt(
        exchange,
        venue,
        currency,
        symbol,
        amount,
        timestamp,
        TransactionType::BondCoupon,
        "coupon",
        "bond coupon",
    )
}

/// Post a stock split as a metadata-only balanced transaction.
pub fn post_stock_split(
    exchange: Exchange,
    venue: &str,
    symbol: &Symbol,
    currency: &Currency,
    old_qty: Quantity,
    new_qty: Quantity,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError> {
    if old_qty.value() == Decimal::ZERO {
        return Err(AccountingError::InvalidCorporateAction {
            reason: "stock split old_qty cannot be zero".into(),
        });
    }
    if old_qty == new_qty {
        return Err(AccountingError::InvalidCorporateAction {
            reason: "stock split old_qty must differ from new_qty".into(),
        });
    }

    let txn_id = TransactionId::new();
    let ratio = new_qty.value() / old_qty.value();
    let metadata = serde_json::json!({
        "symbol": symbol.as_str(),
        "old_qty": old_qty.value().to_string(),
        "new_qty": new_qty.value().to_string(),
        "ratio": ratio.to_string(),
    });

    let zero = Amount::new(Decimal::ZERO);
    let entries = vec![
        make_entry(
            &txn_id,
            AccountId::new(AccountType::Asset, exchange, venue, currency.clone())?,
            EntrySide::Debit,
            zero,
            currency.clone(),
            timestamp,
            None,
        ),
        make_entry(
            &txn_id,
            AccountId::new(AccountType::Revenue, exchange, "split", currency.clone())?,
            EntrySide::Credit,
            zero,
            currency.clone(),
            timestamp,
            None,
        ),
    ];

    let txn = Transaction {
        id: txn_id,
        transaction_type: TransactionType::StockSplit,
        entries,
        timestamp,
        reference_id: None,
        metadata: Some(metadata),
    };
    txn.validate()?;
    Ok(txn)
}

/// Post a merger as a cross-currency asset exchange transaction.
#[allow(clippy::too_many_arguments)]
pub fn post_merger(
    exchange: Exchange,
    venue: &str,
    old_symbol: &Symbol,
    old_currency: &Currency,
    old_qty: Quantity,
    new_symbol: &Symbol,
    new_currency: &Currency,
    new_qty: Quantity,
    cash_consideration: Option<Amount>,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError> {
    if old_qty.value() == Decimal::ZERO {
        return Err(AccountingError::InvalidCorporateAction {
            reason: "merger old_qty cannot be zero".into(),
        });
    }
    if new_qty.value() == Decimal::ZERO {
        return Err(AccountingError::InvalidCorporateAction {
            reason: "merger new_qty cannot be zero".into(),
        });
    }
    if let Some(cash) = &cash_consideration
        && cash.value() <= Decimal::ZERO
    {
        return Err(AccountingError::InvalidAmount {
            reason: "merger cash consideration must be positive".into(),
        });
    }

    let txn_id = TransactionId::new();
    let cash_str = cash_consideration.as_ref().map(|c| c.value().to_string());
    let metadata = serde_json::json!({
        "old_symbol": old_symbol.as_str(),
        "new_symbol": new_symbol.as_str(),
        "old_qty": old_qty.value().to_string(),
        "new_qty": new_qty.value().to_string(),
        "cash": cash_str,
    });

    let mut entries = vec![
        // Debit: new position
        make_entry(
            &txn_id,
            AccountId::new(AccountType::Asset, exchange, venue, new_currency.clone())?,
            EntrySide::Debit,
            Amount::new(new_qty.value()),
            new_currency.clone(),
            timestamp,
            None,
        ),
        // Credit: old position
        make_entry(
            &txn_id,
            AccountId::new(AccountType::Asset, exchange, venue, old_currency.clone())?,
            EntrySide::Credit,
            Amount::new(old_qty.value()),
            old_currency.clone(),
            timestamp,
            None,
        ),
    ];

    if let Some(cash) = cash_consideration {
        // Debit: cash received (in old_currency)
        entries.push(make_entry(
            &txn_id,
            AccountId::new(AccountType::Asset, exchange, venue, old_currency.clone())?,
            EntrySide::Debit,
            cash,
            old_currency.clone(),
            timestamp,
            None,
        ));
    }

    Ok(Transaction {
        id: txn_id,
        transaction_type: TransactionType::Merger,
        entries,
        timestamp,
        reference_id: None,
        metadata: Some(metadata),
    })
}

/// Post a spinoff as a cost basis allocation or zero-value transaction.
#[allow(clippy::too_many_arguments)]
pub fn post_spinoff(
    exchange: Exchange,
    venue: &str,
    parent_symbol: &Symbol,
    new_symbol: &Symbol,
    new_currency: &Currency,
    new_qty: Quantity,
    cost_basis_allocation: Option<Amount>,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError> {
    if new_qty.value() == Decimal::ZERO {
        return Err(AccountingError::InvalidCorporateAction {
            reason: "spinoff new_qty cannot be zero".into(),
        });
    }
    if let Some(cb) = &cost_basis_allocation
        && cb.value() <= Decimal::ZERO
    {
        return Err(AccountingError::InvalidAmount {
            reason: "spinoff cost basis allocation must be positive".into(),
        });
    }

    let txn_id = TransactionId::new();
    let metadata = serde_json::json!({
        "parent_symbol": parent_symbol.as_str(),
        "new_symbol": new_symbol.as_str(),
        "new_qty": new_qty.value().to_string(),
        "cost_basis_allocation": cost_basis_allocation.as_ref().map(|c| c.value().to_string()),
    });

    let entries = if let Some(cb) = cost_basis_allocation {
        vec![
            // Debit: new position at cost basis
            make_entry(
                &txn_id,
                AccountId::new(AccountType::Asset, exchange, venue, new_currency.clone())?,
                EntrySide::Debit,
                cb,
                new_currency.clone(),
                timestamp,
                None,
            ),
            // Credit: parent cost basis reduction
            make_entry(
                &txn_id,
                AccountId::new(AccountType::Asset, exchange, venue, new_currency.clone())?,
                EntrySide::Credit,
                cb,
                new_currency.clone(),
                timestamp,
                None,
            ),
        ]
    } else {
        let zero = Amount::new(Decimal::ZERO);
        vec![
            make_entry(
                &txn_id,
                AccountId::new(AccountType::Asset, exchange, venue, new_currency.clone())?,
                EntrySide::Debit,
                zero,
                new_currency.clone(),
                timestamp,
                None,
            ),
            make_entry(
                &txn_id,
                AccountId::new(
                    AccountType::Revenue,
                    exchange,
                    "spinoff",
                    new_currency.clone(),
                )?,
                EntrySide::Credit,
                zero,
                new_currency.clone(),
                timestamp,
                None,
            ),
        ]
    };

    let txn = Transaction {
        id: txn_id,
        transaction_type: TransactionType::Spinoff,
        entries,
        timestamp,
        reference_id: None,
        metadata: Some(metadata),
    };
    txn.validate()?;
    Ok(txn)
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

    // ── post_dividend ────────────────────────────────────────────────

    #[test]
    fn test_post_dividend_basic() -> Result<(), Box<dyn std::error::Error>> {
        let symbol = ingot_primitives::Symbol::new("AAPL").map_err(|e| format!("{e}"))?;
        let txn = post_dividend(
            Exchange::IBKR,
            "stocks",
            &Currency::USD,
            &symbol,
            Amount::new(dec!(2.50)),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.transaction_type, TransactionType::Dividend);
        txn.validate().map_err(|e| format!("{e}"))?;

        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:ibkr:stocks:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(2.50)));

        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "revenue:ibkr:dividend:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(2.50)));

        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert_eq!(meta["symbol"], "AAPL");

        Ok(())
    }

    #[test]
    fn test_post_dividend_zero_amount_rejected() {
        let symbol = ingot_primitives::Symbol::new("AAPL");
        if let Ok(sym) = symbol {
            let result = post_dividend(
                Exchange::IBKR,
                "stocks",
                &Currency::USD,
                &sym,
                Amount::new(dec!(0)),
                Utc::now(),
            );
            assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        }
    }

    #[test]
    fn test_post_dividend_negative_amount_rejected() {
        let symbol = ingot_primitives::Symbol::new("AAPL");
        if let Ok(sym) = symbol {
            let result = post_dividend(
                Exchange::IBKR,
                "stocks",
                &Currency::USD,
                &sym,
                Amount::new(dec!(-5)),
                Utc::now(),
            );
            assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        }
    }

    // ── post_bond_coupon ─────────────────────────────────────────────

    #[test]
    fn test_post_bond_coupon_basic() -> Result<(), Box<dyn std::error::Error>> {
        let symbol = ingot_primitives::Symbol::new("UST10Y").map_err(|e| format!("{e}"))?;
        let txn = post_bond_coupon(
            Exchange::IBKR,
            "bonds",
            &Currency::USD,
            &symbol,
            Amount::new(dec!(125)),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.transaction_type, TransactionType::BondCoupon);
        txn.validate().map_err(|e| format!("{e}"))?;

        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:ibkr:bonds:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(125)));

        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "revenue:ibkr:coupon:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(125)));

        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert_eq!(meta["symbol"], "UST10Y");

        Ok(())
    }

    #[test]
    fn test_post_bond_coupon_zero_amount_rejected() {
        let symbol = ingot_primitives::Symbol::new("UST10Y");
        if let Ok(sym) = symbol {
            let result = post_bond_coupon(
                Exchange::IBKR,
                "bonds",
                &Currency::USD,
                &sym,
                Amount::new(dec!(0)),
                Utc::now(),
            );
            assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        }
    }

    #[test]
    fn test_post_bond_coupon_negative_amount_rejected() {
        let symbol = ingot_primitives::Symbol::new("UST10Y");
        if let Ok(sym) = symbol {
            let result = post_bond_coupon(
                Exchange::IBKR,
                "bonds",
                &Currency::USD,
                &sym,
                Amount::new(dec!(-50)),
                Utc::now(),
            );
            assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        }
    }

    // ── post_stock_split ─────────────────────────────────────────────

    #[test]
    fn test_post_stock_split_basic() -> Result<(), Box<dyn std::error::Error>> {
        let symbol = ingot_primitives::Symbol::new("AAPL").map_err(|e| format!("{e}"))?;
        let txn = post_stock_split(
            Exchange::IBKR,
            "stocks",
            &symbol,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            Quantity::new(dec!(400)).map_err(|e| format!("{e}"))?,
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.transaction_type, TransactionType::StockSplit);
        txn.validate().map_err(|e| format!("{e}"))?;

        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:ibkr:stocks:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(0)));

        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "revenue:ibkr:split:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(0)));

        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert_eq!(meta["symbol"], "AAPL");
        assert_eq!(meta["old_qty"], "100");
        assert_eq!(meta["new_qty"], "400");
        assert_eq!(meta["ratio"], "4");

        Ok(())
    }

    #[test]
    fn test_post_stock_split_reverse_split() -> Result<(), Box<dyn std::error::Error>> {
        let symbol = ingot_primitives::Symbol::new("GE").map_err(|e| format!("{e}"))?;
        let txn = post_stock_split(
            Exchange::IBKR,
            "stocks",
            &symbol,
            &Currency::USD,
            Quantity::new(dec!(300)).map_err(|e| format!("{e}"))?,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        txn.validate().map_err(|e| format!("{e}"))?;

        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert_eq!(meta["old_qty"], "300");
        assert_eq!(meta["new_qty"], "100");
        // 100/300 = 0.3333...
        let ratio: Decimal = meta["ratio"]
            .as_str()
            .ok_or("ratio not a string")?
            .parse()?;
        assert!(ratio > Decimal::ZERO);
        assert!(ratio < Decimal::ONE);

        Ok(())
    }

    #[test]
    fn test_post_stock_split_same_quantity_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let symbol = ingot_primitives::Symbol::new("AAPL").map_err(|e| format!("{e}"))?;
        let result = post_stock_split(
            Exchange::IBKR,
            "stocks",
            &symbol,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            Utc::now(),
        );
        assert!(matches!(
            result,
            Err(AccountingError::InvalidCorporateAction { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_post_stock_split_zero_old_qty_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let symbol = ingot_primitives::Symbol::new("AAPL").map_err(|e| format!("{e}"))?;
        let result = post_stock_split(
            Exchange::IBKR,
            "stocks",
            &symbol,
            &Currency::USD,
            Quantity::new(dec!(0)).map_err(|e| format!("{e}"))?,
            Quantity::new(dec!(200)).map_err(|e| format!("{e}"))?,
            Utc::now(),
        );
        assert!(matches!(
            result,
            Err(AccountingError::InvalidCorporateAction { .. })
        ));
        Ok(())
    }

    // ── post_merger ──────────────────────────────────────────────────

    #[test]
    fn test_post_merger_same_currency_no_cash() -> Result<(), Box<dyn std::error::Error>> {
        let old_sym = ingot_primitives::Symbol::new("TWTR").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("X").map_err(|e| format!("{e}"))?;
        let txn = post_merger(
            Exchange::IBKR,
            "stocks",
            &old_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            None,
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.transaction_type, TransactionType::Merger);

        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:ibkr:stocks:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(100)));

        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "asset:ibkr:stocks:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(100)));

        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert_eq!(meta["old_symbol"], "TWTR");
        assert_eq!(meta["new_symbol"], "X");
        assert!(meta["cash"].is_null());

        Ok(())
    }

    #[test]
    fn test_post_merger_same_currency_with_cash() -> Result<(), Box<dyn std::error::Error>> {
        let old_sym = ingot_primitives::Symbol::new("TWTR").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("X").map_err(|e| format!("{e}"))?;
        let txn = post_merger(
            Exchange::IBKR,
            "stocks",
            &old_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(50)).map_err(|e| format!("{e}"))?,
            Some(Amount::new(dec!(500))),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 3);

        // Entry 2: cash received
        assert_eq!(
            txn.entries[2].account_id.to_string(),
            "asset:ibkr:stocks:USD"
        );
        assert_eq!(txn.entries[2].side, EntrySide::Debit);
        assert_eq!(txn.entries[2].amount, Amount::new(dec!(500)));

        Ok(())
    }

    #[test]
    fn test_post_merger_cross_currency() -> Result<(), Box<dyn std::error::Error>> {
        let old_sym = ingot_primitives::Symbol::new("VOD").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("VODL").map_err(|e| format!("{e}"))?;
        let txn = post_merger(
            Exchange::IBKR,
            "stocks",
            &old_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            &new_sym,
            &Currency::EUR,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            None,
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.entries[0].currency, Currency::EUR);
        assert_eq!(txn.entries[1].currency, Currency::USD);

        Ok(())
    }

    #[test]
    fn test_post_merger_cross_currency_with_cash() -> Result<(), Box<dyn std::error::Error>> {
        let old_sym = ingot_primitives::Symbol::new("VOD").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("VODL").map_err(|e| format!("{e}"))?;
        let txn = post_merger(
            Exchange::IBKR,
            "stocks",
            &old_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            &new_sym,
            &Currency::EUR,
            Quantity::new(dec!(50)).map_err(|e| format!("{e}"))?,
            Some(Amount::new(dec!(250))),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 3);
        // Cash is in old_currency (USD)
        assert_eq!(txn.entries[2].currency, Currency::USD);
        assert_eq!(txn.entries[2].amount, Amount::new(dec!(250)));

        Ok(())
    }

    #[test]
    fn test_post_merger_zero_old_qty_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let old_sym = ingot_primitives::Symbol::new("A").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("B").map_err(|e| format!("{e}"))?;
        let result = post_merger(
            Exchange::IBKR,
            "stocks",
            &old_sym,
            &Currency::USD,
            Quantity::new(dec!(0)).map_err(|e| format!("{e}"))?,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            None,
            Utc::now(),
        );
        assert!(matches!(
            result,
            Err(AccountingError::InvalidCorporateAction { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_post_merger_zero_new_qty_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let old_sym = ingot_primitives::Symbol::new("A").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("B").map_err(|e| format!("{e}"))?;
        let result = post_merger(
            Exchange::IBKR,
            "stocks",
            &old_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(0)).map_err(|e| format!("{e}"))?,
            None,
            Utc::now(),
        );
        assert!(matches!(
            result,
            Err(AccountingError::InvalidCorporateAction { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_post_merger_zero_cash_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let old_sym = ingot_primitives::Symbol::new("A").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("B").map_err(|e| format!("{e}"))?;
        let result = post_merger(
            Exchange::IBKR,
            "stocks",
            &old_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            Some(Amount::new(dec!(0))),
            Utc::now(),
        );
        assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        Ok(())
    }

    #[test]
    fn test_post_merger_negative_cash_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let old_sym = ingot_primitives::Symbol::new("A").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("B").map_err(|e| format!("{e}"))?;
        let result = post_merger(
            Exchange::IBKR,
            "stocks",
            &old_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(100)).map_err(|e| format!("{e}"))?,
            Some(Amount::new(dec!(-100))),
            Utc::now(),
        );
        assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        Ok(())
    }

    // ── post_spinoff ─────────────────────────────────────────────────

    #[test]
    fn test_post_spinoff_with_cost_basis() -> Result<(), Box<dyn std::error::Error>> {
        let parent = ingot_primitives::Symbol::new("GE").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("GEV").map_err(|e| format!("{e}"))?;
        let txn = post_spinoff(
            Exchange::IBKR,
            "stocks",
            &parent,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(25)).map_err(|e| format!("{e}"))?,
            Some(Amount::new(dec!(1000))),
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        assert_eq!(txn.transaction_type, TransactionType::Spinoff);
        txn.validate().map_err(|e| format!("{e}"))?;

        // Debit new asset
        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:ibkr:stocks:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(1000)));

        // Credit parent asset (cost basis reduction)
        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "asset:ibkr:stocks:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(1000)));

        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert_eq!(meta["parent_symbol"], "GE");
        assert_eq!(meta["new_symbol"], "GEV");
        assert_eq!(meta["new_qty"], "25");
        assert_eq!(meta["cost_basis_allocation"], "1000");

        Ok(())
    }

    #[test]
    fn test_post_spinoff_without_cost_basis() -> Result<(), Box<dyn std::error::Error>> {
        let parent = ingot_primitives::Symbol::new("GE").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("GEV").map_err(|e| format!("{e}"))?;
        let txn = post_spinoff(
            Exchange::IBKR,
            "stocks",
            &parent,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(25)).map_err(|e| format!("{e}"))?,
            None,
            Utc::now(),
        )
        .map_err(|e| format!("{e}"))?;

        assert_eq!(txn.entries.len(), 2);
        txn.validate().map_err(|e| format!("{e}"))?;

        assert_eq!(
            txn.entries[0].account_id.to_string(),
            "asset:ibkr:stocks:USD"
        );
        assert_eq!(txn.entries[0].side, EntrySide::Debit);
        assert_eq!(txn.entries[0].amount, Amount::new(dec!(0)));

        assert_eq!(
            txn.entries[1].account_id.to_string(),
            "revenue:ibkr:spinoff:USD"
        );
        assert_eq!(txn.entries[1].side, EntrySide::Credit);
        assert_eq!(txn.entries[1].amount, Amount::new(dec!(0)));

        let meta = txn.metadata.as_ref().ok_or("missing metadata")?;
        assert!(meta["cost_basis_allocation"].is_null());

        Ok(())
    }

    #[test]
    fn test_post_spinoff_zero_new_qty_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let parent = ingot_primitives::Symbol::new("GE").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("GEV").map_err(|e| format!("{e}"))?;
        let result = post_spinoff(
            Exchange::IBKR,
            "stocks",
            &parent,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(0)).map_err(|e| format!("{e}"))?,
            None,
            Utc::now(),
        );
        assert!(matches!(
            result,
            Err(AccountingError::InvalidCorporateAction { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_post_spinoff_zero_cost_basis_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let parent = ingot_primitives::Symbol::new("GE").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("GEV").map_err(|e| format!("{e}"))?;
        let result = post_spinoff(
            Exchange::IBKR,
            "stocks",
            &parent,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(25)).map_err(|e| format!("{e}"))?,
            Some(Amount::new(dec!(0))),
            Utc::now(),
        );
        assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        Ok(())
    }

    #[test]
    fn test_post_spinoff_negative_cost_basis_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let parent = ingot_primitives::Symbol::new("GE").map_err(|e| format!("{e}"))?;
        let new_sym = ingot_primitives::Symbol::new("GEV").map_err(|e| format!("{e}"))?;
        let result = post_spinoff(
            Exchange::IBKR,
            "stocks",
            &parent,
            &new_sym,
            &Currency::USD,
            Quantity::new(dec!(25)).map_err(|e| format!("{e}"))?,
            Some(Amount::new(dec!(-500))),
            Utc::now(),
        );
        assert!(matches!(result, Err(AccountingError::InvalidAmount { .. })));
        Ok(())
    }

    // ── Corporate action proptests ───────────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_test_post_dividend_always_validates(
            amount in 1i64..=1_000_000i64,
        ) {
            let symbol = ingot_primitives::Symbol::new("AAPL")
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
            let txn = post_dividend(
                Exchange::IBKR,
                "stocks",
                &Currency::USD,
                &symbol,
                Amount::new(Decimal::from(amount)),
                Utc::now(),
            )
            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
            txn.validate()
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
        }

        #[test]
        fn prop_test_post_bond_coupon_always_validates(
            amount in 1i64..=1_000_000i64,
        ) {
            let symbol = ingot_primitives::Symbol::new("UST10Y")
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
            let txn = post_bond_coupon(
                Exchange::IBKR,
                "bonds",
                &Currency::USD,
                &symbol,
                Amount::new(Decimal::from(amount)),
                Utc::now(),
            )
            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
            txn.validate()
                .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;
        }
    }
}
