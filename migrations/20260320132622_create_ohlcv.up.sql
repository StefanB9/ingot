CREATE TABLE ohlcv (
    time        TIMESTAMPTZ NOT NULL,
    symbol      TEXT        NOT NULL,
    exchange    TEXT        NOT NULL,
    interval    TEXT        NOT NULL,
    open        NUMERIC     NOT NULL,
    high        NUMERIC     NOT NULL,
    low         NUMERIC     NOT NULL,
    close       NUMERIC     NOT NULL,
    volume      NUMERIC     NOT NULL,
    trade_count INTEGER,
    PRIMARY KEY (time, symbol, exchange, interval)
);

SELECT create_hypertable('ohlcv', by_range('time'));

CREATE INDEX idx_ohlcv_symbol_time ON ohlcv (symbol, exchange, interval, time DESC);
