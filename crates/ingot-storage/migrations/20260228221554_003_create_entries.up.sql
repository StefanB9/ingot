CREATE TABLE IF NOT EXISTS entries (
    id             BIGSERIAL   PRIMARY KEY,
    transaction_id UUID        NOT NULL REFERENCES transactions(id),
    account_id     UUID        NOT NULL REFERENCES accounts(id),
    side           TEXT        NOT NULL CHECK (side IN ('Debit', 'Credit')),
    amount         NUMERIC     NOT NULL,
    currency_code  TEXT        NOT NULL,
    decimals       SMALLINT    NOT NULL DEFAULT 2,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_entries_transaction ON entries (transaction_id);
CREATE INDEX IF NOT EXISTS idx_entries_account ON entries (account_id);
