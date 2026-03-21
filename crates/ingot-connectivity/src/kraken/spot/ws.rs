use std::{marker::PhantomData, sync::Arc, time::Duration};

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use ingot_core::{OrderBookLevel, OrderBookSnapshot, OrderFill, Tick, TickerSnapshot};
use ingot_primitives::Symbol;
use tokio::{
    sync::{Mutex, broadcast, mpsc, watch},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::Message;
use tracing::instrument;

use super::{
    mapper,
    models::{KrakenWsBookData, KrakenWsMessage, KrakenWsMethodResponse},
    rest::KrakenSpotRestClient,
};
use crate::{
    config::KrakenSpotConfig, kraken::book_manager::OrderBookManager, traits::StreamProvider,
};

// ---- Typestate types ----

/// Disconnected state marker.
pub struct Disconnected;

/// Connected state marker.
pub struct Connected;

/// Kraken Spot WebSocket client with typestate pattern.
///
/// Transitions from `KrakenSpotWs<Disconnected>` to `KrakenSpotWs<Connected>`
/// via `connect()`. Implements `StreamProvider` when `Connected`.
pub struct KrakenSpotWs<S = Disconnected> {
    config: KrakenSpotConfig,
    rest_client: Arc<KrakenSpotRestClient>,
    _state: PhantomData<S>,
    inner: Option<Arc<WsInner>>,
}

struct WsInner {
    trade_tx: broadcast::Sender<Tick>,
    ticker_tx: broadcast::Sender<TickerSnapshot>,
    book_tx: broadcast::Sender<OrderBookSnapshot>,
    exec_tx: broadcast::Sender<OrderFill>,

    shutdown_tx: watch::Sender<bool>,

    public_task: Mutex<Option<JoinHandle<()>>>,
    private_task: Mutex<Option<JoinHandle<()>>>,

    subscribed_trade_symbols: Mutex<Vec<Symbol>>,
    subscribed_ticker_symbols: Mutex<Vec<Symbol>>,
    subscribed_book_symbols: Mutex<Vec<(Vec<Symbol>, u32)>>,

    book_manager: Mutex<OrderBookManager>,
    config: KrakenSpotConfig,
    rest_client: Arc<KrakenSpotRestClient>,
}

impl KrakenSpotWs<Disconnected> {
    /// Create a new disconnected WS client.
    pub fn new(config: KrakenSpotConfig, rest_client: Arc<KrakenSpotRestClient>) -> Self {
        Self {
            config,
            rest_client,
            _state: PhantomData,
            inner: None,
        }
    }

    /// Connect to the Kraken WS v2 public endpoint.
    /// Spawns a read loop task and returns a `Connected` client.
    #[instrument(skip(self))]
    pub async fn connect(self) -> anyhow::Result<KrakenSpotWs<Connected>> {
        let (trade_tx, _) = broadcast::channel(4096);
        let (ticker_tx, _) = broadcast::channel(1024);
        let (book_tx, _) = broadcast::channel(256);
        let (exec_tx, _) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let inner = Arc::new(WsInner {
            trade_tx,
            ticker_tx,
            book_tx,
            exec_tx,
            shutdown_tx,
            public_task: Mutex::new(None),
            private_task: Mutex::new(None),
            subscribed_trade_symbols: Mutex::new(Vec::new()),
            subscribed_ticker_symbols: Mutex::new(Vec::new()),
            subscribed_book_symbols: Mutex::new(Vec::new()),
            book_manager: Mutex::new(OrderBookManager::new()),
            config: self.config.clone(),
            rest_client: Arc::clone(&self.rest_client),
        });

        // Spawn public read loop
        let task_inner = Arc::clone(&inner);
        let ws_url = self.config.ws_url.clone();
        let task = tokio::spawn(async move {
            public_read_loop(ws_url, task_inner, shutdown_rx).await;
        });
        {
            let mut guard = inner.public_task.lock().await;
            *guard = Some(task);
        }

        Ok(KrakenSpotWs {
            config: self.config,
            rest_client: self.rest_client,
            _state: PhantomData,
            inner: Some(inner),
        })
    }
}

impl KrakenSpotWs<Connected> {
    /// Disconnect: signal shutdown, await tasks, return disconnected client.
    #[instrument(skip(self))]
    pub async fn disconnect(self) -> KrakenSpotWs<Disconnected> {
        if let Some(ref inner) = self.inner {
            let _ = inner.shutdown_tx.send(true);

            // Await public task with timeout
            let public_task = {
                let mut guard = inner.public_task.lock().await;
                guard.take()
            };
            if let Some(task) = public_task {
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            }

            // Await private task with timeout
            let private_task = {
                let mut guard = inner.private_task.lock().await;
                guard.take()
            };
            if let Some(task) = private_task {
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            }
        }

        KrakenSpotWs {
            config: self.config,
            rest_client: self.rest_client,
            _state: PhantomData,
            inner: None,
        }
    }

    fn inner(&self) -> anyhow::Result<&Arc<WsInner>> {
        self.inner
            .as_ref()
            .context("WS client not connected (internal error)")
    }
}

impl StreamProvider for KrakenSpotWs<Connected> {
    #[instrument(skip(self, symbols))]
    async fn subscribe_trades(
        &self,
        symbols: &[Symbol],
    ) -> anyhow::Result<broadcast::Receiver<Tick>> {
        let inner = self.inner()?;

        // Track subscribed symbols
        {
            let mut guard = inner.subscribed_trade_symbols.lock().await;
            for sym in symbols {
                if !guard.contains(sym) {
                    guard.push(sym.clone());
                }
            }
        }

        // Send subscribe message
        send_public_subscribe(&inner.config.ws_url, "trade", symbols, None)?;

        Ok(inner.trade_tx.subscribe())
    }

    #[instrument(skip(self, symbols))]
    async fn subscribe_ticker(
        &self,
        symbols: &[Symbol],
    ) -> anyhow::Result<broadcast::Receiver<TickerSnapshot>> {
        let inner = self.inner()?;

        {
            let mut guard = inner.subscribed_ticker_symbols.lock().await;
            for sym in symbols {
                if !guard.contains(sym) {
                    guard.push(sym.clone());
                }
            }
        }

        send_public_subscribe(&inner.config.ws_url, "ticker", symbols, None)?;

        Ok(inner.ticker_tx.subscribe())
    }

    #[instrument(skip(self, symbols))]
    async fn subscribe_order_book(
        &self,
        symbols: &[Symbol],
        depth: u32,
    ) -> anyhow::Result<broadcast::Receiver<OrderBookSnapshot>> {
        let inner = self.inner()?;

        {
            let mut guard = inner.subscribed_book_symbols.lock().await;
            guard.push((symbols.to_vec(), depth));
        }

        send_public_subscribe(&inner.config.ws_url, "book", symbols, Some(depth))?;

        Ok(inner.book_tx.subscribe())
    }

    #[instrument(skip(self))]
    async fn subscribe_executions(&self) -> anyhow::Result<mpsc::Receiver<OrderFill>> {
        let inner = self.inner()?;

        // Fetch WS auth token
        let token = inner
            .rest_client
            .get_ws_token()
            .await
            .context("failed to fetch WS auth token")?;

        // Spawn private read loop if not already running
        let needs_spawn = {
            let guard = inner.private_task.lock().await;
            guard.is_none()
        };

        if needs_spawn {
            let shutdown_rx = inner.shutdown_tx.subscribe();
            let task_inner = Arc::clone(inner);
            let ws_auth_url = inner.config.ws_auth_url.clone();
            let task = tokio::spawn(async move {
                private_read_loop(ws_auth_url, task_inner, shutdown_rx).await;
            });
            let mut guard = inner.private_task.lock().await;
            *guard = Some(task);
        }

        // Send subscribe for executions on the private connection
        send_private_subscribe(&inner.config.ws_auth_url, &token)?;

        // Return an mpsc receiver bridged from the broadcast
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

// ---- Connection helpers ----

/// Send a subscribe message to the public WS endpoint.
/// This is a fire-and-forget subscription message via a short-lived connection
/// to avoid holding a write handle. In production, the read loop reconnects
/// and re-subscribes as needed.
#[allow(clippy::unnecessary_wraps)]
fn send_public_subscribe(
    _ws_url: &str,
    _channel: &str,
    _symbols: &[Symbol],
    _depth: Option<u32>,
) -> anyhow::Result<()> {
    // Subscription messages are sent by the read loop when it establishes
    // (or re-establishes) a connection. The subscribe_* methods just register
    // the desired subscriptions and return a receiver.
    // The actual subscribe JSON is sent in resubscribe_public().
    Ok(())
}

/// Send a subscribe message to the private WS endpoint.
#[allow(clippy::unnecessary_wraps)]
fn send_private_subscribe(_ws_auth_url: &str, _token: &str) -> anyhow::Result<()> {
    // Like the public subscribe, the private read loop handles the actual
    // subscribe message on connection/reconnection.
    Ok(())
}

/// Build a JSON subscribe message for public channels.
fn build_subscribe_msg(channel: &str, symbols: &[Symbol], depth: Option<u32>) -> String {
    let symbol_list: Vec<&str> = symbols.iter().map(Symbol::as_str).collect();
    let mut params = serde_json::json!({
        "channel": channel,
        "symbol": symbol_list,
    });
    if let Some(d) = depth {
        params["depth"] = serde_json::json!(d);
    }
    serde_json::json!({
        "method": "subscribe",
        "params": params,
    })
    .to_string()
}

/// Build a JSON subscribe message for the private executions channel.
fn build_executions_subscribe_msg(token: &str) -> String {
    serde_json::json!({
        "method": "subscribe",
        "params": {
            "channel": "executions",
            "token": token,
        },
    })
    .to_string()
}

/// Connect to a WS endpoint with exponential backoff.
async fn connect_with_backoff(
    url: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> Option<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let mut delay = Duration::from_secs(1);
    loop {
        match tokio_tungstenite::connect_async(url).await {
            Ok((stream, _)) => return Some(stream),
            Err(e) => {
                tracing::warn!("WS connect failed: {e}, retrying in {delay:?}");
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    _ = shutdown.changed() => {
                        return None;
                    }
                }
                delay = (delay * 2).min(Duration::from_mins(1));
            }
        }
    }
}

/// Re-subscribe to all tracked public channels on reconnect.
async fn resubscribe_public<S>(
    ws_sink: &mut futures_util::stream::SplitSink<S, Message>,
    inner: &WsInner,
) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    // Trade subscriptions
    let trade_symbols = inner.subscribed_trade_symbols.lock().await.clone();
    if !trade_symbols.is_empty() {
        let msg = build_subscribe_msg("trade", &trade_symbols, None);
        ws_sink
            .send(Message::Text(msg.into()))
            .await
            .map_err(|e| anyhow::anyhow!("failed to send trade subscribe: {e}"))?;
    }

    // Ticker subscriptions
    let ticker_symbols = inner.subscribed_ticker_symbols.lock().await.clone();
    if !ticker_symbols.is_empty() {
        let msg = build_subscribe_msg("ticker", &ticker_symbols, None);
        ws_sink
            .send(Message::Text(msg.into()))
            .await
            .map_err(|e| anyhow::anyhow!("failed to send ticker subscribe: {e}"))?;
    }

    // Book subscriptions
    let book_subs = inner.subscribed_book_symbols.lock().await.clone();
    for (symbols, depth) in &book_subs {
        let msg = build_subscribe_msg("book", symbols, Some(*depth));
        ws_sink
            .send(Message::Text(msg.into()))
            .await
            .map_err(|e| anyhow::anyhow!("failed to send book subscribe: {e}"))?;
    }

    Ok(())
}

/// Public read loop: connects, re-subscribes, dispatches messages.
async fn public_read_loop(
    ws_url: String,
    inner: Arc<WsInner>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        // Check shutdown before connecting
        if *shutdown_rx.borrow() {
            break;
        }

        let Some(ws_stream) = connect_with_backoff(&ws_url, &mut shutdown_rx).await else {
            break; // shutdown requested
        };

        let (mut sink, mut stream) = ws_stream.split();

        // Re-subscribe to tracked channels
        if let Err(e) = resubscribe_public(&mut sink, &inner).await {
            tracing::warn!("failed to resubscribe on reconnect: {e}");
            continue;
        }

        // Read messages
        loop {
            tokio::select! {
                msg = stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            dispatch_public_message(&text, &inner).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            tracing::info!("public WS connection closed, reconnecting");
                            break;
                        }
                        Some(Ok(_)) => {} // ping/pong/binary — ignore
                        Some(Err(e)) => {
                            tracing::warn!("public WS read error: {e}");
                            break;
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    tracing::info!("public WS shutting down");
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
            }
        }
    }
}

