use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ingot_connectivity::OrderExecutor;
use ingot_core::{OrderBookSnapshot, OrderFill, OrderId, OrderStatus};
use ingot_primitives::{OrderSide, OrderType, Price, TimeInForce};
use rust_decimal::Decimal;

use crate::{config::SmartOrderConfig, error::EngineError, types::OrderIntention};

/// An order being tracked by the manager.
#[derive(Debug, Clone)]
pub struct TrackedOrder {
    pub order_id: OrderId,
    pub intention: OrderIntention,
    pub status: OrderStatus,
    pub submitted_at: DateTime<Utc>,
}

/// Bridges approved `OrderIntention`s to the broker, tracks order lifecycle,
/// and computes smart limit prices from the order book.
pub struct OrderManager {
    tracked_orders: HashMap<OrderId, TrackedOrder>,
    smart_config: SmartOrderConfig,
}

impl OrderManager {
    pub fn new(config: SmartOrderConfig) -> Self {
        Self {
            tracked_orders: HashMap::new(),
            smart_config: config,
        }
    }

    /// Compute a smart limit price from the order book mid-price + offset.
    ///
    /// Returns `EngineError::EmptyOrderBook` if bids or asks are empty.
    pub fn compute_limit_price(
        &self,
        side: OrderSide,
        book: &OrderBookSnapshot,
    ) -> Result<Price, EngineError> {
        if book.bids.is_empty() || book.asks.is_empty() {
            return Err(EngineError::EmptyOrderBook(book.symbol.clone()));
        }

        let best_bid = book.bids[0].price.value();
        let best_ask = book.asks[0].price.value();
        let mid = (best_bid + best_ask) / Decimal::TWO;
        let offset = mid * self.smart_config.offset_bps / Decimal::from(10_000);

        let price = match side {
            OrderSide::Buy => mid + offset,
            OrderSide::Sell => mid - offset,
        };

        Ok(Price::new(price))
    }

    /// Submit an order to the broker. If smart pricing is enabled and the
    /// intention is a Market order with an available book, converts to Limit
    /// with computed mid-price.
    pub async fn submit_order<E: OrderExecutor>(
        &mut self,
        intention: OrderIntention,
        book: Option<&OrderBookSnapshot>,
        executor: &E,
    ) -> Result<OrderId, EngineError> {
        let mut request = intention.request.clone();

        // Smart limit conversion: Market → Limit with mid-price
        if let Some(b) = book
            .filter(|_| self.smart_config.use_mid_price && request.order_type == OrderType::Market)
        {
            let smart_price = self.compute_limit_price(request.side, b)?;
            request.order_type = OrderType::Limit;
            request.limit_price = Some(smart_price);
            request.time_in_force = TimeInForce::GoodTilCancelled;
        }

        let order_id = executor
            .place_order(&request)
            .await
            .map_err(EngineError::Connectivity)?;

        self.tracked_orders.insert(
            order_id.clone(),
            TrackedOrder {
                order_id: order_id.clone(),
                intention,
                status: OrderStatus::Pending,
                submitted_at: Utc::now(),
            },
        );

        Ok(order_id)
    }

    /// Process a fill event. Returns the tracked order if it was being tracked.
    pub fn on_fill(&mut self, fill: &OrderFill) -> Option<&TrackedOrder> {
        if let Some(tracked) = self.tracked_orders.get_mut(&fill.order_id) {
            tracked.status = OrderStatus::Filled;
            Some(tracked)
        } else {
            None
        }
    }

    /// Cancel all tracked orders via the executor.
    pub async fn cancel_all<E: OrderExecutor>(&mut self, executor: &E) -> Result<u32, EngineError> {
        let count = executor
            .cancel_all_orders()
            .await
            .map_err(EngineError::Connectivity)?;

        for tracked in self.tracked_orders.values_mut() {
            if tracked.status == OrderStatus::Pending || tracked.status == OrderStatus::Open {
                tracked.status = OrderStatus::Cancelled;
            }
        }

        Ok(count)
    }

    /// Identify orders that have been pending/open longer than
    /// `fallback_timeout_ms`.
    pub fn stale_orders(&self, now: DateTime<Utc>) -> Vec<OrderId> {
        let timeout_ms =
            i64::from(u32::try_from(self.smart_config.fallback_timeout_ms).unwrap_or(u32::MAX));

        self.tracked_orders
            .values()
            .filter(|t| t.status == OrderStatus::Pending || t.status == OrderStatus::Open)
            .filter(|t| {
                let elapsed = now.signed_duration_since(t.submitted_at).num_milliseconds();
                elapsed > timeout_ms
            })
            .map(|t| t.order_id.clone())
            .collect()
    }

