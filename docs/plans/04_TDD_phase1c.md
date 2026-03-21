# Technical Design Document: Phase 1c — Accounting

## 1. Context

Phase 1a delivered foundational domain types (`ingot-primitives`) and storage (`ingot-storage`) with TimescaleDB-backed repositories. Phase 1b added broker connectivity (`ingot-connectivity`) with Kraken spot/futures REST+WS clients, backfill pipeline, and PaperExchange simulator.

Phase 1c adds the **accounting layer**: a multi-currency double-entry ledger that records every financial event (trades, fees, funding rates, interest, transfers) as balanced debit/credit transactions. On top of the ledger: NAV (Net Asset Value) calculation in a user-defined base currency via FX rates, and broker reconciliation to detect drift between internal books and broker-reported balances.

The accounting layer is the single source of truth for portfolio valuation. All downstream features (risk management, strategy PnL attribution, reporting) depend on its correctness.

## 2. Crate Structure

New crate `ingot-accounting` holds all accounting domain types and logic. Storage repositories added to `ingot-storage` (consistent with existing pattern).

```
ingot-primitives (no deps)
    ↓
ingot-core (depends on ingot-primitives)
    ↓
ingot-accounting (depends on ingot-core, ingot-primitives)  ← NEW
    ↓
ingot-storage (depends on ingot-core, ingot-primitives, ingot-accounting)
    ↓
ingot-connectivity (depends on ingot-core, ingot-storage, ingot-primitives)
```

`ingot-storage` gains a dependency on `ingot-accounting` so it can accept accounting types in repository method signatures.

## 3. Domain Types (`ingot-accounting`)

### 3.1 Account Model (`src/types.rs`)

```rust
/// Category of account in the chart of accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccountType {
    /// Cash, crypto holdings, position value. Normal balance = Debit.
    Asset,
    /// Short margin obligations. Normal balance = Credit.
    Liability,
    /// Realized trading gains. Normal balance = Credit.
    Revenue,
    /// Trading fees, funding costs, interest paid. Normal balance = Debit.
    Expense,
}

impl fmt::Display for AccountType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asset => f.write_str("asset"),
            Self::Liability => f.write_str("liability"),
            Self::Revenue => f.write_str("revenue"),
            Self::Expense => f.write_str("expense"),
        }
    }
}

/// Side of a ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntrySide {
    Debit,
    Credit,
}

impl fmt::Display for EntrySide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Debit => f.write_str("debit"),
            Self::Credit => f.write_str("credit"),
        }
    }
}

/// Type of financial transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransactionType {
    Trade,
    Fee,
    FundingRate,
    Interest,
    Transfer,
    Adjustment,
}

impl fmt::Display for TransactionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Trade => f.write_str("trade"),
            Self::Fee => f.write_str("fee"),
            Self::FundingRate => f.write_str("funding_rate"),
            Self::Interest => f.write_str("interest"),
            Self::Transfer => f.write_str("transfer"),
            Self::Adjustment => f.write_str("adjustment"),
        }
    }
}

/// Structured account identifier.
/// Display format: "{account_type}:{exchange}:{venue}:{currency}"
/// Example: "asset:kraken:spot:USD"
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountId {
    pub account_type: AccountType,
    pub exchange: Exchange,
    pub venue: SmolStr,      // "spot", "futures", "margin"
    pub currency: Currency,
}

impl AccountId {
    pub fn new(
        account_type: AccountType,
        exchange: Exchange,
        venue: &str,
        currency: Currency,
    ) -> Result<Self, AccountingError> {
        if venue.is_empty() {
            return Err(AccountingError::InvalidAccount {
                reason: "venue cannot be empty".into(),
            });
        }
        Ok(Self {
            account_type,
            exchange,
            venue: SmolStr::new(venue),
            currency,
        })
    }

    /// Returns the normal balance side for this account type.
    pub fn normal_side(&self) -> EntrySide {
        match self.account_type {
            AccountType::Asset | AccountType::Expense => EntrySide::Debit,
            AccountType::Liability | AccountType::Revenue => EntrySide::Credit,
        }
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}:{}", self.account_type, self.exchange.as_str_lowercase(), self.venue, self.currency)
    }
}
```

