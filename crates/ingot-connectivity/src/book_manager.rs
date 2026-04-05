use std::collections::{BTreeMap, HashMap};

use chrono::Utc;
use ingot_core::{OrderBookLevel, OrderBookSnapshot};
use ingot_primitives::{Price, Quantity, Symbol};
use rust_decimal::Decimal;

/// Maintains local order book state per symbol, applying snapshots
/// and incremental updates from WS book channels.
pub(crate) struct OrderBookManager {
    books: HashMap<Symbol, OrderBookState>,
}

struct OrderBookState {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
}

impl OrderBookManager {
    pub fn new() -> Self {
        Self {
            books: HashMap::new(),
        }
    }

    /// Replace the entire book for a symbol.
    pub fn apply_snapshot(
        &mut self,
        symbol: Symbol,
        bids: &[OrderBookLevel],
        asks: &[OrderBookLevel],
    ) {
        let mut state = OrderBookState {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
        };
        for bid in bids {
            state.bids.insert(bid.price.value(), bid.quantity.value());
        }
        for ask in asks {
            state.asks.insert(ask.price.value(), ask.quantity.value());
        }
        self.books.insert(symbol, state);
    }

    /// Merge incremental updates: qty=0 removes the level, qty>0 upserts.
    pub fn apply_update(
        &mut self,
        symbol: &Symbol,
        bids: &[OrderBookLevel],
        asks: &[OrderBookLevel],
    ) {
        if let Some(state) = self.books.get_mut(symbol) {
            for bid in bids {
                if bid.quantity.value() == Decimal::ZERO {
                    state.bids.remove(&bid.price.value());
                } else {
                    state.bids.insert(bid.price.value(), bid.quantity.value());
                }
            }
            for ask in asks {
                if ask.quantity.value() == Decimal::ZERO {
                    state.asks.remove(&ask.price.value());
                } else {
                    state.asks.insert(ask.price.value(), ask.quantity.value());
                }
            }
        }
    }

    /// Emit a full snapshot from the current local book state.
    /// Returns `None` if no book exists for the symbol.
    pub fn get_snapshot(&self, symbol: &Symbol) -> Option<OrderBookSnapshot> {
        let state = self.books.get(symbol)?;

        // Bids: descending by price (highest first)
        let bids: Vec<OrderBookLevel> = state
            .bids
            .iter()
            .rev()
            .filter_map(|(&price, &qty)| {
                Some(OrderBookLevel {
                    price: Price::new(price),
                    quantity: Quantity::new(qty).ok()?,
                })
            })
            .collect();

        // Asks: ascending by price (lowest first)
        let asks: Vec<OrderBookLevel> = state
            .asks
            .iter()
            .filter_map(|(&price, &qty)| {
                Some(OrderBookLevel {
                    price: Price::new(price),
                    quantity: Quantity::new(qty).ok()?,
                })
            })
            .collect();

        Some(OrderBookSnapshot {
            symbol: symbol.clone(),
            bids,
            asks,
            timestamp: Utc::now(),
        })
    }
}
