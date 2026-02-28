//! `#[repr(C)]` IPC message types for iceoryx2 shared-memory transport.
//!
//! These types are the on-the-wire format for all IPC communication.
//! Domain types (`OrderAcknowledgment`, `Ticker`, etc.) are converted
//! to/from these types at the IPC boundary via `From`/`Into` impls
//! in the `convert` module.

use iceoryx2::prelude::ZeroCopySend;
use ingot_primitives::{
    Amount, Currency, OrderSide, OrderStatus, OrderType, Price, Quantity, Symbol,
};

// ---------------------------------------------------------------------------
// FixedId — stack-allocated string replacement for IPC
// ---------------------------------------------------------------------------

/// Fixed-size byte buffer for string-like identifiers (exchange IDs, error
/// messages) that must cross the IPC boundary without heap allocation.
#[derive(Clone, Copy, PartialEq, Eq, ZeroCopySend)]
#[repr(C)]
pub struct FixedId([u8; 64]);

impl FixedId {
    /// Maximum number of UTF-8 bytes that can be stored.
    pub const CAPACITY: usize = 64;

    /// Create a `FixedId` from a string slice, truncating to
    /// [`Self::CAPACITY`].
    pub fn from_slice(s: &str) -> Self {
        let mut buf = [0u8; Self::CAPACITY];
        let len = s.len().min(Self::CAPACITY);
        buf[..len].copy_from_slice(&s.as_bytes()[..len]);
        Self(buf)
    }

    /// View the stored bytes as a UTF-8 string (up to the first null byte).
    pub fn as_str(&self) -> &str {
        let len = self
            .0
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(Self::CAPACITY);
        // SAFETY: we only ever write valid UTF-8 via `from_str`.
        std::str::from_utf8(&self.0[..len]).unwrap_or("")
    }
}

impl Default for FixedId {
    fn default() -> Self {
        Self([0u8; Self::CAPACITY])
    }
}

impl std::fmt::Debug for FixedId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FixedId({:?})", self.as_str())
    }
}

impl std::fmt::Display for FixedId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Pub/sub payloads
// ---------------------------------------------------------------------------

/// Ticker price update broadcast by the server on every market data tick.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IpcTickerUpdate {
    pub symbol: Symbol,
    pub price: Price,
}

/// Net asset value snapshot broadcast after portfolio recalculation.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IpcNavUpdate {
    pub amount: Amount,
    pub currency: Currency,
}

/// Order status update broadcast when an order changes state.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IpcOrderUpdate {
    pub exchange_id: FixedId,
    pub has_client_id: bool,
    pub client_id: FixedId,
    pub timestamp_secs: i64,
    pub timestamp_nanos: u32,
    pub status: OrderStatus,
}

// ---------------------------------------------------------------------------
// Request/response payloads
// ---------------------------------------------------------------------------

/// Order placement request embedded in [`IpcCommand::PlaceOrder`].
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IpcOrderRequest {
    pub symbol: Symbol,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: Quantity,
    pub has_price: bool,
    pub price: Price,
    pub validate_only: bool,
}

/// Command sent from a client to the server via request/response IPC.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub enum IpcCommand {
    /// Place or validate an order.
    PlaceOrder(IpcOrderRequest),
    /// Query the current net asset value in the given denomination.
    QueryNav(Currency),
    /// Query the current price for a symbol.
    QueryTicker(Symbol),
    /// Subscribe to a ticker feed for a symbol.
    Subscribe(Symbol),
    /// Health-check ping.
    Heartbeat,
    /// Request graceful shutdown.
    Shutdown,
}

/// Response sent from the server to a client via request/response IPC.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub enum IpcCommandResponse {
    /// Acknowledgment of a placed/validated order.
    OrderAck(IpcOrderUpdate),
    /// Current NAV in the requested denomination.
    Nav(IpcNavUpdate),
    /// Current ticker price (or `found = false` if no data).
    Ticker {
        symbol: Symbol,
        price: Price,
        found: bool,
    },
    /// Acknowledgment of a ticker subscription request.
    SubscribeAck { symbol: Symbol, success: bool },
    /// Heartbeat reply with server uptime.
    Heartbeat { uptime_secs: u64 },
    /// Shutdown acknowledgment.
    ShutdownAck { success: bool },
    /// Error response with numeric code and human-readable message.
    Error { code: u16, message: FixedId },
}

