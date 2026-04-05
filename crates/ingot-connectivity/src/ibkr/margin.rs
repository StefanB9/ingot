// MarginSnapshot is now defined in ingot-core.
// Re-export for crate-internal use and keep proptest here
// (ingot-core doesn't have proptest as a dev-dep).
pub(crate) use ingot_core::MarginSnapshot;

#[cfg(test)]
mod tests {
    use ingot_primitives::Amount;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::*;

    fn sample_snapshot(
        initial_margin: Decimal,
        net_liquidation: Decimal,
        excess_liquidity: Decimal,
    ) -> MarginSnapshot {
        MarginSnapshot {
            account_id: "U1234567".to_string(),
            initial_margin: Amount::new(initial_margin),
            maintenance_margin: Amount::new(dec!(30000)),
            excess_liquidity: Amount::new(excess_liquidity),
            buying_power: Amount::new(dec!(200000)),
            sma: Some(Amount::new(dec!(80000))),
            available_funds: Amount::new(dec!(70000)),
            net_liquidation: Amount::new(net_liquidation),
            timestamp: chrono::Utc::now(),
        }
    }

    // ── Proptest: utilization bounded ──

    mod prop {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1000))]

            #[test]
            fn prop_test_margin_utilization_bounded(
                init_margin in 1i64..=1_000_000,
                net_liq in 1i64..=1_000_000,
            ) {
                let init = Decimal::from(init_margin);
                let net = Decimal::from(net_liq);

                // Only test when init_margin <= net_liquidation (valid margin state)
                if init <= net {
                    let snap = sample_snapshot(init, net, dec!(50000));
                    let util = snap.utilization();
                    prop_assert!(util.is_ok(), "utilization should succeed");
                    let pct = match util {
                        Ok(p) => p,
                        Err(_) => return Err(proptest::test_runner::TestCaseError::fail("utilization returned error")),
                    };
                    prop_assert!(pct.value() >= Decimal::ZERO, "utilization >= 0");
                    prop_assert!(pct.value() <= Decimal::ONE, "utilization <= 1");
                }
            }
        }
    }
}