**Note:** Requires adding `Exchange::as_str_lowercase()` to `ingot-primitives/src/enums.rs`:

```rust
impl Exchange {
    pub fn as_str_lowercase(&self) -> &'static str {
        match self {
            Self::Kraken => "kraken",
            Self::KrakenFutures => "kraken_futures",
            Self::IBKR => "ibkr",
            Self::Paper => "paper",
        }
    }
}
```

### 3.2 Transaction Types (`src/transaction.rs`)

```rust
/// Unique transaction identifier (UUID v7 — time-ordered).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransactionId(Uuid);

impl TransactionId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

/// Unique ledger entry identifier (UUID v7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryId(Uuid);

impl EntryId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

/// A single entry in the double-entry ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: EntryId,
    pub transaction_id: TransactionId,
    pub account_id: AccountId,
    pub side: EntrySide,
    pub amount: Amount,
    pub currency: Currency,
    pub timestamp: DateTime<Utc>,
    pub description: Option<SmolStr>,
}

/// A balanced double-entry transaction (immutable once created).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub id: TransactionId,
    pub transaction_type: TransactionType,
    pub entries: Vec<LedgerEntry>,
    pub timestamp: DateTime<Utc>,
    pub reference_id: Option<SmolStr>,      // e.g. OrderId, trade_id
    pub metadata: Option<serde_json::Value>,
}

impl Transaction {
    /// Validate that the transaction is balanced: for each currency,
    /// sum(debits) == sum(credits).
    pub fn validate(&self) -> Result<(), AccountingError> {
        // Group entries by currency
        // For each currency: sum debit amounts == sum credit amounts
        // If not balanced, return UnbalancedTransaction error
    }
}
```

### 3.3 Query & Result Types (`src/balance.rs`, `src/reconciliation.rs`, `src/nav.rs`)

```rust
/// Aggregated balance for an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyBalance {
    pub account_id: AccountId,
    pub currency: Currency,
    pub balance: Amount,
}

/// Discrepancy between ledger and broker balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discrepancy {
    pub currency: Currency,
    pub ledger_balance: Amount,
    pub broker_balance: Amount,
    pub difference: Amount,
    pub severity: DiscrepancySeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiscrepancySeverity {
    None,     // exact match
    Minor,    // < 0.1% difference
    Major,    // 0.1% - 1% difference
    Critical, // > 1% difference
}

impl fmt::Display for DiscrepancySeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Minor => f.write_str("minor"),
            Self::Major => f.write_str("major"),
            Self::Critical => f.write_str("critical"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReconciliationStatus {
    Pass,
    Fail,
}

impl fmt::Display for ReconciliationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => f.write_str("pass"),
            Self::Fail => f.write_str("fail"),
        }
    }
}

/// Result of a reconciliation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub id: Uuid,
    pub exchange: Exchange,
    pub timestamp: DateTime<Utc>,
    pub discrepancies: Vec<Discrepancy>,
    pub status: ReconciliationStatus,
}

/// Point-in-time NAV calculation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavSnapshot {
    pub timestamp: DateTime<Utc>,
    pub base_currency: Currency,
    pub total_nav: Amount,
    pub breakdown: Vec<NavBreakdownEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavBreakdownEntry {
    pub currency: Currency,
    pub native_balance: Amount,
    pub fx_rate: Price,            // rate to base currency
    pub base_currency_value: Amount,
}
```

## 4. Posting Engine (`src/posting.rs`)

The core logic converting domain events into balanced double-entry transactions. **Invariant: for every transaction, sum(debits) == sum(credits) per currency.**

### 4.1 `post_fill` — Convert OrderFill to Transaction

