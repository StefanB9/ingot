use std::future::Future;

use ingot_accounting::Transaction;

/// Trait for persisting accounting transactions.
///
/// Defined in `ingot-engine` so the engine stays decoupled from storage.
/// Implemented by `PgLedgerRepository` in `ingot-storage`.
pub trait LedgerWriter {
    fn write_transaction(
        &self,
        txn: &Transaction,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock implementation to verify the trait compiles and is usable.
    struct MockLedgerWriter;

    impl LedgerWriter for MockLedgerWriter {
        async fn write_transaction(&self, _txn: &Transaction) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_ledger_writer_mock_compiles() {
        // Verify the trait can be implemented — compile-time check
        fn assert_impl<T: LedgerWriter>() {}
        assert_impl::<MockLedgerWriter>();
    }
}
