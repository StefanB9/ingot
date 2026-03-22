use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ingot_core::{Balance, OrderBookSnapshot, OrderFill, Position, TickerSnapshot};
use ingot_primitives::Symbol;

use crate::types::{OrderIntention, StrategyId};

/// Read-only snapshot of engine state available to strategies.
#[derive(Debug, Clone)]
pub struct StrategyContext {
    pub positions: Vec<Position>,
    pub balances: Vec<Balance>,
    pub latest_tickers: HashMap<Symbol, TickerSnapshot>,
    pub latest_order_books: HashMap<Symbol, OrderBookSnapshot>,
    pub timestamp: DateTime<Utc>,
}

impl StrategyContext {
    /// Look up the most recent ticker for a given symbol.
    pub fn latest_ticker(&self, symbol: &Symbol) -> Option<&TickerSnapshot> {
        self.latest_tickers.get(symbol)
    }

    /// Look up the most recent order book for a given symbol.
    pub fn latest_order_book(&self, symbol: &Symbol) -> Option<&OrderBookSnapshot> {
        self.latest_order_books.get(symbol)
    }
}

/// Core strategy interface. Strategies emit `OrderIntention`s;
/// the engine evaluates and routes them.
pub trait Strategy {
    fn id(&self) -> &StrategyId;
    fn init(&mut self, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_ticker(&mut self, ticker: &TickerSnapshot, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_order_book(
        &mut self,
        book: &OrderBookSnapshot,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntention>;
    fn on_fill(&mut self, fill: &OrderFill, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn on_schedule(&mut self, ctx: &StrategyContext) -> Vec<OrderIntention>;
    fn shutdown(&mut self);
}

/// A strategy that does nothing. Used for testing and as a baseline.
#[derive(Debug, Clone)]
pub struct NoopStrategy {
    id: StrategyId,
}

impl NoopStrategy {
    pub fn new(id: StrategyId) -> Self {
        Self { id }
    }
}

impl Strategy for NoopStrategy {
    fn id(&self) -> &StrategyId {
        &self.id
    }

    fn init(&mut self, _ctx: &StrategyContext) -> Vec<OrderIntention> {
        Vec::new()
    }

    fn on_ticker(
        &mut self,
        _ticker: &TickerSnapshot,
        _ctx: &StrategyContext,
    ) -> Vec<OrderIntention> {
        Vec::new()
    }

    fn on_order_book(
        &mut self,
        _book: &OrderBookSnapshot,
        _ctx: &StrategyContext,
    ) -> Vec<OrderIntention> {
        Vec::new()
    }

    fn on_fill(&mut self, _fill: &OrderFill, _ctx: &StrategyContext) -> Vec<OrderIntention> {
        Vec::new()
    }

    fn on_schedule(&mut self, _ctx: &StrategyContext) -> Vec<OrderIntention> {
        Vec::new()
    }

    fn shutdown(&mut self) {}
}

/// Enum dispatch wrapper for concrete strategy implementations.
/// New strategy variants are added here; binary restart required.
#[derive(Debug)]
pub enum StrategyKind {
    Noop(NoopStrategy),
    #[cfg(test)]
    #[allow(private_interfaces)]
    Mock(MockStrategy),
}

impl Strategy for StrategyKind {
    fn id(&self) -> &StrategyId {
        match self {
            Self::Noop(s) => s.id(),
            #[cfg(test)]
            Self::Mock(s) => s.id(),
        }
    }

    fn init(&mut self, ctx: &StrategyContext) -> Vec<OrderIntention> {
        match self {
            Self::Noop(s) => s.init(ctx),
            #[cfg(test)]
            Self::Mock(s) => s.init(ctx),
        }
    }

    fn on_ticker(&mut self, ticker: &TickerSnapshot, ctx: &StrategyContext) -> Vec<OrderIntention> {
        match self {
            Self::Noop(s) => s.on_ticker(ticker, ctx),
            #[cfg(test)]
            Self::Mock(s) => s.on_ticker(ticker, ctx),
        }
    }

    fn on_order_book(
        &mut self,
        book: &OrderBookSnapshot,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntention> {
        match self {
            Self::Noop(s) => s.on_order_book(book, ctx),
            #[cfg(test)]
            Self::Mock(s) => s.on_order_book(book, ctx),
        }
    }

    fn on_fill(&mut self, fill: &OrderFill, ctx: &StrategyContext) -> Vec<OrderIntention> {
        match self {
            Self::Noop(s) => s.on_fill(fill, ctx),
            #[cfg(test)]
            Self::Mock(s) => s.on_fill(fill, ctx),
        }
    }

    fn on_schedule(&mut self, ctx: &StrategyContext) -> Vec<OrderIntention> {
        match self {
            Self::Noop(s) => s.on_schedule(ctx),
            #[cfg(test)]
            Self::Mock(s) => s.on_schedule(ctx),
        }
    }

    fn shutdown(&mut self) {
        match self {
            Self::Noop(s) => s.shutdown(),
            #[cfg(test)]
            Self::Mock(s) => s.shutdown(),
        }
    }
}

/// Shared state for `MockStrategy`, accessible from both the strategy and test
/// code.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct MockStrategyState {
    pub on_ticker_count: usize,
    pub on_fill_count: usize,
    pub on_order_book_count: usize,
    pub shutdown_called: bool,
}

/// A test-only strategy that records calls and optionally emits configured
/// intentions.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct MockStrategy {
    id: StrategyId,
    state: std::sync::Arc<std::sync::Mutex<MockStrategyState>>,
    ticker_intentions: Vec<OrderIntention>,
    fill_intentions: Vec<OrderIntention>,
}

#[cfg(test)]
impl MockStrategy {
    pub fn new(id: StrategyId, state: std::sync::Arc<std::sync::Mutex<MockStrategyState>>) -> Self {
        Self {
            id,
            state,
            ticker_intentions: Vec::new(),
            fill_intentions: Vec::new(),
        }
    }

    pub fn with_ticker_intentions(mut self, intentions: Vec<OrderIntention>) -> Self {
        self.ticker_intentions = intentions;
        self
    }

    #[allow(dead_code)] // Used by future engine tests (1d.6+)
    pub fn with_fill_intentions(mut self, intentions: Vec<OrderIntention>) -> Self {
        self.fill_intentions = intentions;
        self
    }
}

#[cfg(test)]
impl Strategy for MockStrategy {
    fn id(&self) -> &StrategyId {
        &self.id
    }

    fn init(&mut self, _ctx: &StrategyContext) -> Vec<OrderIntention> {
        Vec::new()
    }

    fn on_ticker(
        &mut self,
        _ticker: &TickerSnapshot,
        _ctx: &StrategyContext,
    ) -> Vec<OrderIntention> {
        if let Ok(mut s) = self.state.lock() {
            s.on_ticker_count += 1;
        }
        self.ticker_intentions.clone()
    }

    fn on_order_book(
        &mut self,
        _book: &OrderBookSnapshot,
        _ctx: &StrategyContext,
    ) -> Vec<OrderIntention> {
        if let Ok(mut s) = self.state.lock() {
            s.on_order_book_count += 1;
        }
        Vec::new()
    }

    fn on_fill(&mut self, _fill: &OrderFill, _ctx: &StrategyContext) -> Vec<OrderIntention> {
        if let Ok(mut s) = self.state.lock() {
            s.on_fill_count += 1;
        }
        self.fill_intentions.clone()
    }

    fn on_schedule(&mut self, _ctx: &StrategyContext) -> Vec<OrderIntention> {
        Vec::new()
    }

    fn shutdown(&mut self) {
        if let Ok(mut s) = self.state.lock() {
            s.shutdown_called = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use ingot_core::{OrderBookSnapshot, OrderFill, OrderId, TickerSnapshot};
    use ingot_primitives::{Amount, Currency, OrderSide, Price, Quantity, Symbol};
    use rust_decimal_macros::dec;

    use super::*;
    use crate::error::EngineError;

    fn sample_context() -> Result<StrategyContext, Box<dyn std::error::Error>> {
        let symbol = Symbol::new("XXBTZUSD")?;
        let ticker = TickerSnapshot {
            symbol: symbol.clone(),
            bid: Price::new(dec!(67000)),
            ask: Price::new(dec!(67010)),
            last: Price::new(dec!(67005)),
            volume_24h: Quantity::new(dec!(1234))?,
            timestamp: Utc::now(),
        };
        let mut latest_tickers = HashMap::new();
        latest_tickers.insert(symbol.clone(), ticker);

        Ok(StrategyContext {
            positions: vec![ingot_core::Position {
                symbol,
                side: OrderSide::Buy,
                quantity: Quantity::new(dec!(0.5))?,
                average_entry_price: Price::new(dec!(67000)),
                unrealized_pnl: Some(Amount::new(dec!(150))),
                liquidation_price: None,
            }],
            balances: vec![ingot_core::Balance {
                currency: Currency::USD,
                total: Amount::new(dec!(10000)),
                available: Amount::new(dec!(8000)),
                held: Amount::new(dec!(2000)),
            }],
            latest_tickers,
            latest_order_books: HashMap::new(),
            timestamp: Utc::now(),
        })
    }

    // ── StrategyContext ────────────────────────────────────────────────

    #[test]
    fn test_strategy_context_construction() -> Result<(), Box<dyn std::error::Error>> {
        let ctx = sample_context()?;
        assert_eq!(ctx.positions.len(), 1);
        assert_eq!(ctx.balances.len(), 1);
        assert_eq!(ctx.latest_tickers.len(), 1);
        assert!(ctx.latest_order_books.is_empty());
        Ok(())
    }

    #[test]
    fn test_strategy_context_latest_ticker_lookup() -> Result<(), Box<dyn std::error::Error>> {
        let ctx = sample_context()?;
        let btc = Symbol::new("XXBTZUSD")?;
        let eth = Symbol::new("XETHZUSD")?;

        let found = ctx.latest_ticker(&btc);
        assert!(found.is_some());
        assert_eq!(found.map(|t| &t.symbol), Some(&btc));

        let not_found = ctx.latest_ticker(&eth);
        assert!(not_found.is_none());
        Ok(())
    }

    // ── NoopStrategy ───────────────────────────────────────────────────

    #[test]
    fn test_noop_strategy_id() -> Result<(), EngineError> {
        let id = StrategyId::new("noop-test")?;
        let strategy = NoopStrategy::new(id.clone());
        assert_eq!(strategy.id(), &id);
        Ok(())
    }

    #[test]
    fn test_noop_strategy_init_empty() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("noop")?;
        let mut strategy = NoopStrategy::new(id);
        let ctx = sample_context()?;
        let intentions = strategy.init(&ctx);
        assert!(intentions.is_empty());
        Ok(())
    }

    #[test]
    fn test_noop_strategy_on_ticker_empty() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("noop")?;
        let mut strategy = NoopStrategy::new(id);
        let ctx = sample_context()?;
        let ticker = TickerSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bid: Price::new(dec!(67000)),
            ask: Price::new(dec!(67010)),
            last: Price::new(dec!(67005)),
            volume_24h: Quantity::new(dec!(1234))?,
            timestamp: Utc::now(),
        };
        let intentions = strategy.on_ticker(&ticker, &ctx);
        assert!(intentions.is_empty());
        Ok(())
    }

    #[test]
    fn test_noop_strategy_on_order_book_empty() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("noop")?;
        let mut strategy = NoopStrategy::new(id);
        let ctx = sample_context()?;
        let book = OrderBookSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bids: vec![],
            asks: vec![],
            timestamp: Utc::now(),
        };
        let intentions = strategy.on_order_book(&book, &ctx);
        assert!(intentions.is_empty());
        Ok(())
    }

