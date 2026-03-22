use std::{collections::HashMap, sync::Mutex};

use anyhow::Context;
use chrono::{DateTime, Utc};
use ingot_connectivity::{OrderExecutor, paper::fill_model};
use ingot_core::{Balance, OpenOrder, OrderFill, OrderId, OrderRequest, OrderStatus, Position};
use ingot_primitives::{Amount, Currency, OrderSide, OrderType, Price, Quantity, Symbol};
use rand::SeedableRng;
use rust_decimal::Decimal;

use crate::{config::BacktestConfig, error::BacktestError};

struct ExchangeState {
    balances: HashMap<Currency, Balance>,
    positions: HashMap<Symbol, Position>,
    open_orders: HashMap<OrderId, OpenOrder>,
    completed_orders: HashMap<OrderId, OpenOrder>,
    fills: Vec<OrderFill>,
    last_prices: HashMap<Symbol, Price>,
    symbol_currencies: HashMap<Symbol, (Currency, Currency)>,
    current_time: DateTime<Utc>,
    next_order_id: u64,
    next_fill_id: u64,
    rng: rand::rngs::StdRng,

    // Config fields
    slippage_bps: Decimal,
    maker_fee_bps: Decimal,
    taker_fee_bps: Decimal,
    partial_fill_probability: Decimal,
}

/// Synchronous, deterministic simulated exchange for backtesting.
///
/// Unlike `PaperExchange`, this exchange accepts historical timestamps,
/// uses a seeded RNG, and processes fills inline (no background matching loop).
pub struct BacktestExchange {
    state: Mutex<ExchangeState>,
}

impl BacktestExchange {
    /// Create a new backtest exchange from config.
    pub fn new(config: &BacktestConfig) -> Result<Self, BacktestError> {
        if config.slippage_bps < Decimal::ZERO {
            return Err(BacktestError::Exchange(format!(
                "slippage_bps must be non-negative, got {}",
                config.slippage_bps
            )));
        }
        if config.partial_fill_probability < Decimal::ZERO
            || config.partial_fill_probability > Decimal::ONE
        {
            return Err(BacktestError::Exchange(format!(
                "partial_fill_probability must be in [0, 1], got {}",
                config.partial_fill_probability
            )));
        }
        if config.maker_fee_bps < Decimal::ZERO {
            return Err(BacktestError::Exchange(format!(
                "maker_fee_bps must be non-negative, got {}",
                config.maker_fee_bps
            )));
        }
        if config.taker_fee_bps < Decimal::ZERO {
            return Err(BacktestError::Exchange(format!(
                "taker_fee_bps must be non-negative, got {}",
                config.taker_fee_bps
            )));
        }

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

        let rng = rand::rngs::StdRng::seed_from_u64(config.rng_seed);

        let state = ExchangeState {
            balances,
            positions: HashMap::new(),
            open_orders: HashMap::new(),
            completed_orders: HashMap::new(),
            fills: Vec::new(),
            last_prices: HashMap::new(),
            symbol_currencies: HashMap::new(),
            current_time: DateTime::UNIX_EPOCH,
            next_order_id: 1,
            next_fill_id: 1,
            rng,
            slippage_bps: config.slippage_bps,
            maker_fee_bps: config.maker_fee_bps,
            taker_fee_bps: config.taker_fee_bps,
            partial_fill_probability: config.partial_fill_probability,
        };

        Ok(Self {
            state: Mutex::new(state),
        })
    }

