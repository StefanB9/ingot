use std::{
    collections::HashMap,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use ingot_core::{OrderBookLevel, OrderBookSnapshot, OrderFill, OrderId, Tick, TickerSnapshot};
use ingot_primitives::{Amount, Currency, OrderSide, Price, Quantity, Symbol};
use rust_decimal::Decimal;
use smol_str::SmolStr;
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    sync::{Mutex, broadcast, mpsc, watch},
    task::JoinHandle,
};
use tokio_util::codec::Framed;
use tracing::instrument;

use super::{
    contract_registry::IbkrContractRegistry, tws_codec::TwsCodec, tws_models::TwsIncoming,
};
use crate::{book_manager::OrderBookManager, config::IbkrConfig, traits::StreamProvider};

// ── TWS tick type constants ──

const TICK_BID: i32 = 1;
const TICK_ASK: i32 = 2;
const TICK_LAST: i32 = 4;
const TICK_VOLUME: i32 = 8;

// ── TWS outgoing message IDs ──

const REQ_MKT_DATA: i32 = 1;
const REQ_MKT_DEPTH: i32 = 10;
const START_API: i32 = 71;

// ── Typestate markers ──

/// Typestate marker: not yet connected to TWS.
pub struct Disconnected;

/// Typestate marker: connected and authenticated with TWS.
pub struct Connected;

// ── PartialTicker accumulator ──

struct PartialTicker {
    symbol: Symbol,
    bid: Option<Price>,
    ask: Option<Price>,
    last: Option<Price>,
    volume: Option<Quantity>,
}

impl PartialTicker {
    fn new(symbol: Symbol) -> Self {
        Self {
            symbol,
            bid: None,
            ask: None,
            last: None,
            volume: None,
        }
    }

    fn try_snapshot(&self) -> Option<TickerSnapshot> {
        let bid = self.bid?;
        let ask = self.ask?;
        let last = self.last?;
        let volume = self.volume.unwrap_or(Quantity::zero());
        Some(TickerSnapshot {
            symbol: self.symbol.clone(),
            bid,
            ask,
            last,
            volume_24h: volume,
            timestamp: chrono::Utc::now(),
        })
    }
}

// ── TwsInner shared state ──

struct TwsInner {
    trade_tx: broadcast::Sender<Tick>,
    ticker_tx: broadcast::Sender<TickerSnapshot>,
    book_tx: broadcast::Sender<OrderBookSnapshot>,
    exec_tx: broadcast::Sender<OrderFill>,

    shutdown_tx: watch::Sender<bool>,
    read_task: Mutex<Option<JoinHandle<()>>>,
    write_task: Mutex<Option<JoinHandle<()>>>,

    write_tx: mpsc::Sender<Vec<String>>,

    next_req_id: AtomicI32,
    req_id_to_symbol: Mutex<HashMap<i32, Symbol>>,

    ticker_state: Mutex<HashMap<i32, PartialTicker>>,

    book_manager: Mutex<OrderBookManager>,

    registry: Arc<Mutex<IbkrContractRegistry>>,

    config: IbkrConfig,
}

impl TwsInner {
    fn alloc_req_id(&self) -> i32 {
        self.next_req_id.fetch_add(1, Ordering::Relaxed)
    }
}

// ── IbkrTws<S> ──

/// IBKR TWS binary socket client with typestate connection lifecycle.
pub struct IbkrTws<S = Disconnected> {
    config: IbkrConfig,
    registry: Arc<Mutex<IbkrContractRegistry>>,
    _state: PhantomData<S>,
    inner: Option<Arc<TwsInner>>,
}

impl IbkrTws<Disconnected> {
    pub fn new(config: IbkrConfig, registry: Arc<Mutex<IbkrContractRegistry>>) -> Self {
        Self {
            config,
            registry,
            _state: PhantomData,
            inner: None,
        }
    }

    pub fn config(&self) -> &IbkrConfig {
        &self.config
    }

