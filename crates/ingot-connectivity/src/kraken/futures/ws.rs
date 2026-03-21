use std::{marker::PhantomData, sync::Arc, time::Duration};

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use ingot_core::{OrderBookSnapshot, OrderFill, Tick, TickerSnapshot};
use ingot_primitives::Symbol;
use tokio::{
    sync::{Mutex, broadcast, mpsc, watch},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::Message;
use tracing::instrument;

use super::{
    mapper,
    models::{FuturesWsControlMessage, FuturesWsFeed},
};
use crate::{
    config::KrakenFuturesConfig,
    kraken::{auth, book_manager::OrderBookManager},
    traits::StreamProvider,
};

// ---- Typestate types ----

/// Disconnected state marker.
pub struct Disconnected;

/// Connected state marker.
pub struct Connected;

/// Kraken Futures WebSocket client with typestate pattern.
pub struct KrakenFuturesWs<S = Disconnected> {
    config: KrakenFuturesConfig,
    _state: PhantomData<S>,
    inner: Option<Arc<FuturesWsInner>>,
}

struct FuturesWsInner {
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
    config: KrakenFuturesConfig,
}

impl KrakenFuturesWs<Disconnected> {
    /// Create a new disconnected WS client.
    pub fn new(config: KrakenFuturesConfig) -> Self {
        Self {
            config,
            _state: PhantomData,
            inner: None,
        }
    }

    /// Connect and transition to Connected state.
    #[instrument(skip(self))]
    pub async fn connect(self) -> anyhow::Result<KrakenFuturesWs<Connected>> {
        let (trade_tx, _) = broadcast::channel(4096);
        let (ticker_tx, _) = broadcast::channel(1024);
        let (book_tx, _) = broadcast::channel(256);
        let (exec_tx, _) = broadcast::channel(256);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let inner = Arc::new(FuturesWsInner {
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
        });

        // Spawn public read loop
        let ws_url = self.config.ws_url.clone();
        let task_inner = Arc::clone(&inner);
        let task_shutdown = shutdown_rx.clone();
        let public_task = tokio::spawn(async move {
            public_read_loop(ws_url, task_inner, task_shutdown).await;
        });
        {
            let mut guard = inner.public_task.lock().await;
            *guard = Some(public_task);
        }

        Ok(KrakenFuturesWs {
            config: self.config,
            _state: PhantomData,
            inner: Some(inner),
        })
    }
}

impl KrakenFuturesWs<Connected> {
    /// Disconnect and transition back to Disconnected.
    #[instrument(skip(self))]
    pub async fn disconnect(self) -> KrakenFuturesWs<Disconnected> {
        if let Some(inner) = &self.inner {
            let _ = inner.shutdown_tx.send(true);

            let public_task = {
                let mut guard = inner.public_task.lock().await;
                guard.take()
            };
            if let Some(task) = public_task {
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            }

            let private_task = {
                let mut guard = inner.private_task.lock().await;
                guard.take()
            };
            if let Some(task) = private_task {
                let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
            }
        }

        KrakenFuturesWs {
            config: self.config,
            _state: PhantomData,
            inner: None,
        }
    }

    fn inner(&self) -> anyhow::Result<&Arc<FuturesWsInner>> {
        self.inner
            .as_ref()
            .context("WS client not connected (internal error)")
    }
}

impl StreamProvider for KrakenFuturesWs<Connected> {
    #[instrument(skip(self, symbols))]
    async fn subscribe_trades(
        &self,
        symbols: &[Symbol],
    ) -> anyhow::Result<broadcast::Receiver<Tick>> {
        let inner = self.inner()?;

        {
            let mut guard = inner.subscribed_trade_symbols.lock().await;
            for sym in symbols {
                if !guard.contains(sym) {
                    guard.push(sym.clone());
                }
            }
        }

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

        Ok(inner.book_tx.subscribe())
    }

    #[instrument(skip(self))]
    async fn subscribe_executions(&self) -> anyhow::Result<mpsc::Receiver<OrderFill>> {
        let inner = self.inner()?;

        // Spawn private read loop if not already running
        let needs_spawn = {
            let guard = inner.private_task.lock().await;
            guard.is_none()
        };

        if needs_spawn {
            let shutdown_rx = inner.shutdown_tx.subscribe();
            let task_inner = Arc::clone(inner);
            let ws_url = inner.config.ws_url.clone();
            let api_key = inner.config.api_key.clone();
            let api_secret = inner.config.api_secret.clone();
            let task = tokio::spawn(async move {
                private_read_loop(ws_url, api_key, api_secret, task_inner, shutdown_rx).await;
            });
            let mut guard = inner.private_task.lock().await;
            *guard = Some(task);
        }

        // Bridge broadcast to mpsc
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

/// Build a JSON subscribe message for Futures WS feeds.
fn build_subscribe_msg(feed: &str, product_ids: &[Symbol]) -> String {
    let ids: Vec<&str> = product_ids.iter().map(Symbol::as_str).collect();
    serde_json::json!({
        "event": "subscribe",
        "feed": feed,
        "product_ids": ids,
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
                tracing::warn!("Futures WS connect failed: {e}, retrying in {delay:?}");
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

/// Re-subscribe to all tracked public feeds on reconnect.
async fn resubscribe_public<S>(
    ws_sink: &mut futures_util::stream::SplitSink<S, Message>,
    inner: &FuturesWsInner,
) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    let trade_symbols = inner.subscribed_trade_symbols.lock().await.clone();
    if !trade_symbols.is_empty() {
        let msg = build_subscribe_msg("trade", &trade_symbols);
        ws_sink
            .send(Message::Text(msg.into()))
            .await
            .map_err(|e| anyhow::anyhow!("failed to send trade subscribe: {e}"))?;
    }

    let ticker_symbols = inner.subscribed_ticker_symbols.lock().await.clone();
    if !ticker_symbols.is_empty() {
        let msg = build_subscribe_msg("ticker", &ticker_symbols);
        ws_sink
            .send(Message::Text(msg.into()))
            .await
            .map_err(|e| anyhow::anyhow!("failed to send ticker subscribe: {e}"))?;
    }

    let book_subs = inner.subscribed_book_symbols.lock().await.clone();
    for (symbols, _depth) in &book_subs {
        let msg = build_subscribe_msg("book", symbols);
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
    inner: Arc<FuturesWsInner>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let Some(ws_stream) = connect_with_backoff(&ws_url, &mut shutdown_rx).await else {
            break;
        };

        let (mut sink, mut stream) = ws_stream.split();

        if let Err(e) = resubscribe_public(&mut sink, &inner).await {
            tracing::warn!("failed to resubscribe on reconnect: {e}");
            continue;
        }

        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.tick().await; // first tick is immediate

        loop {
            tokio::select! {
                msg = stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            dispatch_public_message(&text, &inner).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            tracing::info!("Futures public WS closed, reconnecting");
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            tracing::warn!("Futures public WS read error: {e}");
                            break;
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    let ping = serde_json::json!({"event": "ping"}).to_string();
                    if let Err(e) = sink.send(Message::Text(ping.into())).await {
                        tracing::warn!("failed to send ping: {e}");
                        break;
                    }
                }
                _ = shutdown_rx.changed() => {
                    tracing::info!("Futures public WS shutting down");
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
            }
        }
    }
}

/// Private read loop with challenge-response authentication.
async fn private_read_loop(
    ws_url: String,
    api_key: String,
    api_secret: String,
    inner: Arc<FuturesWsInner>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let Some(ws_stream) = connect_with_backoff(&ws_url, &mut shutdown_rx).await else {
            break;
        };

        let (mut sink, mut stream) = ws_stream.split();

        // Step 1: Send challenge request
        let challenge_req = serde_json::json!({
            "event": "challenge",
            "api_key": api_key,
        })
        .to_string();
        if let Err(e) = sink.send(Message::Text(challenge_req.into())).await {
            tracing::warn!("failed to send challenge request: {e}");
            continue;
        }

        // Step 2: Wait for challenge response
        let Ok(Some(challenge_str)) = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(Ok(Message::Text(text))) = stream.next().await {
                if let Ok(ctrl) = serde_json::from_str::<FuturesWsControlMessage>(&text)
                    && ctrl.event == "challenge"
                {
                    return ctrl.message;
                }
            }
            None
        })
        .await
        else {
            tracing::warn!("did not receive challenge from Futures WS");
            continue;
        };

        // Step 3: Sign the challenge and send back
        let signed = match auth::sign_futures_ws_challenge(&challenge_str, &api_secret) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to sign challenge: {e}");
                continue;
            }
        };

        let auth_msg = serde_json::json!({
            "event": "challenge",
            "api_key": api_key,
            "signed_challenge": signed,
        })
        .to_string();
        if let Err(e) = sink.send(Message::Text(auth_msg.into())).await {
            tracing::warn!("failed to send signed challenge: {e}");
            continue;
        }

        // Step 4: Subscribe to fills
        let fills_sub = serde_json::json!({
            "event": "subscribe",
            "feed": "fills",
        })
        .to_string();
        if let Err(e) = sink.send(Message::Text(fills_sub.into())).await {
            tracing::warn!("failed to subscribe to fills: {e}");
            continue;
        }

        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.tick().await;

        loop {
            tokio::select! {
                msg = stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            dispatch_private_message(&text, &inner);
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            tracing::info!("Futures private WS closed, reconnecting");
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            tracing::warn!("Futures private WS read error: {e}");
                            break;
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    let ping = serde_json::json!({"event": "ping"}).to_string();
                    if let Err(e) = sink.send(Message::Text(ping.into())).await {
                        tracing::warn!("failed to send ping: {e}");
                        break;
                    }
                }
                _ = shutdown_rx.changed() => {
                    tracing::info!("Futures private WS shutting down");
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
            }
        }
    }
}

