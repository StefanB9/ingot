# Implementation Plan: Phase 1f.8 — Margin Monitoring: Extend PortfolioController

## Context
Phase 1f.7 is complete (TWS StreamProvider). Phase 1f.8 adds margin monitoring to the engine's `PortfolioController`. The controller gains awareness of margin state via `MarginSnapshot` events and integrates margin checks into `check_intention()`. This enables the engine to reject orders that would exceed margin limits and auto-halt on margin calls. `MarginSnapshot` moves to `ingot-core` as a shared domain type. Event-driven only — no polling in the controller.

## Design Decisions
1. **MarginSnapshot in ingot-core**: Move from `ingot-connectivity/src/ibkr/margin.rs` to `ingot-core/src/margin.rs` as a shared domain type. Re-import in connectivity. This parallels how `Tick`, `TickerSnapshot`, `OrderFill` etc. live in ingot-core.
2. **Event-driven only**: No `poll_interval_ms`. The controller reacts to `EngineEvent::MarginUpdate(MarginSnapshot)` pushed through the event loop. The caller is responsible for feeding margin data. Keeps the controller pure and testable.
3. **Optional margin config**: `RiskConfig.margin: Option<MarginConfig>`. When `None`, margin checks are skipped entirely (for non-margin accounts or exchanges without margin).

## Files

### Moved (1)
- `MarginSnapshot` + methods from `ingot-connectivity/src/ibkr/margin.rs` → `ingot-core/src/margin.rs`

### Modified (6)
- `crates/ingot-core/src/lib.rs` — Add `pub mod margin;`, re-export `MarginSnapshot`
- `crates/ingot-connectivity/src/ibkr/margin.rs` — Remove `MarginSnapshot` struct + methods, re-import from `ingot_core::MarginSnapshot`. Keep tests that exercise the methods.
- `crates/ingot-connectivity/src/ibkr/mapper.rs` — Update import path for `MarginSnapshot`
- `crates/ingot-engine/src/config.rs` — Add `MarginConfig` struct, add `margin: Option<MarginConfig>` to `RiskConfig`
- `crates/ingot-engine/src/controller.rs` — Add `on_margin_update()`, extend `check_intention()` with margin checks, auto-halt on margin call
- `crates/ingot-engine/src/types.rs` — Add `EngineEvent::MarginUpdate(MarginSnapshot)` variant
- `crates/ingot-engine/src/error.rs` — Add margin-specific error variants

## Type Definitions

### MarginConfig (in engine/config.rs)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginConfig {
    /// Maximum allowed margin utilization (e.g., 0.80 = 80%).
    /// Orders rejected if utilization would exceed this.
    pub max_margin_utilization: Percentage,
    /// Warning threshold for margin utilization (e.g., 0.60 = 60%).
    /// Logged at warn level when exceeded but orders not rejected.
    pub warn_margin_utilization: Percentage,
    /// Minimum required excess liquidity.
    /// Orders rejected if excess liquidity below this.
    pub min_excess_liquidity: Amount,
}
```

### RiskConfig extension

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub global_stop_loss: Amount,
    pub max_currency_exposure: Percentage,
    pub max_asset_exposure: Percentage,
    pub max_order_value: Amount,
    /// Margin monitoring thresholds. None = skip margin checks.
    #[serde(default)]
    pub margin: Option<MarginConfig>,
}
```

### PortfolioController extension

```rust
pub struct PortfolioController {
    config: RiskConfig,
    positions: HashMap<Symbol, Position>,
    current_nav: Amount,
    halted: bool,
    latest_margin: Option<MarginSnapshot>,  // NEW
}
```

### New methods on PortfolioController

```rust
/// Update the latest margin snapshot. Auto-halts on margin call.
pub fn on_margin_update(&mut self, snapshot: MarginSnapshot) {
    if snapshot.is_margin_call() {
        tracing::warn!("margin call detected — auto-halting controller");
        self.halted = true;
    }
    if let Some(ref margin_config) = self.config.margin {
        if let Ok(util) = snapshot.utilization() {
            if util > margin_config.warn_margin_utilization {
                tracing::warn!("margin utilization {util} exceeds warning threshold {}", margin_config.warn_margin_utilization);
            }
        }
    }
    self.latest_margin = Some(snapshot);
}

/// Read-only access to the latest margin snapshot.
pub fn latest_margin(&self) -> Option<&MarginSnapshot> {
    self.latest_margin.as_ref()
}
```

### check_intention() margin extension

Insert after the global stop-loss check (step 2) and before order price resolution (step 3):

```rust
// 2.5. Margin checks (if configured)
if let Some(ref margin_config) = self.config.margin {
    if let Some(ref snapshot) = self.latest_margin {
        // Reject if margin call
        if snapshot.is_margin_call() {
            self.halted = true;
            return RiskDecision::Rejected {
                reason: SmolStr::new("margin call — excess liquidity depleted"),
            };
        }
        // Reject if utilization exceeds max
        if let Ok(util) = snapshot.utilization() {
            if util > margin_config.max_margin_utilization {
                return RiskDecision::Rejected {
                    reason: SmolStr::new(format!(
                        "margin utilization {} exceeds max {}",
                        util, margin_config.max_margin_utilization
                    )),
                };
            }
        }
        // Reject if excess liquidity below minimum
        if snapshot.excess_liquidity < margin_config.min_excess_liquidity {
            return RiskDecision::Rejected {
                reason: SmolStr::new(format!(
                    "excess liquidity {} below minimum {}",
                    snapshot.excess_liquidity, margin_config.min_excess_liquidity
                )),
            };
        }
    }
}
```

