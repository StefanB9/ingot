use ingot_core::{
    accounting::{Account, Transaction},
    execution::{OrderAcknowledgment, OrderRequest},
    feed::Ticker,
};

/// Capacity for the storage event channel.
///
/// Large enough to absorb burst tick rates without blocking the engine.
pub const STORAGE_CHANNEL_CAPACITY: usize = 256;

/// Events sent from the engine to the storage task via mpsc channel.
pub enum StorageEvent {
    /// A new account was created in the ledger.
    AccountCreated { account: Account },

    /// A validated transaction was posted to the ledger.
    TransactionPosted { transaction: Transaction },

    /// An order was submitted to an exchange and acknowledged.
    OrderPlaced {
        request: OrderRequest,
        acknowledgment: OrderAcknowledgment,
    },

    /// A market data tick was received.
    Tick { ticker: Ticker },

    /// Flush all buffered events to storage immediately (graceful shutdown).
    Flush {
        done: tokio::sync::oneshot::Sender<()>,
    },
}