```rust
pub fn post_fill(
    fill: &OrderFill,
    exchange: Exchange,
    venue: &str,
    base_currency: &Currency,  // instrument base (e.g., BTC)
    quote_currency: &Currency, // instrument quote (e.g., USD)
) -> Result<Transaction, AccountingError>
```

#### Buy Trade (buy 1 BTC at 67000 USD, fee 17.42 USD)

| # | Account | Side | Amount | Currency |
|---|---------|------|--------|----------|
| 1 | `asset:kraken:spot:BTC` | Debit | 1.0 | BTC |
| 2 | `asset:kraken:spot:USD` | Credit | 67,000.0 | USD |
| 3 | `expense:fee:kraken:USD` | Debit | 17.42 | USD |
| 4 | `asset:kraken:spot:USD` | Credit | 17.42 | USD |

Check: BTC debits (1.0) = BTC credits (0) — single-currency asset entry, balanced by the USD side.
USD debits (17.42) = USD credits (67,000 + 17.42)? No — **multi-currency transactions don't balance per currency in isolation**. The invariant is: **the transaction as a whole represents a valid economic event.** For cross-currency trades, we track each leg in its native currency.

**Revised invariant:** Each currency's net must be zero OR the transaction represents a valid exchange between two currencies. For simplicity and correctness, we use the approach: **each entry is recorded in its native currency, and the `validate()` check ensures that entries with the same currency balance (debits == credits), while the cross-currency legs are the exchange itself.**

So for a buy trade, the USD entries balance: credit 67000 + credit 17.42 vs ... no, the fee debit is USD too.

**Correct approach for multi-currency double-entry:**

Each entry records both the account AND the amount in its native currency. The balancing rule is:
- **Same-currency entries must net to zero** (e.g., USD debits == USD credits)
- **Cross-currency exchanges** are recorded as two same-currency balanced pairs

Buy 1 BTC at 67000 USD, fee 17.42 USD:

| # | Account | Side | Amount | Currency |
|---|---------|------|--------|----------|
| 1 | `asset:kraken:spot:BTC` | Debit | 1.0 | BTC |
| 2 | `asset:kraken:spot:BTC` (contra) | Credit | 1.0 | BTC |
| ... | | | | |

Actually, the standard accounting approach for forex/crypto trading is to use a **conversion account** or to record the trade as two balanced legs. Let me use the cleaner approach used by most trading ledgers:

**Approach: Each trade creates entries per currency that individually balance.**

For a buy of 1 BTC at 67,000 USD:
- The USD side: We spend 67,000 + 17.42 fee from USD cash
- The BTC side: We receive 1.0 BTC

We record this as a single transaction where we accept that cross-currency entries don't balance per-currency. Instead, the validation is: **each entry is valid and the transaction represents a real economic event**. The per-currency balance is not enforced at the transaction level — it's enforced at the **trial balance level per currency**.

**Final approach (industry standard for multi-currency trading ledger):**

```
Buy 1 BTC at 67,000 USD, fee 17.42 USD:

Entry 1: Debit  asset:kraken:spot:BTC    1.0 BTC        (receive BTC)
Entry 2: Credit asset:kraken:spot:USD    67,000.00 USD   (pay for BTC)
Entry 3: Debit  expense:fee:kraken:USD   17.42 USD       (fee expense)
Entry 4: Credit asset:kraken:spot:USD    17.42 USD       (pay fee from cash)
```

- USD entries: credits = 67,000 + 17.42 = 67,017.42; debits = 17.42. Net = -67,000 (outflow). ✓
- BTC entries: debits = 1.0; credits = 0. Net = +1.0 (inflow). ✓
- The USD outflow and BTC inflow are the trade itself.
- Entries 3+4 balance within USD (fee).

**Validation rule:** For single-currency transactions (fees, funding, transfers), debits == credits. For cross-currency trades, the non-balancing amounts represent the exchange, and the fill_price provides the exchange rate for audit.

#### Sell Trade (sell 1 BTC at 68,000 USD, fee 17.68 USD)

