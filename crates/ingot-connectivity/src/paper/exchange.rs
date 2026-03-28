use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_core::{
    Balance, InstrumentRegistry, OpenOrder, OrderFill, OrderId, OrderRequest, OrderStatus,
    Position, Tick,
};
use ingot_primitives::{Amount, Currency, OrderSide, OrderType, Price, Quantity, Symbol};
use rust_decimal::Decimal;
use tokio::{
    sync::{Mutex, RwLock, broadcast, watch},
    task::JoinHandle,
};
use tracing::{debug, info, instrument, warn};

use super::fill_model;
use crate::{
    config::PaperExchangeConfig,
    error::ConnectivityError,
    traits::{AccountProvider, OrderExecutor},
};

/// A fully in-memory simulated exchange for paper trading and backtesting.
pub struct PaperExchange {
    inner: Arc<PaperExchangeInner>,
}

struct PaperExchangeInner {
    config: PaperExchangeConfig,
    instruments: Arc<InstrumentRegistry>,

    // State
    balances: RwLock<HashMap<Currency, Balance>>,
    positions: RwLock<HashMap<Symbol, Position>>,
    open_orders: RwLock<HashMap<OrderId, OpenOrder>>,
    completed_orders: RwLock<HashMap<OrderId, OpenOrder>>,
    fills: RwLock<Vec<OrderFill>>,
    last_prices: RwLock<HashMap<Symbol, Price>>,

    // ID generation
    next_order_id: AtomicU64,
    next_fill_id: AtomicU64,

    // Fill notification
    fill_tx: broadcast::Sender<OrderFill>,

    // Lifecycle
    shutdown_tx: watch::Sender<bool>,
    match_task: Mutex<Option<JoinHandle<()>>>,

    // RNG for partial fills
    rng: Mutex<rand::rngs::StdRng>,
}

impl PaperExchange {
    /// Create a new `PaperExchange` that subscribes to the given tick feed.
    #[instrument(skip_all, fields(initial_currencies = config.initial_balances.len()))]
    pub fn new(
        config: PaperExchangeConfig,
        instruments: Arc<InstrumentRegistry>,
        tick_feed: &broadcast::Sender<Tick>,
    ) -> anyhow::Result<Self> {
        // Validate config
        anyhow::ensure!(
            config.slippage_bps >= Decimal::ZERO,
            "slippage_bps must be non-negative, got {}",
            config.slippage_bps
        );
        anyhow::ensure!(
            config.partial_fill_probability >= Decimal::ZERO
                && config.partial_fill_probability <= Decimal::ONE,
            "partial_fill_probability must be in [0, 1], got {}",
            config.partial_fill_probability
        );
        anyhow::ensure!(
            config.maker_fee_bps >= Decimal::ZERO,
            "maker_fee_bps must be non-negative, got {}",
            config.maker_fee_bps
        );
        anyhow::ensure!(
            config.taker_fee_bps >= Decimal::ZERO,
            "taker_fee_bps must be non-negative, got {}",
            config.taker_fee_bps
        );

        // Initialize balances
        let mut balances = HashMap::new();
        for (currency, amount) in &config.initial_balances {
            let balance = Balance {
                currency: currency.clone(),
                total: Amount::new(*amount),
                available: Amount::new(*amount),
                held: Amount::zero(),
            };
            balances.insert(currency.clone(), balance);
        }

        let (fill_tx, _) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let tick_rx = tick_feed.subscribe();

        let rng: rand::rngs::StdRng = rand::make_rng();

        let inner = Arc::new(PaperExchangeInner {
            config,
            instruments,
            balances: RwLock::new(balances),
            positions: RwLock::new(HashMap::new()),
            open_orders: RwLock::new(HashMap::new()),
            completed_orders: RwLock::new(HashMap::new()),
            fills: RwLock::new(Vec::new()),
            last_prices: RwLock::new(HashMap::new()),
            next_order_id: AtomicU64::new(1),
            next_fill_id: AtomicU64::new(1),
            fill_tx,
            shutdown_tx,
            match_task: Mutex::new(None),
            rng: Mutex::new(rng),
        });

        // Spawn background matching loop
        let task_inner = Arc::clone(&inner);
        let handle = tokio::spawn(matching_loop(task_inner, tick_rx, shutdown_rx));
        {
            // Store the handle — we can't await the lock in a non-async context,
            // but `Mutex::blocking_lock` works here since we're not inside an async block
            // yet. Actually, `new` is not async, so use `try_lock`.
            if let Ok(mut guard) = inner.match_task.try_lock() {
                *guard = Some(handle);
            }
        }

        info!("paper exchange initialized");
        Ok(Self { inner })
    }

    /// Subscribe to fill notifications.
    pub fn subscribe_fills(&self) -> broadcast::Receiver<OrderFill> {
        self.inner.fill_tx.subscribe()
    }

    /// Shut down the background matching loop.
    pub async fn shutdown(&self) {
        let _ = self.inner.shutdown_tx.send(true);
        let handle = {
            let mut guard = self.inner.match_task.lock().await;
            guard.take()
        };
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
        info!("paper exchange shut down");
    }

