use ingot_core::OrderFill;
use ingot_primitives::{Amount, OrderSide, Price, Quantity, Symbol};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::result::{BacktestResult, EquityPoint};

/// Complete performance metrics computed from a backtest result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    // Return metrics
    /// Total return as a decimal ratio (e.g., 0.15 = 15%).
    pub total_return: Decimal,
    /// Absolute profit/loss (final - initial capital).
    pub total_pnl: Amount,
    /// Sum of all fill fees.
    pub total_fees: Amount,

    // Risk metrics
    /// Maximum drawdown as a negative ratio (e.g., -0.10 = -10%).
    pub max_drawdown: Decimal,
    /// Number of equity curve points from peak to trough.
    pub max_drawdown_duration: i64,

    // Trade statistics
    /// Number of completed round-trip trades.
    pub total_trades: u32,
    /// Number of profitable round-trips.
    pub winning_trades: u32,
    /// Number of losing round-trips.
    pub losing_trades: u32,
    /// Win rate as a decimal ratio (e.g., 0.60 = 60%).
    pub win_rate: Decimal,
    /// Average profit of winning trades.
    pub avg_win: Amount,
    /// Average loss of losing trades (negative).
    pub avg_loss: Amount,
    /// Gross profit divided by gross loss. Zero if no losses.
    pub profit_factor: Decimal,
    /// Largest single-trade profit.
    pub largest_win: Amount,
    /// Largest single-trade loss (negative).
    pub largest_loss: Amount,

    // Risk-adjusted
    /// Annualized Sharpe ratio. Zero if insufficient data.
    pub sharpe_ratio: Decimal,
    /// Annualized Sortino ratio. Zero if insufficient data.
    pub sortino_ratio: Decimal,

    // Efficiency
    /// Average `PnL` per completed trade.
    pub average_trade_pnl: Amount,
    /// Ratio of total fees to absolute `PnL`. Zero if `PnL` is zero.
    pub fee_to_pnl_ratio: Decimal,
}

/// Compute all performance metrics from a backtest result.
///
/// `risk_free_rate` is the annualized risk-free rate as a decimal (e.g., 0.05 =
/// 5%).
#[allow(clippy::too_many_lines)]
pub fn compute_metrics(result: &BacktestResult, risk_free_rate: Decimal) -> PerformanceMetrics {
    // Return metrics
    let total_pnl = result.final_capital - result.initial_capital;
    let total_return = if result.initial_capital.value() == Decimal::ZERO {
        Decimal::ZERO
    } else {
        total_pnl.value() / result.initial_capital.value()
    };
    let total_fees = result
        .fills
        .iter()
        .fold(Amount::zero(), |acc, f| acc + f.fee);

    // Risk metrics
    let (max_drawdown, max_drawdown_duration) = compute_max_drawdown(&result.equity_curve);

    // Trade statistics
    let round_trips = pair_fills(&result.fills);
    #[allow(clippy::cast_possible_truncation)]
    let total_trades = round_trips.len() as u32;

    let mut winning_trades: u32 = 0;
    let mut losing_trades: u32 = 0;
    let mut gross_profit = Amount::zero();
    let mut gross_loss = Amount::zero();
    let mut largest_win = Amount::zero();
    let mut largest_loss = Amount::zero();
    let mut total_win_pnl = Amount::zero();
    let mut total_loss_pnl = Amount::zero();

    for rt in &round_trips {
        if rt.pnl.value() > Decimal::ZERO {
            winning_trades += 1;
            gross_profit = gross_profit + rt.pnl;
            total_win_pnl = total_win_pnl + rt.pnl;
            if rt.pnl.value() > largest_win.value() {
                largest_win = rt.pnl;
            }
        } else {
            losing_trades += 1;
            gross_loss = gross_loss + rt.pnl; // negative
            total_loss_pnl = total_loss_pnl + rt.pnl;
            if rt.pnl.value() < largest_loss.value() {
                largest_loss = rt.pnl;
            }
        }
    }

    let win_rate = if total_trades == 0 {
        Decimal::ZERO
    } else {
        Decimal::from(winning_trades) / Decimal::from(total_trades)
    };

    let avg_win = if winning_trades == 0 {
        Amount::zero()
    } else {
        Amount::new(total_win_pnl.value() / Decimal::from(winning_trades))
    };

    let avg_loss = if losing_trades == 0 {
        Amount::zero()
    } else {
        Amount::new(total_loss_pnl.value() / Decimal::from(losing_trades))
    };

    let profit_factor = if gross_loss.value() == Decimal::ZERO {
        Decimal::ZERO
    } else {
        gross_profit.value() / gross_loss.value().abs()
    };

    let average_trade_pnl = if total_trades == 0 {
        Amount::zero()
    } else {
        let total_rt_pnl: Amount = round_trips.iter().fold(Amount::zero(), |a, rt| a + rt.pnl);
        Amount::new(total_rt_pnl.value() / Decimal::from(total_trades))
    };

    // Risk-adjusted ratios
    let returns = compute_returns(&result.equity_curve);
    let periods_per_year = infer_periods_per_year(&result.equity_curve);
    let sharpe_ratio = compute_sharpe(&returns, risk_free_rate, periods_per_year);
    let sortino_ratio = compute_sortino(&returns, risk_free_rate, periods_per_year);

    // Efficiency
    let fee_to_pnl_ratio = if total_pnl.value() == Decimal::ZERO {
        Decimal::ZERO
    } else {
        total_fees.value() / total_pnl.value().abs()
    };

    PerformanceMetrics {
        total_return,
        total_pnl,
        total_fees,
        max_drawdown,
        max_drawdown_duration,
        total_trades,
        winning_trades,
        losing_trades,
        win_rate,
        avg_win,
        avg_loss,
        profit_factor,
        largest_win,
        largest_loss,
        sharpe_ratio,
        sortino_ratio,
        average_trade_pnl,
        fee_to_pnl_ratio,
    }
}

