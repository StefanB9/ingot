SELECT remove_compression_policy('ledger_entries', if_exists => true);
ALTER TABLE ledger_entries SET (timescaledb.compress = false);