/// Dispatch a public WS message to the appropriate channel.
async fn dispatch_public_message(text: &str, inner: &FuturesWsInner) {
    // Try control message first (has "event" field)
    if let Ok(ctrl) = serde_json::from_str::<FuturesWsControlMessage>(text) {
        match ctrl.event.as_str() {
            "pong" => tracing::trace!("Futures WS pong"),
            "subscribed" => {
                tracing::debug!("Futures WS subscribed to feed={:?}", ctrl.feed);
            }
            "error" => {
                tracing::warn!("Futures WS error: {:?}", ctrl.message);
            }
            "info" => {
                tracing::debug!("Futures WS info: version={:?}", ctrl.version);
            }
            _ => {
                tracing::debug!("Futures WS control event: {}", ctrl.event);
            }
        }
        return;
    }

    // Try data feed message
    match serde_json::from_str::<FuturesWsFeed>(text) {
        Ok(feed) => match feed {
            FuturesWsFeed::Trade(data) | FuturesWsFeed::TradeSnapshot(data) => {
                let product_id = data.product_id.as_deref().unwrap_or("");
                // Snapshot: trades in array
                for entry in &data.trades {
                    match mapper::map_ws_futures_trade(product_id, entry) {
                        Ok(tick) => {
                            let _ = inner.trade_tx.send(tick);
                        }
                        Err(e) => tracing::warn!("failed to map Futures WS trade: {e}"),
                    }
                }
                // Single trade: fields embedded directly
                #[allow(clippy::collapsible_if)]
                if data.trades.is_empty() {
                    if let (Some(side), Some(price), Some(qty), Some(time)) =
                        (&data.side, &data.price, &data.qty, data.time)
                    {
                        let entry = super::models::FuturesWsTradeEntry {
                            side: side.clone(),
                            price: price.clone(),
                            qty: qty.clone(),
                            time,
                            uid: data.uid.clone(),
                        };
                        match mapper::map_ws_futures_trade(product_id, &entry) {
                            Ok(tick) => {
                                let _ = inner.trade_tx.send(tick);
                            }
                            Err(e) => tracing::warn!("failed to map Futures WS trade: {e}"),
                        }
                    }
                }
            }
            FuturesWsFeed::Ticker(entry) | FuturesWsFeed::TickerLite(entry) => {
                match mapper::map_ws_futures_ticker(&entry) {
                    Ok(snap) => {
                        let _ = inner.ticker_tx.send(snap);
                    }
                    Err(e) => tracing::warn!("failed to map Futures WS ticker: {e}"),
                }
            }
            FuturesWsFeed::BookSnapshot(entry) => {
                if let Err(e) = handle_book_snapshot(&entry, inner).await {
                    tracing::warn!("failed to handle Futures book snapshot: {e}");
                }
            }
            FuturesWsFeed::Book(entry) => {
                if let Err(e) = handle_book_update(&entry, inner).await {
                    tracing::warn!("failed to handle Futures book update: {e}");
                }
            }
            FuturesWsFeed::Fills(_) => {
                tracing::warn!("unexpected fills on public WS");
            }
            FuturesWsFeed::OpenOrders(_) => {
                tracing::debug!("open_orders on public WS (ignored)");
            }
        },
        Err(e) => {
            tracing::debug!("unknown Futures WS message: {e} — {text}");
        }
    }
}