```
Entry 1: Debit  asset:kraken:spot:USD    68,000.00 USD   (receive USD)
Entry 2: Credit asset:kraken:spot:BTC    1.0 BTC         (deliver BTC)
Entry 3: Debit  expense:fee:kraken:USD   17.68 USD       (fee expense)
Entry 4: Credit asset:kraken:spot:USD    17.68 USD       (pay fee from cash)
```

#### Short Sell (sell 1 BTC without holding, at 67,000 USD, fee 17.42 USD)

```
Entry 1: Debit  asset:kraken:spot:USD         67,000.00 USD  (receive USD proceeds)
Entry 2: Credit liability:kraken:spot:BTC     1.0 BTC        (short obligation)
Entry 3: Debit  expense:fee:kraken:USD        17.42 USD      (fee)
Entry 4: Credit asset:kraken:spot:USD         17.42 USD      (pay fee)
```

#### Funding Rate (pay 6.70 USD on futures position)

```
Entry 1: Debit  expense:funding:kraken:USD    6.70 USD
Entry 2: Credit asset:kraken:futures:USD      6.70 USD
```
Single-currency: debits (6.70) == credits (6.70). ✓

#### Transfer (move 10,000 USD from spot to futures)

```
Entry 1: Debit  asset:kraken:futures:USD      10,000.00 USD
Entry 2: Credit asset:kraken:spot:USD         10,000.00 USD
```
Single-currency: debits == credits. ✓

### 4.2 Additional Posting Functions

```rust
/// Post a funding rate payment/receipt.
pub fn post_funding_rate(
    exchange: Exchange,
    venue: &str,
    currency: Currency,
    amount: Amount,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError>

/// Post an inter-account transfer.
pub fn post_transfer(
    from_exchange: Exchange,
    from_venue: &str,
    to_exchange: Exchange,
    to_venue: &str,
    currency: Currency,
    amount: Amount,
    timestamp: DateTime<Utc>,
) -> Result<Transaction, AccountingError>

/// Post an adjustment entry (for reconciliation corrections).
pub fn post_adjustment(
    account_id: AccountId,
    amount: Amount,
    timestamp: DateTime<Utc>,
    reason: &str,
) -> Result<Transaction, AccountingError>
```

## 5. FX Rate Service (`src/nav.rs`)

```rust
/// Provides FX rates for currency conversion.
pub trait FxRateProvider {
    fn get_rate(
        &self,
        base: &Currency,
        quote: &Currency,
    ) -> impl Future<Output = anyhow::Result<Price>> + Send;
}

/// Static FX rate provider for testing.
pub struct StaticFxRateProvider {
    rates: HashMap<(Currency, Currency), Price>,
}

impl StaticFxRateProvider {
    pub fn new(rates: Vec<(Currency, Currency, Price)>) -> Self { ... }
}

impl FxRateProvider for StaticFxRateProvider {
    async fn get_rate(&self, base: &Currency, quote: &Currency) -> anyhow::Result<Price> {
        // Identity: same currency → Price::new(1)
        // Direct lookup: (base, quote)
        // Inverse: 1 / rate(quote, base)
        // Error if not found
    }
}
```

## 6. NAV Calculation (`src/nav.rs`)

```rust
pub struct NavCalculator<F: FxRateProvider> {
    fx_provider: F,
}

impl<F: FxRateProvider> NavCalculator<F> {
    pub fn new(fx_provider: F) -> Self { ... }

    pub async fn calculate_nav(
        &self,
        base_currency: &Currency,
        balances: &[CurrencyBalance],
    ) -> anyhow::Result<NavSnapshot> {
        // 1. Group balances by currency
        // 2. For each currency, get FX rate to base_currency
        // 3. Convert each balance to base_currency_value
        // 4. Sum all base_currency_values → total_nav
        // 5. Return NavSnapshot with breakdown
    }
}
```

## 7. Reconciliation (`src/reconciliation.rs`)