    /// Register a symbol's base and quote currencies.
    /// Must be called before placing orders for the symbol.
    pub fn register_symbol(&self, symbol: Symbol, base: Currency, quote: Currency) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.symbol_currencies.insert(symbol, (base, quote));
    }

    /// Advance time and set current price. Returns fills for any limit orders
    /// that crossed at this price.
    pub fn on_price_update(
        &self,
        symbol: &Symbol,
        price: Price,
        timestamp: DateTime<Utc>,
    ) -> Vec<OrderFill> {
        let symbol = symbol.clone();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        state.current_time = timestamp;
        state.last_prices.insert(symbol.clone(), price);

        // Find open limit orders that cross
        let matching_orders: Vec<(OrderId, OrderSide, Price, Quantity)> = state
            .open_orders
            .values()
            .filter(|o| {
                o.request.symbol == symbol
                    && o.request.order_type == OrderType::Limit
                    && o.request.limit_price.is_some()
            })
            .filter(|o| {
                let limit_price = o.request.limit_price.unwrap_or(Price::new(Decimal::ZERO));
                fill_model::tick_crosses_limit(price, limit_price, o.request.side)
            })
            .map(|o| {
                (
                    o.order_id.clone(),
                    o.request.side,
                    o.request.limit_price.unwrap_or(Price::new(Decimal::ZERO)),
                    o.remaining_quantity,
                )
            })
            .collect();

        let mut generated_fills = Vec::new();

        for (order_id, side, limit_price, remaining) in matching_orders {
            let symbol_clone = symbol.clone();
            let currencies = state.symbol_currencies.get(&symbol_clone).cloned();
            let Some((base_currency, quote_currency)) = currencies else {
                continue;
            };

            // Check for partial fill
            let fill_qty = fill_model::partial_fill_quantity(
                remaining,
                state.partial_fill_probability,
                &mut state.rng,
            );

            if let Some(partial) = fill_qty {
                // Partial fill first
                if let Ok(fill) = process_fill(
                    &mut state,
                    &order_id,
                    &symbol_clone,
                    side,
                    limit_price,
                    partial,
                    true,
                    &base_currency,
                    &quote_currency,
                ) {
                    generated_fills.push(fill);
                }
                // Fill the rest
                let rest = remaining - partial;
                if rest.value() > Decimal::ZERO
                    && let Ok(fill) = process_fill(
                        &mut state,
                        &order_id,
                        &symbol_clone,
                        side,
                        limit_price,
                        rest,
                        true,
                        &base_currency,
                        &quote_currency,
                    )
                {
                    generated_fills.push(fill);
                }
            } else {
                // Full fill
                if let Ok(fill) = process_fill(
                    &mut state,
                    &order_id,
                    &symbol_clone,
                    side,
                    limit_price,
                    remaining,
                    true,
                    &base_currency,
                    &quote_currency,
                ) {
                    generated_fills.push(fill);
                }
            }
        }

        generated_fills
    }

    /// Current simulated time.
    pub fn current_time(&self) -> DateTime<Utc> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current_time
    }

    /// All fills generated during the backtest.
    pub fn fills(&self) -> Vec<OrderFill> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .fills
            .clone()
    }

    /// Current positions snapshot.
    pub fn positions(&self) -> HashMap<Symbol, Position> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .positions
            .clone()
    }

    /// Current balances snapshot.
    pub fn balances(&self) -> HashMap<Currency, Balance> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .balances
            .clone()
    }

    /// Last known prices per symbol.
    pub fn last_prices(&self) -> HashMap<Symbol, Price> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_prices
            .clone()
    }

    /// Symbol-to-currency mapping.
    pub fn symbol_currencies(&self) -> HashMap<Symbol, (Currency, Currency)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .symbol_currencies
            .clone()
    }
}

impl OrderExecutor for BacktestExchange {
    async fn place_order(&self, request: &OrderRequest) -> anyhow::Result<OrderId> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let (base_currency, quote_currency) = state
            .symbol_currencies
            .get(&request.symbol)
            .cloned()
            .ok_or_else(|| {
                BacktestError::Exchange(format!("symbol {} not registered", request.symbol))
            })
            .context("resolving symbol currencies")?;

        let seq = state.next_order_id;
        state.next_order_id += 1;
        let order_id =
            OrderId::new(&format!("bt-{seq}")).context("generating backtest order id")?;

        match request.order_type {
            OrderType::Market => {
                execute_market_order(
                    &mut state,
                    &order_id,
                    request,
                    &base_currency,
                    &quote_currency,
                )?;
            }
            OrderType::Limit => {
                let limit_price = request
                    .limit_price
                    .ok_or_else(|| {
                        BacktestError::Exchange("limit order requires limit_price".into())
                    })
                    .context("validating limit order")?;

                place_limit_order(
                    &mut state,
                    &order_id,
                    request,
                    limit_price,
                    &base_currency,
                    &quote_currency,
                )?;
            }
            _ => {
                return Err(BacktestError::Exchange(format!(
                    "order type {:?} not supported in backtest exchange",
                    request.order_type
                )))
                .context("validating order type");
            }
        }

        Ok(order_id)
    }

    async fn cancel_order(&self, order_id: &OrderId) -> anyhow::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cancel_order_impl(&mut state, order_id)
    }

    async fn cancel_all_orders(&self) -> anyhow::Result<u32> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cancel_all_impl(&mut state)
    }

    async fn get_order_status(&self, order_id: &OrderId) -> anyhow::Result<OpenOrder> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(order) = state.open_orders.get(order_id) {
            return Ok(order.clone());
        }
        if let Some(order) = state.completed_orders.get(order_id) {
            return Ok(order.clone());
        }
        Err(BacktestError::Exchange(format!(
            "order {order_id} not found"
        )))
        .context("looking up order status")
    }

    async fn get_open_orders(&self) -> anyhow::Result<Vec<OpenOrder>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(state.open_orders.values().cloned().collect())
    }
}

// --- Free functions operating on ExchangeState ---