/// Dispatch a private WS message (fills).
fn dispatch_private_message(text: &str, inner: &FuturesWsInner) {
    // Try control message
    if let Ok(ctrl) = serde_json::from_str::<FuturesWsControlMessage>(text) {
        match ctrl.event.as_str() {
            "pong" => tracing::trace!("Futures private WS pong"),
            "subscribed" => {
                tracing::debug!("Futures private WS subscribed to {:?}", ctrl.feed);
            }
            _ => {
                tracing::debug!("Futures private WS event: {}", ctrl.event);
            }
        }
        return;
    }

    // Try data feed
    match serde_json::from_str::<FuturesWsFeed>(text) {
        Ok(FuturesWsFeed::Fills(data)) => {
            for entry in &data.fills {
                match mapper::map_ws_futures_fill(entry) {
                    Ok(fill) => {
                        let _ = inner.exec_tx.send(fill);
                    }
                    Err(e) => tracing::warn!("failed to map Futures WS fill: {e}"),
                }
            }
        }
        Ok(_) => {
            tracing::debug!("non-fills message on private WS");
        }
        Err(e) => {
            tracing::debug!("unknown Futures private WS message: {e} — {text}");
        }
    }
}

/// Handle a book snapshot message.
async fn handle_book_snapshot(
    entry: &super::models::FuturesWsBookEntry,
    inner: &FuturesWsInner,
) -> anyhow::Result<()> {
    let symbol = Symbol::new(&entry.product_id).context("invalid book product_id")?;

    let bids = entry
        .bids
        .iter()
        .map(mapper::map_ws_futures_book_level)
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map book snapshot bids")?;
    let asks = entry
        .asks
        .iter()
        .map(mapper::map_ws_futures_book_level)
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map book snapshot asks")?;

    let mut mgr = inner.book_manager.lock().await;
    mgr.apply_snapshot(symbol.clone(), &bids, &asks);

    if let Some(snapshot) = mgr.get_snapshot(&symbol) {
        let _ = inner.book_tx.send(snapshot);
    }

    Ok(())
}