    /// Test helper: insert a tracked order with controlled `submitted_at`.
    #[cfg(test)]
    fn insert_tracked_for_test(&mut self, tracked: TrackedOrder) {
        self.tracked_orders
            .insert(tracked.order_id.clone(), tracked);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::{TimeDelta, Utc};
    use ingot_connectivity::OrderExecutor;
    use ingot_core::{
        OpenOrder, OrderBookLevel, OrderBookSnapshot, OrderFill, OrderId, OrderRequest, OrderStatus,
    };
    use ingot_primitives::{
        Amount, Currency, OrderSide, OrderType, Price, Quantity, Symbol, TimeInForce,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::types::StrategyId;

    // ── Mock executor ──────────────────────────────────────────────────

    #[derive(Debug, Clone)]
    struct MockOrderExecutor {
        placed: Arc<Mutex<Vec<OrderRequest>>>,
        cancel_all_count: Arc<Mutex<u32>>,
        next_order_id: String,
    }

    impl MockOrderExecutor {
        fn new(next_id: &str) -> Self {
            Self {
                placed: Arc::new(Mutex::new(Vec::new())),
                cancel_all_count: Arc::new(Mutex::new(0)),
                next_order_id: next_id.to_owned(),
            }
        }

        fn placed_orders(&self) -> Result<Vec<OrderRequest>, anyhow::Error> {
            let guard = self
                .placed
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            Ok(guard.clone())
        }

        fn cancel_all_called(&self) -> Result<u32, anyhow::Error> {
            let guard = self
                .cancel_all_count
                .lock()
                .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
            Ok(*guard)
        }
    }

    impl OrderExecutor for MockOrderExecutor {
        fn place_order(
            &self,
            request: &OrderRequest,
        ) -> impl Future<Output = anyhow::Result<OrderId>> + Send {
            let placed = Arc::clone(&self.placed);
            let request = request.clone();
            let id = self.next_order_id.clone();
            async move {
                let mut guard = placed
                    .lock()
                    .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
                guard.push(request);
                OrderId::new(&id).map_err(|e| anyhow::anyhow!("{e}"))
            }
        }

        async fn cancel_order(&self, _order_id: &OrderId) -> anyhow::Result<()> {
            Ok(())
        }

        fn cancel_all_orders(&self) -> impl Future<Output = anyhow::Result<u32>> + Send {
            let count = Arc::clone(&self.cancel_all_count);
            async move {
                let mut guard = count
                    .lock()
                    .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
                *guard += 1;
                Ok(2) // pretend 2 orders cancelled
            }
        }

        fn get_order_status(
            &self,
            order_id: &OrderId,
        ) -> impl Future<Output = anyhow::Result<OpenOrder>> + Send {
            let id = order_id.clone();
            async move {
                Ok(OpenOrder {
                    order_id: id,
                    request: OrderRequest {
                        symbol: Symbol::new("XXBTZUSD").map_err(|e| anyhow::anyhow!("{e}"))?,
                        side: OrderSide::Buy,
                        order_type: OrderType::Market,
                        quantity: Quantity::new(dec!(0.1)).map_err(|e| anyhow::anyhow!("{e}"))?,
                        limit_price: None,
                        stop_price: None,
                        time_in_force: TimeInForce::GoodTilCancelled,
                    },
                    status: OrderStatus::Open,
                    filled_quantity: Quantity::zero(),
                    remaining_quantity: Quantity::new(dec!(0.1))
                        .map_err(|e| anyhow::anyhow!("{e}"))?,
                    average_fill_price: None,
                    created_at: Utc::now(),
                })
            }
        }

        async fn get_open_orders(&self) -> anyhow::Result<Vec<OpenOrder>> {
            Ok(vec![])
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    fn sample_smart_config() -> SmartOrderConfig {
        SmartOrderConfig {
            use_mid_price: true,
            offset_bps: dec!(0),
            fallback_timeout_ms: 30_000,
        }
    }

    fn sample_book() -> Result<OrderBookSnapshot, Box<dyn std::error::Error>> {
        Ok(OrderBookSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bids: vec![OrderBookLevel {
                price: Price::new(dec!(67_000)),
                quantity: Quantity::new(dec!(1))?,
            }],
            asks: vec![OrderBookLevel {
                price: Price::new(dec!(67_010)),
                quantity: Quantity::new(dec!(1))?,
            }],
            timestamp: Utc::now(),
        })
    }

    fn sample_market_buy_intention() -> Result<OrderIntention, Box<dyn std::error::Error>> {
        Ok(OrderIntention {
            strategy_id: StrategyId::new("test")?,
            request: OrderRequest {
                symbol: Symbol::new("XXBTZUSD")?,
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: Quantity::new(dec!(0.1))?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::ImmediateOrCancel,
            },
            reason: None,
        })
    }

    // ── compute_limit_price tests ──────────────────────────────────────

    #[test]
    fn test_compute_limit_price_buy_mid() -> Result<(), Box<dyn std::error::Error>> {
        let manager = OrderManager::new(sample_smart_config());
        let book = sample_book()?;
        let price = manager.compute_limit_price(OrderSide::Buy, &book)?;
        // (67_000 + 67_010) / 2 = 67_005
        assert_eq!(price, Price::new(dec!(67_005)));
        Ok(())
    }

    #[test]
    fn test_compute_limit_price_sell_mid() -> Result<(), Box<dyn std::error::Error>> {
        let manager = OrderManager::new(sample_smart_config());
        let book = sample_book()?;
        let price = manager.compute_limit_price(OrderSide::Sell, &book)?;
        assert_eq!(price, Price::new(dec!(67_005)));
        Ok(())
    }

    #[test]
    fn test_compute_limit_price_with_offset() -> Result<(), Box<dyn std::error::Error>> {
        let config = SmartOrderConfig {
            use_mid_price: true,
            offset_bps: dec!(10),
            fallback_timeout_ms: 30_000,
        };
        let manager = OrderManager::new(config);
        let book = sample_book()?;

        // mid = 67_005, offset = 67_005 * 10 / 10_000 = 67.005
        let buy_price = manager.compute_limit_price(OrderSide::Buy, &book)?;
        assert_eq!(buy_price, Price::new(dec!(67_072.005)));

        let sell_price = manager.compute_limit_price(OrderSide::Sell, &book)?;
        assert_eq!(sell_price, Price::new(dec!(66_937.995)));
        Ok(())
    }

    #[test]
    fn test_compute_limit_price_empty_book_error() -> Result<(), Box<dyn std::error::Error>> {
        let manager = OrderManager::new(sample_smart_config());

        // Both empty
        let empty_book = OrderBookSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bids: vec![],
            asks: vec![],
            timestamp: Utc::now(),
        };
        let result = manager.compute_limit_price(OrderSide::Buy, &empty_book);
        assert!(matches!(result, Err(EngineError::EmptyOrderBook(_))));

        // Bids present but asks empty
        let asks_empty = OrderBookSnapshot {
            symbol: Symbol::new("XXBTZUSD")?,
            bids: vec![OrderBookLevel {
                price: Price::new(dec!(67_000)),
                quantity: Quantity::new(dec!(1))?,
            }],
            asks: vec![],
            timestamp: Utc::now(),
        };
        let result = manager.compute_limit_price(OrderSide::Buy, &asks_empty);
        assert!(matches!(result, Err(EngineError::EmptyOrderBook(_))));
        Ok(())
    }

    // ── submit_order tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_submit_order_market() -> Result<(), Box<dyn std::error::Error>> {
        let config = SmartOrderConfig {
            use_mid_price: false,
            offset_bps: dec!(0),
            fallback_timeout_ms: 30_000,
        };
        let mut manager = OrderManager::new(config);
        let executor = MockOrderExecutor::new("ORD-001");
        let intention = sample_market_buy_intention()?;

        let order_id = manager.submit_order(intention, None, &executor).await?;
        assert_eq!(order_id, OrderId::new("ORD-001")?);

        // Verify executor received Market order unchanged
        let placed = executor.placed_orders()?;
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].order_type, OrderType::Market);
        assert!(placed[0].limit_price.is_none());

