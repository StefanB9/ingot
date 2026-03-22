pub mod config;
pub mod error;
pub mod exchange;
pub mod feed;
pub mod ledger;
pub mod metrics;
pub mod result;
pub mod runner;

pub use config::BacktestConfig;
pub use error::BacktestError;
pub use exchange::BacktestExchange;
pub use feed::{BacktestEvent, merge_events, ohlcv_to_events, ticks_to_events, validate_sorted};
pub use ledger::InMemoryLedger;
pub use metrics::{PerformanceMetrics, compute_metrics};
pub use result::{BacktestResult, EquityPoint};
pub use runner::BacktestRunner;