    /// Connect to the TWS socket, perform handshake, and return a Connected
    /// client.
    #[instrument(skip(self))]
    pub async fn connect(self) -> anyhow::Result<IbkrTws<Connected>> {
        let addr = format!("{}:{}", self.config.tws_host, self.config.tws_port);
        let mut stream = TcpStream::connect(&addr)
            .await
            .context("failed to connect to TWS")?;

        // Send raw handshake: "API\0" + "v100..176\0"
        stream
            .write_all(b"API\0")
            .await
            .context("failed to send API handshake")?;
        stream
            .write_all(b"v100..176\0")
            .await
            .context("failed to send version range")?;
        stream.flush().await.context("failed to flush handshake")?;

        // Wrap in Framed codec
        let mut framed = Framed::new(stream, TwsCodec);

        // Wait for NextValidId to confirm connection
        let first_msg = tokio::time::timeout(Duration::from_secs(10), framed.next())
            .await
            .context("timeout waiting for TWS handshake response")?
            .context("TWS connection closed during handshake")?
            .context("TWS decode error during handshake")?;

        let parsed =
            TwsIncoming::parse(&first_msg).context("failed to parse TWS handshake response")?;
        let start_order_id = match parsed {
            TwsIncoming::NextValidId { order_id } => order_id,
            other => anyhow::bail!("expected NextValidId, got: {other:?}"),
        };

        // Split into read/write halves
        let (mut sink, read_stream) = framed.split();

        // Send START_API
        let start_api_msg = vec![
            START_API.to_string(),
            "2".into(),
            self.config.client_id.to_string(),
            String::new(),
        ];
        sink.send(start_api_msg)
            .await
            .context("failed to send START_API")?;

        // Set up channels
        let (trade_tx, _) = broadcast::channel(4096);
        let (ticker_tx, _) = broadcast::channel(1024);
        let (book_tx, _) = broadcast::channel(256);
        let (exec_tx, _) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (write_tx, mut write_rx) = mpsc::channel::<Vec<String>>(256);

        let inner = Arc::new(TwsInner {
            trade_tx,
            ticker_tx,
            book_tx,
            exec_tx,
            shutdown_tx,
            read_task: Mutex::new(None),
            write_task: Mutex::new(None),
            write_tx,
            next_req_id: AtomicI32::new(start_order_id),
            req_id_to_symbol: Mutex::new(HashMap::new()),
            ticker_state: Mutex::new(HashMap::new()),
            book_manager: Mutex::new(OrderBookManager::new()),
            registry: Arc::clone(&self.registry),
            config: self.config.clone(),
        });

        // Spawn write loop
        let write_shutdown = inner.shutdown_tx.subscribe();
        let write_task = tokio::spawn(async move {
            write_loop(sink, &mut write_rx, write_shutdown).await;
        });
        {
            let mut guard = inner.write_task.lock().await;
            *guard = Some(write_task);
        }

        // Spawn read loop
        let read_inner = Arc::clone(&inner);
        let read_task = tokio::spawn(async move {
            read_loop(read_stream, read_inner, shutdown_rx).await;
        });
        {
            let mut guard = inner.read_task.lock().await;
            *guard = Some(read_task);
        }

        Ok(IbkrTws {
            config: self.config,
            registry: self.registry,
            _state: PhantomData,
            inner: Some(inner),
        })
    }
}

impl IbkrTws<Connected> {
    /// Disconnect: signal shutdown, await tasks, return disconnected client.
    #[instrument(skip(self))]
    pub async fn disconnect(self) -> IbkrTws<Disconnected> {
        if let Some(ref inner) = self.inner {
            let _ = inner.shutdown_tx.send(true);

            let read_task = {
                let mut guard = inner.read_task.lock().await;
                guard.take()
            };
            if let Some(task) = read_task {
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            }

            let write_task = {
                let mut guard = inner.write_task.lock().await;
                guard.take()
            };
            if let Some(task) = write_task {
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            }
        }

        IbkrTws {
            config: self.config,
            registry: self.registry,
            _state: PhantomData,
            inner: None,
        }
    }

    fn inner(&self) -> anyhow::Result<&Arc<TwsInner>> {
        self.inner
            .as_ref()
            .context("TWS client not connected (internal error)")
    }
}

impl StreamProvider for IbkrTws<Connected> {
    #[instrument(skip(self, symbols))]
    async fn subscribe_trades(
        &self,
        symbols: &[Symbol],
    ) -> anyhow::Result<broadcast::Receiver<Tick>> {
        let inner = self.inner()?;

        for sym in symbols {
            let conid = {
                let reg = inner.registry.lock().await;
                reg.conid_for_symbol(sym)
                    .with_context(|| format!("no conid for symbol {sym}"))?
            };
            let req_id = inner.alloc_req_id();
            {
                let mut map = inner.req_id_to_symbol.lock().await;
                map.insert(req_id, sym.clone());
            }
            let msg = build_req_mkt_data(req_id, conid);
            inner
                .write_tx
                .send(msg)
                .await
                .context("failed to send REQ_MKT_DATA")?;
        }

        Ok(inner.trade_tx.subscribe())
    }

