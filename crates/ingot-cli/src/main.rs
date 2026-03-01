mod commands;

use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use commands::{ReplCli, ReplCommand};
use iceoryx2::prelude::*;
use ingot_ipc::{
    SERVICE_COMMANDS,
    types::{IpcCommand, IpcCommandResponse, IpcOrderRequest},
};
use ingot_primitives::{Currency, OrderSide, OrderType, Quantity, Symbol};
use rust_decimal::Decimal;
use rustyline::{DefaultEditor, error::ReadlineError};
use tracing_subscriber::EnvFilter;

type IpcClient =
    iceoryx2::port::client::Client<ipc::Service, IpcCommand, (), IpcCommandResponse, ()>;

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    let _config = ingot_config::AppConfig::load();

    let file_appender = tracing_appender::rolling::daily("logs", "ingot-cli.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let (filter, env_err) = match EnvFilter::try_from_default_env() {
        Ok(f) => (f, None),
        Err(e) => (EnvFilter::new("ingot_cli=info"), Some(e)),
    };
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(false)
        .with_env_filter(filter)
        .init();
    if let Some(e) = env_err {
        tracing::warn!(error = %e, "invalid RUST_LOG filter, using default");
    }

    let mut stdout = std::io::stdout();
    writeln!(stdout, "--------------------------------------------------")?;
    writeln!(stdout, " Ingot-Rust v3.0 (IPC Client Mode)")?;
    writeln!(stdout, " Logs redirected to: logs/ingot-cli.log")?;
    writeln!(stdout, "--------------------------------------------------")?;

    // Connect to the iceoryx2 command service
    let node = NodeBuilder::new()
        .create::<ipc::Service>()
        .context("failed to create iceoryx2 node")?;

    let svc_name: ServiceName = SERVICE_COMMANDS
        .try_into()
        .context("invalid command service name")?;

    let service = node
        .service_builder(&svc_name)
        .request_response::<IpcCommand, IpcCommandResponse>()
        .open_or_create()
        .context("failed to open command service — is ingot-server running?")?;

    let client = service
        .client_builder()
        .create()
        .context("failed to create IPC client")?;

    // Verify server connectivity with a heartbeat
    match send_command(&client, IpcCommand::Heartbeat) {
        Ok(IpcCommandResponse::Heartbeat { uptime_secs }) => {
            writeln!(
                stdout,
                "[*] Connected to ingot-server (uptime: {uptime_secs}s)"
            )?;
        }
        Ok(other) => {
            writeln!(stdout, "[!] Unexpected heartbeat response: {other:?}")?;
        }
        Err(_) => {
            writeln!(stdout, "[!] No server response — is ingot-server running?")?;
            writeln!(stdout, "[*] Commands will be queued until server connects.")?;
        }
    }

    writeln!(stdout, "[*] Type 'help' for commands.")?;

    let mut rl = DefaultEditor::new()?;
    let history_path = PathBuf::from(".ingot_history");
    if rl.load_history(&history_path).is_err() {
        // No history exists yet; not an error.
    }

    loop {
        let readline = rl.readline(">> ");

        match readline {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let _ = rl.add_history_entry(line);

                let split_args = match shell_words::split(line) {
                    Ok(args) => args,
                    Err(e) => {
                        writeln!(stdout, "Error parsing command: {e}")?;
                        continue;
                    }
                };

                match ReplCli::try_parse_from(std::iter::once(String::new()).chain(split_args)) {
                    Ok(cli) => {
                        if handle_command(&cli, &client, &mut stdout)? {
                            break;
                        }
                    }
                    Err(e) => {
                        writeln!(stdout, "{e}")?;
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                writeln!(stdout, "(Ctrl-C) Exiting CLI. Server keeps running.")?;
                break;
            }
            Err(ReadlineError::Eof) => {
                writeln!(stdout, "(Ctrl-D) Exiting CLI. Server keeps running.")?;
                break;
            }
            Err(err) => {
                writeln!(stdout, "Error: {err:?}")?;
                break;
            }
        }
    }

    let _ = rl.save_history(&history_path);

    Ok(())
}