fn execute_market_order(
    state: &mut ExchangeState,
    order_id: &OrderId,
    request: &OrderRequest,
    base_currency: &Currency,
    quote_currency: &Currency,
) -> anyhow::Result<()> {
    let last_price = state
        .last_prices
        .get(&request.symbol)
        .copied()
        .ok_or_else(|| BacktestError::Exchange("no market data available for symbol".into()))
        .context("checking last price for market order")?;

    let fill_price = fill_model::apply_slippage(last_price, request.side, state.slippage_bps);

    check_and_hold_balance(
        state,
        request.side,
        fill_price,
        request.quantity,
        base_currency,
        quote_currency,
    )?;

    let open_order = OpenOrder {
        order_id: order_id.clone(),
        request: request.clone(),
        status: OrderStatus::Pending,
        filled_quantity: Quantity::zero(),
        remaining_quantity: request.quantity,
        average_fill_price: None,
        created_at: state.current_time,
    };
    state.open_orders.insert(order_id.clone(), open_order);

    // Execute fill (possibly partial)
    let mut remaining = request.quantity;
    let partial_qty = fill_model::partial_fill_quantity(
        remaining,
        state.partial_fill_probability,
        &mut state.rng,
    );

    if let Some(partial) = partial_qty {
        process_fill(
            state,
            order_id,
            &request.symbol,
            request.side,
            fill_price,
            partial,
            false,
            base_currency,
            quote_currency,
        )?;
        remaining = remaining - partial;
    }

    if remaining.value() > Decimal::ZERO {
        process_fill(
            state,
            order_id,
            &request.symbol,
            request.side,
            fill_price,
            remaining,
            false,
            base_currency,
            quote_currency,
        )?;
    }

    Ok(())
}

fn place_limit_order(
    state: &mut ExchangeState,
    order_id: &OrderId,
    request: &OrderRequest,
    limit_price: Price,
    base_currency: &Currency,
    quote_currency: &Currency,
) -> anyhow::Result<()> {
    check_and_hold_balance(
        state,
        request.side,
        limit_price,
        request.quantity,
        base_currency,
        quote_currency,
    )?;

    let open_order = OpenOrder {
        order_id: order_id.clone(),
        request: request.clone(),
        status: OrderStatus::Open,
        filled_quantity: Quantity::zero(),
        remaining_quantity: request.quantity,
        average_fill_price: None,
        created_at: state.current_time,
    };

    state.open_orders.insert(order_id.clone(), open_order);

    Ok(())
}

