use std::{hint::black_box, str::FromStr};

use criterion::{Criterion, criterion_group, criterion_main};
use ingot_primitives::Symbol;
use rust_decimal::Decimal;

fn bench_symbol_normalization(c: &mut Criterion) {
    let mut group = c.benchmark_group("symbol normalization");
    group.bench_function("Symbol::from_kraken (hot path)", |b| {
        b.iter(|| Symbol::from_kraken(black_box("XBT/USD")));
    });
    group.bench_function("Symbol::parts", |b| {
        let sym = Symbol::new("BTC-USD");
        b.iter(|| sym.parts());
    });
    // Baseline: the old String-based normalization path (pre-Symbol::from_kraken)
    group.bench_function("String::replace chain (old baseline)", |b| {
        b.iter(|| {
            let pair: &str = black_box("XBT/USD");
            pair.replace("XBT", "BTC").replace('/', "-")
        });
    });
    group.finish();
}

fn bench_price_parsing(c: &mut Criterion) {
    c.bench_function("Decimal::from_str (price parse)", |b| {
        b.iter(|| Decimal::from_str(black_box("50000.00000")));
    });
}

fn bench_ws_message_deserialize(c: &mut Criterion) {
    let ticker_msg = r#"[345,{"c":["50000.00000","0.0023"]},"ticker","XBT/USD"]"#;
    let heartbeat_msg = r#"{"event":"heartbeat"}"#;
    let subscription_msg = r#"{"channelID":345,"channelName":"ticker","event":"subscriptionStatus","pair":"XBT/USD","status":"subscribed","subscription":{"name":"ticker"}}"#;

    let mut group = c.benchmark_group("WS message deserialize (serde_json::Value)");
    group.bench_function("ticker", |b| {
        b.iter(|| serde_json::from_str::<serde_json::Value>(black_box(ticker_msg)));
    });
    group.bench_function("heartbeat", |b| {
        b.iter(|| serde_json::from_str::<serde_json::Value>(black_box(heartbeat_msg)));
    });
    group.bench_function("subscription status", |b| {
        b.iter(|| serde_json::from_str::<serde_json::Value>(black_box(subscription_msg)));
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_symbol_normalization,
    bench_price_parsing,
    bench_ws_message_deserialize,
);
criterion_main!(benches);
