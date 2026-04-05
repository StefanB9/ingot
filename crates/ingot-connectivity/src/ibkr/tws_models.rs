use anyhow::Context;

use super::tws_codec::TwsRawMessage;

// ── TWS incoming message IDs ──

pub(crate) const MSG_TICK_PRICE: i32 = 1;
pub(crate) const MSG_TICK_SIZE: i32 = 2;
pub(crate) const MSG_ORDER_STATUS: i32 = 3;
pub(crate) const MSG_ERR_MSG: i32 = 4;
pub(crate) const MSG_NEXT_VALID_ID: i32 = 9;
pub(crate) const MSG_EXECUTION_DATA: i32 = 11;
pub(crate) const MSG_MARKET_DEPTH: i32 = 12;
pub(crate) const MSG_HEARTBEAT: i32 = 49;

// ── Incoming message enum ──

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TwsIncoming {
    NextValidId {
        order_id: i32,
    },
    TickPrice {
        req_id: i32,
        tick_type: i32,
        price: f64,
        size: f64,
    },
    TickSize {
        req_id: i32,
        tick_type: i32,
        size: f64,
    },
    OrderStatus {
        order_id: i32,
        status: String,
        filled: f64,
        remaining: f64,
        avg_fill_price: f64,
    },
    ExecutionData {
        req_id: i32,
        order_id: i32,
        conid: i64,
        side: String,
        shares: f64,
        price: f64,
        exec_id: String,
        time: String,
    },
    MarketDepth {
        req_id: i32,
        position: i32,
        operation: i32,
        side: i32,
        price: f64,
        size: f64,
    },
    ErrorMessage {
        id: i32,
        code: i32,
        message: String,
    },
    Heartbeat,
}

impl TwsIncoming {
    /// Parse a raw TWS message into a typed incoming message.
    pub fn parse(raw: &TwsRawMessage) -> anyhow::Result<Self> {
        let f = &raw.fields;
        match raw.msg_id {
            MSG_NEXT_VALID_ID => {
                // fields: [version, order_id]
                let order_id = field_i32(f, 1, "order_id")?;
                Ok(Self::NextValidId { order_id })
            }
            MSG_TICK_PRICE => {
                // fields: [version, req_id, tick_type, price, size, ...]
                let req_id = field_i32(f, 1, "req_id")?;
                let tick_type = field_i32(f, 2, "tick_type")?;
                let price = field_f64(f, 3, "price")?;
                let size = field_f64(f, 4, "size")?;
                Ok(Self::TickPrice {
                    req_id,
                    tick_type,
                    price,
                    size,
                })
            }
            MSG_TICK_SIZE => {
                // fields: [version, req_id, tick_type, size]
                let req_id = field_i32(f, 1, "req_id")?;
                let tick_type = field_i32(f, 2, "tick_type")?;
                let size = field_f64(f, 3, "size")?;
                Ok(Self::TickSize {
                    req_id,
                    tick_type,
                    size,
                })
            }
            MSG_ORDER_STATUS => {
                // fields: [version, order_id, status, filled, remaining, avg_fill_price, ...]
                let order_id = field_i32(f, 1, "order_id")?;
                let status = field_str(f, 2, "status")?.to_string();
                let filled = field_f64(f, 3, "filled")?;
                let remaining = field_f64(f, 4, "remaining")?;
                let avg_fill_price = field_f64(f, 5, "avg_fill_price")?;
                Ok(Self::OrderStatus {
                    order_id,
                    status,
                    filled,
                    remaining,
                    avg_fill_price,
                })
            }
            MSG_EXECUTION_DATA => {
                // fields: [version, req_id, order_id, conid, symbol, sec_type, expiry,
                //          strike, right, exchange, currency, local_symbol,
                //          exec_id, time, acct, exch, side, shares, price, ...]
                let req_id = field_i32(f, 1, "req_id")?;
                let order_id = field_i32(f, 2, "order_id")?;
                let conid = field_i64(f, 3, "conid")?;
                // indices 4-11 are contract fields we skip for now
                let exec_id = field_str(f, 12, "exec_id")?.to_string();
                let time = field_str(f, 13, "time")?.to_string();
                // index 14 = acct, 15 = exch
                let side = field_str(f, 16, "side")?.to_string();
                let shares = field_f64(f, 17, "shares")?;
                let price = field_f64(f, 18, "price")?;
                Ok(Self::ExecutionData {
                    req_id,
                    order_id,
                    conid,
                    side,
                    shares,
                    price,
                    exec_id,
                    time,
                })
            }
            MSG_MARKET_DEPTH => {
                // fields: [version, req_id, position, operation, side, price, size]
                let req_id = field_i32(f, 1, "req_id")?;
                let position = field_i32(f, 2, "position")?;
                let operation = field_i32(f, 3, "operation")?;
                let side = field_i32(f, 4, "side")?;
                let price = field_f64(f, 5, "price")?;
                let size = field_f64(f, 6, "size")?;
                Ok(Self::MarketDepth {
                    req_id,
                    position,
                    operation,
                    side,
                    price,
                    size,
                })
            }
            MSG_ERR_MSG => {
                // fields: [version, id, code, message, ...]
                let id = field_i32(f, 1, "id")?;
                let code = field_i32(f, 2, "code")?;
                let message = field_str(f, 3, "message")?.to_string();
                Ok(Self::ErrorMessage { id, code, message })
            }
            MSG_HEARTBEAT => Ok(Self::Heartbeat),
            other => anyhow::bail!("unknown TWS message ID: {other}"),
        }
    }
}

