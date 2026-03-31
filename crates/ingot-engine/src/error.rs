use ingot_accounting::AccountingError;
use ingot_primitives::{Amount, Currency, Percentage, Symbol};
use smol_str::SmolStr;

use crate::types::StrategyId;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("strategy not found: {0}")]
    StrategyNotFound(StrategyId),

    #[error("strategy ID cannot be empty")]
    EmptyStrategyId,

    #[error("duplicate strategy ID: {0}")]
    DuplicateStrategyId(StrategyId),

    #[error("risk check rejected: {reason}")]
    RiskRejected { reason: SmolStr },

    #[error("global stop-loss triggered: NAV {nav} below threshold {threshold}")]
    GlobalStopLoss { nav: Amount, threshold: Amount },

    #[error("exposure limit exceeded: {currency} at {current}, max {limit}")]
    ExposureLimitExceeded {
        currency: Currency,
        current: Percentage,
        limit: Percentage,
    },

    #[error("order manager error: {0}")]
    OrderManager(String),

    #[error("engine already running")]
    AlreadyRunning,

    #[error("engine not running")]
    NotRunning,

    #[error("kill switch activated")]
    KillSwitchActivated,

    #[error("smart order: order book empty for {0}")]
    EmptyOrderBook(Symbol),

    #[error("schedule interval must be greater than zero")]
    InvalidScheduleInterval,

    #[error("margin call: excess liquidity depleted")]
    MarginCall,

    #[error("margin utilization {utilization} exceeds maximum {limit}")]
    MarginUtilizationExceeded {
        utilization: Percentage,
        limit: Percentage,
    },

    #[error("excess liquidity {available} below minimum {required}")]
    InsufficientExcessLiquidity { available: Amount, required: Amount },

    #[error("rollover: no far month contract found for {near_symbol}")]
    RolloverFarMonthNotFound { near_symbol: Symbol },

    #[error("rollover: {active} active rollovers exceeds maximum {max}")]
    RolloverLimitExceeded { active: usize, max: usize },

    #[error("connectivity error: {0}")]
    Connectivity(#[source] anyhow::Error),

    #[error("accounting error: {0}")]
    Accounting(#[from] AccountingError),
}

#[cfg(test)]
mod tests {
    use ingot_primitives::{Amount, Currency, Percentage, Symbol};
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;

    #[test]
    fn test_engine_error_display_strategy_not_found() -> Result<(), EngineError> {
        let id = StrategyId::new("alpha")?;
        let err = EngineError::StrategyNotFound(id);
        assert_eq!(err.to_string(), "strategy not found: alpha");
        Ok(())
    }

    #[test]
    fn test_engine_error_display_empty_strategy_id() {
        let err = EngineError::EmptyStrategyId;
        assert_eq!(err.to_string(), "strategy ID cannot be empty");
    }

    #[test]
    fn test_engine_error_display_duplicate_strategy_id() -> Result<(), EngineError> {
        let id = StrategyId::new("beta")?;
        let err = EngineError::DuplicateStrategyId(id);
        assert_eq!(err.to_string(), "duplicate strategy ID: beta");
        Ok(())
    }

    #[test]
    fn test_engine_error_display_risk_rejected() {
        let err = EngineError::RiskRejected {
            reason: SmolStr::new("exceeds limit"),
        };
        assert_eq!(err.to_string(), "risk check rejected: exceeds limit");
    }

    #[test]
    fn test_engine_error_display_global_stop_loss() {
        let err = EngineError::GlobalStopLoss {
            nav: Amount::new(dec!(8000)),
            threshold: Amount::new(dec!(10000)),
        };
        assert_eq!(
            err.to_string(),
            "global stop-loss triggered: NAV 8000 below threshold 10000"
        );
    }

    #[test]
    fn test_engine_error_display_exposure_limit() -> Result<(), Box<dyn std::error::Error>> {
        let err = EngineError::ExposureLimitExceeded {
            currency: Currency::BTC,
            current: Percentage::new(dec!(0.45))?,
            limit: Percentage::new(dec!(0.30))?,
        };
        assert_eq!(
            err.to_string(),
            "exposure limit exceeded: BTC at 45.00%, max 30.00%"
        );
        Ok(())
    }

    #[test]
    fn test_engine_error_display_order_manager() {
        let err = EngineError::OrderManager("timeout".into());
        assert_eq!(err.to_string(), "order manager error: timeout");
    }

    #[test]
    fn test_engine_error_display_already_running() {
        let err = EngineError::AlreadyRunning;
        assert_eq!(err.to_string(), "engine already running");
    }

    #[test]
    fn test_engine_error_display_not_running() {
        let err = EngineError::NotRunning;
        assert_eq!(err.to_string(), "engine not running");
    }

    #[test]
    fn test_engine_error_display_kill_switch() {
        let err = EngineError::KillSwitchActivated;
        assert_eq!(err.to_string(), "kill switch activated");
    }

    #[test]
    fn test_engine_error_display_empty_order_book() -> Result<(), Box<dyn std::error::Error>> {
        let err = EngineError::EmptyOrderBook(Symbol::new("XXBTZUSD")?);
        assert_eq!(
            err.to_string(),
            "smart order: order book empty for XXBTZUSD"
        );
        Ok(())
    }

    #[test]
    fn test_engine_error_display_invalid_schedule_interval() {
        let err = EngineError::InvalidScheduleInterval;
        assert_eq!(
            err.to_string(),
            "schedule interval must be greater than zero"
        );
    }

    #[test]
    fn test_engine_error_display_margin_call() {
        let err = EngineError::MarginCall;
        assert_eq!(err.to_string(), "margin call: excess liquidity depleted");
    }

    #[test]
    fn test_engine_error_display_margin_utilization_exceeded()
    -> Result<(), Box<dyn std::error::Error>> {
        let err = EngineError::MarginUtilizationExceeded {
            utilization: Percentage::new(dec!(0.85))?,
            limit: Percentage::new(dec!(0.80))?,
        };
        assert_eq!(
            err.to_string(),
            "margin utilization 85.00% exceeds maximum 80.00%"
        );
        Ok(())
    }

    #[test]
    fn test_engine_error_display_insufficient_excess_liquidity() {
        let err = EngineError::InsufficientExcessLiquidity {
            available: Amount::new(dec!(3000)),
            required: Amount::new(dec!(10000)),
        };
        assert_eq!(err.to_string(), "excess liquidity 3000 below minimum 10000");
    }

    #[test]
    fn test_engine_error_display_rollover_far_month_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let err = EngineError::RolloverFarMonthNotFound {
            near_symbol: Symbol::new("ESM26")?,
        };
        assert_eq!(
            err.to_string(),
            "rollover: no far month contract found for ESM26"
        );
        Ok(())
    }

    #[test]
    fn test_engine_error_display_rollover_limit_exceeded() {
        let err = EngineError::RolloverLimitExceeded { active: 6, max: 5 };
        assert_eq!(
            err.to_string(),
            "rollover: 6 active rollovers exceeds maximum 5"
        );
    }

    #[test]
    fn test_engine_error_display_connectivity() {
        let err = EngineError::Connectivity(anyhow::anyhow!("connection lost"));
        assert_eq!(err.to_string(), "connectivity error: connection lost");
    }

    #[test]
    fn test_engine_error_from_accounting() {
        let accounting_err = AccountingError::InvalidAccount {
            reason: "bad venue".into(),
        };
        let engine_err: EngineError = accounting_err.into();
        assert_eq!(
            engine_err.to_string(),
            "accounting error: invalid account: bad venue"
        );
    }
}
