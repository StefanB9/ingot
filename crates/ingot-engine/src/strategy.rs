use ingot_core::{
    accounting::{ExchangeRate, QuoteBoard},
    execution::OrderRequest,
    feed::Ticker,
};
use ingot_primitives::{Currency, Price};
use smallvec::SmallVec;
use tracing::{error, info, warn};

/// Mutable context passed to each strategy during tick dispatch.
///
/// Strategies can update the shared `market_data` board and emit orders
/// into `orders`. The engine processes emitted orders after all strategies
/// have run, outside the `QuoteBoard` write lock.
pub struct StrategyContext<'a> {
    pub market_data: &'a mut QuoteBoard,
    pub orders: &'a mut SmallVec<OrderRequest, 2>,
}

/// A synchronous tick handler plugged into the engine's event loop.
///
/// Strategies run inside the engine's async loop but must not block or
/// allocate on the hot path. Background I/O should go through channels,
/// not async in this trait.
pub trait Strategy: Send + Sync {
    /// Human-readable name for logging / diagnostics.
    fn name(&self) -> &'static str;

    /// Called once per market-data tick. Strategies may mutate the
    /// `QuoteBoard` and push `OrderRequest`s into `ctx.orders`.
    fn on_tick(&mut self, tick: &Ticker, ctx: &mut StrategyContext<'_>);
}

/// Updates the `QuoteBoard` with exchange rates derived from each tick.
///
/// This is the first strategy in the chain — subsequent strategies see
/// the freshly-updated rates.
pub struct QuoteBoardStrategy;

impl Strategy for QuoteBoardStrategy {
    fn name(&self) -> &'static str {
        "QuoteBoardUpdater"
    }

    fn on_tick(&mut self, tick: &Ticker, ctx: &mut StrategyContext<'_>) {
        let Some((base_code, quote_code)) = tick.symbol.parts() else {
            warn!(symbol = %tick.symbol, "ignored tick: invalid symbol format");
            return;
        };

        let base = Currency {
            code: base_code,
            decimals: 8,
        };
        let quote = Currency {
            code: quote_code,
            decimals: 2,
        };

        match ExchangeRate::new(base, quote, Price::from(tick.price)) {
            Ok(rate) => {
                ctx.market_data.add_rate(rate);
                info!(symbol = %tick.symbol, price = %tick.price, "quoteboard updated");
            }
            Err(e) => error!(error = %e, "failed to create exchange rate from tick"),
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use ingot_core::accounting::QuoteBoard;
    use ingot_primitives::{Amount, Currency, Money};
    use rust_decimal::dec;
    use smallvec::SmallVec;

    use super::*;

    /// Minimal strategy used only in tests.
    struct StubStrategy;

    impl Strategy for StubStrategy {
        fn name(&self) -> &'static str {
            "Stub"
        }

        fn on_tick(&mut self, _tick: &Ticker, _ctx: &mut StrategyContext<'_>) {}
    }

    #[test]
    fn test_strategy_context_creation() {
        let mut board = QuoteBoard::new();
        let mut orders = SmallVec::new();
        let ctx = StrategyContext {
            market_data: &mut board,
            orders: &mut orders,
        };

        assert!(ctx.orders.is_empty());
    }

    #[test]
    fn test_strategy_trait_object_safety() {
        let mut strategy: Box<dyn Strategy> = Box::new(StubStrategy);

        assert_eq!(strategy.name(), "Stub");

        let mut board = QuoteBoard::new();
        let mut orders = SmallVec::new();
        let mut ctx = StrategyContext {
            market_data: &mut board,
            orders: &mut orders,
        };

        let tick = Ticker::new("BTC-USD", dec!(50000));
        strategy.on_tick(&tick, &mut ctx);
    }

    // --- QuoteBoardStrategy tests ---

    #[test]
    fn test_quote_board_strategy_name() {
        let strategy = QuoteBoardStrategy;
        assert_eq!(strategy.name(), "QuoteBoardUpdater");
    }

    #[test]
    fn test_quote_board_strategy_updates_board_on_valid_tick() -> Result<()> {
        let mut strategy = QuoteBoardStrategy;
        let mut board = QuoteBoard::new();
        let mut orders = SmallVec::new();
        let mut ctx = StrategyContext {
            market_data: &mut board,
            orders: &mut orders,
        };

        let tick = Ticker::new("BTC-USD", dec!(60000));
        strategy.on_tick(&tick, &mut ctx);

        // Verify the rate was added by converting 1 BTC → USD
        let btc = Currency::btc();
        let usd = Currency::usd();
        let one_btc = Money::new(Amount::from(dec!(1)), btc);
        let result = ctx.market_data.convert(&one_btc, &usd)?;
        assert_eq!(result.amount, Amount::from(dec!(60000)));
        Ok(())
    }

    #[test]
    fn test_quote_board_strategy_ignores_invalid_symbol() {
        let mut strategy = QuoteBoardStrategy;
        let mut board = QuoteBoard::new();
        let mut orders = SmallVec::new();
        let mut ctx = StrategyContext {
            market_data: &mut board,
            orders: &mut orders,
        };

        // Symbol without a separator — parts() returns None
        let tick = Ticker::new("INVALID", dec!(100));
        strategy.on_tick(&tick, &mut ctx);

        // Board should remain empty — convert should fail
        let btc = Currency::btc();
        let usd = Currency::usd();
        let money = Money::new(Amount::from(dec!(1)), btc);
        assert!(ctx.market_data.convert(&money, &usd).is_err());
    }

    #[test]
    fn test_quote_board_strategy_emits_no_orders() {
        let mut strategy = QuoteBoardStrategy;
        let mut board = QuoteBoard::new();
        let mut orders = SmallVec::new();
        let mut ctx = StrategyContext {
            market_data: &mut board,
            orders: &mut orders,
        };

        let tick = Ticker::new("BTC-USD", dec!(50000));
        strategy.on_tick(&tick, &mut ctx);

        assert!(ctx.orders.is_empty());
    }
}