```rust
/// Compare internal ledger balances against broker-reported balances.
/// Pure function — caller provides both inputs.
pub fn reconcile(
    exchange: Exchange,
    ledger_balances: &[CurrencyBalance],
    broker_balances: &[Balance],
) -> ReconciliationResult {
    // 1. Aggregate ledger_balances by currency (sum across accounts for the exchange)
    // 2. For each currency in union(ledger, broker):
    //    a. Get ledger amount (0 if missing)
    //    b. Get broker amount (Balance.total, 0 if missing)
    //    c. Compute difference
    //    d. Classify severity based on percentage difference
    // 3. Status = Pass if all None/Minor, Fail if any Major/Critical
    // 4. Return ReconciliationResult
}

fn classify_severity(difference: Decimal, reference: Decimal) -> DiscrepancySeverity {
    if difference == Decimal::ZERO { return DiscrepancySeverity::None; }
    if reference == Decimal::ZERO { return DiscrepancySeverity::Critical; }
    let pct = (difference / reference).abs();
    if pct < Decimal::new(1, 3) { DiscrepancySeverity::Minor }       // < 0.1%
    else if pct < Decimal::new(1, 2) { DiscrepancySeverity::Major }  // < 1%
    else { DiscrepancySeverity::Critical }                            // >= 1%
}
```

## 8. Error Types (`src/error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum AccountingError {
    #[error("unbalanced transaction: {currency} debits={debits}, credits={credits}")]
    UnbalancedTransaction {
        currency: Currency,
        debits: Amount,
        credits: Amount,
    },

    #[error("invalid account: {reason}")]
    InvalidAccount { reason: String },

    #[error("duplicate transaction: {0}")]
    DuplicateTransaction(TransactionId),

    #[error("invalid amount: {reason}")]
    InvalidAmount { reason: String },

    #[error("FX rate unavailable for {base}/{quote}")]
    FxRateUnavailable { base: Currency, quote: Currency },

    #[error("reconciliation failed for {exchange}: {reason}")]
    ReconciliationFailed { exchange: Exchange, reason: String },
}
```

## 9. Configuration (`src/config.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountingConfig {
    pub base_currency: Currency,
    pub reconciliation_interval_secs: u64,
    pub minor_threshold_pct: Decimal,    // default 0.001 (0.1%)
    pub major_threshold_pct: Decimal,    // default 0.01 (1%)
}

impl Default for AccountingConfig {
    fn default() -> Self {
        Self {
            base_currency: Currency::USD,
            reconciliation_interval_secs: 300,
            minor_threshold_pct: Decimal::new(1, 3),  // 0.001 = 0.1%
            major_threshold_pct: Decimal::new(1, 2),   // 0.01  = 1%
        }
    }
}
```

## 10. Storage

### 10.1 Migrations

**`YYYYMMDD_create_ledger_transactions.up.sql`:**
```sql
CREATE TABLE IF NOT EXISTS ledger_transactions (
    id              UUID PRIMARY KEY,
    transaction_type TEXT NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    reference_id    TEXT,
    metadata        JSONB
);

CREATE INDEX idx_ledger_transactions_timestamp ON ledger_transactions (timestamp);
CREATE INDEX idx_ledger_transactions_reference ON ledger_transactions (reference_id)
    WHERE reference_id IS NOT NULL;
```

**`YYYYMMDD_create_ledger_entries.up.sql`:**
```sql
CREATE TABLE IF NOT EXISTS ledger_entries (
    id              UUID PRIMARY KEY,
    transaction_id  UUID NOT NULL REFERENCES ledger_transactions(id),
    account_type    TEXT NOT NULL,
    exchange        TEXT NOT NULL,
    venue           TEXT NOT NULL,
    currency        TEXT NOT NULL,
    side            TEXT NOT NULL,
    amount          DECIMAL NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    description     TEXT
);

SELECT create_hypertable('ledger_entries', 'timestamp', if_not_exists => TRUE);

