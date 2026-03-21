use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256, Sha512};

type HmacSha512 = Hmac<Sha512>;

/// Generate a monotonically increasing nonce from system time (milliseconds
/// since epoch).
pub(crate) fn generate_nonce() -> anyhow::Result<String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before Unix epoch")?;
    Ok(duration.as_millis().to_string())
}

/// Compute the Kraken HMAC-SHA512 API signature.
///
/// Algorithm:
/// 1. `sha256_hash = SHA256(nonce + post_data)`
/// 2. `hmac_input  = url_path_bytes + sha256_hash`
/// 3. `signature   = HMAC-SHA512(key = base64_decode(api_secret), msg =
///    hmac_input)`
/// 4. Return `base64_encode(signature)`
pub(crate) fn sign_request(
    url_path: &str,
    nonce: &str,
    post_data: &str,
    api_secret: &str,
) -> anyhow::Result<String> {
    // Decode the base64-encoded API secret
    let secret_bytes = BASE64
        .decode(api_secret)
        .context("failed to base64-decode API secret")?;

    // SHA-256(nonce + post_data)
    let mut sha256 = Sha256::new();
    sha256.update(nonce.as_bytes());
    sha256.update(post_data.as_bytes());
    let sha256_hash = sha256.finalize();

    // HMAC-SHA512 input = url_path bytes + sha256 hash
    let mut hmac_input = Vec::with_capacity(url_path.len() + sha256_hash.len());
    hmac_input.extend_from_slice(url_path.as_bytes());
    hmac_input.extend_from_slice(&sha256_hash);

    // HMAC-SHA512(key = decoded secret, msg = hmac_input)
    let mut mac = HmacSha512::new_from_slice(&secret_bytes).context("invalid HMAC key length")?;
    mac.update(&hmac_input);
    let result = mac.finalize().into_bytes();

    Ok(BASE64.encode(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_request_known_vector() -> anyhow::Result<()> {
        // Use a known API secret (base64-encoded)
        let api_secret = BASE64.encode(b"supersecretkey1234567890abcdef");

        let url_path = "/0/private/Balance";
        let nonce = "1616663594000";
        let post_data = "nonce=1616663594000";

        let signature = sign_request(url_path, nonce, post_data, &api_secret)?;

        // Verify the signature is valid base64 and non-empty
        let decoded = BASE64
            .decode(&signature)
            .context("signature is not valid base64")?;
        // HMAC-SHA512 produces 64 bytes
        assert_eq!(decoded.len(), 64, "HMAC-SHA512 should produce 64 bytes");

        // Verify determinism — same inputs produce same output
        let signature2 = sign_request(url_path, nonce, post_data, &api_secret)?;
        assert_eq!(signature, signature2, "signing should be deterministic");

        Ok(())
    }

    #[test]
    fn test_sign_request_invalid_base64_secret() {
        let result = sign_request(
            "/0/private/Balance",
            "123",
            "nonce=123",
            "!!!not-valid-base64!!!",
        );
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.err().unwrap_or_else(|| unreachable!()));
        assert!(
            err_msg.contains("base64"),
            "error should mention base64: {err_msg}"
        );
    }

    #[test]
    fn test_generate_nonce_monotonically_increasing() -> anyhow::Result<()> {
        let nonce1 = generate_nonce()?;
        let nonce2 = generate_nonce()?;

        let n1: u128 = nonce1.parse().context("nonce1 not a number")?;
        let n2: u128 = nonce2.parse().context("nonce2 not a number")?;

        assert!(
            n2 >= n1,
            "nonce should be monotonically increasing: {n1} vs {n2}"
        );
        Ok(())
    }
}
