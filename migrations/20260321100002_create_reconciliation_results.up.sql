CREATE TABLE IF NOT EXISTS reconciliation_results (
    id              UUID PRIMARY KEY,
    exchange        TEXT NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    status          TEXT NOT NULL,
    discrepancies   JSONB NOT NULL
);

CREATE INDEX idx_reconciliation_exchange_ts
    ON reconciliation_results (exchange, timestamp DESC);
