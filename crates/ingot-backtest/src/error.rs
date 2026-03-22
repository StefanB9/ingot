use ingot_accounting::AccountingError;
use ingot_engine::EngineError;

#[derive(Debug, thiserror::Error)]
pub enum BacktestError {
    #[error("no data provided")]
    NoData,

    #[error("no strategies registered")]
    NoStrategies,

    #[error("data is not sorted by time")]
    UnsortedData,

    #[error("engine error: {0}")]
    Engine(#[from] EngineError),

    #[error("accounting error: {0}")]
    Accounting(#[from] AccountingError),

    #[error("backtest exchange error: {0}")]
    Exchange(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backtest_error_display() {
        assert_eq!(BacktestError::NoData.to_string(), "no data provided");
        assert_eq!(
            BacktestError::NoStrategies.to_string(),
            "no strategies registered"
        );
        assert_eq!(
            BacktestError::UnsortedData.to_string(),
            "data is not sorted by time"
        );
        assert_eq!(
            BacktestError::Exchange("timeout".into()).to_string(),
            "backtest exchange error: timeout"
        );
    }

    #[test]
    fn test_backtest_error_from_engine() {
        let engine_err = EngineError::EmptyStrategyId;
        let backtest_err: BacktestError = engine_err.into();
        assert_eq!(
            backtest_err.to_string(),
            "engine error: strategy ID cannot be empty"
        );
        assert!(matches!(backtest_err, BacktestError::Engine(_)));
    }

    #[test]
    fn test_backtest_error_from_accounting() {
        let accounting_err = AccountingError::InvalidAmount {
            reason: "negative".into(),
        };
        let backtest_err: BacktestError = accounting_err.into();
        assert!(
            backtest_err
                .to_string()
                .contains("accounting error: invalid amount")
        );
        assert!(matches!(backtest_err, BacktestError::Accounting(_)));
    }
}
