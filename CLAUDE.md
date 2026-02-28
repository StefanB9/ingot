# Ingot Engineering Standards

Portfolio management and systematic trading workstation built in Rust. Multi-broker connectivity, double-entry accounting, automated strategy execution. Long-term fundamental and quant strategies — not HFT. Performance-critical, correctness-critical.

## Workspace

```
ingot-primitives   Zero deps. Copy types: Symbol, Currency, Amount, Price, Quantity, Money, OrderSide, OrderType, OrderStatus
ingot-config       AppConfig/ServerConfig/ClientConfig (clap + dotenvy). No ingot deps.
ingot-core         Accounting (Ledger, Transaction, Account), Execution (OrderRequest, OrderAcknowledgment), Feed (Ticker, QuoteBoard), API types (WsMessage)
ingot-connectivity Exchange trait + adapters: kraken/ (REST, WS, types), PaperExchange (typestate)
ingot-engine       TradingEngine<E: Exchange>. Event loop, command dispatch, broadcast channel, EngineHandle service layer.
ingot-ipc          iceoryx2 shared-memory IPC types (#[repr(C)], ZeroCopySend), conversion functions, service name constants.
ingot-server       Headless trading daemon. Runs engine + iceoryx2 IPC publisher/command threads.
ingot-cli          REPL binary. Thin IPC client over iceoryx2 shared memory.
ingot-gui          egui binary. Thin IPC client over iceoryx2 shared memory.
```

**Dependency direction:** `primitives` <- `core` <- `connectivity` <- `engine` <- `server`. `primitives` <- `ipc` <- `cli`/`gui`. Never reverse.

## Quick Reference

```bash
cargo test --workspace                        # All tests (163+)
cargo test -p ingot-core                      # Single crate
cargo clippy --all-targets --workspace        # Zero warnings required (includes tests, benches)
cargo check --all-targets --workspace         # Type-check everything including tests and benches
cargo fmt --all -- --check                    # Format check
cargo bench --bench hot_path                  # Core benchmarks
cargo bench --bench ws_parse                  # Connectivity benchmarks
cargo bench --no-run                          # Verify all benchmarks compile
```

**`--all-targets` is mandatory** for `clippy` and `check`. Lints must pass in tests, benchmarks, and examples — not just lib/bin targets. Test code follows the same quality standards as production code.

## Test-Driven Development

**Strict red-green-refactor. No exceptions.**

1. **Red** — Write a failing test first. Run it. Confirm it fails for the right reason.
2. **Green** — Write the minimum code to make the test pass. Nothing more.
3. **Refactor** — Clean up while all tests stay green.

### Test Requirements

| Context | Requirement |
|---------|------------|
| Every public function | At least one unit test |
| Financial/accounting logic | Property-based tests (`proptest`, 1000+ cases) |
| Async flows | Integration test with `#[tokio::test]` + `tokio::time::timeout` |
| New exchange adapters | `wiremock`-based integration tests |
| `thiserror` enum variants | Test verifying each variant's Display output |
| Hot-path changes | Benchmark before and after |

### Test Conventions

- **Naming:** `test_<unit>_<scenario>` (e.g., `test_ledger_rejects_unbalanced_transaction`)
- **Location:** `#[cfg(test)] mod tests` inline in the source file. Integration tests in `tests/`.
- **Assertions:** `assert_eq!`, `assert!`, `prop_assert!`, `assert_matches!`
- **Errors:** Tests return `anyhow::Result<()>` with `.context()` for diagnostics.
- **Quality:** Test code follows the same lint rules as production code. No `.unwrap()`, `.expect()`, or `panic!()` in tests — use `?` with `anyhow::Result` or `anyhow::bail!`.
- **Proptest config:** `#![proptest_config(ProptestConfig::with_cases(1000))]`

## Benchmarking

**Framework:** `criterion` with `black_box()` and benchmark groups.

### What Must Be Benchmarked

- Tick processing (Ticker construction, QuoteBoard conversion)
- Order placement (request building, signing, serialization)
- Ledger operations (NAV calculation, transaction validation, post_order_fill)
- Symbol/pair transforms (from_kraken, KrakenPair)
- WS message deserialization (ticker arrays, event objects)
- Any new hot-path function

### Rules

