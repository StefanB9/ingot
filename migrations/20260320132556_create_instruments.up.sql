CREATE TABLE instruments (
    symbol          TEXT        PRIMARY KEY,
    asset_class     TEXT        NOT NULL,
    exchange        TEXT        NOT NULL,
    base_currency   TEXT        NOT NULL,
    quote_currency  TEXT        NOT NULL,
    tick_size       NUMERIC     NOT NULL,
    display_name    TEXT        NOT NULL,
    details_json    JSONB       NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_instruments_exchange ON instruments (exchange);
CREATE INDEX idx_instruments_asset_class ON instruments (asset_class);