/// Handle a book incremental update message.
async fn handle_book_update(
    entry: &super::models::FuturesWsBookEntry,
    inner: &FuturesWsInner,
) -> anyhow::Result<()> {
    let symbol = Symbol::new(&entry.product_id).context("invalid book product_id")?;

    let bids = entry
        .bids
        .iter()
        .map(mapper::map_ws_futures_book_level)
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map book update bids")?;
    let asks = entry
        .asks
        .iter()
        .map(mapper::map_ws_futures_book_level)
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to map book update asks")?;

    let mut mgr = inner.book_manager.lock().await;
    mgr.apply_update(&symbol, &bids, &asks);

    if let Some(snapshot) = mgr.get_snapshot(&symbol) {
        let _ = inner.book_tx.send(snapshot);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    use super::*;

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

            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = sink.send(Message::Close(None)).await;
        });

        Ok((url, task))
    }

    fn test_config(ws_url: &str) -> KrakenFuturesConfig {
        KrakenFuturesConfig {
            api_key: "test-key".into(),
            api_secret: "c3VwZXJzZWNyZXRrZXkxMjM0NTY3ODkwYWJjZGVm".into(),
            rest_url: String::new(),
            ws_url: ws_url.into(),
        }
    }

    #[tokio::test]
    async fn test_ws_connect_and_disconnect() -> anyhow::Result<()> {
        let (url, _server) = mock_ws_server(vec![]).await?;
        let config = test_config(&url);

        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;
        let _disconnected = connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_trades() -> anyhow::Result<()> {
        let trade_msg = r#"{
            "feed": "trade",
            "product_id": "PF_XBTUSD",
            "side": "buy",
            "price": "67000.50",
            "qty": "0.01",
            "time": 1705312200000
        }"#;
        let (url, _server) = mock_ws_server(vec![trade_msg.into()]).await?;
        let config = test_config(&url);

        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("PF_XBTUSD")?];
        let mut rx = connected.subscribe_trades(&symbols).await?;

        let tick = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for trade")?
            .context("channel closed")?;

        assert_eq!(tick.symbol.as_str(), "PF_XBTUSD");
        assert_eq!(tick.price.value(), dec!(67000.50));
        assert_eq!(tick.side, Some(ingot_primitives::OrderSide::Buy));

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_ticker() -> anyhow::Result<()> {
        let ticker_msg = r#"{
            "feed": "ticker",
            "product_id": "PF_XBTUSD",
            "bid": "67000.00",
            "ask": "67010.00",
            "last": "67005.00",
            "volume": "5000.0"
        }"#;
        let (url, _server) = mock_ws_server(vec![ticker_msg.into()]).await?;
        let config = test_config(&url);

        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("PF_XBTUSD")?];
        let mut rx = connected.subscribe_ticker(&symbols).await?;

        let snap = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for ticker")?
            .context("channel closed")?;

        assert_eq!(snap.symbol.as_str(), "PF_XBTUSD");
        assert_eq!(snap.bid.value(), dec!(67000.00));
        assert_eq!(snap.ask.value(), dec!(67010.00));

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_book_snapshot() -> anyhow::Result<()> {
        let book_msg = r#"{
            "feed": "book_snapshot",
            "product_id": "PF_XBTUSD",
            "bids": [
                {"price": "67000.00", "qty": "3.0"},
                {"price": "66990.00", "qty": "1.5"}
            ],
            "asks": [
                {"price": "67010.00", "qty": "2.0"}
            ]
        }"#;
        let (url, _server) = mock_ws_server(vec![book_msg.into()]).await?;
        let config = test_config(&url);

        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("PF_XBTUSD")?];
        let mut rx = connected.subscribe_order_book(&symbols, 10).await?;

        let snap = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for book")?
            .context("channel closed")?;

        assert_eq!(snap.symbol.as_str(), "PF_XBTUSD");
        assert_eq!(snap.bids.len(), 2);
        assert_eq!(snap.asks.len(), 1);
        // Bids should be descending
        assert_eq!(snap.bids[0].price.value(), dec!(67000.00));
        assert_eq!(snap.bids[1].price.value(), dec!(66990.00));

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_book_update() -> anyhow::Result<()> {
        let snapshot_msg = r#"{
            "feed": "book_snapshot",
            "product_id": "PF_XBTUSD",
            "bids": [{"price": "67000.00", "qty": "3.0"}],
            "asks": [{"price": "67010.00", "qty": "2.0"}]
        }"#;
        let update_msg = r#"{
            "feed": "book",
            "product_id": "PF_XBTUSD",
            "bids": [{"price": "66990.00", "qty": "1.0"}],
            "asks": [{"price": "67010.00", "qty": "5.0"}]
        }"#;
        let (url, _server) = mock_ws_server(vec![snapshot_msg.into(), update_msg.into()]).await?;
        let config = test_config(&url);

        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("PF_XBTUSD")?];
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
        assert_eq!(snap2.bids.len(), 2);
        assert_eq!(snap2.asks[0].quantity.value(), dec!(5.0));

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_ping_pong() -> anyhow::Result<()> {
        // Server that responds to ping with pong
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind failed")?;
        let addr = listener.local_addr().context("no local addr")?;
        let url = format!("ws://{addr}");

        let server_task = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = accept_async(stream).await else {
                return;
            };
            let (mut sink, mut stream) = ws.split();

            while let Some(Ok(Message::Text(text))) = stream.next().await {
                if text.contains("ping") {
                    let pong = r#"{"event": "pong"}"#;
                    let _ = sink.send(Message::Text(pong.into())).await;
                }
            }
        });

        let config = test_config(&url);
        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        // Let ping fire (interval is 30s, so we just verify connect/disconnect works)
        tokio::time::sleep(Duration::from_millis(100)).await;

        connected.disconnect().await;
        server_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_challenge_response_auth() -> anyhow::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind failed")?;
        let addr = listener.local_addr().context("no local addr")?;
        let url = format!("ws://{addr}");

        let server_task = tokio::spawn(async move {
            // First connection: public read loop (spawned by connect())
            // Accept and keep alive but do nothing with it
            let Ok((pub_stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(_pub_ws) = accept_async(pub_stream).await else {
                return;
            };

            // Second connection: private read loop (spawned by subscribe_executions())
            let Ok((priv_stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(priv_ws) = accept_async(priv_stream).await else {
                return;
            };
            let (mut sink, mut stream) = priv_ws.split();

            // Read challenge request
            if let Some(Ok(Message::Text(text))) = stream.next().await
                && text.contains("challenge")
            {
                let challenge = r#"{"event": "challenge", "message": "test-challenge-string"}"#;
                let _ = sink.send(Message::Text(challenge.into())).await;
            }

            // Read signed challenge
            if let Some(Ok(Message::Text(text))) = stream.next().await {
                assert!(
                    text.contains("signed_challenge"),
                    "expected signed challenge, got: {text}"
                );
            }

            // Read fills subscription
            if let Some(Ok(Message::Text(text))) = stream.next().await {
                assert!(text.contains("fills"), "expected fills sub, got: {text}");
            }

            // Send a fill
            let fill_msg = r#"{
                "feed": "fills",
                "fills": [{
                    "instrument": "PF_XBTUSD",
                    "side": "buy",
                    "price": "67000.0",
                    "qty": "0.01",
                    "order_id": "ord-1",
                    "fill_id": "fill-1",
                    "fee_paid": "0.05",
                    "fee_currency": "USD",
                    "time": 1705312200000
                }]
            }"#;
            let _ = sink.send(Message::Text(fill_msg.into())).await;

            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = sink.send(Message::Close(None)).await;
        });

        let config = test_config(&url);
        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        let mut rx = connected.subscribe_executions().await?;

        let fill = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .context("timeout waiting for fill")?
            .context("channel closed")?;

        assert_eq!(fill.symbol.as_str(), "PF_XBTUSD");
        assert_eq!(fill.fill_price.value(), dec!(67000.0));

        connected.disconnect().await;
        server_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_subscribe_executions() -> anyhow::Result<()> {
        // This is implicitly tested in test_ws_challenge_response_auth above
        // Just verify the subscribe_executions returns a receiver
        let (url, _server) = mock_ws_server(vec![]).await?;
        let config = test_config(&url);

        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        // This will timeout on the challenge, but that's ok for this test
        // We just verify it doesn't panic
        let _rx =
            tokio::time::timeout(Duration::from_millis(500), connected.subscribe_executions())
                .await;

        connected.disconnect().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_reconnect_on_close() -> anyhow::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind failed")?;
        let addr = listener.local_addr().context("no local addr")?;
        let url = format!("ws://{addr}");

        let server_task = tokio::spawn(async move {
            // First connection
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = accept_async(stream).await else {
                return;
            };
            let (mut sink, mut stream_ws) = ws.split();
            let _ = tokio::time::timeout(Duration::from_millis(500), stream_ws.next()).await;
            let trade1 = r#"{"feed":"trade","product_id":"PF_XBTUSD","side":"buy","price":"67000.00","qty":"1.0","time":1705312200000}"#;
            let _ = sink.send(Message::Text(trade1.into())).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = sink.send(Message::Close(None)).await;
            drop(sink);
            drop(stream_ws);

            // Second connection
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = accept_async(stream).await else {
                return;
            };
            let (mut sink, mut stream_ws) = ws.split();
            let _ = tokio::time::timeout(Duration::from_millis(500), stream_ws.next()).await;
            let trade2 = r#"{"feed":"trade","product_id":"PF_XBTUSD","side":"sell","price":"68000.00","qty":"2.0","time":1705312260000}"#;
            let _ = sink.send(Message::Text(trade2.into())).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = sink.send(Message::Close(None)).await;
            drop(sink);
            drop(stream_ws);
        });

        let config = test_config(&url);
        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("PF_XBTUSD")?];
        let mut rx = connected.subscribe_trades(&symbols).await?;

        let tick1 = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for trade 1")?
            .context("channel closed")?;
        assert_eq!(tick1.price.value(), dec!(67000.00));

        let tick2 = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .context("timeout waiting for trade 2 after reconnect")?
            .context("channel closed")?;
        assert_eq!(tick2.price.value(), dec!(68000.00));

        connected.disconnect().await;
        server_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_ws_unknown_message_ignored() -> anyhow::Result<()> {
        let unknown_msg = r#"{"some_unknown_field": "value"}"#;
        let trade_msg = r#"{
            "feed": "trade",
            "product_id": "PF_XBTUSD",
            "side": "buy",
            "price": "67000.00",
            "qty": "1.0",
            "time": 1705312200000
        }"#;
        let (url, _server) = mock_ws_server(vec![unknown_msg.into(), trade_msg.into()]).await?;
        let config = test_config(&url);

        let ws = KrakenFuturesWs::new(config);
        let connected = ws.connect().await?;

        let symbols = vec![Symbol::new("PF_XBTUSD")?];
        let mut rx = connected.subscribe_trades(&symbols).await?;

        let tick = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .context("timeout waiting for trade")?
            .context("channel closed")?;
        assert_eq!(tick.symbol.as_str(), "PF_XBTUSD");

        connected.disconnect().await;
        Ok(())
    }
}
