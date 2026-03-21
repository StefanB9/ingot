use ingot_primitives::{Amount, OrderSide, Price, Quantity};
use rust_decimal::Decimal;

/// Apply slippage to a price. Buy → price goes up, Sell → price goes down.
pub(crate) fn apply_slippage(price: Price, side: OrderSide, slippage_bps: Decimal) -> Price {
    let factor = slippage_bps / Decimal::new(10_000, 0);
    let adjustment = price.value() * factor;
    match side {
        OrderSide::Buy => Price::new(price.value() + adjustment),
        OrderSide::Sell => Price::new(price.value() - adjustment),
    }
}

/// Calculate fee in quote currency.
pub(crate) fn calculate_fee(fill_price: Price, fill_qty: Quantity, fee_bps: Decimal) -> Amount {
    let notional = fill_price.value() * fill_qty.value();
    let fee = notional * fee_bps / Decimal::new(10_000, 0);
    Amount::new(fee)
}

/// Determine partial fill quantity (random 20–80% of remaining).
/// Returns `None` if partial fill should not trigger (based on probability).
pub(crate) fn partial_fill_quantity(
    remaining: Quantity,
    probability: Decimal,
    rng: &mut impl rand::Rng,
) -> Option<Quantity> {
    if probability <= Decimal::ZERO {
        return None;
    }

    // Roll against probability
    let roll: f64 = rng.random();
    let prob_f64 = probability.try_into().unwrap_or(0.0);
    if roll >= prob_f64 {
        return None;
    }

    // Random 20–80% of remaining
    let pct: f64 = rng.random_range(0.20..=0.80);
    #[allow(clippy::unwrap_used)]
    let pct_decimal = Decimal::try_from(pct).unwrap_or(Decimal::new(50, 2));
    let partial = remaining.value() * pct_decimal;
    Quantity::new(partial).ok()
}

/// Check if a tick price crosses a limit order.
/// Buy limit: `tick_price <= limit_price`.
/// Sell limit: `tick_price >= limit_price`.
pub(crate) fn tick_crosses_limit(tick_price: Price, limit_price: Price, side: OrderSide) -> bool {
    match side {
        OrderSide::Buy => tick_price <= limit_price,
        OrderSide::Sell => tick_price >= limit_price,
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_slippage_buy_increases_price() {
        let result = apply_slippage(Price::new(dec!(100)), OrderSide::Buy, dec!(50));
        assert_eq!(result, Price::new(dec!(100.50)));
    }

    #[test]
    fn test_slippage_sell_decreases_price() {
        let result = apply_slippage(Price::new(dec!(100)), OrderSide::Sell, dec!(50));
        assert_eq!(result, Price::new(dec!(99.50)));
    }

    #[test]
    fn test_slippage_zero_no_change() {
        let price = Price::new(dec!(67000));
        assert_eq!(apply_slippage(price, OrderSide::Buy, Decimal::ZERO), price);
        assert_eq!(apply_slippage(price, OrderSide::Sell, Decimal::ZERO), price);
    }

    #[test]
    fn test_fee_calculation() -> anyhow::Result<()> {
        // 1 BTC at 67000 with 26bps → 67000 * 0.0026 = 174.20
        let fee = calculate_fee(Price::new(dec!(67000)), Quantity::new(dec!(1))?, dec!(26));
        assert_eq!(fee, Amount::new(dec!(174.20)));
        Ok(())
    }

    #[test]
    fn test_fee_zero_bps() -> anyhow::Result<()> {
        let fee = calculate_fee(
            Price::new(dec!(67000)),
            Quantity::new(dec!(1))?,
            Decimal::ZERO,
        );
        assert_eq!(fee, Amount::zero());
        Ok(())
    }

    #[test]
    fn test_partial_fill_within_range() -> anyhow::Result<()> {
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let remaining = Quantity::new(dec!(10))?;

        // With probability 1.0, should always trigger
        let result = partial_fill_quantity(remaining, dec!(1.0), &mut rng);
        assert!(result.is_some());
        let qty = result.ok_or_else(|| anyhow::anyhow!("expected Some"))?;
        // 20–80% of 10 → between 2.0 and 8.0
        assert!(qty.value() >= dec!(2.0), "got {}", qty.value());
        assert!(qty.value() <= dec!(8.0), "got {}", qty.value());
        Ok(())
    }

    #[test]
    fn test_partial_fill_zero_probability() -> anyhow::Result<()> {
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let remaining = Quantity::new(dec!(10))?;
        let result = partial_fill_quantity(remaining, Decimal::ZERO, &mut rng);
        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn test_tick_crosses_buy_limit() {
        let limit = Price::new(dec!(100));
        // tick at limit → crosses
        assert!(tick_crosses_limit(
            Price::new(dec!(100)),
            limit,
            OrderSide::Buy
        ));
        // tick below limit → crosses
        assert!(tick_crosses_limit(
            Price::new(dec!(99)),
            limit,
            OrderSide::Buy
        ));
        // tick above limit → does not cross
        assert!(!tick_crosses_limit(
            Price::new(dec!(101)),
            limit,
            OrderSide::Buy
        ));
    }

    #[test]
    fn test_tick_crosses_sell_limit() {
        let limit = Price::new(dec!(100));
        // tick at limit → crosses
        assert!(tick_crosses_limit(
            Price::new(dec!(100)),
            limit,
            OrderSide::Sell
        ));
        // tick above limit → crosses
        assert!(tick_crosses_limit(
            Price::new(dec!(101)),
            limit,
            OrderSide::Sell
        ));
        // tick below limit → does not cross
        assert!(!tick_crosses_limit(
            Price::new(dec!(99)),
            limit,
            OrderSide::Sell
        ));
    }
}
