CREATE TABLE IF NOT EXISTS accounts (
    id            UUID        PRIMARY KEY,
    name          TEXT        NOT NULL,
    account_type  TEXT        NOT NULL CHECK (account_type IN (
                      'Asset', 'Liability', 'Equity', 'Revenue', 'Expense'
                  )),
    currency_code TEXT        NOT NULL,
    decimals      SMALLINT    NOT NULL DEFAULT 2,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_accounts_currency ON accounts (currency_code);
CREATE INDEX IF NOT EXISTS idx_accounts_type ON accounts (account_type);
