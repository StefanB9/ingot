use anyhow::Context;
use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Raw decoded TWS message: message ID extracted, remaining fields as strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TwsRawMessage {
    pub msg_id: i32,
    pub fields: Vec<String>,
}

/// Codec for IBKR TWS binary protocol.
///
/// Wire format: `[4-byte BE length][field0\0field1\0...fieldN\0]`
///
/// Fields are null-separated UTF-8 strings. The first field is always the
/// message type ID (an integer encoded as its string representation).
#[derive(Debug, Default)]
pub(crate) struct TwsCodec;

impl Decoder for TwsCodec {
    type Error = anyhow::Error;
    type Item = TwsRawMessage;

    fn decode(&mut self, src: &mut BytesMut) -> anyhow::Result<Option<Self::Item>> {
        if src.len() < 4 {
            return Ok(None);
        }

        let length = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;

        if src.len() < 4 + length {
            src.reserve(4 + length - src.len());
            return Ok(None);
        }

        src.advance(4);
        let payload = src.split_to(length);

        let text = std::str::from_utf8(&payload).context("TWS message contains invalid UTF-8")?;

        let mut parts: Vec<&str> = text.split('\0').collect();
        // Remove trailing empty string from trailing null
        if parts.last() == Some(&"") {
            parts.pop();
        }

        let msg_id_str = parts.first().context("TWS message has no fields")?;
        let msg_id: i32 = msg_id_str
            .parse()
            .with_context(|| format!("invalid TWS message ID: {msg_id_str}"))?;

        let fields = parts[1..].iter().map(|s| (*s).to_string()).collect();

        Ok(Some(TwsRawMessage { msg_id, fields }))
    }
}

impl Encoder<Vec<String>> for TwsCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, fields: Vec<String>, dst: &mut BytesMut) -> anyhow::Result<()> {
        let mut payload = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                payload.push(b'\0');
            }
            payload.extend_from_slice(field.as_bytes());
        }
        payload.push(b'\0');

        let length = payload.len() as u32;
        dst.reserve(4 + payload.len());
        dst.put_u32(length);
        dst.extend_from_slice(&payload);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 1: encode single field ──

    #[test]
    fn test_encode_single_field() -> anyhow::Result<()> {
        let mut codec = TwsCodec;
        let mut buf = BytesMut::new();

        codec.encode(vec!["9".into()], &mut buf)?;

        // Payload: "9\0" = 2 bytes
        assert_eq!(buf.len(), 6); // 4 prefix + 2 payload
        let length = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(length, 2);
        assert_eq!(&buf[4..], b"9\0");
        Ok(())
    }

    // ── Test 2: encode multiple fields ──

    #[test]
    fn test_encode_multiple_fields() -> anyhow::Result<()> {
        let mut codec = TwsCodec;
        let mut buf = BytesMut::new();

        codec.encode(
            vec!["1".into(), "2".into(), "100".into(), "178.5".into()],
            &mut buf,
        )?;

        // Payload: "1\02\0100\0178.5\0"
        let length = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let payload = &buf[4..4 + length];
        let text = std::str::from_utf8(payload)?;
        assert_eq!(text, "1\02\0100\0178.5\0");
        Ok(())
    }

    // ── Test 3: decode roundtrip ──

    #[test]
    fn test_decode_roundtrip() -> anyhow::Result<()> {
        let mut codec = TwsCodec;
        let mut buf = BytesMut::new();

        codec.encode(vec!["9".into(), "1".into(), "42".into()], &mut buf)?;

        let msg = codec.decode(&mut buf)?.context("expected a message")?;
        assert_eq!(msg.msg_id, 9);
        assert_eq!(msg.fields, vec!["1", "42"]);
        assert!(buf.is_empty());
        Ok(())
    }

    // ── Test 4: decode incomplete ──

    #[test]
    fn test_decode_incomplete() -> anyhow::Result<()> {
        let mut codec = TwsCodec;

        // Only 2 bytes — not enough for length prefix
        let mut buf = BytesMut::from(&[0u8, 5][..]);
        assert!(codec.decode(&mut buf)?.is_none());
        assert_eq!(buf.len(), 2); // no data consumed

        // 4-byte length says 10 bytes, but only 6 available total
        let mut buf = BytesMut::from(&[0u8, 0, 0, 10, 0, 0][..]);
        assert!(codec.decode(&mut buf)?.is_none());
        assert_eq!(buf.len(), 6); // no data consumed
        Ok(())
    }

    // ── Test 5: decode malformed msg_id ──

    #[test]
    fn test_decode_malformed_msg_id() -> anyhow::Result<()> {
        let mut codec = TwsCodec;
        let mut buf = BytesMut::new();

        // Manually encode a message with non-numeric first field
        let payload = b"hello\0world\0";
        buf.put_u32(payload.len() as u32);
        buf.extend_from_slice(payload);

        let result = codec.decode(&mut buf);
        assert!(result.is_err());
        let err = format!("{}", result.err().context("expected error")?);
        assert!(err.contains("invalid TWS message ID"), "got: {err}");
        Ok(())
    }

    // ── Test 15: proptest codec roundtrip ──

    mod prop {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #![proptest_config(proptest::prelude::ProptestConfig::with_cases(1000))]

            #[test]
            fn prop_test_codec_roundtrip(
                msg_id in 1i32..=100,
                fields in proptest::collection::vec("[a-zA-Z0-9.]{0,20}", 0..5),
            ) {
                let mut codec = TwsCodec;
                let mut buf = BytesMut::new();

                let mut all_fields = vec![msg_id.to_string()];
                all_fields.extend(fields.clone());

                let encode_result = codec.encode(all_fields, &mut buf);
                prop_assert!(encode_result.is_ok(), "encode failed");

                let decode_result = codec.decode(&mut buf);
                prop_assert!(decode_result.is_ok(), "decode failed");

                let msg = decode_result.ok().flatten();
                prop_assert!(msg.is_some(), "decode returned None");

                let msg = msg.unwrap_or_else(|| TwsRawMessage { msg_id: -1, fields: vec![] });
                prop_assert_eq!(msg.msg_id, msg_id);
                prop_assert_eq!(msg.fields, fields);
            }
        }
    }
}