- New hot-path code ships with benchmarks in the same PR.
- No merge if any existing benchmark regresses >5% without written justification.
- Each crate with hot-path code has a `benches/` directory with `harness = false`.
- Use `benchmark_group()` for related operations. Use `black_box()` on all inputs.

### Existing Suites

- `ingot-core/benches/hot_path.rs` — Ticker, QuoteBoard, Transaction, Ledger NAV
- `ingot-connectivity/benches/ws_parse.rs` — Symbol normalization, price parsing, WS deserialization

## Performance Rules

### Mandatory

- **Zero-copy by default.** `&str` and borrowed lifetimes over `String` cloning. `Cow<'static, str>` for values that are usually static but occasionally dynamic.
- **Stack allocation for bounded data.** `[u8; N]` for symbols, currency codes, pair strings. `SmallVec<[T; 4]>` for small collections.
- **Pre-allocate when size is known.** `String::with_capacity()`, `Vec::with_capacity()`.
- **Single-pass transforms.** No intermediate `String` allocations in parsing/normalization.
- **No allocation in hot loops.** Ticker processing, order routing, WS message dispatch must be allocation-free where possible.
- **`rust_decimal::Decimal` for all financial math.** Never `f64`.
- **Named channel capacity constants.** `MARKET_DATA_CHANNEL_CAPACITY`, `COMMAND_CHANNEL_CAPACITY`.

### Avoid

- `format!()` in hot paths — use `write!()` into pre-allocated buffers
- `clone()` when a borrow suffices
- `Box<dyn Trait>` when monomorphic generics work — `TradingEngine<E: Exchange>` not `Box<dyn Exchange>`
- `String` for fixed-vocabulary identifiers — use newtypes with stack arrays
- `async-trait` crate — use `#[allow(async_fn_in_trait)]` for monomorphic-only traits
- `Arc<Mutex<T>>` in hot paths — prefer channels or `RwLock` where contention is read-heavy

## Error Handling

- **Public APIs:** Return `anyhow::Result<T>`. Add `.context("msg")` or `.with_context(|| format!(...))` (lazy) on every `?`.
- **Domain errors:** `thiserror` enums for recoverable, matchable errors (e.g., `AccountingError`).
- **Forbidden everywhere (including tests):** `.unwrap()`, `.expect()`, `panic!()`, `todo!()` — enforced by workspace lints with `--all-targets`.
- **Fallible constructors:** Return `Result<Self>` not `Self`. Validate inputs at construction.

## Coding Standards

### Lints (workspace-enforced, applied to all targets)

```
unsafe_code       = "forbid"
unwrap_used       = "deny"
expect_used       = "deny"
panic             = "deny"
todo              = "deny"
print_stdout      = "warn"
print_stderr      = "warn"
clippy::pedantic  = "warn" (priority -1)
```

These lints apply to **all code** — lib, bin, tests, benchmarks, examples. Run `cargo clippy --all-targets --workspace` to verify.

### Formatting (`rustfmt.toml`)

- `max_width = 100`
- `imports_granularity = "Crate"`
- `group_imports = "StdExternalCrate"`
- Edition 2024

### Import Order

```rust
use std::...;

use anyhow::...;       // external crates
use serde::...;

use ingot_core::...;   // workspace crates
use ingot_primitives::...;

use crate::...;        // local
use super::...;
```

One blank line between groups.

### Visibility

Minimum necessary. `pub(super)` or `pub(crate)` for internal types. Private fields by default. Child modules access parent struct fields via `self` in `impl super::Type` blocks.

### Module Organization

- One responsibility per file.
- Split at ~300 lines. Convert to directory module (`mod.rs` + submodules) at ~500 lines.
- Directory modules: `mod.rs` is the coordinator (struct, trait impl, re-exports). Submodules own specific concerns.

### Comments

- `///` doc comments on all public items.
- `// SAFETY:` for correctness reasoning (cancellation safety, invariant guarantees, why a conversion is sound).
- No trivial comments restating what code does. Comments explain *why*.

### Type Safety

- Newtypes for domain concepts: `Amount`, `Price`, `Quantity`, `Symbol`. No raw `Decimal` or `String` in public APIs.
- `Copy` types (`Symbol`, `Currency`, `CurrencyCode`, `OrderSide`, `OrderType`, `OrderStatus`): pass by value, not reference.
- Typestate pattern for connection lifecycle (see `PaperExchange<Disconnected>` -> `PaperExchange<Connected>`).

