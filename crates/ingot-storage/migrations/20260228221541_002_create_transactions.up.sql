CREATE TABLE IF NOT EXISTS transactions (
    id          UUID        PRIMARY KEY,
    posted_at   TIMESTAMPTZ NOT NULL,
    description TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_transactions_posted_at ON transactions (posted_at);
