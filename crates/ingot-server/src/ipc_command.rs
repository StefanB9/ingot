//! Bridge from iceoryx2 request/response to [`EngineHandle`] async methods.
//!
//! Runs on a dedicated OS thread because iceoryx2 has a synchronous API.
//! Uses `tokio::runtime::Handle::block_on()` to call async `EngineHandle`
//! methods from the synchronous context.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result};
use iceoryx2::{config::Config, prelude::*};
use ingot_engine::handle::EngineHandle;
use ingot_ipc::{
    SERVICE_COMMANDS,
    types::{FixedId, IpcCommand, IpcCommandResponse, IpcNavUpdate, IpcOrderUpdate},
};
use ingot_primitives::Price;
use tokio::runtime::Handle as TokioHandle;
use tracing::{debug, error, info, warn};

/// Spawn the IPC command server thread.
///
/// Returns the `JoinHandle` so the caller can join on shutdown.
pub fn spawn(
    handle: EngineHandle,
    rt_handle: TokioHandle,
    shutdown: Arc<AtomicBool>,
    start_time: Instant,
    config: Config,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("ipc-command".into())
        .spawn(move || {
            if let Err(e) = run(&handle, &rt_handle, &shutdown, start_time, &config) {
                error!(error = %e, "IPC command thread crashed");
            }
        })
        .context("failed to spawn IPC command thread")
}

fn run(
    handle: &EngineHandle,
    rt_handle: &TokioHandle,
    shutdown: &Arc<AtomicBool>,
    start_time: Instant,
    config: &Config,
) -> Result<()> {
    let node = NodeBuilder::new().config(config).create::<ipc::Service>()?;

    let svc_name: ServiceName = SERVICE_COMMANDS
        .try_into()
        .context("invalid command service name")?;

    let service = node
        .service_builder(&svc_name)
        .request_response::<IpcCommand, IpcCommandResponse>()
        .open_or_create()?;

    let server = service.server_builder().create()?;

    info!("IPC command thread started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("IPC command: shutdown flag set, exiting");
            break;
        }

        match server.receive() {
            Ok(Some(active_request)) => {
                let response = dispatch(handle, rt_handle, &active_request, start_time, shutdown);
                match active_request.loan_uninit() {
                    Ok(resp) => {
                        let resp = resp.write_payload(response);
                        if let Err(e) = resp.send() {
                            warn!(error = %e, "failed to send IPC response");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to loan response buffer");
                    }
                }
            }
            Ok(None) => {
                // No pending requests — poll again after a short sleep
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                error!(error = %e, "IPC server receive error");
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    Ok(())
}

fn dispatch(
    handle: &EngineHandle,
    rt_handle: &TokioHandle,
    cmd: &IpcCommand,
    start_time: Instant,
    shutdown: &Arc<AtomicBool>,
) -> IpcCommandResponse {
    match *cmd {
        IpcCommand::PlaceOrder(ref req) => {
            let order = ingot_core::execution::OrderRequest::from(*req);
            match rt_handle.block_on(handle.place_order_with_ack(order)) {
                Ok(ack) => IpcCommandResponse::OrderAck(IpcOrderUpdate::from(&ack)),
                Err(e) => IpcCommandResponse::Error {
                    code: 500,
                    message: FixedId::from_slice(&e.to_string()),
                },
            }
        }
        IpcCommand::QueryNav(ref currency) => match rt_handle.block_on(handle.nav(currency)) {
            Ok(money) => IpcCommandResponse::Nav(IpcNavUpdate {
                amount: money.amount,
                currency: money.currency,
            }),
            Err(e) => IpcCommandResponse::Error {
                code: 500,
                message: FixedId::from_slice(&e.to_string()),
            },
        },
        IpcCommand::QueryTicker(symbol) => match rt_handle.block_on(handle.ticker(symbol)) {
            Ok(Some(price)) => IpcCommandResponse::Ticker {
                symbol,
                price,
                found: true,
            },
            Ok(None) => IpcCommandResponse::Ticker {
                symbol,
                price: Price::from(rust_decimal::Decimal::ZERO),
                found: false,
            },
            Err(e) => IpcCommandResponse::Error {
                code: 400,
                message: FixedId::from_slice(&e.to_string()),
            },
        },
        IpcCommand::Subscribe(symbol) => {
            info!(symbol = %symbol, "subscribe command received via IPC");
            match rt_handle.block_on(handle.subscribe_ticker(symbol)) {
                Ok(()) => IpcCommandResponse::SubscribeAck {
                    symbol,
                    success: true,
                },
                Err(e) => IpcCommandResponse::Error {
                    code: 500,
                    message: FixedId::from_slice(&e.to_string()),
                },
            }
        }
        IpcCommand::Heartbeat => {
            debug!("heartbeat received via IPC");
            IpcCommandResponse::Heartbeat {
                uptime_secs: start_time.elapsed().as_secs(),
            }
        }
        IpcCommand::Shutdown => {
            info!("shutdown command received via IPC");
            shutdown.store(true, Ordering::Relaxed);
            if let Err(e) = rt_handle.block_on(handle.shutdown()) {
                warn!(error = %e, "failed to send engine shutdown command");
            }
            IpcCommandResponse::ShutdownAck { success: true }
        }
    }
}