// ── Internal helpers ────────────────────────────────────────────────────

/// A completed round-trip trade (entry + exit).
#[derive(Debug, Clone)]
struct RoundTrip {
    #[allow(dead_code)]
    symbol: Symbol,
    #[allow(dead_code)]
    side: OrderSide,
    #[allow(dead_code)]
    quantity: Quantity,
    #[allow(dead_code)]
    entry_price: Price,
    #[allow(dead_code)]
    exit_price: Price,
    #[allow(dead_code)]
    entry_fee: Amount,
    #[allow(dead_code)]
    exit_fee: Amount,
    pnl: Amount,
}

/// Pair fills into round-trip trades using FIFO per symbol.
///
/// A Buy opens a long; the next Sell on the same symbol closes it.
/// Unmatched fills (open positions at end) are not counted.
fn pair_fills(fills: &[OrderFill]) -> Vec<RoundTrip> {
    use std::collections::HashMap;

    // Per-symbol queues of opening fills
    let mut open_buys: HashMap<Symbol, Vec<&OrderFill>> = HashMap::new();
    let mut open_sells: HashMap<Symbol, Vec<&OrderFill>> = HashMap::new();
    let mut round_trips = Vec::new();

    for fill in fills {
        match fill.side {
            OrderSide::Buy => {
                // Check if this closes an existing short
                let closes_short = open_sells
                    .get(&fill.symbol)
                    .is_some_and(|sells| !sells.is_empty());

                if closes_short {
                    let entry = open_sells.get_mut(&fill.symbol).and_then(|sells| {
                        if sells.is_empty() {
                            None
                        } else {
                            Some(sells.remove(0))
                        }
                    });
                    if let Some(entry_fill) = entry {
                        let pnl = compute_trade_pnl(entry_fill, fill);
                        round_trips.push(RoundTrip {
                            symbol: fill.symbol.clone(),
                            side: OrderSide::Sell,
                            quantity: fill.fill_quantity,
                            entry_price: entry_fill.fill_price,
                            exit_price: fill.fill_price,
                            entry_fee: entry_fill.fee,
                            exit_fee: fill.fee,
                            pnl,
                        });
                    }
                } else {
                    open_buys.entry(fill.symbol.clone()).or_default().push(fill);
                }
            }
            OrderSide::Sell => {
                // Check if this closes an existing long
                let closes_long = open_buys
                    .get(&fill.symbol)
                    .is_some_and(|buys| !buys.is_empty());

                if closes_long {
                    let entry = open_buys.get_mut(&fill.symbol).and_then(|buys| {
                        if buys.is_empty() {
                            None
                        } else {
                            Some(buys.remove(0))
                        }
                    });
                    if let Some(entry_fill) = entry {
                        let pnl = compute_trade_pnl(entry_fill, fill);
                        round_trips.push(RoundTrip {
                            symbol: fill.symbol.clone(),
                            side: OrderSide::Buy,
                            quantity: fill.fill_quantity,
                            entry_price: entry_fill.fill_price,
                            exit_price: fill.fill_price,
                            entry_fee: entry_fill.fee,
                            exit_fee: fill.fee,
                            pnl,
                        });
                    }
                } else {
                    open_sells
                        .entry(fill.symbol.clone())
                        .or_default()
                        .push(fill);
                }
            }
        }
    }

    round_trips
}

/// Compute `PnL` for a round-trip: `(exit_price - entry_price) * quantity -
/// fees`. For short trades (entry is `Sell`), `PnL` is inverted.
fn compute_trade_pnl(entry: &OrderFill, exit: &OrderFill) -> Amount {
    let price_diff = exit.fill_price.value() - entry.fill_price.value();
    let direction = if entry.side == OrderSide::Buy {
        Decimal::ONE
    } else {
        -Decimal::ONE
    };
    let gross = price_diff * entry.fill_quantity.value() * direction;
    Amount::new(gross) - entry.fee - exit.fee
}