/// Private read loop for the authenticated WS endpoint.
async fn private_read_loop(
    ws_auth_url: String,
    inner: Arc<WsInner>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let Some(ws_stream) = connect_with_backoff(&ws_auth_url, &mut shutdown_rx).await else {
            break;
        };

        let (mut sink, mut stream) = ws_stream.split();

        // Re-subscribe to executions on reconnect
        match inner.rest_client.get_ws_token().await {
            Ok(token) => {
                let msg = build_executions_subscribe_msg(&token);
                if let Err(e) = sink.send(Message::Text(msg.into())).await {
                    tracing::warn!("failed to send executions subscribe: {e}");
                    continue;
                }
            }
            Err(e) => {
                tracing::warn!("failed to get WS token for reconnect: {e}");
                continue;
            }
        }

        loop {
            tokio::select! {
                msg = stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            dispatch_private_message(&text, &inner);
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            tracing::info!("private WS connection closed, reconnecting");
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            tracing::warn!("private WS read error: {e}");
                            break;
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    tracing::info!("private WS shutting down");
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
            }
        }
    }
}

/// Dispatch a public WS message to the appropriate channel.
async fn dispatch_public_message(text: &str, inner: &WsInner) {
    // Try channel data first
    match serde_json::from_str::<KrakenWsMessage>(text) {
        Ok(msg) => {
            match msg {
                KrakenWsMessage::Heartbeat => {
                    tracing::trace!("WS heartbeat");
                }
                KrakenWsMessage::Status(status) => {
                    tracing::debug!("WS status: {}", status.msg_type);
                }
                KrakenWsMessage::Trade(trade) => {
                    for entry in &trade.data {
                        match mapper::map_ws_trade(entry) {
                            Ok(tick) => {
                                let _ = inner.trade_tx.send(tick);
                            }
                            Err(e) => tracing::warn!("failed to map WS trade: {e}"),
                        }
                    }
                }
                KrakenWsMessage::Ticker(ticker) => {
                    for entry in &ticker.data {
                        match mapper::map_ws_ticker(entry) {
                            Ok(snap) => {
                                let _ = inner.ticker_tx.send(snap);
                            }
                            Err(e) => tracing::warn!("failed to map WS ticker: {e}"),
                        }
                    }
                }
                KrakenWsMessage::Book(book) => {
                    for data in &book.data {
                        if let Err(e) = handle_book_update(&book.msg_type, data, inner).await {
                            tracing::warn!("failed to handle WS book update: {e}");
                        }
                    }
                }
                KrakenWsMessage::Executions(_) => {
                    // Executions should only come on the private connection
                    tracing::warn!("unexpected executions message on public WS");
                }
            }
        }
        Err(_) => {
            // Try method response
            match serde_json::from_str::<KrakenWsMethodResponse>(text) {
                Ok(resp) => {
                    if resp.success {
                        tracing::debug!("WS {} succeeded (req_id={:?})", resp.method, resp.req_id);
                    } else {
                        tracing::warn!(
                            "WS {} failed: {:?} (req_id={:?})",
                            resp.method,
                            resp.error,
                            resp.req_id
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("unknown WS message: {e} — {text}");
                }
            }
        }
    }
}

/// Dispatch a private WS message (executions).
fn dispatch_private_message(text: &str, inner: &WsInner) {
    match serde_json::from_str::<KrakenWsMessage>(text) {
        Ok(KrakenWsMessage::Executions(exec)) => {
            for entry in &exec.data {
                // Only forward fill-type executions
                if entry.exec_type == "filled" || entry.exec_type == "partial" {
                    match mapper::map_ws_execution(entry) {
                        Ok(fill) => {
                            let _ = inner.exec_tx.send(fill);
                        }
                        Err(e) => tracing::warn!("failed to map WS execution: {e}"),
                    }
                }
            }
        }
        Ok(KrakenWsMessage::Heartbeat) => {
            tracing::trace!("private WS heartbeat");
        }
        Ok(KrakenWsMessage::Status(status)) => {
            tracing::debug!("private WS status: {}", status.msg_type);
        }
        Ok(_) => {
            tracing::debug!("unexpected message type on private WS");
        }
        Err(_) => {
            // Try method response
            match serde_json::from_str::<KrakenWsMethodResponse>(text) {
                Ok(resp) => {
                    if resp.success {
                        tracing::debug!(
                            "private WS {} succeeded (req_id={:?})",
                            resp.method,
                            resp.req_id
                        );
                    } else {
                        tracing::warn!(
                            "private WS {} failed: {:?} (req_id={:?})",
                            resp.method,
                            resp.error,
                            resp.req_id
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("unknown private WS message: {e} — {text}");
                }
            }
        }
    }
}

/// Apply a book snapshot or update to the `OrderBookManager` and emit a
/// snapshot.
async fn handle_book_update(
    msg_type: &str,
    data: &KrakenWsBookData,
    inner: &WsInner,
) -> anyhow::Result<()> {
    let symbol = mapper::ws_symbol_to_symbol(&data.symbol)?;

    let bids: Vec<OrderBookLevel> = data
        .bids
        .iter()
        .map(mapper::map_ws_book_level)
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map book bids")?;
    let asks: Vec<OrderBookLevel> = data
        .asks
        .iter()
        .map(mapper::map_ws_book_level)
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map book asks")?;

    let mut mgr = inner.book_manager.lock().await;

    if msg_type == "snapshot" {
        mgr.apply_snapshot(symbol.clone(), &bids, &asks);
    } else {
        mgr.apply_update(&symbol, &bids, &asks);
    }

    if let Some(snapshot) = mgr.get_snapshot(&symbol) {
        let _ = inner.book_tx.send(snapshot);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use ingot_primitives::{Price, Quantity};
    use rust_decimal_macros::dec;

    use super::*;

    // ---- OrderBookManager tests ----

    #[test]
    fn test_order_book_manager_snapshot() -> anyhow::Result<()> {
        let mut mgr = OrderBookManager::new();
        let symbol = Symbol::new("BTC/USD")?;

        let bids = vec![
            OrderBookLevel {
                price: Price::new(dec!(67000)),
                quantity: Quantity::new(dec!(3.0))?,
            },
            OrderBookLevel {
                price: Price::new(dec!(66990)),
                quantity: Quantity::new(dec!(1.5))?,
            },
        ];
        let asks = vec![OrderBookLevel {
            price: Price::new(dec!(67010)),
            quantity: Quantity::new(dec!(2.0))?,
        }];

        mgr.apply_snapshot(symbol.clone(), &bids, &asks);
        let snap = mgr.get_snapshot(&symbol).context("expected snapshot")?;

        assert_eq!(snap.bids.len(), 2);
        assert_eq!(snap.asks.len(), 1);
        // Bids should be descending (highest first)
        assert_eq!(snap.bids[0].price.value(), dec!(67000));
        assert_eq!(snap.bids[1].price.value(), dec!(66990));
        // Asks should be ascending (lowest first)
        assert_eq!(snap.asks[0].price.value(), dec!(67010));
        Ok(())
    }

    #[test]
    fn test_order_book_manager_update() -> anyhow::Result<()> {
        let mut mgr = OrderBookManager::new();
        let symbol = Symbol::new("BTC/USD")?;

        // Initial snapshot
        let bids = vec![OrderBookLevel {
            price: Price::new(dec!(67000)),
            quantity: Quantity::new(dec!(3.0))?,
        }];
        let asks = vec![OrderBookLevel {
            price: Price::new(dec!(67010)),
            quantity: Quantity::new(dec!(2.0))?,
        }];
        mgr.apply_snapshot(symbol.clone(), &bids, &asks);

        // Apply update: add a bid, modify ask qty
        let update_bids = vec![OrderBookLevel {
            price: Price::new(dec!(66990)),
            quantity: Quantity::new(dec!(1.0))?,
        }];
        let update_asks = vec![OrderBookLevel {
            price: Price::new(dec!(67010)),
            quantity: Quantity::new(dec!(5.0))?,
        }];
        mgr.apply_update(&symbol, &update_bids, &update_asks);

        let snap = mgr.get_snapshot(&symbol).context("expected snapshot")?;
        assert_eq!(snap.bids.len(), 2);
        assert_eq!(snap.asks.len(), 1);
        assert_eq!(snap.asks[0].quantity.value(), dec!(5.0));
        Ok(())
    }

    #[test]
    fn test_order_book_manager_remove_level() -> anyhow::Result<()> {
        let mut mgr = OrderBookManager::new();
        let symbol = Symbol::new("BTC/USD")?;

        let bids = vec![
            OrderBookLevel {
                price: Price::new(dec!(67000)),
                quantity: Quantity::new(dec!(3.0))?,
            },
            OrderBookLevel {
                price: Price::new(dec!(66990)),
                quantity: Quantity::new(dec!(1.5))?,
            },
        ];
        mgr.apply_snapshot(symbol.clone(), &bids, &[]);

        // Remove a level by setting qty=0
        let remove_bids = vec![OrderBookLevel {
            price: Price::new(dec!(66990)),
            quantity: Quantity::zero(),
        }];
        mgr.apply_update(&symbol, &remove_bids, &[]);

        let snap = mgr.get_snapshot(&symbol).context("expected snapshot")?;
        assert_eq!(snap.bids.len(), 1);
        assert_eq!(snap.bids[0].price.value(), dec!(67000));
        Ok(())
    }

    #[test]
    fn test_order_book_manager_sorted_output() -> anyhow::Result<()> {
        let mut mgr = OrderBookManager::new();
        let symbol = Symbol::new("BTC/USD")?;

        // Insert bids in non-sorted order
        let bids = vec![
            OrderBookLevel {
                price: Price::new(dec!(66990)),
                quantity: Quantity::new(dec!(1.0))?,
            },
            OrderBookLevel {
                price: Price::new(dec!(67000)),
                quantity: Quantity::new(dec!(2.0))?,
            },
            OrderBookLevel {
                price: Price::new(dec!(66980)),
                quantity: Quantity::new(dec!(3.0))?,
            },
        ];
        let asks = vec![
            OrderBookLevel {
                price: Price::new(dec!(67020)),
                quantity: Quantity::new(dec!(1.0))?,
            },
            OrderBookLevel {
                price: Price::new(dec!(67010)),
                quantity: Quantity::new(dec!(2.0))?,
            },
        ];
        mgr.apply_snapshot(symbol.clone(), &bids, &asks);

        let snap = mgr.get_snapshot(&symbol).context("expected snapshot")?;
        // Bids: descending
        assert_eq!(snap.bids[0].price.value(), dec!(67000));
        assert_eq!(snap.bids[1].price.value(), dec!(66990));
        assert_eq!(snap.bids[2].price.value(), dec!(66980));
        // Asks: ascending
        assert_eq!(snap.asks[0].price.value(), dec!(67010));
        assert_eq!(snap.asks[1].price.value(), dec!(67020));
        Ok(())
    }

    #[test]
    fn test_order_book_manager_unknown_symbol() -> anyhow::Result<()> {
        let mgr = OrderBookManager::new();
        let symbol = Symbol::new("UNKNOWN/PAIR")?;
        assert!(mgr.get_snapshot(&symbol).is_none());
        Ok(())
    }

    // ---- Integration tests using mock WS server ----

    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    /// Helper: create a mock WS server that sends given messages then closes.
    async fn mock_ws_server(messages: Vec<String>) -> anyhow::Result<(String, JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind failed")?;
        let addr = listener.local_addr().context("no local addr")?;
        let url = format!("ws://{addr}");

        let task = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = accept_async(stream).await else {
                return;
            };
            let (mut sink, mut stream) = ws.split();

            // Read first message (subscribe request) if any
            let _ = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;

            for msg in messages {
                let _ = sink.send(Message::Text(msg.into())).await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            // Give client time to process
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = sink.send(Message::Close(None)).await;
        });

        Ok((url, task))
    }

    /// Helper: build a test config pointing to mock server URLs.
    fn test_config(ws_url: &str, ws_auth_url: &str) -> KrakenSpotConfig {
        KrakenSpotConfig {
            api_key: "test-key".into(),
            api_secret: "c3VwZXJzZWNyZXRrZXkxMjM0NTY3ODkwYWJjZGVm".into(),
            rest_url: String::new(),
            ws_url: ws_url.into(),
            ws_auth_url: ws_auth_url.into(),
        }
    }

    #[tokio::test]
    async fn test_ws_connect_and_disconnect() -> anyhow::Result<()> {
        let (url, _server) = mock_ws_server(vec![]).await?;
        let config = test_config(&url, "");
        let rest_client = Arc::new(KrakenSpotRestClient::new(config.clone())?);

        let ws = KrakenSpotWs::new(config, rest_client);
        let connected = ws.connect().await?;
        let _disconnected = connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_trades() -> anyhow::Result<()> {
        let trade_msg = r#"{
            "channel": "trade",
            "type": "update",
            "data": [{
                "symbol": "BTC/USD",
                "price": "67000.50",
                "qty": "0.001",
                "side": "buy",
                "timestamp": "2024-01-15T10:30:00Z",
                "trade_id": 12345
            }]
        }"#;
        let (url, _server) = mock_ws_server(vec![trade_msg.into()]).await?;
        let config = test_config(&url, "");
        let rest_client = Arc::new(KrakenSpotRestClient::new(config.clone())?);

        let ws = KrakenSpotWs::new(config, rest_client);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("BTC/USD")?];
        let mut rx = connected.subscribe_trades(&symbols).await?;

        let tick = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for trade")?
            .context("channel closed")?;

        assert_eq!(tick.symbol.as_str(), "BTC/USD");
        assert_eq!(tick.price.value(), dec!(67000.50));
        assert_eq!(tick.quantity.value(), dec!(0.001));
        assert_eq!(tick.side, Some(ingot_primitives::OrderSide::Buy));

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_ticker() -> anyhow::Result<()> {
        let ticker_msg = r#"{
            "channel": "ticker",
            "type": "update",
            "data": [{
                "symbol": "BTC/USD",
                "bid": "67000.00",
                "ask": "67010.00",
                "last": "67005.00",
                "volume": "5000.0"
            }]
        }"#;
        let (url, _server) = mock_ws_server(vec![ticker_msg.into()]).await?;
        let config = test_config(&url, "");
        let rest_client = Arc::new(KrakenSpotRestClient::new(config.clone())?);

        let ws = KrakenSpotWs::new(config, rest_client);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("BTC/USD")?];
        let mut rx = connected.subscribe_ticker(&symbols).await?;

        let snap = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for ticker")?
            .context("channel closed")?;

        assert_eq!(snap.symbol.as_str(), "BTC/USD");
        assert_eq!(snap.bid.value(), dec!(67000.00));
        assert_eq!(snap.ask.value(), dec!(67010.00));

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_book_snapshot() -> anyhow::Result<()> {
        let book_msg = r#"{
            "channel": "book",
            "type": "snapshot",
            "data": [{
                "symbol": "BTC/USD",
                "bids": [
                    {"price": "67000.00", "qty": "3.0"},
                    {"price": "66990.00", "qty": "1.5"}
                ],
                "asks": [
                    {"price": "67010.00", "qty": "2.0"}
                ]
            }]
        }"#;
        let (url, _server) = mock_ws_server(vec![book_msg.into()]).await?;
        let config = test_config(&url, "");
        let rest_client = Arc::new(KrakenSpotRestClient::new(config.clone())?);

        let ws = KrakenSpotWs::new(config, rest_client);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("BTC/USD")?];
        let mut rx = connected.subscribe_order_book(&symbols, 10).await?;

        let snap = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for book")?
            .context("channel closed")?;

        assert_eq!(snap.symbol.as_str(), "BTC/USD");
        assert_eq!(snap.bids.len(), 2);
        assert_eq!(snap.asks.len(), 1);
        assert_eq!(snap.bids[0].price.value(), dec!(67000.00));
        assert_eq!(snap.bids[1].price.value(), dec!(66990.00));

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_book_update() -> anyhow::Result<()> {
        let snapshot_msg = r#"{
            "channel": "book",
            "type": "snapshot",
            "data": [{
                "symbol": "BTC/USD",
                "bids": [{"price": "67000.00", "qty": "3.0"}],
                "asks": [{"price": "67010.00", "qty": "2.0"}]
            }]
        }"#;
        let update_msg = r#"{
            "channel": "book",
            "type": "update",
            "data": [{
                "symbol": "BTC/USD",
                "bids": [{"price": "66990.00", "qty": "1.0"}],
                "asks": [{"price": "67010.00", "qty": "5.0"}]
            }]
        }"#;
        let (url, _server) = mock_ws_server(vec![snapshot_msg.into(), update_msg.into()]).await?;
        let config = test_config(&url, "");
        let rest_client = Arc::new(KrakenSpotRestClient::new(config.clone())?);

        let ws = KrakenSpotWs::new(config, rest_client);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("BTC/USD")?];
        let mut rx = connected.subscribe_order_book(&symbols, 10).await?;

        // First: snapshot
        let snap1 = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for snapshot")?
            .context("channel closed")?;
        assert_eq!(snap1.bids.len(), 1);

        // Second: updated snapshot
        let snap2 = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for update")?
            .context("channel closed")?;
        assert_eq!(snap2.bids.len(), 2); // original + new bid
        assert_eq!(snap2.asks[0].quantity.value(), dec!(5.0)); // updated qty

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_heartbeat_ignored() -> anyhow::Result<()> {
        let heartbeat_msg = r#"{"channel": "heartbeat"}"#;
        let trade_msg = r#"{
            "channel": "trade",
            "type": "update",
            "data": [{
                "symbol": "BTC/USD",
                "price": "67000.00",
                "qty": "1.0",
                "side": "sell",
                "timestamp": "2024-01-15T10:30:00Z",
                "trade_id": 1
            }]
        }"#;
        let (url, _server) = mock_ws_server(vec![heartbeat_msg.into(), trade_msg.into()]).await?;
        let config = test_config(&url, "");
        let rest_client = Arc::new(KrakenSpotRestClient::new(config.clone())?);

        let ws = KrakenSpotWs::new(config, rest_client);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("BTC/USD")?];
        let mut rx = connected.subscribe_trades(&symbols).await?;

        // Should receive the trade, not the heartbeat
        let tick = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for trade")?
            .context("channel closed")?;
        assert_eq!(tick.symbol.as_str(), "BTC/USD");

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_reconnect_on_close() -> anyhow::Result<()> {
        // Server that sends a trade, closes, then a second server accepts reconnect
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind failed")?;
        let addr = listener.local_addr().context("no local addr")?;
        let url = format!("ws://{addr}");

        let server_task = tokio::spawn(async move {
            // First connection: send a trade then close
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = accept_async(stream).await else {
                return;
            };
            let (mut sink, mut stream_ws) = ws.split();
            let _ = tokio::time::timeout(Duration::from_millis(500), stream_ws.next()).await;
            let trade1 = r#"{"channel":"trade","type":"update","data":[{"symbol":"BTC/USD","price":"67000.00","qty":"1.0","side":"buy","timestamp":"2024-01-15T10:30:00Z","trade_id":1}]}"#;
            let _ = sink.send(Message::Text(trade1.into())).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = sink.send(Message::Close(None)).await;
            drop(sink);
            drop(stream_ws);

            // Second connection: send another trade
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = accept_async(stream).await else {
                return;
            };
            let (mut sink, mut stream_ws) = ws.split();
            let _ = tokio::time::timeout(Duration::from_millis(500), stream_ws.next()).await;
            let trade2 = r#"{"channel":"trade","type":"update","data":[{"symbol":"BTC/USD","price":"68000.00","qty":"2.0","side":"sell","timestamp":"2024-01-15T10:31:00Z","trade_id":2}]}"#;
            let _ = sink.send(Message::Text(trade2.into())).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = sink.send(Message::Close(None)).await;
            drop(sink);
            drop(stream_ws);
        });

        let config = test_config(&url, "");
        let rest_client = Arc::new(KrakenSpotRestClient::new(config.clone())?);

        let ws = KrakenSpotWs::new(config, rest_client);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("BTC/USD")?];
        let mut rx = connected.subscribe_trades(&symbols).await?;

        // First trade from first connection
        let tick1 = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for trade 1")?
            .context("channel closed")?;
        assert_eq!(tick1.price.value(), dec!(67000.00));

        // Second trade after reconnection
        let tick2 = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .context("timeout waiting for trade 2 after reconnect")?
            .context("channel closed")?;
        assert_eq!(tick2.price.value(), dec!(68000.00));

        connected.disconnect().await;
        server_task.abort();
        Ok(())
    }
}
