//! HMAC-signed email unsubscribe tokens.
//!
//! Every digest email carries a `?u=<user_id>&t=<token>` unsubscribe
//! link. We sign the user id with `jwt_secret` so the link can't be
//! forged by a guess-and-check adversary, and we honour RFC 8058's
//! one-click `POST /email/unsub` pattern (Gmail / Apple Mail attach
//! an empty-body POST to the `List-Unsubscribe-Post` header).

use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub fn sign(user_id: Uuid, secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(user_id.as_bytes());
    let bytes = mac.finalize().into_bytes();
    hex::encode(bytes)
}

pub fn verify(user_id: Uuid, token: &str, secret: &str) -> bool {
    let expected = sign(user_id, secret);
    // Constant-time compare.
    constant_time_eq(expected.as_bytes(), token.as_bytes())
}

/// Same HMAC scheme as `sign`, but mixes a `kind` prefix into the MAC
/// input so a token minted for one id-namespace can't be replayed
/// against another namespace that happens to reuse the same UUID
/// (voters and users are separate tables with independently-generated
/// ids, so collisions are possible).
///
/// `sign`/`verify` above are the ORIGINAL, unprefixed format and MUST
/// NOT change — every digest email already sent encodes an unprefixed
/// user-unsubscribe token, and changing the format would invalidate
/// all of them. `sign_kind`/`verify_kind` are for new callers only
/// (currently: voter unsubscribe, kind = "voter").
pub fn sign_kind(kind: &str, id: Uuid, secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(kind.as_bytes());
    mac.update(id.as_bytes());
    let bytes = mac.finalize().into_bytes();
    hex::encode(bytes)
}

pub fn verify_kind(kind: &str, id: Uuid, token: &str, secret: &str) -> bool {
    let expected = sign_kind(kind, id, secret);
    constant_time_eq(expected.as_bytes(), token.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_is_deterministic() {
        let u = Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap();
        let a = sign(u, "shh");
        let b = sign(u, "shh");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // hex SHA-256
    }

    #[test]
    fn sign_is_secret_dependent() {
        let u = Uuid::new_v4();
        assert_ne!(sign(u, "a"), sign(u, "b"));
    }

    #[test]
    fn verify_accepts_valid() {
        let u = Uuid::new_v4();
        let t = sign(u, "secret");
        assert!(verify(u, &t, "secret"));
    }

    #[test]
    fn verify_rejects_wrong_secret() {
        let u = Uuid::new_v4();
        let t = sign(u, "secret");
        assert!(!verify(u, &t, "other"));
    }

    #[test]
    fn verify_rejects_wrong_user() {
        let t = sign(Uuid::new_v4(), "secret");
        assert!(!verify(Uuid::new_v4(), &t, "secret"));
    }

    #[test]
    fn verify_rejects_truncated_token() {
        let u = Uuid::new_v4();
        let t = sign(u, "secret");
        assert!(!verify(u, &t[..t.len() - 1], "secret"));
    }

    #[test]
    fn sign_kind_voter_round_trips() {
        let id = Uuid::new_v4();
        let t = sign_kind("voter", id, "secret");
        assert!(verify_kind("voter", id, &t, "secret"));
    }

    #[test]
    fn sign_kind_different_kinds_produce_different_tokens_for_same_id() {
        let id = Uuid::new_v4();
        let voter_token = sign_kind("voter", id, "secret");
        let user_token = sign_kind("user", id, "secret");
        assert_ne!(voter_token, user_token);
    }

    /// A user-unsubscribe token (the original, unprefixed `sign`
    /// format) must NOT validate as a voter-unsubscribe token for the
    /// same uuid — otherwise a leaked/guessed digest-email link for a
    /// user id that happens to collide with a voter id could opt that
    /// voter out too.
    #[test]
    fn user_token_does_not_validate_as_voter_token_for_same_uuid() {
        let id = Uuid::new_v4();
        let user_token = sign(id, "secret");
        assert!(!verify_kind("voter", id, &user_token, "secret"));
    }

    /// And the reverse: a voter token must not pass the original
    /// unprefixed `verify` used for user tokens.
    #[test]
    fn voter_token_does_not_validate_as_user_token_for_same_uuid() {
        let id = Uuid::new_v4();
        let voter_token = sign_kind("voter", id, "secret");
        assert!(!verify(id, &voter_token, "secret"));
    }

    /// Existing user tokens (pre-this-change format) must keep
    /// validating — `sign`/`verify` are untouched by the `sign_kind`
    /// addition.
    #[test]
    fn legacy_user_sign_verify_format_is_unchanged() {
        let u = Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap();
        // Pinned expected value: HMAC-SHA256("shh", uuid bytes), hex.
        let t = sign(u, "shh");
        assert!(verify(u, &t, "shh"));
        assert_eq!(t.len(), 64);
    }
}

#[cfg(test)]
mod vector_tests {
    use super::*;
    use uuid::Uuid;

    /// Cross-language vector pinned against krabagent's
    /// `scripts/digest_recipients.py::sign_voter_unsub` — HMAC-SHA256
    /// over b"voter" || uuid bytes with the shared JWT secret. If this
    /// test changes, the python mirror must change in lockstep.
    #[test]
    fn sign_kind_matches_krabagent_python_vector() {
        let id = Uuid::parse_str("51a6b67c-4144-4d52-9fa3-c5f6c75013d8").unwrap();
        assert_eq!(
            sign_kind("voter", id, "vector-secret"),
            "bd97ad6170d9217ed33246b002f22d72a8439d53de0c5fa5e34b3049a079c01f"
        );
    }
}
