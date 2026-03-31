pub mod config;
pub mod controller;
pub mod engine;
pub mod error;
pub mod kill_switch;
pub mod order_manager;
pub mod strategy;
pub mod traits;
pub mod types;

pub use config::{EngineConfig, MarginConfig, RiskConfig, ScheduleConfig, SmartOrderConfig};
pub use controller::PortfolioController;
pub use engine::Engine;
pub use error::EngineError;
pub use kill_switch::KillSwitch;
pub use order_manager::OrderManager;
pub use strategy::{NoopStrategy, Strategy, StrategyContext, StrategyKind};
pub use traits::LedgerWriter;
pub use types::{EngineEvent, OrderIntention, RiskDecision, StrategyId};
