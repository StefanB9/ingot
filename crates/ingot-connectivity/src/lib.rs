pub mod config;
pub mod error;
pub mod kraken;
pub(crate) mod rate_limiter;
pub mod traits;

pub use config::{KrakenFuturesConfig, KrakenSpotConfig, PaperExchangeConfig};
pub use error::ConnectivityError;
pub use kraken::spot::KrakenSpotRestClient;
pub use traits::{AccountProvider, MarketDataProvider, OrderExecutor, StreamProvider};