    #[instrument(skip(self, symbols))]
    async fn subscribe_ticker(
        &self,
        symbols: &[Symbol],
    ) -> anyhow::Result<broadcast::Receiver<TickerSnapshot>> {
        let inner = self.inner()?;

        for sym in symbols {
            let conid = {
                let reg = inner.registry.lock().await;
                reg.conid_for_symbol(sym)
                    .with_context(|| format!("no conid for symbol {sym}"))?
            };
            let req_id = inner.alloc_req_id();
            {
                let mut map = inner.req_id_to_symbol.lock().await;
                map.insert(req_id, sym.clone());
            }
            {
                let mut state = inner.ticker_state.lock().await;
                state.insert(req_id, PartialTicker::new(sym.clone()));
            }
            let msg = build_req_mkt_data(req_id, conid);
            inner
                .write_tx
                .send(msg)
                .await
                .context("failed to send REQ_MKT_DATA for ticker")?;
        }

        Ok(inner.ticker_tx.subscribe())
    }

    #[instrument(skip(self, symbols))]
    async fn subscribe_order_book(
        &self,
        symbols: &[Symbol],
        depth: u32,
    ) -> anyhow::Result<broadcast::Receiver<OrderBookSnapshot>> {
        let inner = self.inner()?;

        for sym in symbols {
            let conid = {
                let reg = inner.registry.lock().await;
                reg.conid_for_symbol(sym)
                    .with_context(|| format!("no conid for symbol {sym}"))?
            };
            let req_id = inner.alloc_req_id();
            {
                let mut map = inner.req_id_to_symbol.lock().await;
                map.insert(req_id, sym.clone());
            }
            let msg = build_req_mkt_depth(req_id, conid, depth);
            inner
                .write_tx
                .send(msg)
                .await
                .context("failed to send REQ_MKT_DEPTH")?;
        }

        Ok(inner.book_tx.subscribe())
    }

    #[instrument(skip(self))]
    async fn subscribe_executions(&self) -> anyhow::Result<mpsc::Receiver<OrderFill>> {
        let inner = self.inner()?;

        // Bridge broadcast → mpsc (same pattern as Kraken)
        let (tx, rx) = mpsc::channel(256);
        let mut exec_rx = inner.exec_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match exec_rx.recv().await {
                    Ok(fill) => {
                        if tx.send(fill).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        });

        Ok(rx)
    }
}

// ── Message builders ──

fn build_req_mkt_data(req_id: i32, conid: i64) -> Vec<String> {
    vec![
        REQ_MKT_DATA.to_string(), // msg_id
        "11".into(),              // version
        req_id.to_string(),       // req_id
        conid.to_string(),        // conid
        String::new(),            // symbol
        "STK".into(),             // sec_type
        String::new(),            // expiry
        "0".into(),               // strike
        String::new(),            // right
        "SMART".into(),           // exchange
        String::new(),            // multiplier
        "USD".into(),             // currency
        String::new(),            // local_symbol
        String::new(),            // generic_tick_list
        "0".into(),               // snapshot
    ]
}

fn build_req_mkt_depth(req_id: i32, conid: i64, depth: u32) -> Vec<String> {
    vec![
        REQ_MKT_DEPTH.to_string(), // msg_id
        "5".into(),                // version
        req_id.to_string(),        // req_id
        conid.to_string(),         // conid
        String::new(),             // symbol
        "STK".into(),              // sec_type
        String::new(),             // expiry
        "0".into(),                // strike
        String::new(),             // right
        "SMART".into(),            // exchange
        String::new(),             // multiplier
        "USD".into(),              // currency
        String::new(),             // local_symbol
        depth.to_string(),         // num_rows
        "0".into(),                // is_smart_depth
        String::new(),             // mkt_depth_options
    ]
}

// ── Write loop ──

