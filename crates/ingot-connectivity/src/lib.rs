pub mod backfill;
pub(crate) mod book_manager;
pub mod config;
pub mod error;
pub mod ibkr;
pub mod kraken;
pub mod paper;
pub(crate) mod rate_limiter;
pub mod traits;

pub use backfill::BackfillWorker;
pub use config::{IbkrConfig, KrakenFuturesConfig, KrakenSpotConfig, PaperExchangeConfig};
pub use error::ConnectivityError;
pub use ibkr::{
    contract_registry::IbkrContractRegistry,
    error::IbkrError,
    rest::IbkrRestClient,
    tws::{Connected, Disconnected, IbkrTws},
};
pub use kraken::{
    futures::{KrakenFuturesRestClient, KrakenFuturesWs},
    spot::{KrakenSpotRestClient, KrakenSpotWs},
};
pub use paper::PaperExchange;
pub use traits::{AccountProvider, MarketDataProvider, OrderExecutor, StreamProvider};