// ── Field extraction helpers ──

fn field_str<'a>(fields: &'a [String], idx: usize, name: &str) -> anyhow::Result<&'a str> {
    fields
        .get(idx)
        .map(String::as_str)
        .with_context(|| format!("missing TWS field {name} at index {idx}"))
}

fn field_i32(fields: &[String], idx: usize, name: &str) -> anyhow::Result<i32> {
    let s = field_str(fields, idx, name)?;
    s.parse()
        .with_context(|| format!("invalid i32 for TWS field {name}: {s}"))
}

fn field_i64(fields: &[String], idx: usize, name: &str) -> anyhow::Result<i64> {
    let s = field_str(fields, idx, name)?;
    s.parse()
        .with_context(|| format!("invalid i64 for TWS field {name}: {s}"))
}

fn field_f64(fields: &[String], idx: usize, name: &str) -> anyhow::Result<f64> {
    let s = field_str(fields, idx, name)?;
    s.parse()
        .with_context(|| format!("invalid f64 for TWS field {name}: {s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 6: parse next_valid_id ──

    #[test]
    fn test_parse_next_valid_id() -> anyhow::Result<()> {
        let raw = TwsRawMessage {
            msg_id: MSG_NEXT_VALID_ID,
            fields: vec!["1".into(), "42".into()],
        };
        let msg = TwsIncoming::parse(&raw)?;
        assert_eq!(msg, TwsIncoming::NextValidId { order_id: 42 });
        Ok(())
    }

    // ── Test 7: parse tick_price ──

    #[test]
    fn test_parse_tick_price() -> anyhow::Result<()> {
        let raw = TwsRawMessage {
            msg_id: MSG_TICK_PRICE,
            fields: vec![
                "6".into(),      // version
                "1001".into(),   // req_id
                "4".into(),      // tick_type (LAST)
                "178.25".into(), // price
                "100".into(),    // size
            ],
        };
        let msg = TwsIncoming::parse(&raw)?;
        assert_eq!(
            msg,
            TwsIncoming::TickPrice {
                req_id: 1001,
                tick_type: 4,
                price: 178.25,
                size: 100.0,
            }
        );
        Ok(())
    }

    // ── Test 8: parse tick_size ──

    #[test]
    fn test_parse_tick_size() -> anyhow::Result<()> {
        let raw = TwsRawMessage {
            msg_id: MSG_TICK_SIZE,
            fields: vec![
                "1".into(),    // version
                "1001".into(), // req_id
                "0".into(),    // tick_type (BID_SIZE)
                "500".into(),  // size
            ],
        };
        let msg = TwsIncoming::parse(&raw)?;
        assert_eq!(
            msg,
            TwsIncoming::TickSize {
                req_id: 1001,
                tick_type: 0,
                size: 500.0,
            }
        );
        Ok(())
    }

    // ── Test 9: parse order_status ──

    #[test]
    fn test_parse_order_status() -> anyhow::Result<()> {
        let raw = TwsRawMessage {
            msg_id: MSG_ORDER_STATUS,
            fields: vec![
                "1".into(),      // version
                "42".into(),     // order_id
                "Filled".into(), // status
                "100".into(),    // filled
                "0".into(),      // remaining
                "178.50".into(), // avg_fill_price
                "12345".into(),  // perm_id
                "0".into(),      // parent_id
                "178.50".into(), // last_fill_price
                "1".into(),      // client_id
                "".into(),       // why_held
                "0".into(),      // mkt_cap_price
            ],
        };
        let msg = TwsIncoming::parse(&raw)?;
        assert_eq!(
            msg,
            TwsIncoming::OrderStatus {
                order_id: 42,
                status: "Filled".into(),
                filled: 100.0,
                remaining: 0.0,
                avg_fill_price: 178.50,
            }
        );
        Ok(())
    }

    // ── Test 10: parse execution_data ──

    #[test]
    fn test_parse_execution_data() -> anyhow::Result<()> {
        let raw = TwsRawMessage {
            msg_id: MSG_EXECUTION_DATA,
            fields: vec![
                "1".into(),                 // 0: version
                "5001".into(),              // 1: req_id
                "42".into(),                // 2: order_id
                "265598".into(),            // 3: conid
                "AAPL".into(),              // 4: symbol
                "STK".into(),               // 5: sec_type
                "".into(),                  // 6: expiry
                "0".into(),                 // 7: strike
                "".into(),                  // 8: right
                "SMART".into(),             // 9: exchange
                "USD".into(),               // 10: currency
                "AAPL".into(),              // 11: local_symbol
                "EXEC001".into(),           // 12: exec_id
                "20260329-14:30:00".into(), // 13: time
                "DU_TEST".into(),           // 14: acct
                "ISLAND".into(),            // 15: exch
                "BOT".into(),               // 16: side
                "50".into(),                // 17: shares
                "178.25".into(),            // 18: price
            ],
        };
        let msg = TwsIncoming::parse(&raw)?;
        assert_eq!(
            msg,
            TwsIncoming::ExecutionData {
                req_id: 5001,
                order_id: 42,
                conid: 265598,
                side: "BOT".into(),
                shares: 50.0,
                price: 178.25,
                exec_id: "EXEC001".into(),
                time: "20260329-14:30:00".into(),
            }
        );
        Ok(())
    }

    // ── Test 11: parse market_depth ──

    #[test]
    fn test_parse_market_depth() -> anyhow::Result<()> {
        let raw = TwsRawMessage {
            msg_id: MSG_MARKET_DEPTH,
            fields: vec![
                "1".into(),      // version
                "2001".into(),   // req_id
                "0".into(),      // position
                "0".into(),      // operation (insert)
                "1".into(),      // side (bid)
                "178.20".into(), // price
                "200".into(),    // size
            ],
        };
        let msg = TwsIncoming::parse(&raw)?;
        assert_eq!(
            msg,
            TwsIncoming::MarketDepth {
                req_id: 2001,
                position: 0,
                operation: 0,
                side: 1,
                price: 178.20,
                size: 200.0,
            }
        );
        Ok(())
    }

    // ── Test 12: parse error_message ──

    #[test]
    fn test_parse_error_message() -> anyhow::Result<()> {
        let raw = TwsRawMessage {
            msg_id: MSG_ERR_MSG,
            fields: vec![
                "2".into(),                                           // version
                "-1".into(),                                          // id
                "2104".into(),                                        // code
                "Market data farm connection is OK:usfarm.nj".into(), // message
            ],
        };
        let msg = TwsIncoming::parse(&raw)?;
        assert_eq!(
            msg,
            TwsIncoming::ErrorMessage {
                id: -1,
                code: 2104,
                message: "Market data farm connection is OK:usfarm.nj".into(),
            }
        );
        Ok(())
    }

    // ── Test 13: parse unknown msg_id ──

    #[test]
    fn test_parse_unknown_msg_id() -> anyhow::Result<()> {
        let raw = TwsRawMessage {
            msg_id: 999,
            fields: vec!["1".into()],
        };
        let result = TwsIncoming::parse(&raw);
        assert!(result.is_err());
        let err = format!("{}", result.err().context("expected error")?);
        assert!(err.contains("unknown TWS message ID: 999"), "got: {err}");
        Ok(())
    }
}