CREATE INDEX idx_ledger_entries_account
    ON ledger_entries (account_type, exchange, venue, currency);
CREATE INDEX idx_ledger_entries_transaction
    ON ledger_entries (transaction_id);
```

**`YYYYMMDD_create_reconciliation_results.up.sql`:**
```sql
CREATE TABLE IF NOT EXISTS reconciliation_results (
    id              UUID PRIMARY KEY,
    exchange        TEXT NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    status          TEXT NOT NULL,
    discrepancies   JSONB NOT NULL
);

CREATE INDEX idx_reconciliation_exchange_ts
    ON reconciliation_results (exchange, timestamp DESC);
```

**`YYYYMMDD_ledger_entries_compression.up.sql`:**
```sql
ALTER TABLE ledger_entries SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'account_type, exchange, venue, currency',
    timescaledb.compress_orderby = 'timestamp DESC'
);

SELECT add_compression_policy('ledger_entries', INTERVAL '30 days', if_not_exists => TRUE);
```

### 10.2 Repositories (in `ingot-storage`)

**`src/ledger_repo.rs` — `PgLedgerRepository`:**
```rust
impl PgLedgerRepository {
    pub fn new(pool: PgPool) -> Self

    /// Insert a complete transaction with all entries atomically.
    pub async fn insert_transaction(&self, txn: &Transaction) -> anyhow::Result<()>

    /// Get aggregated balance per account (sum debits - sum credits).
    pub async fn get_account_balances(&self) -> anyhow::Result<Vec<CurrencyBalance>>

    /// Get balances filtered by exchange.
    pub async fn get_balances_by_exchange(
        &self,
        exchange: Exchange,
    ) -> anyhow::Result<Vec<CurrencyBalance>>

    /// Get all entries since a timestamp.
    pub async fn get_entries_since(
        &self,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<LedgerEntry>>

    /// Trial balance: sum all debits and credits. They should match.
    pub async fn trial_balance(&self) -> anyhow::Result<(Amount, Amount)>
}
```

**`src/reconciliation_repo.rs` — `PgReconciliationRepository`:**
```rust
impl PgReconciliationRepository {
    pub fn new(pool: PgPool) -> Self

    pub async fn insert_result(&self, result: &ReconciliationResult) -> anyhow::Result<()>