    #[test]
    fn test_noop_strategy_on_fill_empty() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("noop")?;
        let mut strategy = NoopStrategy::new(id);
        let ctx = sample_context()?;
        let fill = OrderFill {
            order_id: OrderId::new("ORD-001")?,
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67000)),
            fill_quantity: Quantity::new(dec!(1))?,
            fee: Amount::new(dec!(0.26)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };
        let intentions = strategy.on_fill(&fill, &ctx);
        assert!(intentions.is_empty());
        Ok(())
    }

    #[test]
    fn test_noop_strategy_on_schedule_empty() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("noop")?;
        let mut strategy = NoopStrategy::new(id);
        let ctx = sample_context()?;
        let intentions = strategy.on_schedule(&ctx);
        assert!(intentions.is_empty());
        Ok(())
    }

    // ── StrategyKind ───────────────────────────────────────────────────

    #[test]
    fn test_strategy_kind_delegates_to_noop() -> Result<(), Box<dyn std::error::Error>> {
        let id = StrategyId::new("noop-kind")?;
        let mut kind = StrategyKind::Noop(NoopStrategy::new(id.clone()));

        // id() delegates
        assert_eq!(kind.id(), &id);

        let ctx = sample_context()?;

        // init delegates
        assert!(kind.init(&ctx).is_empty());

        // on_ticker delegates
        let ticker = TickerSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bid: Price::new(dec!(67000)),
            ask: Price::new(dec!(67010)),
            last: Price::new(dec!(67005)),
            volume_24h: Quantity::new(dec!(1234))?,
            timestamp: Utc::now(),
        };
        assert!(kind.on_ticker(&ticker, &ctx).is_empty());

        // on_order_book delegates
        let book = OrderBookSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bids: vec![],
            asks: vec![],
            timestamp: Utc::now(),
        };
        assert!(kind.on_order_book(&book, &ctx).is_empty());

        // on_fill delegates
        let fill = OrderFill {
            order_id: OrderId::new("ORD-001")?,
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67000)),
            fill_quantity: Quantity::new(dec!(1))?,
            fee: Amount::new(dec!(0.26)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };
        assert!(kind.on_fill(&fill, &ctx).is_empty());

        // on_schedule delegates
        assert!(kind.on_schedule(&ctx).is_empty());

        // shutdown delegates (no panic = success)
        kind.shutdown();

        Ok(())
    }
}
