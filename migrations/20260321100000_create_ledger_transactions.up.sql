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