/// Handle a parsed REPL command. Returns `true` if the REPL should exit.
fn handle_command(cli: &ReplCli, client: &IpcClient, stdout: &mut std::io::Stdout) -> Result<bool> {
    match cli.command {
        ReplCommand::Heartbeat => {
            writeln!(stdout, "[*] Sending Heartbeat...")?;
            match send_command(client, IpcCommand::Heartbeat) {
                Ok(IpcCommandResponse::Heartbeat { uptime_secs }) => {
                    writeln!(stdout, "[*] Server alive (uptime: {uptime_secs}s)")?;
                }
                Ok(resp) => writeln!(stdout, "[!] Unexpected: {resp:?}")?,
                Err(e) => writeln!(stdout, "[!] {e}")?,
            }
        }
        ReplCommand::Quit => {
            writeln!(stdout, "[*] Exiting CLI. Server keeps running.")?;
            return Ok(true);
        }
        ReplCommand::Shutdown => {
            writeln!(stdout, "[*] Shutting down server...")?;
            match send_command(client, IpcCommand::Shutdown) {
                Ok(IpcCommandResponse::ShutdownAck { success: true }) => {
                    writeln!(stdout, "[*] Server acknowledged shutdown")?;
                }
                Ok(resp) => writeln!(stdout, "[!] Unexpected: {resp:?}")?,
                Err(e) => writeln!(stdout, "[!] Shutdown error: {e}")?,
            }
            return Ok(true);
        }
        ReplCommand::Balances => {
            match send_command(client, IpcCommand::QueryNav(Currency::usd())) {
                Ok(IpcCommandResponse::Nav(nav)) => {
                    writeln!(
                        stdout,
                        "Net Asset Value: {:.2} {}",
                        nav.amount, nav.currency
                    )?;
                }
                Ok(IpcCommandResponse::Error { message, .. }) => {
                    writeln!(stdout, "Error calculating NAV: {}", message.as_str())?;
                }
                Ok(resp) => writeln!(stdout, "[!] Unexpected: {resp:?}")?,
                Err(e) => writeln!(stdout, "[!] {e}")?,
            }
        }
        ReplCommand::Ticker { symbol } => {
            match send_command(client, IpcCommand::QueryTicker(symbol)) {
                Ok(IpcCommandResponse::Ticker {
                    symbol: sym,
                    price,
                    found: true,
                }) => {
                    writeln!(stdout, "{sym} = {price}")?;
                }
                Ok(IpcCommandResponse::Ticker { found: false, .. }) => {
                    writeln!(stdout, "No rate data for {symbol}")?;
                }
                Ok(IpcCommandResponse::Error { message, .. }) => {
                    writeln!(stdout, "Error: {}", message.as_str())?;
                }
                Ok(resp) => writeln!(stdout, "[!] Unexpected: {resp:?}")?,
                Err(e) => writeln!(stdout, "[!] {e}")?,
            }
        }
        ReplCommand::Order { side, qty, symbol } => {
            handle_order(client, stdout, side, qty, symbol)?;
        }
        ReplCommand::Subscribe { symbol } => {
            writeln!(stdout, "[*] Subscribing to {symbol}...")?;
            match send_command(client, IpcCommand::Subscribe(symbol)) {
                Ok(IpcCommandResponse::SubscribeAck {
                    symbol: sym,
                    success: true,
                }) => {
                    writeln!(stdout, "[+] Subscribed to {sym}")?;
                }
                Ok(IpcCommandResponse::SubscribeAck { success: false, .. }) => {
                    writeln!(stdout, "[!] Subscription failed for {symbol}")?;
                }
                Ok(IpcCommandResponse::Error { message, .. }) => {
                    writeln!(stdout, "[!] Error: {}", message.as_str())?;
                }
                Ok(resp) => writeln!(stdout, "[!] Unexpected: {resp:?}")?,
                Err(e) => writeln!(stdout, "[!] {e}")?,
            }
        }
        ReplCommand::Help => {
            print_help(stdout)?;
        }
    }
    Ok(false)
}

fn handle_order(
    client: &IpcClient,
    stdout: &mut std::io::Stdout,
    side: OrderSide,
    qty: Decimal,
    symbol: Symbol,
) -> Result<()> {
    let ipc_req = IpcOrderRequest {
        symbol,
        side,
        order_type: OrderType::Market,
        quantity: Quantity::from(qty),
        has_price: false,
        price: ingot_primitives::Price::from(Decimal::ZERO),
        validate_only: false,
    };

    writeln!(stdout, "[>] Sending Order: {side} {qty} {symbol}")?;
    match send_command(client, IpcCommand::PlaceOrder(ipc_req)) {
        Ok(IpcCommandResponse::OrderAck(ack)) => {
            writeln!(
                stdout,
                "[*] Order {}: {}",
                ack.exchange_id.as_str(),
                ack.status
            )?;
        }
        Ok(IpcCommandResponse::Error { message, .. }) => {
            writeln!(stdout, "Error placing order: {}", message.as_str())?;
        }
        Ok(resp) => writeln!(stdout, "[!] Unexpected: {resp:?}")?,
        Err(e) => writeln!(stdout, "[!] {e}")?,
    }
    Ok(())
}

fn print_help(stdout: &mut std::io::Stdout) -> Result<()> {
    writeln!(stdout, "Available commands:")?;
    writeln!(
        stdout,
        "  heartbeat              Send a heartbeat to the server"
    )?;
    writeln!(stdout, "  balances               Show net asset value")?;
    writeln!(
        stdout,
        "  ticker [SYMBOL]        Show price (default: BTC-USD)"
    )?;
    writeln!(
        stdout,
        "  subscribe <SYMBOL>     Subscribe to ticker feed (e.g., ETH-USD)"
    )?;
    writeln!(
        stdout,
        "  order -s <buy|sell> -q <qty> [--symbol <SYM>]  Place a market order"
    )?;
    writeln!(
        stdout,
        "  quit | exit            Exit the CLI (server keeps running)"
    )?;
    writeln!(
        stdout,
        "  shutdown               Shut down the remote ingot-server"
    )?;
    writeln!(stdout, "  help                   Show this help message")?;
    Ok(())
}

/// Send an IPC command and wait for a response with a timeout.
fn send_command(client: &IpcClient, cmd: IpcCommand) -> Result<IpcCommandResponse> {
    let pending = client
        .send_copy(cmd)
        .map_err(|e| anyhow::anyhow!("failed to send IPC command: {e:?}"))?;

    // Poll for response with a timeout (up to 5 seconds)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match pending.receive() {
            Ok(Some(response)) => return Ok(*response),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    anyhow::bail!("IPC command timed out (no response within 5s)");
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => anyhow::bail!("IPC receive error: {e:?}"),
        }
    }
}
