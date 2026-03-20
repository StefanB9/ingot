SELECT remove_retention_policy('ticks', if_exists => true);
SELECT remove_compression_policy('ticks', if_exists => true);
SELECT remove_compression_policy('ohlcv', if_exists => true);

ALTER TABLE ticks SET (timescaledb.compress = false);
ALTER TABLE ohlcv SET (timescaledb.compress = false);
