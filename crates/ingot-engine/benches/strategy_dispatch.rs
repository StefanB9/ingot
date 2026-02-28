use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use ingot_core::{
    accounting::{ExchangeRate, QuoteBoard},
    execution::OrderRequest,
    feed::Ticker,
};
use ingot_engine::strategy::{QuoteBoardStrategy, Strategy, StrategyContext};
use ingot_primitives::{Currency, OrderSide, Price, Quantity, Symbol};
use rust_decimal::dec;
use smallvec::SmallVec;

/// No-op strategy for measuring per-strategy dispatch overhead.
struct NoopStrategy;

impl Strategy for NoopStrategy {
    fn name(&self) -> &'static str {
        "Noop"
    }

    fn on_tick(&mut self, _tick: &Ticker, _ctx: &mut StrategyContext<'_>) {}
}

fn make_tick() -> Ticker {
    Ticker::new("BTC-USD", dec!(50000))
}

fn make_board_with_rate() -> QuoteBoard {
    let mut board = QuoteBoard::new();
    let btc = Currency::btc();
    let usd = Currency::usd();
    if let Ok(rate) = ExchangeRate::new(btc, usd, Price::from(dec!(50000))) {
        board.add_rate(rate);
    }
    board
}

fn bench_single_quoteboard_strategy(c: &mut Criterion) {
    let mut strategy = QuoteBoardStrategy;
    let tick = make_tick();

    c.bench_function("single_quoteboard_strategy_dispatch", |b| {
        b.iter(|| {
            let mut board = QuoteBoard::new();
            let mut orders = SmallVec::new();
            let mut ctx = StrategyContext {
                market_data: &mut board,
                orders: &mut orders,
            };
            strategy.on_tick(black_box(&tick), &mut ctx);
        });
    });
}

fn bench_multi_strategy_fanout(c: &mut Criterion) {
    let tick = make_tick();

    let mut group = c.benchmark_group("multi_strategy_fanout");
    for n in [1, 4, 8] {
        group.bench_with_input(
            criterion::BenchmarkId::from_parameter(n),
            &n,
            |b, &count| {
                let mut strategies: Vec<Box<dyn Strategy>> = vec![Box::new(QuoteBoardStrategy)];
                for _ in 1..count {
                    strategies.push(Box::new(NoopStrategy));
                }

                b.iter(|| {
                    let mut board = QuoteBoard::new();
                    let mut orders = SmallVec::new();
                    let mut ctx = StrategyContext {
                        market_data: &mut board,
                        orders: &mut orders,
                    };
                    for strategy in &mut strategies {
                        strategy.on_tick(black_box(&tick), &mut ctx);
                    }
                });
            },
        );
    }
    group.finish();
}

fn bench_noop_strategy_overhead(c: &mut Criterion) {
    let mut strategy = NoopStrategy;
    let tick = make_tick();

    c.bench_function("noop_strategy_overhead", |b| {
        b.iter(|| {
            let mut board = make_board_with_rate();
            let mut orders = SmallVec::new();
            let mut ctx = StrategyContext {
                market_data: &mut board,
                orders: &mut orders,
            };
            strategy.on_tick(black_box(&tick), &mut ctx);
        });
    });
}

fn bench_order_emission_smallvec(c: &mut Criterion) {
    let tick = make_tick();

    c.bench_function("order_emission_smallvec", |b| {
        b.iter(|| {
            let mut board = make_board_with_rate();
            let mut orders: SmallVec<OrderRequest, 2> = SmallVec::new();
            orders.push(OrderRequest::new_market(
                Symbol::new("BTC-USD"),
                OrderSide::Buy,
                Quantity::from(dec!(0.01)),
            ));
            let mut ctx = StrategyContext {
                market_data: &mut board,
                orders: &mut orders,
            };
            // Measure the overhead of a strategy that reads an already-pushed order
            let mut noop = NoopStrategy;
            noop.on_tick(black_box(&tick), &mut ctx);
            black_box(&orders);
        });
    });
}

criterion_group!(
    benches,
    bench_single_quoteboard_strategy,
    bench_multi_strategy_fanout,
    bench_noop_strategy_overhead,
    bench_order_emission_smallvec,
);
criterion_main!(benches);
