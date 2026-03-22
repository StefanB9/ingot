pub mod config;
pub(crate) mod controller;
pub mod engine;
pub mod error;
pub mod kill_switch;
pub(crate) mod order_manager;
pub mod strategy;
pub mod traits;
pub mod types;

pub use config::{EngineConfig, RiskConfig, ScheduleConfig, SmartOrderConfig};
pub use engine::Engine;
pub use error::EngineError;
pub use kill_switch::KillSwitch;
pub use strategy::{NoopStrategy, Strategy, StrategyContext, StrategyKind};
pub use traits::LedgerWriter;
pub use types::{EngineEvent, OrderIntention, RiskDecision, StrategyId};