    async fn place_order_inner(&self, request: &OrderRequest) -> anyhow::Result<OrderId> {
        // 1. Resolve symbol
        let instrument = self
            .inner
            .instruments
            .get(&request.symbol)
            .ok_or(ConnectivityError::SymbolNotFound(request.symbol.clone()))
            .context("resolving instrument for order")?;

        let base_currency = instrument.base_currency.clone();
        let quote_currency = instrument.quote_currency.clone();

        // 2. Generate order ID
        let seq = self.inner.next_order_id.fetch_add(1, Ordering::Relaxed);
        let order_id =
            OrderId::new(&format!("paper-{seq}")).context("generating paper order id")?;

        // 3. Simulate latency
        if self.inner.config.latency_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(
                self.inner.config.latency_ms,
            ))
            .await;
        }

        // 4. Match on order type
        match request.order_type {
            OrderType::Market => {
                self.execute_market_order(&order_id, request, &base_currency, &quote_currency)
                    .await?;
            }
            OrderType::Limit => {
                let limit_price = request
                    .limit_price
                    .ok_or(ConnectivityError::OrderRejected {
                        reason: "limit order requires limit_price".into(),
                    })
                    .context("validating limit order")?;

                self.place_limit_order(
                    &order_id,
                    request,
                    limit_price,
                    &base_currency,
                    &quote_currency,
                )
                .await?;
            }
            OrderType::StopLoss
            | OrderType::StopLossLimit
            | OrderType::TakeProfit
            | OrderType::TakeProfitLimit => {
                return Err(ConnectivityError::OrderRejected {
                    reason: "stop orders not yet supported in paper exchange".into(),
                })
                .context("validating order type");
            }
        }

        Ok(order_id)
    }

    async fn execute_market_order(
        &self,
        order_id: &OrderId,
        request: &OrderRequest,
        base_currency: &Currency,
        quote_currency: &Currency,
    ) -> anyhow::Result<()> {
        // Get last price
        let last_price = {
            let prices = self.inner.last_prices.read().await;
            prices.get(&request.symbol).copied()
        };
        let last_price = last_price
            .ok_or(ConnectivityError::OrderRejected {
                reason: "no market data available for symbol".into(),
            })
            .context("checking last price for market order")?;

        // Apply slippage
        let fill_price =
            fill_model::apply_slippage(last_price, request.side, self.inner.config.slippage_bps);

        // Check balance
        self.check_and_hold_balance(
            request.side,
            fill_price,
            request.quantity,
            base_currency,
            quote_currency,
        )
        .await?;

        // Create the open order record
        let open_order = OpenOrder {
            order_id: order_id.clone(),
            request: request.clone(),
            status: OrderStatus::Pending,
            filled_quantity: Quantity::zero(),
            remaining_quantity: request.quantity,
            average_fill_price: None,
            created_at: Utc::now(),
        };
        self.inner
            .open_orders
            .write()
            .await
            .insert(order_id.clone(), open_order);

        // Execute fill (possibly partial)
        let mut remaining = request.quantity;
        let partial_qty = {
            let mut rng = self.inner.rng.lock().await;
            fill_model::partial_fill_quantity(
                remaining,
                self.inner.config.partial_fill_probability,
                &mut *rng,
            )
        };

        if let Some(partial) = partial_qty {
            // First partial fill
            self.process_fill(
                order_id,
                &request.symbol,
                request.side,
                fill_price,
                partial,
                false, // market = taker
                base_currency,
                quote_currency,
            )
            .await?;
            remaining = remaining - partial;
        }

        // Fill the rest
        if remaining.value() > Decimal::ZERO {
            self.process_fill(
                order_id,
                &request.symbol,
                request.side,
                fill_price,
                remaining,
                false,
                base_currency,
                quote_currency,
            )
            .await?;
        }

        Ok(())
    }

    async fn place_limit_order(
        &self,
        order_id: &OrderId,
        request: &OrderRequest,
        limit_price: Price,
        base_currency: &Currency,
        quote_currency: &Currency,
    ) -> anyhow::Result<()> {
        // Check and hold balance at limit price
        self.check_and_hold_balance(
            request.side,
            limit_price,
            request.quantity,
            base_currency,
            quote_currency,
        )
        .await?;

        let open_order = OpenOrder {
            order_id: order_id.clone(),
            request: request.clone(),
            status: OrderStatus::Open,
            filled_quantity: Quantity::zero(),
            remaining_quantity: request.quantity,
            average_fill_price: None,
            created_at: Utc::now(),
        };

        self.inner
            .open_orders
            .write()
            .await
            .insert(order_id.clone(), open_order);

        debug!(order_id = %order_id, "limit order resting on book");
        Ok(())
    }

    async fn check_and_hold_balance(
        &self,
        side: OrderSide,
        price: Price,
        quantity: Quantity,
        base_currency: &Currency,
        quote_currency: &Currency,
    ) -> anyhow::Result<()> {
        let cost = price * quantity;
        let mut balances = self.inner.balances.write().await;

        match side {
            OrderSide::Buy => {
                // Need quote currency
                let balance = balances
                    .get_mut(quote_currency)
                    .ok_or(ConnectivityError::InsufficientBalance {
                        required: cost.value(),
                        available: Decimal::ZERO,
                    })
                    .context("checking quote balance for buy")?;

                if balance.available.value() < cost.value() {
                    return Err(ConnectivityError::InsufficientBalance {
                        required: cost.value(),
                        available: balance.available.value(),
                    })
                    .context("insufficient quote balance for buy");
                }

                balance.available = balance.available - cost;
                balance.held = balance.held + cost;
            }
            OrderSide::Sell => {
                // Check if we have base currency (long position close)
                // or if we need to hold quote currency as margin (short open)
                let has_base = balances
                    .get(base_currency)
                    .is_some_and(|b| b.available.value() >= quantity.value());

                if has_base {
                    let balance = balances
                        .get_mut(base_currency)
                        .ok_or(ConnectivityError::InsufficientBalance {
                            required: quantity.value(),
                            available: Decimal::ZERO,
                        })
                        .context("checking base balance for sell")?;
                    let qty_amount = Amount::new(quantity.value());
                    balance.available = balance.available - qty_amount;
                    balance.held = balance.held + qty_amount;
                } else {
                    // Short sell — hold quote currency as margin
                    let balance = balances
                        .get_mut(quote_currency)
                        .ok_or(ConnectivityError::InsufficientBalance {
                            required: cost.value(),
                            available: Decimal::ZERO,
                        })
                        .context("checking quote balance for short sell")?;

                    if balance.available.value() < cost.value() {
                        return Err(ConnectivityError::InsufficientBalance {
                            required: cost.value(),
                            available: balance.available.value(),
                        })
                        .context("insufficient quote balance for short sell");
                    }

                    balance.available = balance.available - cost;
                    balance.held = balance.held + cost;
                }
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_fill(
        &self,
        order_id: &OrderId,
        symbol: &Symbol,
        side: OrderSide,
        fill_price: Price,
        fill_qty: Quantity,
        is_maker: bool,
        base_currency: &Currency,
        quote_currency: &Currency,
    ) -> anyhow::Result<OrderFill> {
        let fee_bps = if is_maker {
            self.inner.config.maker_fee_bps
        } else {
            self.inner.config.taker_fee_bps
        };
        let fee = fill_model::calculate_fee(fill_price, fill_qty, fee_bps);
        let cost = fill_price * fill_qty;

        self.update_balances_for_fill(side, cost, fee, fill_qty, base_currency, quote_currency)
            .await;
        self.update_position(symbol, side, fill_price, fill_qty)
            .await?;
        self.update_order_on_fill(order_id, fill_price, fill_qty)
            .await;

        // Create fill
        let fill_seq = self.inner.next_fill_id.fetch_add(1, Ordering::Relaxed);
        let order_fill = OrderFill {
            order_id: order_id.clone(),
            symbol: symbol.clone(),
            side,
            fill_price,
            fill_quantity: fill_qty,
            fee,
            fee_currency: quote_currency.clone(),
            timestamp: Utc::now(),
            trade_id: Some(smol_str::SmolStr::new(format!("paper-fill-{fill_seq}"))),
        };

        self.inner.fills.write().await.push(order_fill.clone());
        let _ = self.inner.fill_tx.send(order_fill.clone());

        debug!(
            order_id = %order_id,
            fill_price = %fill_price,
            fill_qty = %fill_qty,
            "fill processed"
        );

        Ok(order_fill)
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_balances_for_fill(
        &self,
        side: OrderSide,
        cost: Amount,
        fee: Amount,
        fill_qty: Quantity,
        base_currency: &Currency,
        quote_currency: &Currency,
    ) {
        let mut balances = self.inner.balances.write().await;

        match side {
            OrderSide::Buy => {
                if let Some(quote_bal) = balances.get_mut(quote_currency) {
                    quote_bal.held = quote_bal.held - cost;
                    quote_bal.total = quote_bal.total - fee;
                    quote_bal.held = quote_bal.held - fee;
                }
                let base_bal = balances
                    .entry(base_currency.clone())
                    .or_insert_with(|| Balance {
                        currency: base_currency.clone(),
                        total: Amount::zero(),
                        available: Amount::zero(),
                        held: Amount::zero(),
                    });
                let qty_amount = Amount::new(fill_qty.value());
                base_bal.available = base_bal.available + qty_amount;
                base_bal.total = base_bal.total + qty_amount;
            }
            OrderSide::Sell => {
                let base_held = balances
                    .get(base_currency)
                    .map_or(Decimal::ZERO, |b| b.held.value());

                if base_held >= fill_qty.value() {
                    if let Some(base_bal) = balances.get_mut(base_currency) {
                        let qty_amount = Amount::new(fill_qty.value());
                        base_bal.held = base_bal.held - qty_amount;
                        base_bal.total = base_bal.total - qty_amount;
                    }
                    let quote_bal =
                        balances
                            .entry(quote_currency.clone())
                            .or_insert_with(|| Balance {
                                currency: quote_currency.clone(),
                                total: Amount::zero(),
                                available: Amount::zero(),
                                held: Amount::zero(),
                            });
                    let proceeds = cost - fee;
                    quote_bal.available = quote_bal.available + proceeds;
                    quote_bal.total = quote_bal.total + proceeds;
                } else if let Some(quote_bal) = balances.get_mut(quote_currency) {
                    quote_bal.held = quote_bal.held - cost;
                    quote_bal.total = quote_bal.total - fee;
                    quote_bal.held = quote_bal.held - fee;
                }
            }
        }
    }

    async fn update_position(
        &self,
        symbol: &Symbol,
        side: OrderSide,
        fill_price: Price,
        fill_qty: Quantity,
    ) -> anyhow::Result<()> {
        let mut positions = self.inner.positions.write().await;
        match positions.get(symbol) {
            Some(existing) if existing.side == side => {
                let pos = positions
                    .get_mut(symbol)
                    .ok_or_else(|| anyhow::anyhow!("position disappeared"))
                    .context("updating same-side position")?;
                let old_notional = pos.average_entry_price.value() * pos.quantity.value();
                let new_notional = fill_price.value() * fill_qty.value();
                let new_qty = pos.quantity + fill_qty;
                let avg = if new_qty.value() > Decimal::ZERO {
                    (old_notional + new_notional) / new_qty.value()
                } else {
                    Decimal::ZERO
                };
                pos.quantity = new_qty;
                pos.average_entry_price = Price::new(avg);
            }
            Some(existing) => {
                let existing_qty = existing.quantity;
                match fill_qty.value().cmp(&existing_qty.value()) {
                    std::cmp::Ordering::Less => {
                        let pos = positions
                            .get_mut(symbol)
                            .ok_or_else(|| anyhow::anyhow!("position disappeared"))
                            .context("partially closing position")?;
                        pos.quantity = existing_qty - fill_qty;
                    }
                    std::cmp::Ordering::Equal => {
                        positions.remove(symbol);
                    }
                    std::cmp::Ordering::Greater => {
                        let remaining_qty = fill_qty - existing_qty;
                        if remaining_qty.value() > Decimal::ZERO {
                            positions.insert(
                                symbol.clone(),
                                Position {
                                    symbol: symbol.clone(),
                                    side,
                                    quantity: remaining_qty,
                                    average_entry_price: fill_price,
                                    unrealized_pnl: None,
                                    liquidation_price: None,
                                },
                            );
                        } else {
                            positions.remove(symbol);
                        }
                    }
                }
            }
            None => {
                positions.insert(
                    symbol.clone(),
                    Position {
                        symbol: symbol.clone(),
                        side,
                        quantity: fill_qty,
                        average_entry_price: fill_price,
                        unrealized_pnl: None,
                        liquidation_price: None,
                    },
                );
            }
        }
        Ok(())
    }

    async fn update_order_on_fill(
        &self,
        order_id: &OrderId,
        fill_price: Price,
        fill_qty: Quantity,
    ) {
        let mut open_orders = self.inner.open_orders.write().await;
        if let Some(order) = open_orders.get_mut(order_id) {
            order.filled_quantity = order.filled_quantity + fill_qty;
            order.remaining_quantity = order.remaining_quantity - fill_qty;

            let prev_filled = order.filled_quantity - fill_qty;
            let prev_notional = order
                .average_fill_price
                .unwrap_or(Price::new(Decimal::ZERO))
                .value()
                * prev_filled.value();
            let new_notional = fill_price.value() * fill_qty.value();
            let total_filled = order.filled_quantity.value();
            if total_filled > Decimal::ZERO {
                order.average_fill_price =
                    Some(Price::new((prev_notional + new_notional) / total_filled));
            }

            if order.remaining_quantity.value() <= Decimal::ZERO {
                order.status = OrderStatus::Filled;
                let completed = order.clone();
                let id = order_id.clone();
                open_orders.remove(order_id);
                // We can't await inside this block easily, so use try_write
                // Actually we can since this is an async fn
                drop(open_orders);
                self.inner
                    .completed_orders
                    .write()
                    .await
                    .insert(id, completed);
            } else {
                order.status = OrderStatus::PartiallyFilled;
            }
        }
    }

    async fn cancel_order_inner(&self, order_id: &OrderId) -> anyhow::Result<()> {
        let mut open_orders = self.inner.open_orders.write().await;
        let order = open_orders
            .remove(order_id)
            .ok_or(ConnectivityError::OrderRejected {
                reason: format!("order {order_id} not found"),
            })
            .context("cancelling order")?;

        // Return held funds
        let instrument = self
            .inner
            .instruments
            .get(&order.request.symbol)
            .ok_or(ConnectivityError::SymbolNotFound(
                order.request.symbol.clone(),
            ))
            .context("resolving instrument for cancel")?;

        let base_currency = &instrument.base_currency;
        let quote_currency = &instrument.quote_currency;

        let remaining = order.remaining_quantity;
        let mut balances = self.inner.balances.write().await;

        match order.request.side {
            OrderSide::Buy => {
                let limit_price = order
                    .request
                    .limit_price
                    .unwrap_or(Price::new(Decimal::ZERO));
                let held_amount = limit_price * remaining;
                if let Some(bal) = balances.get_mut(quote_currency) {
                    bal.held = bal.held - held_amount;
                    bal.available = bal.available + held_amount;
                }
            }
            OrderSide::Sell => {
                // Check if base was held or quote was held (short)
                let base_held = balances
                    .get(base_currency)
                    .map_or(Decimal::ZERO, |b| b.held.value());
                if base_held >= remaining.value() {
                    if let Some(bal) = balances.get_mut(base_currency) {
                        let qty_amount = Amount::new(remaining.value());
                        bal.held = bal.held - qty_amount;
                        bal.available = bal.available + qty_amount;
                    }
                } else {
                    // Short sell margin return
                    let limit_price = order
                        .request
                        .limit_price
                        .unwrap_or(Price::new(Decimal::ZERO));
                    let held_amount = limit_price * remaining;
                    if let Some(bal) = balances.get_mut(quote_currency) {
                        bal.held = bal.held - held_amount;
                        bal.available = bal.available + held_amount;
                    }
                }
            }
        }

        // Store as completed
        let mut cancelled = order;
        cancelled.status = OrderStatus::Cancelled;
        drop(open_orders);
        self.inner
            .completed_orders
            .write()
            .await
            .insert(order_id.clone(), cancelled);

        debug!(order_id = %order_id, "order cancelled");
        Ok(())
    }

    async fn cancel_all_inner(&self) -> anyhow::Result<u32> {
        let order_ids: Vec<OrderId> = {
            let open_orders = self.inner.open_orders.read().await;
            open_orders.keys().cloned().collect()
        };

        let mut count = 0u32;
        for id in &order_ids {
            self.cancel_order_inner(id).await?;
            count += 1;
        }
        Ok(count)
    }
}

impl OrderExecutor for PaperExchange {
    #[instrument(skip_all, fields(symbol = %request.symbol, side = ?request.side, order_type = ?request.order_type))]
    async fn place_order(&self, request: &OrderRequest) -> anyhow::Result<OrderId> {
        self.place_order_inner(request).await
    }

    #[instrument(skip(self), fields(order_id = %order_id))]
    async fn cancel_order(&self, order_id: &OrderId) -> anyhow::Result<()> {
        self.cancel_order_inner(order_id).await
    }

    #[instrument(skip(self))]
    async fn cancel_all_orders(&self) -> anyhow::Result<u32> {
        self.cancel_all_inner().await
    }

    #[instrument(skip(self), fields(order_id = %order_id))]
    async fn get_order_status(&self, order_id: &OrderId) -> anyhow::Result<OpenOrder> {
        // Check open orders first
        if let Some(order) = self.inner.open_orders.read().await.get(order_id) {
            return Ok(order.clone());
        }
        // Check completed orders
        if let Some(order) = self.inner.completed_orders.read().await.get(order_id) {
            return Ok(order.clone());
        }
        Err(ConnectivityError::OrderRejected {
            reason: format!("order {order_id} not found"),
        })
        .context("looking up order status")
    }

    #[instrument(skip(self))]
    async fn get_open_orders(&self) -> anyhow::Result<Vec<OpenOrder>> {
        Ok(self
            .inner
            .open_orders
            .read()
            .await
            .values()
            .cloned()
            .collect())
    }
}

impl AccountProvider for PaperExchange {
    #[instrument(skip(self))]
    async fn get_balances(&self) -> anyhow::Result<Vec<Balance>> {
        Ok(self.inner.balances.read().await.values().cloned().collect())
    }

    #[instrument(skip(self))]
    async fn get_positions(&self) -> anyhow::Result<Vec<Position>> {
        Ok(self
            .inner
            .positions
            .read()
            .await
            .values()
            .filter(|p| p.quantity.value() > Decimal::ZERO)
            .cloned()
            .collect())
    }

    #[instrument(skip(self))]
    async fn get_trade_history(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<OrderFill>> {
        let fills = self.inner.fills.read().await;
        match since {
            Some(since) => Ok(fills
                .iter()
                .filter(|f| f.timestamp >= since)
                .cloned()
                .collect()),
            None => Ok(fills.clone()),
        }
    }
}

/// Background task: match limit orders against incoming ticks.
async fn matching_loop(
    inner: Arc<PaperExchangeInner>,
    mut tick_rx: broadcast::Receiver<Tick>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            tick = tick_rx.recv() => {
                match tick {
                    Ok(tick) => {
                        // Update last price
                        inner.last_prices.write().await.insert(tick.symbol.clone(), tick.price);

                        // Check open limit orders for this symbol
                        let matching_orders: Vec<(OrderId, OrderSide, Price, Quantity)> = {
                            let orders = inner.open_orders.read().await;
                            orders.values()
                                .filter(|o| {
                                    o.request.symbol == tick.symbol
                                        && o.request.order_type == OrderType::Limit
                                        && o.request.limit_price.is_some()
                                })
                                .filter(|o| {
                                    let limit_price = o.request.limit_price.unwrap_or(Price::new(Decimal::ZERO));
                                    fill_model::tick_crosses_limit(tick.price, limit_price, o.request.side)
                                })
                                .map(|o| {
                                    (
                                        o.order_id.clone(),
                                        o.request.side,
                                        o.request.limit_price.unwrap_or(Price::new(Decimal::ZERO)),
                                        o.remaining_quantity,
                                    )
                                })
                                .collect()
                        };

                        // Process fills for matching orders
                        for (order_id, side, limit_price, remaining) in matching_orders {
                            // Resolve instrument
                            let symbol = tick.symbol.clone();
                            let Some(instrument) = inner.instruments.get(&symbol) else {
                                continue;
                            };
                            let base_currency = instrument.base_currency.clone();
                            let quote_currency = instrument.quote_currency.clone();

                            // Create a temporary PaperExchange reference to call process_fill
                            // Instead, inline the fill logic or use a free function
                            // We'll create a helper that works with the inner directly
                            let fill_qty = {
                                let mut rng = inner.rng.lock().await;
                                fill_model::partial_fill_quantity(
                                    remaining,
                                    inner.config.partial_fill_probability,
                                    &mut *rng,
                                )
                            };

                            let exchange = PaperExchange { inner: Arc::clone(&inner) };

                            if let Some(partial) = fill_qty {
                                let _ = exchange.process_fill(
                                    &order_id, &symbol, side, limit_price,
                                    partial, true, &base_currency, &quote_currency,
                                ).await;
                            } else {
                                let _ = exchange.process_fill(
                                    &order_id, &symbol, side, limit_price,
                                    remaining, true, &base_currency, &quote_currency,
                                ).await;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("paper exchange lagged {n} ticks");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = shutdown_rx.changed() => break,
        }
    }
    debug!("matching loop exited");
}

#[cfg(test)]
mod tests {
    use ingot_core::{Instrument, InstrumentDetails};
    use ingot_primitives::{AssetClass, Exchange, TimeInForce};
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;

    use super::*;

    fn make_test_config() -> PaperExchangeConfig {
        PaperExchangeConfig {
            initial_balances: vec![(Currency::USD, dec!(100000)), (Currency::BTC, dec!(10))],
            slippage_bps: dec!(10),                  // 10 bps = 0.1%
            latency_ms: 0,                           // no latency in tests
            partial_fill_probability: Decimal::ZERO, // no partials by default
            maker_fee_bps: dec!(16),
            taker_fee_bps: dec!(26),
        }
    }

    fn make_test_instruments() -> anyhow::Result<Arc<InstrumentRegistry>> {
        Ok(Arc::new(InstrumentRegistry::new(vec![
            Instrument {
                symbol: Symbol::new("XXBTZUSD")?,
                asset_class: AssetClass::CryptoSpot,
                exchange: Exchange::Paper,
                base_currency: Currency::BTC,
                quote_currency: Currency::USD,
                tick_size: Price::new(dec!(0.1)),
                display_name: SmolStr::new("BTC/USD"),
                details: InstrumentDetails::CryptoSpot {
                    order_min: Quantity::new(dec!(0.0001))?,
                    cost_min: Amount::new(dec!(0.5)),
                    lot_decimals: 8,
                    margin_eligible: false,
                    leverage_tiers: vec![],
                },
            },
            Instrument {
                symbol: Symbol::new("XETHZUSD")?,
                asset_class: AssetClass::CryptoSpot,
                exchange: Exchange::Paper,
                base_currency: Currency::ETH,
                quote_currency: Currency::USD,
                tick_size: Price::new(dec!(0.01)),
                display_name: SmolStr::new("ETH/USD"),
                details: InstrumentDetails::CryptoSpot {
                    order_min: Quantity::new(dec!(0.001))?,
                    cost_min: Amount::new(dec!(0.5)),
                    lot_decimals: 8,
                    margin_eligible: false,
                    leverage_tiers: vec![],
                },
            },
        ])))
    }

    fn make_tick(symbol: &str, price: Decimal) -> anyhow::Result<Tick> {
        Ok(Tick {
            time: Utc::now(),
            symbol: Symbol::new(symbol)?,
            exchange: SmolStr::new("paper"),
            price: Price::new(price),
            quantity: Quantity::new(dec!(1))?,
            side: Some(OrderSide::Buy),
            trade_id: None,
        })
    }

    fn buy_market(symbol: &str, qty: Decimal) -> anyhow::Result<OrderRequest> {
        Ok(OrderRequest {
            symbol: Symbol::new(symbol)?,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::new(qty)?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        })
    }

    fn sell_market(symbol: &str, qty: Decimal) -> anyhow::Result<OrderRequest> {
        Ok(OrderRequest {
            symbol: Symbol::new(symbol)?,
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity: Quantity::new(qty)?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        })
    }

    fn buy_limit(symbol: &str, qty: Decimal, price: Decimal) -> anyhow::Result<OrderRequest> {
        Ok(OrderRequest {
            symbol: Symbol::new(symbol)?,
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(qty)?,
            limit_price: Some(Price::new(price)),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        })
    }

    fn sell_limit(symbol: &str, qty: Decimal, price: Decimal) -> anyhow::Result<OrderRequest> {
        Ok(OrderRequest {
            symbol: Symbol::new(symbol)?,
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            quantity: Quantity::new(qty)?,
            limit_price: Some(Price::new(price)),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        })
    }

    /// Helper to create an exchange and feed an initial tick so market orders
    /// work.
    async fn setup_exchange_with_price(
        price: Decimal,
    ) -> anyhow::Result<(PaperExchange, broadcast::Sender<Tick>)> {
        let config = make_test_config();
        let instruments = make_test_instruments()?;
        let (tick_tx, _) = broadcast::channel(64);
        let exchange = PaperExchange::new(config, instruments, &tick_tx)?;

        // Feed a tick so last_price is set
        tick_tx
            .send(make_tick("XXBTZUSD", price)?)
            .context("sending initial tick")?;
        // Give the matching loop time to process
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        Ok((exchange, tick_tx))
    }

    // ==================== Construction & Config ====================

    #[tokio::test]
    async fn test_new_initializes_balances() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        let balances = exchange.get_balances().await?;
        assert_eq!(balances.len(), 2);

        let usd = balances.iter().find(|b| b.currency == Currency::USD);
        assert!(usd.is_some());
        let usd = usd.ok_or_else(|| anyhow::anyhow!("no USD balance"))?;
        assert_eq!(usd.total, Amount::new(dec!(100000)));
        assert_eq!(usd.available, Amount::new(dec!(100000)));
        assert_eq!(usd.held, Amount::zero());

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_new_invalid_slippage_rejected() -> anyhow::Result<()> {
        let mut config = make_test_config();
        config.slippage_bps = dec!(-1);
        let instruments = make_test_instruments()?;
        let (tick_tx, _) = broadcast::channel(64);
        let result = PaperExchange::new(config, instruments, &tick_tx);
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_new_invalid_probability_rejected() -> anyhow::Result<()> {
        let mut config = make_test_config();
        config.partial_fill_probability = dec!(1.5);
        let instruments = make_test_instruments()?;
        let (tick_tx, _) = broadcast::channel(64);
        let result = PaperExchange::new(config, instruments, &tick_tx);
        assert!(result.is_err());
        Ok(())
    }

    // ==================== Market Orders ====================

    #[tokio::test]
    async fn test_market_buy_success() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        let order_id = exchange
            .place_order(&buy_market("XXBTZUSD", dec!(1))?)
            .await?;
        assert!(order_id.as_str().starts_with("paper-"));

        // Check balances changed
        let balances = exchange.get_balances().await?;
        let usd = balances
            .iter()
            .find(|b| b.currency == Currency::USD)
            .ok_or_else(|| anyhow::anyhow!("no USD"))?;
        // Cost = 67000 * (1 + 10/10000) = 67000 * 1.001 = 67067 + fee
        // Slippage: 67000 + 67000*10/10000 = 67000 + 67 = 67067
        // Fee: 67067 * 26 / 10000 = 17.43742
        assert!(
            usd.available.value() < dec!(100000),
            "USD should have decreased"
        );

        // Check BTC balance increased
        let btc = balances
            .iter()
            .find(|b| b.currency == Currency::BTC)
            .ok_or_else(|| anyhow::anyhow!("no BTC"))?;
        assert_eq!(btc.available, Amount::new(dec!(11))); // started with 10, bought 1

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_market_sell_success() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        let order_id = exchange
            .place_order(&sell_market("XXBTZUSD", dec!(1))?)
            .await?;
        assert!(order_id.as_str().starts_with("paper-"));

        // BTC should decrease
        let balances = exchange.get_balances().await?;
        let btc = balances
            .iter()
            .find(|b| b.currency == Currency::BTC)
            .ok_or_else(|| anyhow::anyhow!("no BTC"))?;
        assert_eq!(btc.available, Amount::new(dec!(9))); // started with 10, sold 1

        // USD should increase (proceeds minus fee)
        let usd = balances
            .iter()
            .find(|b| b.currency == Currency::USD)
            .ok_or_else(|| anyhow::anyhow!("no USD"))?;
        assert!(
            usd.available.value() > dec!(100000),
            "USD should have increased from sale proceeds"
        );

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_market_buy_insufficient_balance() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        // Try to buy 2 BTC at 67000 = 134000+ but we only have 100000 USD
        let result = exchange
            .place_order(&buy_market("XXBTZUSD", dec!(2))?)
            .await;
        assert!(result.is_err());

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_market_buy_no_price_data() -> anyhow::Result<()> {
        let config = make_test_config();
        let instruments = make_test_instruments()?;
        let (tick_tx, _) = broadcast::channel(64);
        let exchange = PaperExchange::new(config, instruments, &tick_tx)?;

        // No tick sent, so no price data
        let result = exchange
            .place_order(&buy_market("XXBTZUSD", dec!(1))?)
            .await;
        assert!(result.is_err());

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_market_buy_creates_position() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(1))?)
            .await?;

        let positions = exchange.get_positions().await?;
        assert_eq!(positions.len(), 1);
        let pos = &positions[0];
        assert_eq!(pos.symbol.as_str(), "XXBTZUSD");
        assert_eq!(pos.side, OrderSide::Buy);
        assert_eq!(pos.quantity, Quantity::new(dec!(1))?);

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_market_sell_short() -> anyhow::Result<()> {
        // Start with no BTC, only USD — sell should create a short
        let mut config = make_test_config();
        config.initial_balances = vec![(Currency::USD, dec!(100000))];
        let instruments = make_test_instruments()?;
        let (tick_tx, _) = broadcast::channel(64);
        let exchange = PaperExchange::new(config, instruments, &tick_tx)?;

        // Feed price
        tick_tx.send(make_tick("XXBTZUSD", dec!(67000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        exchange
            .place_order(&sell_market("XXBTZUSD", dec!(0.5))?)
            .await?;

        let positions = exchange.get_positions().await?;
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].side, OrderSide::Sell);
        assert_eq!(positions[0].quantity, Quantity::new(dec!(0.5))?);

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_market_buy_closes_short() -> anyhow::Result<()> {
        // Create a short first, then buy to close
        let mut config = make_test_config();
        config.initial_balances = vec![(Currency::USD, dec!(200000))];
        let instruments = make_test_instruments()?;
        let (tick_tx, _) = broadcast::channel(64);
        let exchange = PaperExchange::new(config, instruments, &tick_tx)?;

        tick_tx.send(make_tick("XXBTZUSD", dec!(67000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Open short
        exchange
            .place_order(&sell_market("XXBTZUSD", dec!(1))?)
            .await?;

        let positions = exchange.get_positions().await?;
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].side, OrderSide::Sell);

        // Close short with buy
        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(1))?)
            .await?;

        let positions = exchange.get_positions().await?;
        assert!(positions.is_empty(), "position should be closed");

        exchange.shutdown().await;
        Ok(())
    }

    // ==================== Limit Orders ====================

    #[tokio::test]
    async fn test_limit_buy_rests_on_book() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        let order_id = exchange
            .place_order(&buy_limit("XXBTZUSD", dec!(1), dec!(66000))?)
            .await?;

        let open = exchange.get_open_orders().await?;
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].order_id, order_id);
        assert_eq!(open[0].status, OrderStatus::Open);

        // Balance should be held
        let balances = exchange.get_balances().await?;
        let usd = balances
            .iter()
            .find(|b| b.currency == Currency::USD)
            .ok_or_else(|| anyhow::anyhow!("no USD"))?;
        assert_eq!(usd.held, Amount::new(dec!(66000))); // 66000 * 1

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_limit_buy_filled_on_tick() -> anyhow::Result<()> {
        let (exchange, tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        // Place buy limit at 66000
        let order_id = exchange
            .place_order(&buy_limit("XXBTZUSD", dec!(1), dec!(66000))?)
            .await?;

        // Tick above limit → no fill
        tick_tx.send(make_tick("XXBTZUSD", dec!(66500))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let open = exchange.get_open_orders().await?;
        assert_eq!(open.len(), 1, "order should still be open");

        // Tick at/below limit → fill
        tick_tx.send(make_tick("XXBTZUSD", dec!(65900))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let open = exchange.get_open_orders().await?;
        assert!(open.is_empty(), "order should be filled");

        let order = exchange.get_order_status(&order_id).await?;
        assert_eq!(order.status, OrderStatus::Filled);

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_limit_sell_filled_on_tick() -> anyhow::Result<()> {
        let (exchange, tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        // Place sell limit at 68000
        let _order_id = exchange
            .place_order(&sell_limit("XXBTZUSD", dec!(1), dec!(68000))?)
            .await?;

        // Tick below limit → no fill
        tick_tx.send(make_tick("XXBTZUSD", dec!(67500))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(exchange.get_open_orders().await?.len(), 1);

        // Tick at limit → fill
        tick_tx.send(make_tick("XXBTZUSD", dec!(68000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(exchange.get_open_orders().await?.is_empty());

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_limit_order_not_filled_wrong_price() -> anyhow::Result<()> {
        let (exchange, tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        // Buy limit at 60000 — price never drops that low
        exchange
            .place_order(&buy_limit("XXBTZUSD", dec!(0.5), dec!(60000))?)
            .await?;

        tick_tx.send(make_tick("XXBTZUSD", dec!(66000))?)?;
        tick_tx.send(make_tick("XXBTZUSD", dec!(65000))?)?;
        tick_tx.send(make_tick("XXBTZUSD", dec!(61000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        assert_eq!(
            exchange.get_open_orders().await?.len(),
            1,
            "order should still be open"
        );

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_limit_order_maker_fee() -> anyhow::Result<()> {
        let (exchange, tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        // Place buy limit at 66000
        exchange
            .place_order(&buy_limit("XXBTZUSD", dec!(1), dec!(66000))?)
            .await?;

        // Trigger fill
        tick_tx.send(make_tick("XXBTZUSD", dec!(65000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let fills = exchange.get_trade_history(None).await?;
        assert_eq!(fills.len(), 1);
        // Maker fee: 66000 * 1 * 16/10000 = 105.60
        assert_eq!(fills[0].fee, Amount::new(dec!(105.60)));

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_limit_order_partial_fill() -> anyhow::Result<()> {
        let mut config = make_test_config();
        config.partial_fill_probability = Decimal::ONE; // always partial fill
        let instruments = make_test_instruments()?;
        let (tick_tx, _) = broadcast::channel(64);
        let exchange = PaperExchange::new(config, instruments, &tick_tx)?;

        // Feed initial price
        tick_tx.send(make_tick("XXBTZUSD", dec!(67000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Place buy limit at 66000
        let order_id = exchange
            .place_order(&buy_limit("XXBTZUSD", dec!(1), dec!(66000))?)
            .await?;

        // First tick crossing triggers partial fill
        tick_tx.send(make_tick("XXBTZUSD", dec!(65000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        let order = exchange.get_order_status(&order_id).await?;
        // Should be partially filled (first fill is partial, rest fills remaining)
        // With partial_fill_probability=1.0, the matching loop does one partial fill
        // but then needs another tick to fill the rest
        let fills = exchange.get_trade_history(None).await?;
        assert!(!fills.is_empty(), "should have at least one fill");

        // Send another tick to complete the fill if partially filled
        if order.status == OrderStatus::PartiallyFilled {
            tick_tx.send(make_tick("XXBTZUSD", dec!(65000))?)?;
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }

        // Eventually should be fully filled
        let final_fills = exchange.get_trade_history(None).await?;
        assert!(
            final_fills.len() >= 2,
            "should have multiple fills from partial filling"
        );

        exchange.shutdown().await;
        Ok(())
    }

    // ==================== Cancel Orders ====================

    #[tokio::test]
    async fn test_cancel_order_success() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        let order_id = exchange
            .place_order(&buy_limit("XXBTZUSD", dec!(1), dec!(60000))?)
            .await?;

        // Balance should be held
        let balances = exchange.get_balances().await?;
        let usd = balances
            .iter()
            .find(|b| b.currency == Currency::USD)
            .ok_or_else(|| anyhow::anyhow!("no USD"))?;
        assert_eq!(usd.held, Amount::new(dec!(60000)));

        // Cancel
        exchange.cancel_order(&order_id).await?;

        // Open orders should be empty
        assert!(exchange.get_open_orders().await?.is_empty());

        // Balance should be returned
        let balances = exchange.get_balances().await?;
        let usd = balances
            .iter()
            .find(|b| b.currency == Currency::USD)
            .ok_or_else(|| anyhow::anyhow!("no USD"))?;
        assert_eq!(usd.held, Amount::zero());
        assert_eq!(usd.available, Amount::new(dec!(100000)));

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_order_not_found() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        let fake_id = OrderId::new("paper-999")?;
        let result = exchange.cancel_order(&fake_id).await;
        assert!(result.is_err());

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_all_orders() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        exchange
            .place_order(&buy_limit("XXBTZUSD", dec!(0.1), dec!(60000))?)
            .await?;
        exchange
            .place_order(&buy_limit("XXBTZUSD", dec!(0.1), dec!(59000))?)
            .await?;

        let count = exchange.cancel_all_orders().await?;
        assert_eq!(count, 2);
        assert!(exchange.get_open_orders().await?.is_empty());

        exchange.shutdown().await;
        Ok(())
    }

    // ==================== Account Queries ====================

    #[tokio::test]
    async fn test_get_balances() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        let balances = exchange.get_balances().await?;
        assert_eq!(balances.len(), 2);
        assert!(balances.iter().any(|b| b.currency == Currency::USD));
        assert!(balances.iter().any(|b| b.currency == Currency::BTC));

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_get_positions_after_fills() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        // No positions initially
        assert!(exchange.get_positions().await?.is_empty());

        // Buy → creates position
        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(0.5))?)
            .await?;

        let positions = exchange.get_positions().await?;
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].quantity, Quantity::new(dec!(0.5))?);

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_get_trade_history_filtered_by_since() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        let before = Utc::now();
        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(0.1))?)
            .await?;
        let between = Utc::now();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(0.1))?)
            .await?;

        // All fills
        let all = exchange.get_trade_history(None).await?;
        assert_eq!(all.len(), 2);

        // Only fills after between
        let recent = exchange.get_trade_history(Some(between)).await?;
        assert_eq!(recent.len(), 1);

        // Fills after before should be all
        let all_since = exchange.get_trade_history(Some(before)).await?;
        assert_eq!(all_since.len(), 2);

        exchange.shutdown().await;
        Ok(())
    }

    // ==================== Position Tracking ====================

    #[tokio::test]
    async fn test_position_average_entry_price() -> anyhow::Result<()> {
        // Need more USD since we're buying 2 BTC at ~60k and ~70k
        let mut config = make_test_config();
        config.initial_balances = vec![(Currency::USD, dec!(200000)), (Currency::BTC, dec!(10))];
        let instruments = make_test_instruments()?;
        let (tick_tx, _) = broadcast::channel(64);
        let exchange = PaperExchange::new(config, instruments, &tick_tx)?;
        tick_tx.send(make_tick("XXBTZUSD", dec!(60000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let (exchange, tick_tx) = (exchange, tick_tx);

        // Buy 1 BTC at ~60000
        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(1))?)
            .await?;

        // Change price and buy more
        tick_tx.send(make_tick("XXBTZUSD", dec!(70000))?)?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(1))?)
            .await?;

        let positions = exchange.get_positions().await?;
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].quantity, Quantity::new(dec!(2))?);

        // Average entry should be between 60000 and 70000 (with slippage)
        let avg = positions[0].average_entry_price.value();
        assert!(avg > dec!(60000), "avg {avg} should be > 60000");
        assert!(avg < dec!(70100), "avg {avg} should be < 70100");

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_position_partial_close() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        // Open long 2 BTC
        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(1))?)
            .await?;

        // Sell 0.5 BTC to partially close — but we need to be sure the sell
        // side sees base available (the buy added BTC to available)
        exchange
            .place_order(&sell_market("XXBTZUSD", dec!(0.5))?)
            .await?;

        let positions = exchange.get_positions().await?;
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].side, OrderSide::Buy);
        assert_eq!(positions[0].quantity, Quantity::new(dec!(0.5))?);

        exchange.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_position_flip_side() -> anyhow::Result<()> {
        let (exchange, _tick_tx) = setup_exchange_with_price(dec!(67000)).await?;

        // Buy 1 BTC (long)
        exchange
            .place_order(&buy_market("XXBTZUSD", dec!(1))?)
            .await?;

        // Sell 2 BTC (should flip to short 1 BTC)
        // We have 11 BTC available (10 initial + 1 bought)
        exchange
            .place_order(&sell_market("XXBTZUSD", dec!(2))?)
            .await?;

        let positions = exchange.get_positions().await?;
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].side, OrderSide::Sell);
        assert_eq!(positions[0].quantity, Quantity::new(dec!(1))?);

        exchange.shutdown().await;
        Ok(())
    }
}