/// Compute period returns from equity curve NAV values.
///
/// Returns `(nav[i] - nav[i-1]) / nav[i-1]` for each consecutive pair.
fn compute_returns(equity_curve: &[EquityPoint]) -> Vec<Decimal> {
    if equity_curve.len() < 2 {
        return Vec::new();
    }

    equity_curve
        .windows(2)
        .filter_map(|w| {
            let prev = w[0].nav.value();
            if prev == Decimal::ZERO {
                None
            } else {
                Some((w[1].nav.value() - prev) / prev)
            }
        })
        .collect()
}

/// Compute max drawdown from equity curve.
///
/// Returns `(drawdown_ratio, duration)` where `drawdown_ratio` is negative
/// and duration is the number of equity points from peak to trough.
fn compute_max_drawdown(equity_curve: &[EquityPoint]) -> (Decimal, i64) {
    if equity_curve.len() < 2 {
        return (Decimal::ZERO, 0);
    }

    let mut peak = equity_curve[0].nav.value();
    let mut max_dd = Decimal::ZERO;
    let mut max_dd_duration: i64 = 0;
    let mut peak_idx: usize = 0;

    for (i, point) in equity_curve.iter().enumerate() {
        let nav = point.nav.value();
        if nav >= peak {
            peak = nav;
            peak_idx = i;
        } else if peak > Decimal::ZERO {
            let dd = (nav - peak) / peak;
            if dd < max_dd {
                max_dd = dd;
                #[allow(clippy::cast_possible_wrap)]
                {
                    max_dd_duration = (i - peak_idx) as i64;
                }
            }
        }
    }

    (max_dd, max_dd_duration)
}

/// Infer periods per year from equity curve timestamp intervals.
///
/// Uses the median interval between consecutive points.
/// Returns `Decimal::ZERO` if fewer than 2 points.
fn infer_periods_per_year(equity_curve: &[EquityPoint]) -> Decimal {
    if equity_curve.len() < 2 {
        return Decimal::ZERO;
    }

    let mut intervals: Vec<i64> = equity_curve
        .windows(2)
        .map(|w| {
            w[1].timestamp
                .signed_duration_since(w[0].timestamp)
                .num_seconds()
        })
        .filter(|s| *s > 0)
        .collect();

    if intervals.is_empty() {
        return Decimal::ZERO;
    }

    intervals.sort_unstable();
    let median_seconds = intervals[intervals.len() / 2];

    if median_seconds == 0 {
        return Decimal::ZERO;
    }

    // 365.25 days * 24 * 3600 = 31_557_600 seconds per year
    let seconds_per_year = Decimal::new(31_557_600, 0);
    seconds_per_year / Decimal::from(median_seconds)
}

/// Annualized Sharpe ratio from period returns.
///
/// `sharpe = (mean_return - rf_per_period) / std_dev * sqrt(periods_per_year)`
///
/// Returns `Decimal::ZERO` if fewer than 2 returns or zero standard deviation.
fn compute_sharpe(
    returns: &[Decimal],
    risk_free_rate: Decimal,
    periods_per_year: Decimal,
) -> Decimal {
    if returns.len() < 2 || periods_per_year == Decimal::ZERO {
        return Decimal::ZERO;
    }

    let n = Decimal::from(returns.len() as u64);
    let mean = returns.iter().copied().sum::<Decimal>() / n;
    let rf_per_period = risk_free_rate / periods_per_year;
    let excess = mean - rf_per_period;
    let sigma = std_dev(returns);

    if sigma == Decimal::ZERO {
        return Decimal::ZERO;
    }

    (excess / sigma) * decimal_sqrt(periods_per_year)
}

/// Annualized Sortino ratio. Uses downside deviation instead of full std dev.
///
/// Returns `Decimal::ZERO` if fewer than 2 returns or zero downside deviation.
fn compute_sortino(
    returns: &[Decimal],
    risk_free_rate: Decimal,
    periods_per_year: Decimal,
) -> Decimal {
    if returns.len() < 2 || periods_per_year == Decimal::ZERO {
        return Decimal::ZERO;
    }

    let n = Decimal::from(returns.len() as u64);
    let mean = returns.iter().copied().sum::<Decimal>() / n;
    let rf_per_period = risk_free_rate / periods_per_year;
    let excess = mean - rf_per_period;
    let dd = downside_dev(returns, rf_per_period);

    if dd == Decimal::ZERO {
        return Decimal::ZERO;
    }

    (excess / dd) * decimal_sqrt(periods_per_year)
}