        // Verify tracked
        let tracked = manager.tracked_orders.get(&order_id);
        assert!(tracked.is_some());
        assert_eq!(tracked.map(|t| t.status), Some(OrderStatus::Pending));
        Ok(())
    }

    #[tokio::test]
    async fn test_submit_order_smart_limit() -> Result<(), Box<dyn std::error::Error>> {
        let mut manager = OrderManager::new(sample_smart_config());
        let executor = MockOrderExecutor::new("ORD-002");
        let intention = sample_market_buy_intention()?;
        let book = sample_book()?;

        let order_id = manager
            .submit_order(intention, Some(&book), &executor)
            .await?;
        assert_eq!(order_id, OrderId::new("ORD-002")?);

        // Verify executor received Limit order with smart price
        let placed = executor.placed_orders()?;
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].order_type, OrderType::Limit);
        assert_eq!(placed[0].limit_price, Some(Price::new(dec!(67_005))));
        Ok(())
    }

    // ── on_fill tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_on_fill_updates_tracked_order() -> Result<(), Box<dyn std::error::Error>> {
        let mut manager = OrderManager::new(sample_smart_config());
        let executor = MockOrderExecutor::new("ORD-003");
        let intention = sample_market_buy_intention()?;
        let book = sample_book()?;

        let order_id = manager
            .submit_order(intention, Some(&book), &executor)
            .await?;

        let fill = OrderFill {
            order_id: order_id.clone(),
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67_005)),
            fill_quantity: Quantity::new(dec!(0.1))?,
            fee: Amount::new(dec!(0.26)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };

        let tracked = manager.on_fill(&fill);
        assert!(tracked.is_some());
        assert_eq!(tracked.map(|t| t.status), Some(OrderStatus::Filled));
        Ok(())
    }

    #[test]
    fn test_on_fill_unknown_order_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let mut manager = OrderManager::new(sample_smart_config());

        let fill = OrderFill {
            order_id: OrderId::new("UNKNOWN-001")?,
            symbol: Symbol::new("XXBTZUSD")?,
            side: OrderSide::Buy,
            fill_price: Price::new(dec!(67_000)),
            fill_quantity: Quantity::new(dec!(0.1))?,
            fee: Amount::new(dec!(0.26)),
            fee_currency: Currency::USD,
            timestamp: Utc::now(),
            trade_id: None,
        };

        let result = manager.on_fill(&fill);
        assert!(result.is_none());
        Ok(())
    }

    // ── cancel_all tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_cancel_all_cancels_tracked() -> Result<(), Box<dyn std::error::Error>> {
        let mut manager = OrderManager::new(sample_smart_config());
        let executor = MockOrderExecutor::new("ORD-004");

        // Submit two orders
        let intention1 = sample_market_buy_intention()?;
        let book = sample_book()?;
        manager
            .submit_order(intention1, Some(&book), &executor)
            .await?;

        // Use a second executor with different ID for second order
        let executor2 = MockOrderExecutor::new("ORD-005");
        let intention2 = sample_market_buy_intention()?;
        manager
            .submit_order(intention2, Some(&book), &executor2)
            .await?;

        // Cancel all via first executor
        let count = manager.cancel_all(&executor).await?;
        assert_eq!(count, 2);

        // Verify executor was called
        assert_eq!(executor.cancel_all_called()?, 1);

        // Verify both tracked orders are Cancelled
        for tracked in manager.tracked_orders.values() {
            assert_eq!(tracked.status, OrderStatus::Cancelled);
        }
        Ok(())
    }

    // ── stale_orders tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_stale_orders_identifies_expired() -> Result<(), Box<dyn std::error::Error>> {
        let config = SmartOrderConfig {
            use_mid_price: false,
            offset_bps: dec!(0),
            fallback_timeout_ms: 5_000,
        };
        let mut manager = OrderManager::new(config);

        // Insert a stale order (submitted 10s ago)
        let stale_time = Utc::now()
            - TimeDelta::try_milliseconds(10_000)
                .ok_or_else(|| anyhow::anyhow!("invalid duration"))?;
        manager.insert_tracked_for_test(TrackedOrder {
            order_id: OrderId::new("STALE-001")?,
            intention: sample_market_buy_intention()?,
            status: OrderStatus::Pending,
            submitted_at: stale_time,
        });

        // Insert a fresh order (submitted just now)
        let executor = MockOrderExecutor::new("FRESH-001");
        let intention = sample_market_buy_intention()?;
        manager.submit_order(intention, None, &executor).await?;

        let stale = manager.stale_orders(Utc::now());
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], OrderId::new("STALE-001")?);
        Ok(())
    }

    // ── Property-based tests ───────────────────────────────────────────

    mod proptests {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(1000))]

            #[test]
            fn prop_test_mid_price_between_bid_ask(
                bid_raw in 1i64..=100_000,
                spread in 1i64..=10_000,
            ) {
                let ask_raw = bid_raw + spread;
                let bid = Decimal::from(bid_raw);
                let ask = Decimal::from(ask_raw);

                let book = OrderBookSnapshot {
                    symbol: Symbol::new("XXBTZUSD")
                        .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                    bids: vec![OrderBookLevel {
                        price: Price::new(bid),
                        quantity: Quantity::new(dec!(1))
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                    }],
                    asks: vec![OrderBookLevel {
                        price: Price::new(ask),
                        quantity: Quantity::new(dec!(1))
                            .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?,
                    }],
                    timestamp: Utc::now(),
                };

                let manager = OrderManager::new(sample_smart_config()); // offset_bps = 0
                let mid = manager.compute_limit_price(OrderSide::Buy, &book)
                    .map_err(|e| TestCaseError::Fail(format!("{e}").into()))?;

                prop_assert!(mid.value() >= bid);
                prop_assert!(mid.value() <= ask);
            }
        }
    }
}
