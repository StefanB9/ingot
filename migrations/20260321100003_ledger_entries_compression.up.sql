ALTER TABLE ledger_entries SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'account_type, exchange, venue, currency',
    timescaledb.compress_orderby = 'timestamp DESC'
);

SELECT add_compression_policy('ledger_entries', INTERVAL '30 days', if_not_exists => TRUE);
