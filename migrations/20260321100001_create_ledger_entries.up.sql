CREATE TABLE IF NOT EXISTS ledger_entries (
    id              UUID NOT NULL,
    transaction_id  UUID NOT NULL REFERENCES ledger_transactions(id),
    account_type    TEXT NOT NULL,
    exchange        TEXT NOT NULL,
    venue           TEXT NOT NULL,
    currency        TEXT NOT NULL,
    side            TEXT NOT NULL,
    amount          NUMERIC NOT NULL,
    timestamp       TIMESTAMPTZ NOT NULL,
    description     TEXT
);

SELECT create_hypertable('ledger_entries', by_range('timestamp'), if_not_exists => TRUE);

CREATE INDEX idx_ledger_entries_account
    ON ledger_entries (account_type, exchange, venue, currency);
CREATE INDEX idx_ledger_entries_transaction
    ON ledger_entries (transaction_id);
