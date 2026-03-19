# Ingot Engineering Standards

Portfolio management and systematic trading workstation built in Rust and Svelte. Multi-broker connectivity, automated strategy execution, and risk management. Performance-critical, correctness-critical.

## Planning Process

Planning is interactive, not autonomous. When asked to plan a step or feature:

1. Research the codebase and external dependencies. Present findings.
2. Ask questions before finalizing the plan — surface ambiguities, trade-offs, API changes, and design decisions that need the user's input.
3. Only finalize and save the plan after the user has reviewed and approved the approach.
4. Save the final plan to `docs/plans/` before implementation begins.

Do not silently make architectural decisions. If the implementation plan document conflicts with the current codebase state (e.g., outdated dependency versions, changed APIs), flag the discrepancy and ask how to proceed.

## Quick Reference

# Rust Backend
cargo nextest run --workspace                 # All tests (use nextest, not cargo test)
cargo clippy --all-targets --workspace        # Zero warnings required (includes tests, benches)
cargo check --all-targets --workspace         # Type-check everything including tests and benches
cargo fmt --all -- --check                    # Format check
cargo bench --bench hot_path                  # Core benchmarks
cargo bench --no-run                          # Verify all benchmarks compile

# Svelte Frontend (Run from /ingot-ui once initialized)
npm run dev                                   # Start local dev server
npm run check                                 # Svelte-check for TypeScript errors
npm run format                                # Prettier formatting

Always use `cargo nextest run` instead of `cargo test`.
`--all-targets` is mandatory for `clippy` and `check`. Lints must pass in tests, benchmarks, and examples.

## Test-Driven Development

Strict red-green-refactor. No exceptions.

1. Red — Write a failing test first.
2. Green — Write the minimum code to make the test pass.
3. Refactor — Clean up while all tests stay green.

### Test Requirements

Context | Requirement
Every public function | At least one unit test
Financial/accounting logic | Property-based tests (`proptest`, 1000+ cases)
Async flows | Integration test with `#[tokio::test]` + `tokio::time::timeout`
New exchange adapters | `wiremock`-based integration tests
`thiserror` enum variants | Test verifying each variant's Display output

### Test Conventions

- Naming: `test_<unit>_<scenario>` (e.g., `test_risk_rejects_overexposure`)
- Location: `#[cfg(test)] mod tests` inline in the source file. Integration tests in `tests/`.
- Quality: No `.unwrap()`, `.expect()`, or `panic!()` in tests — use `?` with `anyhow::Result`.
- Proptest config: `#![proptest_config(ProptestConfig::with_cases(1000))]`

## Benchmarking

Framework: `criterion` with `black_box()` and benchmark groups.

### What Must Be Benchmarked
- Tick processing and Order Book updates
- Order sizing and fractional math routing
- TimescaleDB batch insert performance
- WS message deserialization

### Rules
- New hot-path code ships with benchmarks in the same PR.
- No merge if any existing benchmark regresses >5% without written justification.

## Performance Rules

### Mandatory
- Zero-copy by default. `&str` and borrowed lifetimes over `String` cloning.
- Stack allocation for bounded data. `[u8; N]` for symbols. `SmallVec<[T; 4]>` for small collections.
- No allocation in hot loops. Ticker processing and WS message dispatch must be allocation-free where possible.
- `rust_decimal::Decimal` for all financial math. Never `f64`.
- Inter-process Communication: Use `tokio::sync` (`mpsc`, `broadcast`) within the core engine. Do not use mutexes (`Arc<Mutex<T>>`) in hot paths if a channel or `RwLock` suffices.

## Error Handling

- Public APIs: Return `anyhow::Result<T>`. Add `.context("msg")` on every `?`.
- Domain errors: `thiserror` enums for recoverable, matchable errors.
- Forbidden everywhere: `.unwrap()`, `.expect()`, `panic!()`, `todo!()` — enforced via workspace lints.
- Fallible constructors: Return `Result<Self>` not `Self`. Validate inputs at construction.

## Coding Standards

### Lints (workspace-enforced)
unsafe_code       = "forbid"
unwrap_used       = "deny"
expect_used       = "deny"
panic             = "deny"
todo              = "deny"
print_stdout      = "warn"
print_stderr      = "warn"
clippy::pedantic  = "warn"

### Type Safety
- Newtypes for domain concepts: `Amount`, `Price`, `Quantity`, `Symbol`.
- `Copy` types (`Symbol`, `OrderSide`, `OrderType`): pass by value, not reference.
- Typestate pattern for connection lifecycle (`PaperExchange<Disconnected>` -> `PaperExchange<Connected>`).

### Dependencies
Rule 1: Zero Duplicate Versions. All versions must be strictly centralized in `[workspace.dependencies]` at the root.
Rule 2: Always Disable Default Features. Explicitly set `default-features = false`.
Rule 3: Granular Feature Flags. Append features locally in member crates (e.g., `tokio = { workspace = true, features = ["fs"] }`).

## Database (TimescaleDB & sqlx)

- Engine: Must utilize TimescaleDB optimizations (e.g., hypertables) for tick/OHLCV data.
- Migrations: Use `cargo sqlx migrate add -r <name>` to create reversible migration files. Run from `ingot-storage/`.
- Offline cache: Regenerate with `cargo sqlx prepare --workspace` from workspace root. Commit the `.sqlx/` directory.

## Tracing & Observability

- `#[instrument]` on all public `async fn` in connectivity and engine crates.
- Levels: `error!` = failures requiring attention. `warn!` = degraded states. `info!` = lifecycle events. `debug!` = protocol detail.
- Default filter: `ingot_core=info,ingot_connectivity=debug,ingot_api=info`

## Git Workflow
No CI, no hooks. All verification is the developer's responsibility before merging.

### Pull Requests
All PRs target `dev` unless it is a `dev` → `main` promotion.

#### Pre-Merge Checklist
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --workspace` — zero warnings
- [ ] `cargo nextest run --workspace` — all tests pass
- [ ] `cargo bench --no-run` — all benchmarks compile
- [ ] `npm run check` (in UI folder) — zero TS errors
- [ ] Tests written first (TDD evidence in commit history)
- [ ] No `.unwrap()`, `.expect()`, `panic!()`, `todo!()`
- [ ] Minimum visibility applied (`pub(crate)` over `pub`)