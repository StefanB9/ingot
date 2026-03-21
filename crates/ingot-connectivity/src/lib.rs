pub mod backfill;
pub mod config;
pub mod error;
pub mod kraken;
pub mod paper;
pub(crate) mod rate_limiter;
pub mod traits;

pub use backfill::BackfillWorker;
pub use config::{KrakenFuturesConfig, KrakenSpotConfig, PaperExchangeConfig};
pub use error::ConnectivityError;
pub use kraken::{
    futures::{KrakenFuturesRestClient, KrakenFuturesWs},
    spot::{KrakenSpotRestClient, KrakenSpotWs},
};
pub use paper::PaperExchange;
pub use traits::{AccountProvider, MarketDataProvider, OrderExecutor, StreamProvider};