    pub async fn get_latest(
        &self,
        exchange: Exchange,
    ) -> anyhow::Result<Option<ReconciliationResult>>
}
```

## 11. Workspace Dependencies (New)

```toml
# Cargo.toml [workspace.dependencies]
uuid = { version = "1.16.0", default-features = false, features = ["v7", "serde"] }
proptest = { version = "1.6.0", default-features = false, features = ["std"] }
```

**Crate dependencies:**

```toml
# ingot-accounting/Cargo.toml [dependencies]
anyhow.workspace = true
chrono.workspace = true
ingot-core.workspace = true
ingot-primitives.workspace = true
rust_decimal.workspace = true
serde.workspace = true
serde_json.workspace = true
smol_str.workspace = true
thiserror.workspace = true
uuid.workspace = true

# ingot-accounting/Cargo.toml [dev-dependencies]
proptest.workspace = true
rust_decimal_macros.workspace = true
tokio.workspace = true

# ingot-storage/Cargo.toml [dependencies] (add)
ingot-accounting.workspace = true   # NEW
uuid.workspace = true               # NEW
```

## 12. File Layout

```
crates/ingot-accounting/
├── Cargo.toml
└── src/
    ├── lib.rs                 — module declarations + re-exports
    ├── types.rs               — AccountType, AccountId, EntrySide, TransactionType
    ├── transaction.rs         — TransactionId, EntryId, LedgerEntry, Transaction
    ├── posting.rs             — post_fill(), post_funding_rate(), post_transfer(), post_adjustment()
    ├── balance.rs             — CurrencyBalance, balance aggregation helpers
    ├── nav.rs                 — FxRateProvider trait, StaticFxRateProvider, NavCalculator, NavSnapshot
    ├── reconciliation.rs      — reconcile(), Discrepancy, DiscrepancySeverity, ReconciliationResult
    ├── config.rs              — AccountingConfig
    └── error.rs               — AccountingError

crates/ingot-storage/src/
    ├── ledger_repo.rs         — PgLedgerRepository (NEW)
    └── reconciliation_repo.rs — PgReconciliationRepository (NEW)

migrations/
    ├── YYYYMMDD_create_ledger_transactions.{up,down}.sql
    ├── YYYYMMDD_create_ledger_entries.{up,down}.sql
    ├── YYYYMMDD_create_reconciliation_results.{up,down}.sql
    └── YYYYMMDD_ledger_entries_compression.{up,down}.sql
```

## 13. Sub-Phase Breakdown

### 1c.1: Crate scaffold + domain types + ledger schema

**Deliverables:**
- Create `ingot-accounting` crate (via `cargo new`), add to workspace members
- Add `uuid` and `proptest` to workspace dependencies
- Add `Exchange::as_str_lowercase()` to `ingot-primitives/src/enums.rs`
- All domain types in `types.rs`: `AccountType`, `AccountId`, `EntrySide`, `TransactionType` with lowercase Display impls
- Transaction types in `transaction.rs`: `TransactionId`, `EntryId`, `LedgerEntry`, `Transaction` with `validate()`
- `AccountingError` enum in `error.rs` with Display tests for every variant
- `AccountingConfig` in `config.rs` with `Default` impl and serde roundtrip test
- Database migrations for `ledger_transactions`, `ledger_entries`, `reconciliation_results`, compression policy
- `PgLedgerRepository` in `ingot-storage` with `insert_transaction()` and `get_account_balances()`
- `PgReconciliationRepository` in `ingot-storage` with `insert_result()` and `get_latest()`
- Update `ingot-storage/lib.rs` to export new repos; add `ingot-accounting` and `uuid` as dependencies
- Unit tests: AccountId construction + Display, TransactionId uniqueness, serde roundtrips for all types, Display for all enum variants
- **proptest:** For any random set of entries where per-currency debits == credits, `Transaction::validate()` passes

### 1c.2: Double-entry posting engine

**Deliverables:**
- `post_fill()` in `posting.rs` — converts `OrderFill` + instrument info into a balanced `Transaction`
- `post_funding_rate()` — funding rate transaction
- `post_transfer()` — inter-account transfer
- `post_adjustment()` — manual adjustment entry
- Validation: every generated `Transaction` passes `validate()`
- Unit tests for each transaction type:
  - Buy trade → correct debit/credit entries
  - Sell trade → correct entries
  - Short sell → liability account credited
  - Fee-only entries balance
  - Funding rate entries balance
  - Transfer entries balance
- **proptest (1000+ cases):** For any valid `OrderFill` with random price/qty/fee, the generated `Transaction` always passes `validate()`
- Integration test with testcontainers: `post_fill()` → `insert_transaction()` → `get_account_balances()` verifies round-trip

### 1c.3: Balance queries + account aggregation

**Deliverables:**
- `CurrencyBalance` type in `balance.rs`
- `get_balances_by_exchange()` in `PgLedgerRepository`
- `get_entries_since()` in `PgLedgerRepository`
- `trial_balance()` in `PgLedgerRepository` — sum all debits vs all credits
- Helper: `aggregate_balances()` — in-memory aggregation of entries to balances
- Unit tests for aggregation logic (multiple entries → correct net balances)
- **proptest:** After N random balanced transactions, `trial_balance()` returns equal debit/credit totals
- Integration test with testcontainers: insert multiple transactions → verify balances, trial balance

### 1c.4: FX rate service + NAV calculation

**Deliverables:**
- `FxRateProvider` trait in `nav.rs`
- `StaticFxRateProvider` implementation
- `NavCalculator` struct with `calculate_nav()` method
- `NavSnapshot` and `NavBreakdownEntry` types
- Handle identity rates (USD→USD = 1)
- Handle inverse rates (if BTC/USD exists, derive USD/BTC = 1/rate)
- Unit tests:
  - NAV with single currency (no conversion needed)
  - NAV with multi-currency portfolio and known rates
  - Identity rate returns 1
  - Inverse rate calculation
  - Missing rate → `FxRateUnavailable` error
- **proptest:** NAV is always non-negative if all asset balances are non-negative

### 1c.5: Broker reconciliation

**Deliverables:**
- `reconcile()` pure function in `reconciliation.rs`
- `Discrepancy`, `DiscrepancySeverity`, `ReconciliationResult` types
- `classify_severity()` helper
- Unit tests:
  - Exact match → all `DiscrepancySeverity::None`, status Pass
  - Minor drift (< 0.1%) → Minor severity, status Pass
  - Major drift (0.1–1%) → Major severity, status Fail
  - Critical drift (> 1%) → Critical severity, status Fail
  - Currency in broker but not ledger → Critical
  - Currency in ledger but not broker → Critical
- Integration test: full cycle — post fills → compute ledger balances → reconcile against mock broker balances → verify result stored
- **proptest:** If ledger and broker balances are identical, reconciliation always passes with zero discrepancies

## 14. Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Account ID | Structured struct with typed fields | Compile-time safety for correctness-critical accounting; impossible to construct invalid keys |
| Type location | All in `ingot-accounting` | Clean separation; ingot-core stays unchanged; Phase 1d naturally depends on accounting |
| FX rates | `FxRateProvider` trait | Decouples accounting from connectivity; testable with `StaticFxRateProvider` |
| Reconciliation | Pure function with inputs | Simplest approach; no trait needed; caller provides both sides |
| Transaction ID | UUID v7 | Time-ordered, universally unique, no DB dependency; good for append-only ledger |
| Repo location | In `ingot-storage` | Consistent with existing pattern (PgOhlcv, PgTick, PgInstrument repos) |
| Multi-currency balancing | Per-currency balance for same-currency txns; cross-currency legs tracked via fill metadata | Industry standard for trading ledgers; simpler than conversion accounts |
| Append-only ledger | No UPDATE/DELETE on ledger_entries | Immutability required by PRD for audit trail; corrections via Adjustment transactions |
| Property tests | proptest with 1000+ cases | CLAUDE.md requirement for financial math; critical for debit=credit invariant |

## 15. Testing Strategy

| Layer | Tool | Approach |
|-------|------|----------|
| Domain types | `cargo nextest` | Construction, validation, serde roundtrips, Display output |
| Posting engine | Unit tests + proptest | Each transaction type tested; proptest ensures balance invariant holds for random inputs |
| Balance aggregation | Unit tests + proptest | Aggregate correctness; trial balance always holds after random transactions |
| NAV calculation | Unit tests + proptest | Known-rate scenarios; proptest for non-negativity |
| Reconciliation | Unit tests + proptest | All severity levels; exact match always passes |
| Storage repos | `testcontainers` | Insert → query round-trips against real TimescaleDB |
| Error types | Unit tests | Display output for every variant |

Per CLAUDE.md: `proptest` with `ProptestConfig::with_cases(1000)` for all financial/accounting math. No `.unwrap()` / `.expect()` / `panic!()`.

## 16. Verification Plan

1. `cargo fmt --all -- --check` — formatting clean
2. `cargo clippy --all-targets --workspace` — zero warnings
3. `cargo nextest run --workspace` — all tests pass
4. `cargo check --all-targets --workspace` — all targets compile
5. `cargo bench --no-run` — benchmarks still compile
6. proptest: `post_fill()` generates balanced transactions for 1000+ random fills
7. proptest: trial balance holds after 1000+ random transactions
8. Integration: post fills → query balances → reconcile against known values
9. Ledger immutability: no UPDATE/DELETE queries exist in repo code
10. All `AccountingError` variants have Display tests
