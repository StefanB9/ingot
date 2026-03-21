CREATE TABLE ticks (
    time        TIMESTAMPTZ NOT NULL,
    symbol      TEXT        NOT NULL,
    exchange    TEXT        NOT NULL,
    price       NUMERIC     NOT NULL,
    quantity    NUMERIC     NOT NULL,
    side        TEXT,
    trade_id    TEXT,
    PRIMARY KEY (time, symbol, exchange)
);

SELECT create_hypertable('ticks', by_range('time'));

CREATE INDEX idx_ticks_symbol_time ON ticks (symbol, exchange, time DESC);