### Dependencies

- `default-features = false` with explicit feature selection.
- New dependencies require justification: what problem, why this crate, what alternatives were considered.
- Prefer zero-cost abstractions over convenience crates.

## Tracing & Observability

- **`#[instrument]`** on all public `async fn` in connectivity and engine crates. Use `skip(self)` or `skip_all` with explicit `fields(...)`.
- **Field syntax:** `%value` for Display, `?value` for Debug, bare primitives for Copy types.
- **Levels:** `error!` = failures requiring attention. `warn!` = degraded states. `info!` = lifecycle events. `debug!` = protocol detail.
- **Default filter:** `ingot_core=info,ingot_connectivity=debug,ingot_engine=info`
- **Override:** `RUST_LOG` env var via `EnvFilter::try_from_default_env()`.

## Git Workflow

No CI, no hooks. All verification is the developer's responsibility before merging.

### Branch Structure

```
main          Stable release branch. Always clean: compiles, all tests pass, zero warnings.
  └── dev     Integration branch. Features merge here first via PR. Periodically merged to main.
       ├── feature/<name>   New functionality (e.g., feature/ibkr-adapter)
       ├── fix/<name>       Bug fixes (e.g., fix/ledger-rounding)
       ├── refactor/<name>  Code restructuring (e.g., refactor/exchange-trait)
       ├── test/<name>      Test additions/improvements
       ├── bench/<name>     Benchmark additions
       └── docs/<name>      Documentation changes
```

### Branch Lifecycle

1. **Create** a feature branch from `dev`:
   ```bash
   git checkout dev
   git pull origin dev
   git checkout -b feature/<name>
   ```

2. **Work** on the branch. Make atomic commits (each compiles, passes tests, zero warnings).

3. **Push** the branch and open a PR targeting `dev`:
   ```bash
   git push -u origin feature/<name>
   ```

4. **Verify** locally before merging (no CI — you must run these):
   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets --workspace
   cargo test --workspace
   cargo bench --no-run
   ```

5. **Merge** the PR into `dev` via merge commit (no squash — preserve commit history).

6. **Delete** the feature branch after merge.

7. **Promote** `dev` to `main` when a stable milestone is reached:
   ```bash
   git checkout main
   git merge dev
   git push origin main
   ```

### Branch Rules

- **Never commit directly to `main` or `dev`.** All changes go through feature branches + PRs.
- **Never force-push** to `main` or `dev`.
- **No WIP commits** on `dev` or `main`. Feature branches may have WIP commits but clean them up before PR.
- **New commits over amends** — preserve history.
- **Delete merged branches** to keep the branch list clean.

### Commit Messages

```
<type>: <imperative summary, max 72 chars>

<optional body: explains WHY, not what. Motivation, trade-offs, context.>

Co-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>
```

Types: `feat`, `fix`, `refactor`, `test`, `bench`, `docs`, `chore`

### Pull Requests

All PRs target `dev` unless it is a `dev` → `main` promotion.

#### PR Format

```
Title: <type>: <short description> (under 72 chars)

## Summary
- Bullet points of what changed and why

## Test Plan
- [ ] Tests written first (red-green-refactor)
- [ ] cargo fmt --all -- --check passes
- [ ] cargo test --workspace passes
- [ ] cargo clippy --all-targets --workspace clean
- [ ] cargo check --all-targets --workspace clean
- [ ] Benchmarks: cargo bench --no-run compiles / no regressions >5%
```

#### Pre-Merge Checklist

Run locally before every merge — there is no CI to catch failures:

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --workspace` — zero warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] `cargo check --all-targets --workspace` — all targets type-check
- [ ] `cargo bench --no-run` — all benchmarks compile
- [ ] No benchmark regressions >5% without written justification
- [ ] Tests written first (TDD evidence in commit history)
- [ ] No `.unwrap()`, `.expect()`, `panic!()`, `todo!()`
- [ ] Hot-path changes benchmarked
- [ ] Error paths have `.context()`
- [ ] New public items have doc comments
- [ ] No unnecessary allocations in hot paths
- [ ] Minimum visibility applied