fn check_and_hold_balance(
    state: &mut ExchangeState,
    side: OrderSide,
    price: Price,
    quantity: Quantity,
    base_currency: &Currency,
    quote_currency: &Currency,
) -> anyhow::Result<()> {
    let cost = price * quantity;

    match side {
        OrderSide::Buy => {
            let balance = state
                .balances
                .get_mut(quote_currency)
                .ok_or_else(|| {
                    BacktestError::Exchange(format!(
                        "insufficient balance: need {}, have 0",
                        cost.value()
                    ))
                })
                .context("checking quote balance for buy")?;

            if balance.available.value() < cost.value() {
                return Err(BacktestError::Exchange(format!(
                    "insufficient balance: need {}, have {}",
                    cost.value(),
                    balance.available.value()
                )))
                .context("insufficient quote balance for buy");
            }

            balance.available = balance.available - cost;
            balance.held = balance.held + cost;
        }
        OrderSide::Sell => {
            let has_base = state
                .balances
                .get(base_currency)
                .is_some_and(|b| b.available.value() >= quantity.value());

            if has_base {
                let balance = state
                    .balances
                    .get_mut(base_currency)
                    .ok_or_else(|| {
                        BacktestError::Exchange(format!(
                            "insufficient balance: need {}, have 0",
                            quantity.value()
                        ))
                    })
                    .context("checking base balance for sell")?;
                let qty_amount = Amount::new(quantity.value());
                balance.available = balance.available - qty_amount;
                balance.held = balance.held + qty_amount;
            } else {
                let balance = state
                    .balances
                    .get_mut(quote_currency)
                    .ok_or_else(|| {
                        BacktestError::Exchange(format!(
                            "insufficient balance: need {}, have 0",
                            cost.value()
                        ))
                    })
                    .context("checking quote balance for short sell")?;

                if balance.available.value() < cost.value() {
                    return Err(BacktestError::Exchange(format!(
                        "insufficient balance: need {}, have {}",
                        cost.value(),
                        balance.available.value()
                    )))
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
fn process_fill(
    state: &mut ExchangeState,
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
        state.maker_fee_bps
    } else {
        state.taker_fee_bps
    };
    let fee = fill_model::calculate_fee(fill_price, fill_qty, fee_bps);
    let cost = fill_price * fill_qty;

    update_balances_for_fill(
        state,
        side,
        cost,
        fee,
        fill_qty,
        base_currency,
        quote_currency,
    );
    update_position(state, symbol, side, fill_price, fill_qty)?;
    update_order_on_fill(state, order_id, fill_price, fill_qty);

    let fill_seq = state.next_fill_id;
    state.next_fill_id += 1;
    let order_fill = OrderFill {
        order_id: order_id.clone(),
        symbol: symbol.clone(),
        side,
        fill_price,
        fill_quantity: fill_qty,
        fee,
        fee_currency: quote_currency.clone(),
        timestamp: state.current_time,
        trade_id: Some(smol_str::SmolStr::new(format!("bt-fill-{fill_seq}"))),
    };

    state.fills.push(order_fill.clone());
    Ok(order_fill)
}

#[allow(clippy::too_many_arguments)]
fn update_balances_for_fill(
    state: &mut ExchangeState,
    side: OrderSide,
    cost: Amount,
    fee: Amount,
    fill_qty: Quantity,
    base_currency: &Currency,
    quote_currency: &Currency,
) {
    match side {
        OrderSide::Buy => {
            if let Some(quote_bal) = state.balances.get_mut(quote_currency) {
                // Release held cost (spent on purchase) and deduct fee
                quote_bal.held = quote_bal.held - cost;
                quote_bal.total = quote_bal.total - cost;
                quote_bal.total = quote_bal.total - fee;
                quote_bal.held = quote_bal.held - fee;
            }
            let base_bal = state
                .balances
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
            let base_held = state
                .balances
                .get(base_currency)
                .map_or(Decimal::ZERO, |b| b.held.value());

            if base_held >= fill_qty.value() {
                if let Some(base_bal) = state.balances.get_mut(base_currency) {
                    let qty_amount = Amount::new(fill_qty.value());
                    base_bal.held = base_bal.held - qty_amount;
                    base_bal.total = base_bal.total - qty_amount;
                }
                let quote_bal = state
                    .balances
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
            } else if let Some(quote_bal) = state.balances.get_mut(quote_currency) {
                quote_bal.held = quote_bal.held - cost;
                quote_bal.total = quote_bal.total - fee;
                quote_bal.held = quote_bal.held - fee;
            }
        }
    }
}

fn update_position(
    state: &mut ExchangeState,
    symbol: &Symbol,
    side: OrderSide,
    fill_price: Price,
    fill_qty: Quantity,
) -> anyhow::Result<()> {
    match state.positions.get(symbol) {
        Some(existing) if existing.side == side => {
            let pos = state
                .positions
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
                    let pos = state
                        .positions
                        .get_mut(symbol)
                        .ok_or_else(|| anyhow::anyhow!("position disappeared"))
                        .context("partially closing position")?;
                    pos.quantity = existing_qty - fill_qty;
                }
                std::cmp::Ordering::Equal => {
                    state.positions.remove(symbol);
                }
                std::cmp::Ordering::Greater => {
                    let remaining_qty = fill_qty - existing_qty;
                    if remaining_qty.value() > Decimal::ZERO {
                        state.positions.insert(
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
                        state.positions.remove(symbol);
                    }
                }
            }
        }
        None => {
            state.positions.insert(
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

fn update_order_on_fill(
    state: &mut ExchangeState,
    order_id: &OrderId,
    fill_price: Price,
    fill_qty: Quantity,
) {
    let should_complete = if let Some(order) = state.open_orders.get_mut(order_id) {
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
            true
        } else {
            order.status = OrderStatus::PartiallyFilled;
            false
        }
    } else {
        false
    };

    if should_complete && let Some(completed) = state.open_orders.remove(order_id) {
        state.completed_orders.insert(order_id.clone(), completed);
    }
}

fn cancel_order_impl(state: &mut ExchangeState, order_id: &OrderId) -> anyhow::Result<()> {
    let order = state
        .open_orders
        .remove(order_id)
        .ok_or_else(|| BacktestError::Exchange(format!("order {order_id} not found")))
        .context("cancelling order")?;

    let remaining = order.remaining_quantity;

    // Return held funds
    let currencies = state.symbol_currencies.get(&order.request.symbol).cloned();
    if let Some((base_currency, quote_currency)) = currencies {
        match order.request.side {
            OrderSide::Buy => {
                let limit_price = order
                    .request
                    .limit_price
                    .unwrap_or(Price::new(Decimal::ZERO));
                let held_amount = limit_price * remaining;
                if let Some(bal) = state.balances.get_mut(&quote_currency) {
                    bal.held = bal.held - held_amount;
                    bal.available = bal.available + held_amount;
                }
            }
            OrderSide::Sell => {
                let base_held = state
                    .balances
                    .get(&base_currency)
                    .map_or(Decimal::ZERO, |b| b.held.value());
                if base_held >= remaining.value() {
                    if let Some(bal) = state.balances.get_mut(&base_currency) {
                        let qty_amount = Amount::new(remaining.value());
                        bal.held = bal.held - qty_amount;
                        bal.available = bal.available + qty_amount;
                    }
                } else {
                    let limit_price = order
                        .request
                        .limit_price
                        .unwrap_or(Price::new(Decimal::ZERO));
                    let held_amount = limit_price * remaining;
                    if let Some(bal) = state.balances.get_mut(&quote_currency) {
                        bal.held = bal.held - held_amount;
                        bal.available = bal.available + held_amount;
                    }
                }
            }
        }
    }

    let mut cancelled = order;
    cancelled.status = OrderStatus::Cancelled;
    state.completed_orders.insert(order_id.clone(), cancelled);

    Ok(())
}

fn cancel_all_impl(state: &mut ExchangeState) -> anyhow::Result<u32> {
    let order_ids: Vec<OrderId> = state.open_orders.keys().cloned().collect();
    let mut count = 0u32;
    for id in &order_ids {
        cancel_order_impl(state, id)?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use ingot_connectivity::OrderExecutor;
    use ingot_engine::{RiskConfig, SmartOrderConfig};
    use ingot_primitives::{
        Amount, Currency, OrderSide, OrderType, Percentage, Price, Quantity, Symbol, TimeInForce,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::config::BacktestConfig;

    fn sample_config() -> Result<BacktestConfig, Box<dyn std::error::Error>> {
        Ok(BacktestConfig {
            initial_balances: vec![(Currency::USD, dec!(100000)), (Currency::BTC, dec!(10))],
            base_currency: Currency::USD,
            slippage_bps: dec!(10),
            maker_fee_bps: dec!(16),
            taker_fee_bps: dec!(26),
            partial_fill_probability: dec!(0.0),
            rng_seed: 42,
            risk: RiskConfig {
                global_stop_loss: Amount::new(dec!(10000)),
                max_currency_exposure: Percentage::new(dec!(0.50))?,
                max_asset_exposure: Percentage::new(dec!(0.30))?,
                max_order_value: Amount::new(dec!(50000)),
            },
            smart_order: SmartOrderConfig {
                use_mid_price: false,
                offset_bps: dec!(0),
                fallback_timeout_ms: 30_000,
            },
        })
    }

    fn symbol_btcusd() -> Result<Symbol, Box<dyn std::error::Error>> {
        Ok(Symbol::new("BTCUSD")?)
    }

    fn setup_exchange_with_price(
        config: &BacktestConfig,
    ) -> Result<BacktestExchange, Box<dyn std::error::Error>> {
        let exchange = BacktestExchange::new(config)?;
        let symbol = symbol_btcusd()?;
        exchange.register_symbol(symbol.clone(), Currency::BTC, Currency::USD);
        let ts = chrono::DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")?.to_utc();
        exchange.on_price_update(&symbol, Price::new(dec!(67000)), ts);
        Ok(exchange)
    }

    // --- Test 1: Construction - initial balances ---
    #[test]
    fn test_exchange_new_initial_balances() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = BacktestExchange::new(&config)?;
        let balances = exchange.balances();

        assert_eq!(balances.len(), 2);
        let usd = balances.get(&Currency::USD).ok_or("no USD balance")?;
        assert_eq!(usd.total, Amount::new(dec!(100000)));
        assert_eq!(usd.available, Amount::new(dec!(100000)));
        assert_eq!(usd.held, Amount::zero());

        let btc = balances.get(&Currency::BTC).ok_or("no BTC balance")?;
        assert_eq!(btc.total, Amount::new(dec!(10)));
        assert_eq!(btc.available, Amount::new(dec!(10)));
        assert_eq!(btc.held, Amount::zero());
        Ok(())
    }

    // --- Test 2: Construction - initial time is UNIX_EPOCH ---
    #[test]
    fn test_exchange_new_zero_time() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = BacktestExchange::new(&config)?;
        assert_eq!(exchange.current_time(), DateTime::UNIX_EPOCH);
        Ok(())
    }

    // --- Test 3: Market buy fills immediately ---
    #[tokio::test]
    async fn test_place_market_order_buy_fills_immediately()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };

        let order_id = exchange.place_order(&request).await?;

        let fills = exchange.fills();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].order_id, order_id);
        assert_eq!(fills[0].side, OrderSide::Buy);
        // Price should be 67000 + slippage (10bps = 0.1%)
        // 67000 * 10 / 10000 = 67.0 → fill_price = 67067.0
        let expected_price = Price::new(dec!(67067.0));
        assert_eq!(fills[0].fill_price, expected_price);
        Ok(())
    }

    // --- Test 4: Market sell fills immediately ---
    #[tokio::test]
    async fn test_place_market_order_sell_fills_immediately()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol: symbol.clone(),
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };

        let order_id = exchange.place_order(&request).await?;

        let fills = exchange.fills();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].order_id, order_id);
        assert_eq!(fills[0].side, OrderSide::Sell);
        // 67000 - 67000 * 10/10000 = 67000 - 67 = 66933
        let expected_price = Price::new(dec!(66933.0));
        assert_eq!(fills[0].fill_price, expected_price);
        Ok(())
    }

    // --- Test 5: Market order fee ---
    #[tokio::test]
    async fn test_place_market_order_fee_calculated() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };

        exchange.place_order(&request).await?;

        let fills = exchange.fills();
        assert_eq!(fills.len(), 1);
        // fee = fill_price * qty * taker_fee_bps / 10000
        // = 67067 * 1 * 26 / 10000 = 174.3742
        let expected_fee = fill_model::calculate_fee(
            fills[0].fill_price,
            fills[0].fill_quantity,
            config.taker_fee_bps,
        );
        assert_eq!(fills[0].fee, expected_fee);
        Ok(())
    }

    // --- Test 6: Limit order stored as open ---
    #[tokio::test]
    async fn test_place_limit_order_stored() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(1))?,
            limit_price: Some(Price::new(dec!(60000))),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        };

        let order_id = exchange.place_order(&request).await?;

        let open = exchange.get_open_orders().await?;
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].order_id, order_id);
        assert_eq!(open[0].status, OrderStatus::Open);

        // No fills yet
        assert!(exchange.fills().is_empty());
        Ok(())
    }

    // --- Test 7: Price update crosses buy limit ---
    #[tokio::test]
    async fn test_on_price_update_crosses_buy_limit() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(1))?,
            limit_price: Some(Price::new(dec!(65000))),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        };

        exchange.place_order(&request).await?;
        assert!(exchange.fills().is_empty());

        // Price drops below limit → fill
        let ts = chrono::DateTime::parse_from_rfc3339("2025-01-01T01:00:00Z")?.to_utc();
        let fills = exchange.on_price_update(&symbol, Price::new(dec!(64000)), ts);

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].fill_price, Price::new(dec!(65000)));
        assert_eq!(fills[0].side, OrderSide::Buy);

        // Order should be completed now
        let open = exchange.get_open_orders().await?;
        assert!(open.is_empty());
        Ok(())
    }

    // --- Test 8: Price update crosses sell limit ---
    #[tokio::test]
    async fn test_on_price_update_crosses_sell_limit() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        // We have 10 BTC available
        let request = OrderRequest {
            symbol: symbol.clone(),
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(1))?,
            limit_price: Some(Price::new(dec!(70000))),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        };

        exchange.place_order(&request).await?;
        assert!(exchange.fills().is_empty());

        // Price rises above limit → fill
        let ts = chrono::DateTime::parse_from_rfc3339("2025-01-01T01:00:00Z")?.to_utc();
        let fills = exchange.on_price_update(&symbol, Price::new(dec!(71000)), ts);

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].fill_price, Price::new(dec!(70000)));
        assert_eq!(fills[0].side, OrderSide::Sell);

        let open = exchange.get_open_orders().await?;
        assert!(open.is_empty());
        Ok(())
    }

    // --- Test 9: Price update doesn't cross ---
    #[tokio::test]
    async fn test_on_price_update_no_cross() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(1))?,
            limit_price: Some(Price::new(dec!(60000))),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        };

        exchange.place_order(&request).await?;

        // Price stays above limit → no fill
        let ts = chrono::DateTime::parse_from_rfc3339("2025-01-01T01:00:00Z")?.to_utc();
        let fills = exchange.on_price_update(&symbol, Price::new(dec!(66000)), ts);

        assert!(fills.is_empty());

        let open = exchange.get_open_orders().await?;
        assert_eq!(open.len(), 1);
        Ok(())
    }

    // --- Test 10: Partial fill with seed ---
    #[tokio::test]
    async fn test_partial_fill_with_seed() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = sample_config()?;
        config.partial_fill_probability = dec!(1.0);
        config.rng_seed = 42;

        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        // Use 1 BTC (≈67067 USD cost after slippage, within 100k balance)
        let request = OrderRequest {
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };

        exchange.place_order(&request).await?;

        // With partial_fill_probability=1.0, we should get 2 fills (partial + rest)
        let fills = exchange.fills();
        assert_eq!(fills.len(), 2);

        let total_qty = fills[0].fill_quantity + fills[1].fill_quantity;
        assert_eq!(total_qty, Quantity::new(dec!(1))?);

        // Partial should be 20-80% of 1
        assert!(fills[0].fill_quantity.value() >= dec!(0.2));
        assert!(fills[0].fill_quantity.value() <= dec!(0.8));
        Ok(())
    }

    // --- Test 11: Cancel order ---
    #[tokio::test]
    async fn test_cancel_order() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(1))?,
            limit_price: Some(Price::new(dec!(60000))),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        };

        let order_id = exchange.place_order(&request).await?;

        // Balance should have held amount
        let balances_before = exchange.balances();
        let usd_before = balances_before.get(&Currency::USD).ok_or("no USD")?;
        assert!(usd_before.held.value() > Decimal::ZERO);

        exchange.cancel_order(&order_id).await?;

        // Balance should be restored
        let balances_after = exchange.balances();
        let usd_after = balances_after.get(&Currency::USD).ok_or("no USD")?;
        assert_eq!(usd_after.held, Amount::zero());
        assert_eq!(usd_after.available, Amount::new(dec!(100000)));

        let open = exchange.get_open_orders().await?;
        assert!(open.is_empty());

        // Should be in completed as cancelled
        let status = exchange.get_order_status(&order_id).await?;
        assert_eq!(status.status, OrderStatus::Cancelled);
        Ok(())
    }

    // --- Test 12: Cancel all orders ---
    #[tokio::test]
    async fn test_cancel_all_orders() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        for limit in [dec!(20000), dec!(25000), dec!(30000)] {
            let request = OrderRequest {
                symbol: symbol.clone(),
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: Quantity::new(dec!(1))?,
                limit_price: Some(Price::new(limit)),
                stop_price: None,
                time_in_force: TimeInForce::GoodTilCancelled,
            };
            exchange.place_order(&request).await?;
        }

        let open = exchange.get_open_orders().await?;
        assert_eq!(open.len(), 3);

        let cancelled = exchange.cancel_all_orders().await?;
        assert_eq!(cancelled, 3);

        let open = exchange.get_open_orders().await?;
        assert!(open.is_empty());
        Ok(())
    }

    // --- Test 13: Get order status ---
    #[tokio::test]
    async fn test_get_order_status() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        // Open limit order
        let limit_request = OrderRequest {
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(1))?,
            limit_price: Some(Price::new(dec!(60000))),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        };
        let limit_id = exchange.place_order(&limit_request).await?;

        // Filled market order
        let market_request = OrderRequest {
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(0.1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };
        let market_id = exchange.place_order(&market_request).await?;

        let limit_status = exchange.get_order_status(&limit_id).await?;
        assert_eq!(limit_status.status, OrderStatus::Open);

        let market_status = exchange.get_order_status(&market_id).await?;
        assert_eq!(market_status.status, OrderStatus::Filled);
        Ok(())
    }

    // --- Test 14: Get open orders ---
    #[tokio::test]
    async fn test_get_open_orders() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        // Place a limit order (stays open)
        let limit_request = OrderRequest {
            symbol: symbol.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::new(dec!(1))?,
            limit_price: Some(Price::new(dec!(60000))),
            stop_price: None,
            time_in_force: TimeInForce::GoodTilCancelled,
        };
        exchange.place_order(&limit_request).await?;

        // Place a market order (fills immediately)
        let market_request = OrderRequest {
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(0.1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };
        exchange.place_order(&market_request).await?;

        let open = exchange.get_open_orders().await?;
        // Only the limit order should be open
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].request.order_type, OrderType::Limit);
        Ok(())
    }

    // --- Test 15: Balance updated on buy fill ---
    #[tokio::test]
    async fn test_balance_updated_on_buy_fill() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };

        exchange.place_order(&request).await?;

        let balances = exchange.balances();
        let fills = exchange.fills();
        let fill = &fills[0];

        // USD total should be reduced by cost + fee
        let cost = fill.fill_price * fill.fill_quantity;
        let usd = balances.get(&Currency::USD).ok_or("no USD")?;
        assert_eq!(usd.total, Amount::new(dec!(100000)) - cost - fill.fee);

        // BTC should have increased by fill quantity
        let btc = balances.get(&Currency::BTC).ok_or("no BTC")?;
        assert_eq!(
            btc.total,
            Amount::new(dec!(10)) + Amount::new(fill.fill_quantity.value())
        );

        // Position should exist
        let positions = exchange.positions();
        assert!(!positions.is_empty());
        Ok(())
    }

    // --- Test 16: Balance updated on sell fill ---
    #[tokio::test]
    async fn test_balance_updated_on_sell_fill() -> Result<(), Box<dyn std::error::Error>> {
        let config = sample_config()?;
        let exchange = setup_exchange_with_price(&config)?;
        let symbol = symbol_btcusd()?;

        let request = OrderRequest {
            symbol,
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity: Quantity::new(dec!(1))?,
            limit_price: None,
            stop_price: None,
            time_in_force: TimeInForce::ImmediateOrCancel,
        };

        exchange.place_order(&request).await?;

        let balances = exchange.balances();
        let fills = exchange.fills();
        let fill = &fills[0];

        // BTC total should decrease by fill quantity
        let btc = balances.get(&Currency::BTC).ok_or("no BTC")?;
        assert_eq!(
            btc.total,
            Amount::new(dec!(10)) - Amount::new(fill.fill_quantity.value())
        );

        // USD should increase by proceeds (cost - fee)
        let cost = fill.fill_price * fill.fill_quantity;
        let proceeds = cost - fill.fee;
        let usd = balances.get(&Currency::USD).ok_or("no USD")?;
        assert_eq!(usd.total, Amount::new(dec!(100000)) + proceeds);
        Ok(())
    }

    // --- Test 17: Deterministic fills same seed ---
    #[tokio::test]
    async fn test_deterministic_fills_same_seed() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = sample_config()?;
        config.partial_fill_probability = dec!(1.0);
        config.rng_seed = 123;

        let fills_1 = {
            let exchange = setup_exchange_with_price(&config)?;
            let symbol = symbol_btcusd()?;
            let request = OrderRequest {
                symbol,
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: Quantity::new(dec!(1))?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::ImmediateOrCancel,
            };
            exchange.place_order(&request).await?;
            exchange.fills()
        };

        let fills_2 = {
            let exchange = setup_exchange_with_price(&config)?;
            let symbol = symbol_btcusd()?;
            let request = OrderRequest {
                symbol,
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: Quantity::new(dec!(1))?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::ImmediateOrCancel,
            };
            exchange.place_order(&request).await?;
            exchange.fills()
        };

        assert_eq!(fills_1.len(), fills_2.len());
        for (f1, f2) in fills_1.iter().zip(fills_2.iter()) {
            assert_eq!(f1.fill_price, f2.fill_price);
            assert_eq!(f1.fill_quantity, f2.fill_quantity);
            assert_eq!(f1.fee, f2.fee);
        }
        Ok(())
    }

    // --- Test 18: Different seeds produce different partial fills ---
    #[tokio::test]
    async fn test_deterministic_fills_different_seed() -> Result<(), Box<dyn std::error::Error>> {
        let mut config1 = sample_config()?;
        config1.partial_fill_probability = dec!(1.0);
        config1.rng_seed = 42;

        let mut config2 = sample_config()?;
        config2.partial_fill_probability = dec!(1.0);
        config2.rng_seed = 999;

        let fills_1 = {
            let exchange = setup_exchange_with_price(&config1)?;
            let symbol = symbol_btcusd()?;
            let request = OrderRequest {
                symbol,
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: Quantity::new(dec!(1))?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::ImmediateOrCancel,
            };
            exchange.place_order(&request).await?;
            exchange.fills()
        };

        let fills_2 = {
            let exchange = setup_exchange_with_price(&config2)?;
            let symbol = symbol_btcusd()?;
            let request = OrderRequest {
                symbol,
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: Quantity::new(dec!(1))?,
                limit_price: None,
                stop_price: None,
                time_in_force: TimeInForce::ImmediateOrCancel,
            };
            exchange.place_order(&request).await?;
            exchange.fills()
        };

        // Both should have 2 fills (partial + rest), but different quantities
        assert_eq!(fills_1.len(), 2);
        assert_eq!(fills_2.len(), 2);
        // The partial fill quantities should differ due to different seeds
        assert_ne!(fills_1[0].fill_quantity, fills_2[0].fill_quantity);
        Ok(())
    }
}

