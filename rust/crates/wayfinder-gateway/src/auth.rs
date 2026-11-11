//! Virtual-key hashing, extraction, and constant-time verification.
//!
//! Only SHA-256 digests enter configuration. Plaintext gateway credentials are
//! accepted transiently for verification and are never formatted, logged, or
//! returned by this module.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Hash a presented virtual key into the lowercase stored representation.
#[must_use]
pub fn hash_key(presented: &str) -> String {
    let digest = Sha256::digest(presented.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        encoded.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// Return the matching key id while comparing every configured digest.
///
/// Iterating the complete collection avoids leaking the matching collection
/// position through an early return. Duplicate hashes retain the last id, as
/// in the Python oracle.
pub fn match_key<'a>(
    presented: Option<&str>,
    hashes: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<String> {
    let presented = presented.filter(|value| !value.is_empty())?;
    let actual = Sha256::digest(presented.as_bytes());
    let mut found = None;
    for (key_id, expected_hash) in hashes {
        let matched = parse_digest(expected_hash)
            .is_some_and(|expected| bool::from(actual.as_slice().ct_eq(&expected)));
        if matched {
            found = Some(key_id.to_owned());
        }
    }
    found
}

/// Extract `Bearer <token>` or the current bare-token compatibility form.
#[must_use]
pub fn extract_bearer(authorization: Option<&str>) -> Option<String> {
    let value = authorization?.trim();
    if value.is_empty() {
        return None;
    }
    let lowercase = value.to_ascii_lowercase();
    if lowercase == "bearer" || lowercase.starts_with("bearer ") {
        return value
            .get(6..)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_owned);
    }
    Some(value.to_owned())
}

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    let value = value.trim();
    if value.len() != 64 {
        return None;
    }
    let mut result = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(*pair.first()?)?;
        let low = hex_nibble(*pair.get(1)?)?;
        let slot = result.get_mut(index)?;
        *slot = (high << 4) | low;
    }
    Some(result)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_python_sha256() {
        assert_eq!(
            hash_key("wf-example"),
            "5a41a8e23e22d241c76385d4f388118985f151c46500d9bc02f668aa08d1461c"
        );
    }

    #[test]
    fn match_accepts_case_and_surrounding_whitespace() {
        let digest = hash_key("secret");
        let padded = format!("  {}  ", digest.to_ascii_uppercase());
        assert_eq!(
            match_key(Some("secret"), [("key", padded.as_str())]),
            Some("key".to_owned())
        );
        assert_eq!(match_key(Some("wrong"), [("key", digest.as_str())]), None);
        assert_eq!(match_key(Some("secret"), [("key", "not-a-digest")]), None);
    }

    #[test]
    fn match_checks_collection_and_retains_last_duplicate() {
        let digest = hash_key("secret");
        let wrong = hash_key("other");
        let hashes = [
            ("first", wrong.as_str()),
            ("second", digest.as_str()),
            ("third", digest.as_str()),
        ];
        assert_eq!(match_key(Some("secret"), hashes), Some("third".to_owned()));
        assert_eq!(match_key(Some("absent"), hashes), None);
        assert_eq!(match_key(None, hashes), None);
    }

    #[test]
    fn authorization_extraction_matches_legacy_tolerance() {
        assert_eq!(
            extract_bearer(Some(" Bearer wf-token ")),
            Some("wf-token".to_owned())
        );
        assert_eq!(
            extract_bearer(Some("bearer    wf-token")),
            Some("wf-token".to_owned())
        );
        assert_eq!(extract_bearer(Some("wf-bare")), Some("wf-bare".to_owned()));
        assert_eq!(extract_bearer(Some("Bearer")), None);
        assert_eq!(extract_bearer(Some("  ")), None);
        assert_eq!(extract_bearer(None), None);
    }

    #[test]
    fn tab_after_scheme_remains_a_bare_token_for_python_parity() {
        assert_eq!(
            extract_bearer(Some("Bearer\twf-token")),
            Some("Bearer\twf-token".to_owned())
        );
    }
}
