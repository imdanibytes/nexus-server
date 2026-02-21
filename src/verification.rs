use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum VerificationError {
    #[error("invalid signature format")]
    InvalidFormat,
    #[error("signature mismatch")]
    SignatureMismatch,
}

/// Verify a GitHub webhook HMAC-SHA256 signature.
///
/// `signature_header` is the value of `X-Hub-Signature-256`, e.g. `sha256=abc123...`.
pub fn verify_github_hmac(
    secret: &[u8],
    signature_header: &str,
    body: &[u8],
) -> Result<(), VerificationError> {
    let hex_sig = signature_header
        .strip_prefix("sha256=")
        .ok_or(VerificationError::InvalidFormat)?;

    let expected =
        hex::decode(hex_sig).map_err(|_| VerificationError::InvalidFormat)?;

    let mut mac =
        HmacSha256::new_from_slice(secret).expect("HMAC accepts any key size");
    mac.update(body);

    mac.verify_slice(&expected)
        .map_err(|_| VerificationError::SignatureMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_signature() {
        let secret = b"test-secret";
        let body = b"hello world";

        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());

        let header = format!("sha256={sig}");
        assert!(verify_github_hmac(secret, &header, body).is_ok());
    }

    #[test]
    fn invalid_signature() {
        let secret = b"test-secret";
        let body = b"hello world";
        let header = "sha256=0000000000000000000000000000000000000000000000000000000000000000";
        assert!(matches!(
            verify_github_hmac(secret, header, body),
            Err(VerificationError::SignatureMismatch)
        ));
    }

    #[test]
    fn missing_prefix() {
        assert!(matches!(
            verify_github_hmac(b"s", "bad-header", b"body"),
            Err(VerificationError::InvalidFormat)
        ));
    }

    #[test]
    fn invalid_hex() {
        assert!(matches!(
            verify_github_hmac(b"s", "sha256=not-hex!", b"body"),
            Err(VerificationError::InvalidFormat)
        ));
    }
}
