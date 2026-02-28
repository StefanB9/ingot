//! iceoryx2 service name constants.
//!
//! Each constant defines a hierarchical service name used for
//! shared-memory IPC between `ingot-server` and its clients.

/// Pub/sub: server publishes ticker updates on every market data tick.
pub const SERVICE_TICKER: &str = "Ingot/MarketData/Ticker";

/// Pub/sub: server publishes NAV snapshots after portfolio recalculation.
pub const SERVICE_NAV: &str = "Ingot/MarketData/Nav";

/// Pub/sub: server publishes order status updates (sent, filled, rejected).
pub const SERVICE_ORDERS: &str = "Ingot/Orders/Update";

/// Request/response: clients send commands, server replies synchronously.
pub const SERVICE_COMMANDS: &str = "Ingot/Commands";