// ---------------------------------------------------------------------------
// ZeroCopySend implementations
// ---------------------------------------------------------------------------
//
// SAFETY: All types below satisfy iceoryx2's ZeroCopySend requirements:
// - `#[repr(C)]` for deterministic memory layout
// - `Copy` (therefore no `Drop`)
// - No heap allocations, pointers, or references
// - All fields are either primitive types, fixed-size byte arrays, or composed
//   of such types
//
// We cannot derive `ZeroCopySend` because the field types from
// `ingot-primitives` (Symbol, Price, etc.) do not themselves implement
// `ZeroCopySend` (iceoryx2 is not a dependency of ingot-primitives).
// The orphan rule prevents us from implementing a foreign trait on
// foreign types. Since these IPC structs are defined in this crate,
// we can safely implement the trait here.

#[allow(unsafe_code)]
// SAFETY: IpcTickerUpdate is #[repr(C)], Copy, contains only Symbol([u8;16])
// and Price(Decimal) — both stack-allocated, no Drop, no heap.
unsafe impl ZeroCopySend for IpcTickerUpdate {}

#[allow(unsafe_code)]
// SAFETY: IpcNavUpdate is #[repr(C)], Copy, contains only Amount(Decimal)
// and Currency { CurrencyCode([u8;8]), u8 } — all stack-allocated.
unsafe impl ZeroCopySend for IpcNavUpdate {}

#[allow(unsafe_code)]
// SAFETY: IpcOrderUpdate is #[repr(C)], Copy, contains FixedId([u8;64]),
// bool, i64, u32, OrderStatus (C-like enum) — all POD.
unsafe impl ZeroCopySend for IpcOrderUpdate {}

#[allow(unsafe_code)]
// SAFETY: IpcOrderRequest is #[repr(C)], Copy, all fields are stack-allocated
// primitives (Symbol, OrderSide, OrderType, Quantity, Price, bool).
unsafe impl ZeroCopySend for IpcOrderRequest {}

#[allow(unsafe_code)]
// SAFETY: IpcCommand is #[repr(C)], Copy, all variants contain only
// IPC-safe types (IpcOrderRequest, Currency, Symbol, or no data).
unsafe impl ZeroCopySend for IpcCommand {}