/// Standard deviation of a series of Decimal values.
///
/// Returns `Decimal::ZERO` for fewer than 2 values.
fn std_dev(values: &[Decimal]) -> Decimal {
    if values.len() < 2 {
        return Decimal::ZERO;
    }

    let n = Decimal::from(values.len() as u64);
    let mean = values.iter().copied().sum::<Decimal>() / n;
    let variance = values
        .iter()
        .map(|v| {
            let diff = *v - mean;
            diff * diff
        })
        .sum::<Decimal>()
        / (n - Decimal::ONE);

    decimal_sqrt(variance)
}

/// Downside deviation: standard deviation of values below the target.
///
/// Returns `Decimal::ZERO` if no values are below the target.
fn downside_dev(values: &[Decimal], target: Decimal) -> Decimal {
    if values.len() < 2 {
        return Decimal::ZERO;
    }

    let n = Decimal::from(values.len() as u64);
    let downside_sum = values
        .iter()
        .map(|v| {
            let diff = *v - target;
            if diff < Decimal::ZERO {
                diff * diff
            } else {
                Decimal::ZERO
            }
        })
        .sum::<Decimal>();

    decimal_sqrt(downside_sum / n)
}

/// Newton's method square root for `Decimal`.
///
/// Returns `Decimal::ZERO` for non-positive inputs.
fn decimal_sqrt(value: Decimal) -> Decimal {
    if value <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    let mut guess = value / Decimal::TWO;
    if guess == Decimal::ZERO {
        guess = Decimal::ONE;
    }

    for _ in 0..30 {
        let next = (guess + value / guess) / Decimal::TWO;
        if next == guess {
            break;
        }
        guess = next;
    }

    guess
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use ingot_core::{OrderFill, OrderId};
    use ingot_primitives::{Amount, Currency, OrderSide, Price, Quantity, Symbol};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::result::{BacktestResult, EquityPoint};

    // ── Helpers ─────────────────────────────────────────────────────────

    fn make_equity_point(
        time_str: &str,
        nav: Decimal,
    ) -> Result<EquityPoint, Box<dyn std::error::Error>> {
        Ok(EquityPoint {
            timestamp: DateTime::parse_from_rfc3339(time_str)?.to_utc(),
            nav: Amount::new(nav),
            cash: Amount::new(nav),
            positions_value: Amount::zero(),
        })
    }

    fn make_fill(
        symbol: &str,
        side: OrderSide,
        price: Decimal,
        qty: Decimal,
        fee: Decimal,
        time_str: &str,
    ) -> Result<OrderFill, Box<dyn std::error::Error>> {
        Ok(OrderFill {
            order_id: OrderId::new(&format!("ORD-{time_str}"))?,
            symbol: Symbol::new(symbol)?,
            side,
            fill_price: Price::new(price),
            fill_quantity: Quantity::new(qty)?,
            fee: Amount::new(fee),
            fee_currency: Currency::USD,
            timestamp: DateTime::parse_from_rfc3339(time_str)?.to_utc(),
            trade_id: None,
        })
    }

    fn make_result(
        fills: Vec<OrderFill>,
        equity_curve: Vec<EquityPoint>,
        initial: Decimal,
        final_val: Decimal,
    ) -> BacktestResult {
        let start = equity_curve.first().map_or_else(Utc::now, |e| e.timestamp);
        let end = equity_curve.last().map_or_else(Utc::now, |e| e.timestamp);
        BacktestResult {
            fills,
            transactions: Vec::new(),
            equity_curve,
            start_time: start,
            end_time: end,
            initial_capital: Amount::new(initial),
            final_capital: Amount::new(final_val),
        }
    }

    // ── Test 1: decimal_sqrt perfect squares ────────────────────────────

    #[test]
    fn test_decimal_sqrt_perfect_square() {
        assert_eq!(decimal_sqrt(dec!(4)), dec!(2));
        assert_eq!(decimal_sqrt(dec!(9)), dec!(3));
        assert_eq!(decimal_sqrt(dec!(100)), dec!(10));
        assert_eq!(decimal_sqrt(dec!(1)), dec!(1));
    }

    // ── Test 2: decimal_sqrt non-perfect ────────────────────────────────

    #[test]
    fn test_decimal_sqrt_non_perfect() {
        let sqrt2 = decimal_sqrt(dec!(2));
        let expected = dec!(1.4142135623730950488);
        let diff = (sqrt2 - expected).abs();
        assert!(
            diff < dec!(0.0000000001),
            "sqrt(2) = {sqrt2}, diff = {diff}"
        );
    }

    // ── Test 3: decimal_sqrt zero and negative ──────────────────────────

    #[test]
    fn test_decimal_sqrt_zero_and_negative() {
        assert_eq!(decimal_sqrt(Decimal::ZERO), Decimal::ZERO);
        assert_eq!(decimal_sqrt(dec!(-1)), Decimal::ZERO);
        assert_eq!(decimal_sqrt(dec!(-100)), Decimal::ZERO);
    }

    // ── Test 4: compute_returns basic ───────────────────────────────────

    #[test]
    fn test_compute_returns_basic() -> Result<(), Box<dyn std::error::Error>> {
        let curve = vec![
            make_equity_point("2025-01-01T00:00:00Z", dec!(100))?,
            make_equity_point("2025-01-01T01:00:00Z", dec!(110))?,
            make_equity_point("2025-01-01T02:00:00Z", dec!(105))?,
        ];
        let returns = compute_returns(&curve);
        assert_eq!(returns.len(), 2);
        assert_eq!(returns[0], dec!(0.1)); // 10/100
        // (105-110)/110 = -5/110
        let expected = dec!(-5) / dec!(110);
        assert_eq!(returns[1], expected);
        Ok(())
    }

    // ── Test 5: compute_returns empty and single ────────────────────────

    #[test]
    fn test_compute_returns_empty_and_single() -> Result<(), Box<dyn std::error::Error>> {
        assert!(compute_returns(&[]).is_empty());
        let single = vec![make_equity_point("2025-01-01T00:00:00Z", dec!(100))?];
        assert!(compute_returns(&single).is_empty());
        Ok(())
    }

    // ── Test 6: pair_fills single round trip ────────────────────────────

    #[test]
    fn test_pair_fills_single_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let fills = vec![
            make_fill(
                "BTCUSD",
                OrderSide::Buy,
                dec!(100),
                dec!(1),
                dec!(1),
                "2025-01-01T00:00:00Z",
            )?,
            make_fill(
                "BTCUSD",
                OrderSide::Sell,
                dec!(110),
                dec!(1),
                dec!(1),
                "2025-01-01T01:00:00Z",
            )?,
        ];
        let rts = pair_fills(&fills);
        assert_eq!(rts.len(), 1);
        // PnL = (110 - 100) * 1 - 1 - 1 = 8
        assert_eq!(rts[0].pnl, Amount::new(dec!(8)));
        Ok(())
    }

    // ── Test 7: pair_fills multiple round trips ─────────────────────────

    #[test]
    fn test_pair_fills_multiple_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let fills = vec![
            make_fill(
                "BTCUSD",
                OrderSide::Buy,
                dec!(100),
                dec!(1),
                dec!(0),
                "2025-01-01T00:00:00Z",
            )?,
            make_fill(
                "BTCUSD",
                OrderSide::Sell,
                dec!(110),
                dec!(1),
                dec!(0),
                "2025-01-01T01:00:00Z",
            )?,
            make_fill(
                "BTCUSD",
                OrderSide::Buy,
                dec!(105),
                dec!(1),
                dec!(0),
                "2025-01-01T02:00:00Z",
            )?,
            make_fill(
                "BTCUSD",
                OrderSide::Sell,
                dec!(95),
                dec!(1),
                dec!(0),
                "2025-01-01T03:00:00Z",
            )?,
        ];
        let rts = pair_fills(&fills);
        assert_eq!(rts.len(), 2);
        assert_eq!(rts[0].pnl, Amount::new(dec!(10))); // 110 - 100
        assert_eq!(rts[1].pnl, Amount::new(dec!(-10))); // 95 - 105
        Ok(())
    }

    // ── Test 8: pair_fills unmatched open position ──────────────────────

    #[test]
    fn test_pair_fills_unmatched_open_position() -> Result<(), Box<dyn std::error::Error>> {
        let fills = vec![make_fill(
            "BTCUSD",
            OrderSide::Buy,
            dec!(100),
            dec!(1),
            dec!(0),
            "2025-01-01T00:00:00Z",
        )?];
        let rts = pair_fills(&fills);
        assert!(rts.is_empty());
        Ok(())
    }

    // ── Test 9: pair_fills empty ────────────────────────────────────────

    #[test]
    fn test_pair_fills_empty() {
        let rts = pair_fills(&[]);
        assert!(rts.is_empty());
    }

    // ── Test 10: max drawdown no drawdown ───────────────────────────────

    #[test]
    fn test_max_drawdown_no_drawdown() -> Result<(), Box<dyn std::error::Error>> {
        let curve = vec![
            make_equity_point("2025-01-01T00:00:00Z", dec!(100))?,
            make_equity_point("2025-01-01T01:00:00Z", dec!(110))?,
            make_equity_point("2025-01-01T02:00:00Z", dec!(120))?,
        ];
        let (dd, dur) = compute_max_drawdown(&curve);
        assert_eq!(dd, Decimal::ZERO);
        assert_eq!(dur, 0);
        Ok(())
    }

    // ── Test 11: max drawdown simple ────────────────────────────────────

    #[test]
    fn test_max_drawdown_simple() -> Result<(), Box<dyn std::error::Error>> {
        let curve = vec![
            make_equity_point("2025-01-01T00:00:00Z", dec!(100))?,
            make_equity_point("2025-01-01T01:00:00Z", dec!(120))?,
            make_equity_point("2025-01-01T02:00:00Z", dec!(90))?,
            make_equity_point("2025-01-01T03:00:00Z", dec!(110))?,
        ];
        let (dd, dur) = compute_max_drawdown(&curve);
        // DD from 120 to 90 = -30/120 = -0.25
        assert_eq!(dd, dec!(-30) / dec!(120));
        assert_eq!(dur, 1); // peak at idx 1, trough at idx 2
        Ok(())
    }

    // ── Test 12: max drawdown at end ────────────────────────────────────

    #[test]
    fn test_max_drawdown_at_end() -> Result<(), Box<dyn std::error::Error>> {
        let curve = vec![
            make_equity_point("2025-01-01T00:00:00Z", dec!(100))?,
            make_equity_point("2025-01-01T01:00:00Z", dec!(120))?,
            make_equity_point("2025-01-01T02:00:00Z", dec!(80))?,
        ];
        let (dd, _dur) = compute_max_drawdown(&curve);
        // DD from 120 to 80 = -40/120 = -1/3
        let expected = dec!(-40) / dec!(120);
        assert_eq!(dd, expected);
        Ok(())
    }

    // ── Test 13: sharpe ratio positive returns ──────────────────────────

    #[test]
    fn test_sharpe_ratio_positive_returns() {
        // Simple known case: returns [0.01, 0.02, 0.03, 0.01, 0.02]
        let returns = vec![dec!(0.01), dec!(0.02), dec!(0.03), dec!(0.01), dec!(0.02)];
        let sharpe = compute_sharpe(&returns, dec!(0.0), dec!(252)); // daily, rf=0
        // Mean = 0.018, std_dev should be small, sharpe should be positive
        assert!(sharpe > Decimal::ZERO, "sharpe = {sharpe}");
    }

    // ── Test 14: sharpe ratio zero volatility ───────────────────────────

    #[test]
    fn test_sharpe_ratio_zero_volatility() {
        let returns = vec![dec!(0.01), dec!(0.01), dec!(0.01)];
        let sharpe = compute_sharpe(&returns, dec!(0.0), dec!(252));
        assert_eq!(sharpe, Decimal::ZERO);
    }

    // ── Test 15: sortino ratio no downside ──────────────────────────────

    #[test]
    fn test_sortino_ratio_no_downside() {
        let returns = vec![dec!(0.01), dec!(0.02), dec!(0.03)];
        let sortino = compute_sortino(&returns, dec!(0.0), dec!(252));
        // All returns above target (0), downside dev = 0 → sortino = 0
        assert_eq!(sortino, Decimal::ZERO);
    }

    // ── Test 16: sortino ratio with downside ────────────────────────────

    #[test]
    fn test_sortino_ratio_with_downside() {
        let returns = vec![dec!(0.05), dec!(-0.02), dec!(0.03), dec!(-0.01), dec!(0.04)];
        let sortino = compute_sortino(&returns, dec!(0.0), dec!(252));
        // Has downside returns, mean is positive → sortino should be positive
        assert!(sortino > Decimal::ZERO, "sortino = {sortino}");
    }

    // ── Test 17: compute_metrics zero fills ─────────────────────────────

    #[test]
    fn test_compute_metrics_zero_fills() -> Result<(), Box<dyn std::error::Error>> {
        let curve = vec![
            make_equity_point("2025-01-01T00:00:00Z", dec!(100000))?,
            make_equity_point("2025-01-01T01:00:00Z", dec!(100000))?,
        ];
        let result = make_result(vec![], curve, dec!(100000), dec!(100000));
        let metrics = compute_metrics(&result, dec!(0.05));

        assert_eq!(metrics.total_return, Decimal::ZERO);
        assert_eq!(metrics.total_pnl, Amount::zero());
        assert_eq!(metrics.total_fees, Amount::zero());
        assert_eq!(metrics.total_trades, 0);
        assert_eq!(metrics.win_rate, Decimal::ZERO);
        assert_eq!(metrics.profit_factor, Decimal::ZERO);
        Ok(())
    }

    // ── Test 18: compute_metrics single profitable round trip ───────────

    #[test]
    fn test_compute_metrics_single_profitable_round_trip() -> Result<(), Box<dyn std::error::Error>>
    {
        let fills = vec![
            make_fill(
                "BTCUSD",
                OrderSide::Buy,
                dec!(100),
                dec!(10),
                dec!(1),
                "2025-01-01T00:00:00Z",
            )?,
            make_fill(
                "BTCUSD",
                OrderSide::Sell,
                dec!(110),
                dec!(10),
                dec!(1),
                "2025-01-01T01:00:00Z",
            )?,
        ];
        let curve = vec![
            make_equity_point("2025-01-01T00:00:00Z", dec!(10000))?,
            make_equity_point("2025-01-01T00:30:00Z", dec!(9001))?, // after buy
            make_equity_point("2025-01-01T01:00:00Z", dec!(10098))?, // after sell
        ];
        let result = make_result(fills, curve, dec!(10000), dec!(10098));
        let metrics = compute_metrics(&result, dec!(0.0));

        assert_eq!(metrics.total_trades, 1);
        assert_eq!(metrics.winning_trades, 1);
        assert_eq!(metrics.losing_trades, 0);
        assert_eq!(metrics.win_rate, dec!(1));
        assert_eq!(metrics.total_fees, Amount::new(dec!(2)));
        assert!(metrics.total_return > Decimal::ZERO);
        Ok(())
    }

    // ── Test 19: compute_metrics mixed trades ───────────────────────────

    #[test]
    fn test_compute_metrics_mixed_trades() -> Result<(), Box<dyn std::error::Error>> {
        let fills = vec![
            // Trade 1: Win — buy 100, sell 120, no fees → PnL = +20
            make_fill(
                "BTCUSD",
                OrderSide::Buy,
                dec!(100),
                dec!(1),
                dec!(0),
                "2025-01-01T00:00:00Z",
            )?,
            make_fill(
                "BTCUSD",
                OrderSide::Sell,
                dec!(120),
                dec!(1),
                dec!(0),
                "2025-01-01T01:00:00Z",
            )?,
            // Trade 2: Loss — buy 110, sell 90, no fees → PnL = -20
            make_fill(
                "BTCUSD",
                OrderSide::Buy,
                dec!(110),
                dec!(1),
                dec!(0),
                "2025-01-01T02:00:00Z",
            )?,
            make_fill(
                "BTCUSD",
                OrderSide::Sell,
                dec!(90),
                dec!(1),
                dec!(0),
                "2025-01-01T03:00:00Z",
            )?,
        ];
        let curve = vec![
            make_equity_point("2025-01-01T00:00:00Z", dec!(10000))?,
            make_equity_point("2025-01-01T03:00:00Z", dec!(10000))?,
        ];
        let result = make_result(fills, curve, dec!(10000), dec!(10000));
        let metrics = compute_metrics(&result, dec!(0.0));

        assert_eq!(metrics.total_trades, 2);
        assert_eq!(metrics.winning_trades, 1);
        assert_eq!(metrics.losing_trades, 1);
        assert_eq!(metrics.win_rate, dec!(0.5));
        assert_eq!(metrics.avg_win, Amount::new(dec!(20)));
        assert_eq!(metrics.avg_loss, Amount::new(dec!(-20)));
        // profit_factor = 20 / 20 = 1
        assert_eq!(metrics.profit_factor, dec!(1));
        assert_eq!(metrics.largest_win, Amount::new(dec!(20)));
        assert_eq!(metrics.largest_loss, Amount::new(dec!(-20)));
        Ok(())
    }

    // ── Test 20: fee_to_pnl_ratio ───────────────────────────────────────

    #[test]
    fn test_compute_metrics_fee_to_pnl_ratio() -> Result<(), Box<dyn std::error::Error>> {
        let fills = vec![
            make_fill(
                "BTCUSD",
                OrderSide::Buy,
                dec!(100),
                dec!(1),
                dec!(5),
                "2025-01-01T00:00:00Z",
            )?,
            make_fill(
                "BTCUSD",
                OrderSide::Sell,
                dec!(200),
                dec!(1),
                dec!(5),
                "2025-01-01T01:00:00Z",
            )?,
        ];
        let curve = vec![
            make_equity_point("2025-01-01T00:00:00Z", dec!(10000))?,
            make_equity_point("2025-01-01T01:00:00Z", dec!(10090))?,
        ];
        // PnL = 90 (from 100 to 200, minus 10 in fees)
        let result = make_result(fills, curve, dec!(10000), dec!(10090));
        let metrics = compute_metrics(&result, dec!(0.0));

        // fee_to_pnl = 10 / |90| = 1/9
        let expected = dec!(10) / dec!(90);
        assert_eq!(metrics.fee_to_pnl_ratio, expected);
        Ok(())
    }

    // ── Proptests ───────────────────────────────────────────────────────

    mod prop {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1000))]

            // Test 21: total_return sign matches PnL sign
            #[test]
            fn prop_test_total_return_sign_matches_pnl(
                initial in 1i64..=1_000_000,
                final_val in 1i64..=2_000_000,
            ) {
                let initial_dec = Decimal::from(initial);
                let final_dec = Decimal::from(final_val);
                let curve = vec![
                    EquityPoint {
                        timestamp: DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
                            .map_err(|e| TestCaseError::fail(format!("{e}")))?.to_utc(),
                        nav: Amount::new(initial_dec),
                        cash: Amount::new(initial_dec),
                        positions_value: Amount::zero(),
                    },
                    EquityPoint {
                        timestamp: DateTime::parse_from_rfc3339("2025-01-01T01:00:00Z")
                            .map_err(|e| TestCaseError::fail(format!("{e}")))?.to_utc(),
                        nav: Amount::new(final_dec),
                        cash: Amount::new(final_dec),
                        positions_value: Amount::zero(),
                    },
                ];
                let result = make_result(vec![], curve, initial_dec, final_dec);
                let metrics = compute_metrics(&result, dec!(0));

                if final_dec > initial_dec {
                    prop_assert!(metrics.total_return > Decimal::ZERO);
                } else if final_dec < initial_dec {
                    prop_assert!(metrics.total_return < Decimal::ZERO);
                } else {
                    prop_assert_eq!(metrics.total_return, Decimal::ZERO);
                }
            }

            // Test 22: max_drawdown is always in [-1.0, 0.0]
            #[test]
            fn prop_test_max_drawdown_bounded(
                navs in proptest::collection::vec(1i64..=1_000_000, 2..20),
            ) {
                let curve: Vec<EquityPoint> = navs.iter().enumerate().map(|(i, nav)| {
                    EquityPoint {
                        timestamp: DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
                            .unwrap_or_else(|_| unreachable!()).to_utc()
                            + chrono::Duration::hours(i as i64),
                        nav: Amount::new(Decimal::from(*nav)),
                        cash: Amount::new(Decimal::from(*nav)),
                        positions_value: Amount::zero(),
                    }
                }).collect();

                let (dd, _) = compute_max_drawdown(&curve);
                prop_assert!(dd <= Decimal::ZERO, "max_drawdown = {dd} > 0");
                prop_assert!(dd >= dec!(-1), "max_drawdown = {dd} < -1");
            }

            // Test 23: win_rate is always in [0.0, 1.0]
            #[test]
            fn prop_test_win_rate_bounded(
                num_wins in 0u32..=50,
                num_losses in 0u32..=50,
            ) {
                let mut fills = Vec::new();
                let mut time_offset = 0i64;

                for i in 0..num_wins {
                    let ts_buy = format!("2025-01-01T{:02}:{:02}:00Z", (time_offset / 60) % 24, time_offset % 60);
                    time_offset += 1;
                    let ts_sell = format!("2025-01-01T{:02}:{:02}:00Z", (time_offset / 60) % 24, time_offset % 60);
                    time_offset += 1;

                    // Win: buy 100, sell 110
                    if let (Ok(buy), Ok(sell)) = (
                        make_fill(&format!("SYM{i}W"), OrderSide::Buy, dec!(100), dec!(1), dec!(0), &ts_buy),
                        make_fill(&format!("SYM{i}W"), OrderSide::Sell, dec!(110), dec!(1), dec!(0), &ts_sell),
                    ) {
                        fills.push(buy);
                        fills.push(sell);
                    }
                }

                for i in 0..num_losses {
                    let ts_buy = format!("2025-01-01T{:02}:{:02}:00Z", (time_offset / 60) % 24, time_offset % 60);
                    time_offset += 1;
                    let ts_sell = format!("2025-01-01T{:02}:{:02}:00Z", (time_offset / 60) % 24, time_offset % 60);
                    time_offset += 1;

                    // Loss: buy 110, sell 100
                    if let (Ok(buy), Ok(sell)) = (
                        make_fill(&format!("SYM{i}L"), OrderSide::Buy, dec!(110), dec!(1), dec!(0), &ts_buy),
                        make_fill(&format!("SYM{i}L"), OrderSide::Sell, dec!(100), dec!(1), dec!(0), &ts_sell),
                    ) {
                        fills.push(buy);
                        fills.push(sell);
                    }
                }

                let result = make_result(fills, vec![], dec!(100000), dec!(100000));
                let metrics = compute_metrics(&result, dec!(0));

                prop_assert!(metrics.win_rate >= Decimal::ZERO, "win_rate = {} < 0", metrics.win_rate);
                prop_assert!(metrics.win_rate <= Decimal::ONE, "win_rate = {} > 1", metrics.win_rate);
            }

            // Test 24: sharpe is always finite (no panics)
            #[test]
            fn prop_test_sharpe_finite(
                returns in proptest::collection::vec(-100i64..=100, 0..20),
            ) {
                let dec_returns: Vec<Decimal> = returns.iter()
                    .map(|r| Decimal::from(*r) / dec!(100))
                    .collect();
                let sharpe = compute_sharpe(&dec_returns, dec!(0.05), dec!(252));
                // Decimal is always finite; this test validates no panics occur
                let _ = sharpe;
            }
        }
    }
}