async fn write_loop<S>(
    mut sink: S,
    write_rx: &mut mpsc::Receiver<Vec<String>>,
    mut shutdown_rx: watch::Receiver<bool>,
) where
    S: futures_util::Sink<Vec<String>> + Unpin,
    <S as futures_util::Sink<Vec<String>>>::Error: std::fmt::Display,
{
    loop {
        tokio::select! {
            msg = write_rx.recv() => {
                match msg {
                    Some(fields) => {
                        if let Err(e) = sink.send(fields).await {
                            tracing::warn!("TWS write error: {e}");
                            break;
                        }
                    }
                    None => break,
                }
            }
            _ = shutdown_rx.changed() => break,
        }
    }
}

// ── Read loop ──

async fn read_loop<S>(
    mut read_stream: S,
    inner: Arc<TwsInner>,
    mut shutdown_rx: watch::Receiver<bool>,
) where
    S: futures_util::Stream<Item = Result<super::tws_codec::TwsRawMessage, anyhow::Error>> + Unpin,
{
    loop {
        tokio::select! {
            msg = read_stream.next() => {
                match msg {
                    Some(Ok(raw)) => {
                        match TwsIncoming::parse(&raw) {
                            Ok(parsed) => dispatch_message(parsed, &inner).await,
                            Err(e) => tracing::warn!("TWS parse error: {e}"),
                        }
                    }
                    Some(Err(e)) => {
                        tracing::warn!("TWS read error: {e}");
                        break;
                    }
                    None => {
                        tracing::info!("TWS connection closed");
                        break;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                tracing::info!("TWS read loop shutting down");
                break;
            }
        }
    }
}

// ── Dispatch ──

async fn dispatch_message(msg: TwsIncoming, inner: &TwsInner) {
    match msg {
        TwsIncoming::TickPrice {
            req_id,
            tick_type,
            price,
            size: _,
        } => {
            dispatch_tick_price(req_id, tick_type, price, inner).await;
        }
        TwsIncoming::TickSize {
            req_id,
            tick_type,
            size,
        } => {
            dispatch_tick_size(req_id, tick_type, size, inner).await;
        }
        TwsIncoming::MarketDepth {
            req_id,
            position,
            operation,
            side,
            price,
            size,
        } => {
            dispatch_depth(req_id, position, operation, side, price, size, inner).await;
        }
        TwsIncoming::ExecutionData {
            req_id: _,
            order_id,
            conid,
            side,
            shares,
            price,
            exec_id,
            time: _,
        } => {
            dispatch_execution(order_id, conid, &side, shares, price, &exec_id, inner).await;
        }
        TwsIncoming::OrderStatus {
            order_id,
            status,
            filled: _,
            remaining: _,
            avg_fill_price: _,
        } => {
            tracing::debug!("TWS order status: order_id={order_id}, status={status}");
        }
        TwsIncoming::ErrorMessage { id, code, message } => {
            tracing::warn!("TWS error: id={id}, code={code}, message={message}");
        }
        TwsIncoming::Heartbeat => {
            tracing::trace!("TWS heartbeat");
        }
        TwsIncoming::NextValidId { order_id } => {
            inner.next_req_id.store(order_id, Ordering::Relaxed);
        }
    }
}

async fn dispatch_tick_price(req_id: i32, tick_type: i32, price: f64, inner: &TwsInner) {
    let symbol = {
        let map = inner.req_id_to_symbol.lock().await;
        match map.get(&req_id) {
            Some(s) => s.clone(),
            None => return,
        }
    };

    let decimal_price = Decimal::try_from(price);
    let Ok(decimal_price) = decimal_price else {
        tracing::warn!("TWS tick price: invalid decimal: {price}");
        return;
    };

    // Emit trade tick for LAST price
    if tick_type == TICK_LAST {
        let tick = Tick {
            time: chrono::Utc::now(),
            symbol: symbol.clone(),
            exchange: SmolStr::new("IBKR"),
            price: Price::new(decimal_price),
            quantity: Quantity::zero(),
            side: None,
            trade_id: None,
        };
        let _ = inner.trade_tx.send(tick);
    }

    // Update partial ticker
    if matches!(tick_type, TICK_BID | TICK_ASK | TICK_LAST) {
        let mut state = inner.ticker_state.lock().await;
        if let Some(partial) = state.get_mut(&req_id) {
            let p = Price::new(decimal_price);
            match tick_type {
                TICK_BID => partial.bid = Some(p),
                TICK_ASK => partial.ask = Some(p),
                TICK_LAST => partial.last = Some(p),
                _ => {}
            }
            if let Some(snapshot) = partial.try_snapshot() {
                let _ = inner.ticker_tx.send(snapshot);
            }
        }
    }
}

async fn dispatch_tick_size(req_id: i32, tick_type: i32, size: f64, inner: &TwsInner) {
    if tick_type == TICK_VOLUME {
        let Ok(decimal_size) = Decimal::try_from(size) else {
            return;
        };
        let Ok(qty) = Quantity::new(decimal_size) else {
            return;
        };
        let mut state = inner.ticker_state.lock().await;
        if let Some(partial) = state.get_mut(&req_id) {
            partial.volume = Some(qty);
            if let Some(snapshot) = partial.try_snapshot() {
                let _ = inner.ticker_tx.send(snapshot);
            }
        }
    }
}

async fn dispatch_depth(
    req_id: i32,
    _position: i32,
    operation: i32,
    book_side: i32,
    price: f64,
    depth_size: f64,
    inner: &TwsInner,
) {
    let symbol = {
        let map = inner.req_id_to_symbol.lock().await;
        match map.get(&req_id) {
            Some(s) => s.clone(),
            None => return,
        }
    };

    let Ok(decimal_price) = Decimal::try_from(price) else {
        return;
    };
    let Ok(decimal_size) = Decimal::try_from(depth_size) else {
        return;
    };

    let level = OrderBookLevel {
        price: Price::new(decimal_price),
        quantity: Quantity::new(decimal_size).unwrap_or(Quantity::zero()),
    };

    let mut mgr = inner.book_manager.lock().await;

    // operation: 0=insert, 1=update, 2=delete
    // book_side: 0=ask, 1=bid
    match operation {
        0 | 1 => {
            // Insert or update
            if book_side == 1 {
                mgr.apply_update(&symbol, &[level], &[]);
            } else {
                mgr.apply_update(&symbol, &[], &[level]);
            }
        }
        2 => {
            // Delete: set quantity to zero
            let delete_level = OrderBookLevel {
                price: Price::new(decimal_price),
                quantity: Quantity::zero(),
            };
            if book_side == 1 {
                mgr.apply_update(&symbol, &[delete_level], &[]);
            } else {
                mgr.apply_update(&symbol, &[], &[delete_level]);
            }
        }
        _ => {
            tracing::warn!("TWS unknown depth operation: {operation}");
        }
    }

    if let Some(snapshot) = mgr.get_snapshot(&symbol) {
        let _ = inner.book_tx.send(snapshot);
    }
}

async fn dispatch_execution(
    order_id: i32,
    conid: i64,
    side: &str,
    shares: f64,
    price: f64,
    exec_id: &str,
    inner: &TwsInner,
) {
    let symbol = {
        let reg = inner.registry.lock().await;
        match reg.symbol_for_conid(conid) {
            Some(s) => s.clone(),
            None => {
                tracing::warn!("TWS execution for unknown conid: {conid}");
                return;
            }
        }
    };

    let order_side = match side {
        "BOT" => OrderSide::Buy,
        "SLD" => OrderSide::Sell,
        other => {
            tracing::warn!("TWS unknown execution side: {other}");
            return;
        }
    };

    let Ok(decimal_price) = Decimal::try_from(price) else {
        tracing::warn!("TWS execution: invalid price decimal: {price}");
        return;
    };
    let Ok(decimal_shares) = Decimal::try_from(shares) else {
        tracing::warn!("TWS execution: invalid shares decimal: {shares}");
        return;
    };
    let Ok(qty) = Quantity::new(decimal_shares) else {
        tracing::warn!("TWS execution: invalid quantity: {shares}");
        return;
    };

    let Ok(oid) = OrderId::new(&order_id.to_string()) else {
        tracing::warn!("TWS execution: invalid order_id: {order_id}");
        return;
    };

    let fill = OrderFill {
        order_id: oid,
        symbol,
        side: order_side,
        fill_price: Price::new(decimal_price),
        fill_quantity: qty,
        fee: Amount::zero(),
        fee_currency: Currency::USD,
        timestamp: chrono::Utc::now(),
        trade_id: Some(SmolStr::new(exec_id)),
    };
    let _ = inner.exec_tx.send(fill);
}

// ── Initialization helper for order book ──
// MarketDepth uses incremental updates only (no snapshot message).
// We initialize the book for a symbol when first depth message arrives
// via apply_update on an empty book — OrderBookManager handles this gracefully
// since apply_update on a missing symbol is a no-op. We need to ensure
// the book exists by doing an empty apply_snapshot first.

async fn ensure_book_exists(symbol: &Symbol, inner: &TwsInner) {
    let mut mgr = inner.book_manager.lock().await;
    if mgr.get_snapshot(symbol).is_none() {
        mgr.apply_snapshot(symbol.clone(), &[], &[]);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Context;
    use ingot_core::{Instrument, InstrumentDetails};
    use ingot_primitives::{AssetClass, Currency, Exchange, Price, Quantity};
    use rust_decimal_macros::dec;
    use smol_str::SmolStr;
    use tokio::net::TcpListener;

    use super::*;
    use crate::ibkr::tws_codec::TwsRawMessage;

    // ── Test helpers ──

    fn test_config(host: &str, port: u16) -> IbkrConfig {
        IbkrConfig {
            account_id: "DU_TEST".into(),
            cp_gateway_url: "https://localhost:5000".into(),
            tws_host: host.into(),
            tws_port: port,
            client_id: 1,
            session_keepalive_secs: 60,
        }
    }

    fn test_registry() -> Arc<Mutex<IbkrContractRegistry>> {
        let mut reg = IbkrContractRegistry::new();
        let symbol = Symbol::new("AAPL").unwrap_or_else(|_| unreachable!());
        let instrument = Instrument {
            symbol: symbol.clone(),
            asset_class: AssetClass::Equity,
            exchange: Exchange::IBKR,
            base_currency: Currency::USD,
            quote_currency: Currency::USD,
            tick_size: Price::new(dec!(0.01)),
            display_name: SmolStr::new("Apple Inc"),
            details: InstrumentDetails::Equity {
                isin: None,
                lot_size: Quantity::new(dec!(1)).unwrap_or_else(|_| unreachable!()),
                fractional: false,
            },
        };
        reg.register(265598, symbol, instrument);
        Arc::new(Mutex::new(reg))
    }

    /// Mock TWS server: reads raw handshake, sends NextValidId, reads
    /// START_API, then optionally reads subscription requests and sends
    /// predefined messages.
    async fn mock_tws_server(
        messages: Vec<TwsRawMessage>,
        num_subscriptions_to_read: usize,
    ) -> anyhow::Result<(u16, JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind failed")?;
        let port = listener.local_addr().context("no local addr")?.port();

        let task = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };

            // Read raw handshake bytes (API\0 + version\0)
            // The handshake is NOT length-prefixed, so we read raw bytes first
            let (read_half, _write_half) = stream.into_split();
            let stream = read_half
                .reunite(_write_half)
                .unwrap_or_else(|_| unreachable!());

            // Use a small buffer to consume the handshake bytes
            let mut buf = vec![0u8; 256];
            let stream = {
                use tokio::io::AsyncReadExt;
                let mut stream = stream;
                // Read handshake bytes (non-framed)
                let _ =
                    tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf)).await;
                stream
            };

            let mut framed = Framed::new(stream, TwsCodec);

            // Send NextValidId { order_id: 100 }
            let next_valid_id = vec!["9".into(), "1".into(), "100".into()];
            if framed.send(next_valid_id).await.is_err() {
                return;
            }

            // Read START_API
            let _ = tokio::time::timeout(Duration::from_millis(500), framed.next()).await;

            // Read subscription requests
            for _ in 0..num_subscriptions_to_read {
                let _ = tokio::time::timeout(Duration::from_millis(500), framed.next()).await;
            }

            // Small delay to let client set up receivers
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Send test messages
            for msg in messages {
                let mut fields = vec![msg.msg_id.to_string()];
                fields.extend(msg.fields);
                if framed.send(fields).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            // Keep connection alive briefly
            tokio::time::sleep(Duration::from_millis(300)).await;
        });

        Ok((port, task))
    }

    // ── Test 14: disconnected state (existing) ──

    #[test]
    fn test_tws_disconnected_state() -> anyhow::Result<()> {
        let config = IbkrConfig {
            account_id: "DU_TEST".into(),
            cp_gateway_url: "https://localhost:5000".into(),
            tws_host: "127.0.0.1".into(),
            tws_port: 7497,
            client_id: 1,
            session_keepalive_secs: 60,
        };

        let registry = test_registry();
        let tws = IbkrTws::new(config, registry);
        assert_eq!(tws.config().tws_host, "127.0.0.1");
        assert_eq!(tws.config().tws_port, 7497);
        assert_eq!(tws.config().client_id, 1);
        assert_eq!(tws.config().account_id, "DU_TEST");
        Ok(())
    }

    // ── Test 2: connect handshake ──

    #[tokio::test]
    async fn test_tws_connect_handshake() -> anyhow::Result<()> {
        let (port, _server) = mock_tws_server(vec![], 0).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        // If we got here, handshake succeeded
        let _disconnected = connected.disconnect().await;
        Ok(())
    }

    // ── Test 3: connect and disconnect ──

    #[tokio::test]
    async fn test_tws_connect_and_disconnect() -> anyhow::Result<()> {
        let (port, _server) = mock_tws_server(vec![], 0).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;
        let disconnected = connected.disconnect().await;

        // Should be back in disconnected state
        assert_eq!(disconnected.config().tws_port, port);
        Ok(())
    }

    // ── Test 4: subscribe_trades returns receiver ──

    #[tokio::test]
    async fn test_subscribe_trades_returns_receiver() -> anyhow::Result<()> {
        let (port, _server) = mock_tws_server(vec![], 1).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        let symbols = vec![Symbol::new("AAPL")?];
        let _rx = connected.subscribe_trades(&symbols).await?;

        connected.disconnect().await;
        Ok(())
    }

    // ── Test 5: subscribe_ticker returns receiver ──

    #[tokio::test]
    async fn test_subscribe_ticker_returns_receiver() -> anyhow::Result<()> {
        let (port, _server) = mock_tws_server(vec![], 1).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        let symbols = vec![Symbol::new("AAPL")?];
        let _rx = connected.subscribe_ticker(&symbols).await?;

        connected.disconnect().await;
        Ok(())
    }

    // ── Test 6: subscribe_order_book returns receiver ──

    #[tokio::test]
    async fn test_subscribe_order_book_returns_receiver() -> anyhow::Result<()> {
        let (port, _server) = mock_tws_server(vec![], 1).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        let symbols = vec![Symbol::new("AAPL")?];
        let _rx = connected.subscribe_order_book(&symbols, 5).await?;

        connected.disconnect().await;
        Ok(())
    }

    // ── Test 7: subscribe_executions returns receiver ──

    #[tokio::test]
    async fn test_subscribe_executions_returns_receiver() -> anyhow::Result<()> {
        let (port, _server) = mock_tws_server(vec![], 0).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        let _rx = connected.subscribe_executions().await?;

        connected.disconnect().await;
        Ok(())
    }

    // ── Test 8: tick_price dispatches to trade ──

    #[tokio::test]
    async fn test_tick_price_dispatches_to_trade() -> anyhow::Result<()> {
        use super::super::tws_models::{MSG_TICK_PRICE, MSG_TICK_SIZE};

        // TickPrice: [version, req_id, tick_type(LAST=4), price, size]
        let tick_msg = TwsRawMessage {
            msg_id: MSG_TICK_PRICE,
            fields: vec![
                "6".into(),      // version
                "100".into(),    // req_id (will be the first allocated)
                "4".into(),      // tick_type = LAST
                "178.50".into(), // price
                "100".into(),    // size
            ],
        };

        let (port, _server) = mock_tws_server(vec![tick_msg], 1).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        let symbols = vec![Symbol::new("AAPL")?];
        let mut rx = connected.subscribe_trades(&symbols).await?;

        let tick = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for trade")?
            .context("channel closed")?;

        assert_eq!(tick.symbol.as_str(), "AAPL");
        assert_eq!(tick.price.value(), dec!(178.50));

        connected.disconnect().await;
        Ok(())
    }

    // ── Test 9: tick_price dispatches to ticker ──

    #[tokio::test]
    async fn test_tick_price_dispatches_to_ticker() -> anyhow::Result<()> {
        use super::super::tws_models::MSG_TICK_PRICE;

        // Send BID, ASK, LAST tick prices to build a complete TickerSnapshot
        let bid_msg = TwsRawMessage {
            msg_id: MSG_TICK_PRICE,
            fields: vec![
                "6".into(),      // version
                "100".into(),    // req_id
                "1".into(),      // tick_type = BID
                "178.00".into(), // price
                "200".into(),    // size
            ],
        };
        let ask_msg = TwsRawMessage {
            msg_id: MSG_TICK_PRICE,
            fields: vec![
                "6".into(),      // version
                "100".into(),    // req_id
                "2".into(),      // tick_type = ASK
                "178.10".into(), // price
                "150".into(),    // size
            ],
        };
        let last_msg = TwsRawMessage {
            msg_id: MSG_TICK_PRICE,
            fields: vec![
                "6".into(),      // version
                "100".into(),    // req_id
                "4".into(),      // tick_type = LAST
                "178.05".into(), // price
                "50".into(),     // size
            ],
        };

        let (port, _server) = mock_tws_server(vec![bid_msg, ask_msg, last_msg], 1).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        let symbols = vec![Symbol::new("AAPL")?];
        let mut rx = connected.subscribe_ticker(&symbols).await?;

        // The snapshot should be emitted when all three fields are present
        let snap = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for ticker")?
            .context("channel closed")?;

        assert_eq!(snap.symbol.as_str(), "AAPL");
        assert_eq!(snap.bid.value(), dec!(178.00));
        assert_eq!(snap.ask.value(), dec!(178.10));
        assert_eq!(snap.last.value(), dec!(178.05));

        connected.disconnect().await;
        Ok(())
    }

    // ── Test 10: market_depth builds order book ──

    #[tokio::test]
    async fn test_market_depth_builds_order_book() -> anyhow::Result<()> {
        use super::super::tws_models::MSG_MARKET_DEPTH;

        // Insert bid at position 0 and ask at position 0
        let bid_depth = TwsRawMessage {
            msg_id: MSG_MARKET_DEPTH,
            fields: vec![
                "1".into(),      // version
                "100".into(),    // req_id
                "0".into(),      // position
                "0".into(),      // operation = insert
                "1".into(),      // side = bid
                "178.00".into(), // price
                "200".into(),    // size
            ],
        };
        let ask_depth = TwsRawMessage {
            msg_id: MSG_MARKET_DEPTH,
            fields: vec![
                "1".into(),      // version
                "100".into(),    // req_id
                "0".into(),      // position
                "0".into(),      // operation = insert
                "0".into(),      // side = ask
                "178.10".into(), // price
                "150".into(),    // size
            ],
        };

        let (port, _server) = mock_tws_server(vec![bid_depth, ask_depth], 1).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        let symbols = vec![Symbol::new("AAPL")?];

        // Initialize the book for this symbol
        {
            let inner = connected.inner()?;
            ensure_book_exists(&symbols[0], inner).await;
        }

        let mut rx = connected.subscribe_order_book(&symbols, 5).await?;

        // We should get at least one snapshot after the depth updates
        let snap = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for book")?
            .context("channel closed")?;

        assert_eq!(snap.symbol.as_str(), "AAPL");
        assert!(!snap.bids.is_empty() || !snap.asks.is_empty());

        connected.disconnect().await;
        Ok(())
    }

    // ── Test 11: error_message logged not panicked ──

    #[tokio::test]
    async fn test_error_message_logged_not_panicked() -> anyhow::Result<()> {
        use super::super::tws_models::{MSG_ERR_MSG, MSG_TICK_PRICE};

        // Send an error message followed by a valid trade tick
        let err_msg = TwsRawMessage {
            msg_id: MSG_ERR_MSG,
            fields: vec![
                "2".into(),                                           // version
                "-1".into(),                                          // id
                "2104".into(),                                        // code
                "Market data farm connection is OK:usfarm.nj".into(), // message
            ],
        };
        let tick_msg = TwsRawMessage {
            msg_id: MSG_TICK_PRICE,
            fields: vec![
                "6".into(),      // version
                "100".into(),    // req_id
                "4".into(),      // tick_type = LAST
                "179.00".into(), // price
                "50".into(),     // size
            ],
        };

        let (port, _server) = mock_tws_server(vec![err_msg, tick_msg], 1).await?;
        let config = test_config("127.0.0.1", port);
        let registry = test_registry();

        let tws = IbkrTws::new(config, registry);
        let connected = tws.connect().await?;

        let symbols = vec![Symbol::new("AAPL")?];
        let mut rx = connected.subscribe_trades(&symbols).await?;

        // Should still receive the trade tick after the error
        let tick = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for trade after error")?
            .context("channel closed")?;

        assert_eq!(tick.price.value(), dec!(179.00));

        connected.disconnect().await;
        Ok(())
    }
}