### EngineEvent extension (types.rs)

```rust
pub enum EngineEvent {
    Ticker(TickerSnapshot),
    OrderBook(OrderBookSnapshot),
    Fill(OrderFill),
    MarginUpdate(MarginSnapshot),  // NEW
    ScheduleTrigger(StrategyId),
    KillSwitch,
    Shutdown,
}
```

### EngineError extension (error.rs)

```rust
// Add variants:
#[error("margin call: excess liquidity depleted")]
MarginCall,

#[error("margin utilization {utilization} exceeds maximum {limit}")]
MarginUtilizationExceeded {
    utilization: Percentage,
    limit: Percentage,
},

#[error("excess liquidity {available} below minimum {required}")]
InsufficientExcessLiquidity {
    available: Amount,
    required: Amount,
},
```

## TDD Steps (11 tests)

### Config tests (2 in config.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 1 | `test_margin_config_serde_roundtrip` | MarginConfig serializes/deserializes correctly |
| 2 | `test_risk_config_with_margin_serde` | RiskConfig with `margin: Some(...)` and `margin: None` both roundtrip |

### Controller margin tests (6 in controller.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 3 | `test_on_margin_update_stores_snapshot` | `on_margin_update()` stores snapshot, `latest_margin()` returns it |
| 4 | `test_check_intention_approved_with_margin_headroom` | Margin utilization and excess liquidity both within limits → Approved |
| 5 | `test_check_intention_rejected_margin_utilization` | Utilization above max → Rejected with reason |
| 6 | `test_check_intention_rejected_low_excess_liquidity` | Excess liquidity below minimum → Rejected with reason |
| 7 | `test_margin_update_auto_halts_on_margin_call` | `on_margin_update()` with `is_margin_call() == true` → halted |
| 8 | `test_no_margin_config_skips_margin_check` | `margin: None` in RiskConfig → margin checks skipped, order approved |

### Event/error tests (2 in types.rs + error.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 9 | `test_engine_event_margin_update_variant` | `EngineEvent::MarginUpdate(snapshot)` constructs and pattern-matches correctly |
| 10 | `test_error_margin_variants_display` | All 3 new EngineError variants produce correct Display output |

### Property test (1 in controller.rs)

| # | Test Name | Verifies |
|---|-----------|----------|
| 11 | `prop_test_margin_check_never_approves_above_limit` | For any utilization > max, check_intention always rejects |

## Implementation Order

### Step 1: Move MarginSnapshot to ingot-core
- Create `crates/ingot-core/src/margin.rs` with `MarginSnapshot` struct + `utilization()`, `is_margin_call()`, `available_margin()` methods
- Add `pub mod margin;` and re-export in `ingot-core/src/lib.rs`
- Update `ingot-connectivity/src/ibkr/margin.rs` to re-import from `ingot_core::MarginSnapshot`, remove the struct definition, keep tests
- Update `ingot-connectivity/src/ibkr/mapper.rs` import path
- Verify all existing tests pass

### Step 2: MarginConfig + serde tests 1-2 (config.rs)
- Add `MarginConfig` struct
- Add `margin: Option<MarginConfig>` to `RiskConfig` with `#[serde(default)]`
- Write tests 1-2

### Step 3: EngineEvent + EngineError extensions + tests 9-10
- Add `MarginUpdate(MarginSnapshot)` to `EngineEvent`
- Add 3 margin error variants to `EngineError`
- Write tests 9-10

### Step 4: PortfolioController margin logic + tests 3-8, 11
- Add `latest_margin` field to `PortfolioController`
- Implement `on_margin_update()` with auto-halt on margin call
- Extend `check_intention()` with margin checks
- Add `latest_margin()` accessor
- Write tests 3-8, 11

### Step 5: Verify
```bash
SQLX_OFFLINE=true cargo fmt --all -- --check
SQLX_OFFLINE=true cargo clippy --all-targets --workspace
SQLX_OFFLINE=true cargo nextest run --workspace
SQLX_OFFLINE=true cargo check --all-targets --workspace
SQLX_OFFLINE=true cargo bench --no-run
```

## Key Files (reference)
- `crates/ingot-core/src/margin.rs` — NEW: MarginSnapshot (moved from connectivity)
- `crates/ingot-engine/src/config.rs` — MODIFIED: MarginConfig, RiskConfig.margin
- `crates/ingot-engine/src/controller.rs` — MODIFIED: on_margin_update, check_intention margin checks
- `crates/ingot-engine/src/types.rs` — MODIFIED: EngineEvent::MarginUpdate
- `crates/ingot-engine/src/error.rs` — MODIFIED: 3 new margin error variants
- `crates/ingot-connectivity/src/ibkr/margin.rs` — MODIFIED: remove struct, re-import from ingot-core
- `crates/ingot-connectivity/src/ibkr/mapper.rs` — MODIFIED: update MarginSnapshot import
