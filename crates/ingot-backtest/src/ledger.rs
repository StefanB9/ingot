use std::sync::Mutex;

use ingot_accounting::Transaction;
use ingot_engine::LedgerWriter;

/// In-memory ledger writer for backtesting.
///
/// Captures all accounting transactions without database persistence.
/// Uses `Mutex` for interior mutability since `LedgerWriter::write_transaction`
/// takes `&self` and the trait requires `Send`.
pub struct InMemoryLedger {
    transactions: Mutex<Vec<Transaction>>,
}

impl Default for InMemoryLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryLedger {
    pub fn new() -> Self {
        Self {
            transactions: Mutex::new(Vec::new()),
        }
    }

    /// Returns a cloned copy of all stored transactions.
    pub fn transactions(&self) -> Vec<Transaction> {
        self.transactions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Consumes the ledger and returns the owned transaction list.
    pub fn into_transactions(self) -> Vec<Transaction> {
        self.transactions
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl LedgerWriter for InMemoryLedger {
    async fn write_transaction(&self, txn: &Transaction) -> anyhow::Result<()> {
        self.transactions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(txn.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_accounting::{Transaction, post_fill};
    use ingot_core::OrderFill;
    use ingot_engine::LedgerWriter;
    use ingot_primitives::{Amount, Currency, Exchange, OrderSide, Price, Quantity, Symbol};
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;

    /// Build a minimal valid Transaction via `post_fill`.
    fn make_test_transaction(trade_id: &str) -> Result<Transaction, Box<dyn std::error::Error>> {
        let fill = OrderFill {
            order_id: ingot_core::OrderId::new("test-order").map_err(|e| format!("{e}"))?,
            symbol: Symbol::new("BTCUSD").map_err(|e| format!("{e}"))?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67000)),
            fill_quantity: Quantity::new(dec!(1)).map_err(|e| format!("{e}"))?,
            fee: Amount::new(dec!(17.42)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: Some(SmolStr::new(trade_id)),
        };
        let txn = post_fill(
            &fill,
            Exchange::Paper,
            "backtest",
            &Currency::BTC,
            &Currency::USD,
            false,
        )
        .map_err(|e| format!("{e}"))?;
        Ok(txn)
    }

    #[test]
    fn test_in_memory_ledger_new_empty() {
        let ledger = InMemoryLedger::new();
        assert!(ledger.transactions().is_empty());
    }

    #[tokio::test]
    async fn test_in_memory_ledger_write_transaction() -> Result<(), Box<dyn std::error::Error>> {
        let ledger = InMemoryLedger::new();
        let txn = make_test_transaction("fill-1")?;
        let txn_id = txn.id.clone();

        ledger.write_transaction(&txn).await?;

        let stored = ledger.transactions();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, txn_id);
        Ok(())
    }

    #[tokio::test]
    async fn test_in_memory_ledger_multiple_writes() -> Result<(), Box<dyn std::error::Error>> {
        let ledger = InMemoryLedger::new();
        let txn1 = make_test_transaction("fill-1")?;
        let txn2 = make_test_transaction("fill-2")?;
        let txn3 = make_test_transaction("fill-3")?;

        let id1 = txn1.id.clone();
        let id2 = txn2.id.clone();
        let id3 = txn3.id.clone();

        ledger.write_transaction(&txn1).await?;
        ledger.write_transaction(&txn2).await?;
        ledger.write_transaction(&txn3).await?;

        let stored = ledger.transactions();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[0].id, id1);
        assert_eq!(stored[1].id, id2);
        assert_eq!(stored[2].id, id3);
        Ok(())
    }

    #[tokio::test]
    async fn test_in_memory_ledger_into_transactions() -> Result<(), Box<dyn std::error::Error>> {
        let ledger = InMemoryLedger::new();
        let txn1 = make_test_transaction("fill-a")?;
        let txn2 = make_test_transaction("fill-b")?;

        let id1 = txn1.id.clone();
        let id2 = txn2.id.clone();

        ledger.write_transaction(&txn1).await?;
        ledger.write_transaction(&txn2).await?;

        let owned = ledger.into_transactions();
        assert_eq!(owned.len(), 2);
        assert_eq!(owned[0].id, id1);
        assert_eq!(owned[1].id, id2);
        Ok(())
    }
}