#[allow(unsafe_code)]
// SAFETY: IpcCommandResponse is #[repr(C)], Copy, all variants contain only
// IPC-safe types (IpcOrderUpdate, IpcNavUpdate, Symbol, Price, FixedId, bool,
// u16, u64).
unsafe impl ZeroCopySend for IpcCommandResponse {}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rust_decimal::dec;

    use super::*;

    // -- FixedId tests -------------------------------------------------------

    #[test]
    fn test_fixed_id_from_str_and_back() {
        let id = FixedId::from_slice("order-12345");
        assert_eq!(id.as_str(), "order-12345");
    }

    #[test]
    fn test_fixed_id_empty() {
        let id = FixedId::default();
        assert_eq!(id.as_str(), "");
    }

    #[test]
    fn test_fixed_id_truncates_at_capacity() {
        let long = "A".repeat(100);
        let id = FixedId::from_slice(&long);
        assert_eq!(id.as_str().len(), FixedId::CAPACITY);
        assert_eq!(id.as_str(), &long[..FixedId::CAPACITY]);
    }

    #[test]
    fn test_fixed_id_exact_capacity() {
        let exact = "B".repeat(FixedId::CAPACITY);
        let id = FixedId::from_slice(&exact);
        assert_eq!(id.as_str(), exact);
    }

    #[test]
    fn test_fixed_id_equality() {
        let a = FixedId::from_slice("hello");
        let b = FixedId::from_slice("hello");
        let c = FixedId::from_slice("world");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_fixed_id_display() {
        let id = FixedId::from_slice("test-id");
        assert_eq!(format!("{id}"), "test-id");
    }

    // -- Copy semantics tests ------------------------------------------------

    #[test]
    fn test_ipc_ticker_update_is_copy() {
        let t = IpcTickerUpdate {
            symbol: Symbol::new("BTC-USD"),
            price: Price::from(dec!(50000)),
        };
        let t2 = t; // Copy
        assert_eq!(t.symbol, t2.symbol);
    }

    #[test]
    fn test_ipc_nav_update_is_copy() {
        let n = IpcNavUpdate {
            amount: Amount::from(dec!(10000)),
            currency: Currency::usd(),
        };
        let n2 = n; // Copy
        assert_eq!(n.amount, n2.amount);
    }

    #[test]
    fn test_ipc_order_request_is_copy() {
        let r = IpcOrderRequest {
            symbol: Symbol::new("ETH-USD"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::from(dec!(10)),
            has_price: true,
            price: Price::from(dec!(3000)),
            validate_only: false,
        };
        let r2 = r; // Copy
        assert_eq!(r.symbol, r2.symbol);
    }

    // -- Size assertions (catch accidental layout changes) -------------------

    #[test]
    fn test_fixed_id_size() {
        assert_eq!(std::mem::size_of::<FixedId>(), 64);
    }

    #[test]
    fn test_ipc_types_are_nonzero_size() {
        assert!(std::mem::size_of::<IpcTickerUpdate>() > 0);
        assert!(std::mem::size_of::<IpcNavUpdate>() > 0);
        assert!(std::mem::size_of::<IpcOrderUpdate>() > 0);
        assert!(std::mem::size_of::<IpcOrderRequest>() > 0);
        assert!(std::mem::size_of::<IpcCommand>() > 0);
        assert!(std::mem::size_of::<IpcCommandResponse>() > 0);
    }

    // -- Enum variant tests --------------------------------------------------

    #[test]
    fn test_ipc_command_variants() {
        let cmd = IpcCommand::Heartbeat;
        assert!(matches!(cmd, IpcCommand::Heartbeat));

        let cmd = IpcCommand::Shutdown;
        assert!(matches!(cmd, IpcCommand::Shutdown));

        let cmd = IpcCommand::QueryTicker(Symbol::new("BTC-USD"));
        assert!(matches!(cmd, IpcCommand::QueryTicker(_)));

        let cmd = IpcCommand::QueryNav(Currency::usd());
        assert!(matches!(cmd, IpcCommand::QueryNav(_)));

        let cmd = IpcCommand::PlaceOrder(IpcOrderRequest {
            symbol: Symbol::new("BTC-USD"),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::from(dec!(1)),
            has_price: false,
            price: Price::from(dec!(0)),
            validate_only: false,
        });
        assert!(matches!(cmd, IpcCommand::PlaceOrder(_)));

        let cmd = IpcCommand::Subscribe(Symbol::new("ETH-USD"));
        assert!(matches!(cmd, IpcCommand::Subscribe(_)));
        if let IpcCommand::Subscribe(sym) = cmd {
            assert_eq!(sym, Symbol::new("ETH-USD"));
        }
    }

    #[test]
    fn test_ipc_command_response_variants() -> Result<()> {
        let resp = IpcCommandResponse::Heartbeat { uptime_secs: 42 };
        assert!(matches!(
            resp,
            IpcCommandResponse::Heartbeat { uptime_secs: 42 }
        ));

        let resp = IpcCommandResponse::ShutdownAck { success: true };
        assert!(matches!(
            resp,
            IpcCommandResponse::ShutdownAck { success: true }
        ));

        let resp = IpcCommandResponse::Error {
            code: 404,
            message: FixedId::from_slice("not found"),
        };
        if let IpcCommandResponse::Error { code, message } = resp {
            assert_eq!(code, 404);
            assert_eq!(message.as_str(), "not found");
        } else {
            anyhow::bail!("expected Error variant");
        }

        let resp = IpcCommandResponse::SubscribeAck {
            symbol: Symbol::new("ETH-USD"),
            success: true,
        };
        if let IpcCommandResponse::SubscribeAck { symbol, success } = resp {
            assert_eq!(symbol, Symbol::new("ETH-USD"));
            assert!(success);
        } else {
            anyhow::bail!("expected SubscribeAck variant");
        }

        Ok(())
    }
}
