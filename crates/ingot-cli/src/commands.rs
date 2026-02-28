use clap::{Parser, Subcommand};
use ingot_primitives::{OrderSide, Symbol};
use rust_decimal::Decimal;

fn parse_order_side(s: &str) -> Result<OrderSide, String> {
    if s.eq_ignore_ascii_case("buy") {
        Ok(OrderSide::Buy)
    } else if s.eq_ignore_ascii_case("sell") {
        Ok(OrderSide::Sell)
    } else {
        Err(format!(
            "unknown order side '{s}': expected 'buy' or 'sell'"
        ))
    }
}

#[derive(Parser, Debug)]
#[command(name = "", bin_name = "", disable_help_subcommand = true)]
pub struct ReplCli {
    #[command(subcommand)]
    pub command: ReplCommand,
}

#[derive(Subcommand, Debug)]
pub enum ReplCommand {
    Heartbeat,
    Balances,
    Ticker {
        #[arg(default_value = "BTC-USD")]
        symbol: Symbol,
    },
    Order {
        #[arg(short, long, value_parser = parse_order_side)]
        side: OrderSide,
        #[arg(short, long)]
        qty: Decimal,
        #[arg(long, default_value = "BTC-USD")]
        symbol: Symbol,
    },
    /// Subscribe to a ticker feed for a trading pair.
    Subscribe {
        /// Trading pair symbol (e.g., ETH-USD, SOL-USD)
        symbol: Symbol,
    },
    #[command(name = "quit", visible_alias = "exit")]
    Quit,
    Help,
}