// --- Proptests ---
#[cfg(test)]
mod proptests {
    use ingot_connectivity::paper::fill_model;
    use ingot_primitives::{OrderSide, Price, Quantity};
    use proptest::prelude::*;
    use rust_decimal::Decimal;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn prop_test_fill_price_includes_slippage(
            price_val in 1i64..1_000_000i64,
            slippage in 0i64..1000i64,
            is_buy in proptest::bool::ANY,
        ) {
            let price = Price::new(Decimal::new(price_val, 0));
            let slippage_bps = Decimal::new(slippage, 0);
            let side = if is_buy { OrderSide::Buy } else { OrderSide::Sell };

            let fill = fill_model::apply_slippage(price, side, slippage_bps);

            match side {
                OrderSide::Buy => prop_assert!(fill.value() >= price.value()),
                OrderSide::Sell => prop_assert!(fill.value() <= price.value()),
            }
        }

        #[test]
        fn prop_test_fee_always_non_negative(
            price_val in 1i64..1_000_000i64,
            qty_val in 1i64..10000i64,
            fee_bps in 0i64..1000i64,
        ) {
            let price = Price::new(Decimal::new(price_val, 0));
            let qty = Quantity::new(Decimal::new(qty_val, 2));
            if let Ok(qty) = qty {
                let fee = fill_model::calculate_fee(price, qty, Decimal::new(fee_bps, 0));
                prop_assert!(fee.value() >= Decimal::ZERO);
            }
        }
    }
}
