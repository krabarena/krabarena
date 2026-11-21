//! Resolve a `{battle_id}` / `{claim_id}` path segment that may arrive
//! as either a canonical UUID, a human-readable slug, or a full
//! `https://krabarena.com/(battles|claims)/<id-or-slug>` browser URL.
//! The `.org` apex still works (301 → .com) and the prefix stripper
//! is host-agnostic, so links pasted from the legacy domain resolve
//! the same way.
//!
//! The web layer has always done UUID/slug lookup (so browser URLs
//! like `/battles/what-is-the-best-…` work) but the JSON API was
//! UUID-only, which made life hard for agents: they would see a
//! browser URL, try `krab battle get <slug>`, and get a cryptic
//! `UUID parsing failed` from axum's extractor before the handler
//! even ran. This helper closes that gap — the same string works on
//! every endpoint that takes a battle or claim id, **including**
//! the literal URL an agent reads from a Codex / Claude transcript.
//!
//! On a cache miss (neither a UUID nor a known slug) we return the
//! specific not-found variant so clients get `BATTLE_NOT_FOUND` or
//! `CLAIM_NOT_FOUND` instead of the generic 400 axum produces for a
//! malformed UUID.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;

/// Strip a leading browser URL prefix so agents can paste the exact
/// URL they see in a Codex / Claude / chat transcript without
/// pre-processing. Accepts any `http(s)://<host>/(battles|claims)/`
/// shape — host-agnostic so .com (canonical), .org (legacy 301
/// source), dev, and test envs all parse the same way. A trailing
/// slash / `#anchor` / `?query` is also tolerated. Returns the input
/// unchanged when no URL prefix matches.
fn strip_url_prefix(input: &str) -> &str {
    let s = input.trim();
    // Find the last `/battles/` or `/claims/` segment; the suffix
    // after it is the slug or UUID we actually want to resolve.
    let s = match s.rfind("/battles/").or_else(|| s.rfind("/claims/")) {
        Some(pos) => {
            let after = &s[pos..];
            after
                .strip_prefix("/battles/")
                .or_else(|| after.strip_prefix("/claims/"))
                .unwrap_or(after)
        }
        None => s,
    };
    // Drop fragment + query so a pasted URL with anchors still
    // resolves cleanly.
    let s = s.split(['#', '?']).next().unwrap_or(s);
    s.trim_end_matches('/')
}

pub async fn resolve_battle(pool: &PgPool, id_or_slug: &str) -> Result<Uuid, ApiError> {
    let key = strip_url_prefix(id_or_slug);
    if let Ok(uuid) = key.parse::<Uuid>() {
        return Ok(uuid);
    }
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM battles WHERE slug = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::BattleNotFound)
}

pub async fn resolve_claim(pool: &PgPool, id_or_slug: &str) -> Result<Uuid, ApiError> {
    let key = strip_url_prefix(id_or_slug);
    if let Ok(uuid) = key.parse::<Uuid>() {
        return Ok(uuid);
    }
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM claims WHERE slug = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::ClaimNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_url_prefix_battle_url() {
        assert_eq!(
            strip_url_prefix("https://krabarena.com/battles/foo-bar"),
            "foo-bar"
        );
        // .org still resolves so links shared before the 2026-05-22
        // rebrand keep working through the 301.
        assert_eq!(
            strip_url_prefix("https://krabarena.org/battles/foo-bar"),
            "foo-bar"
        );
    }

    #[test]
    fn strip_url_prefix_claim_url() {
        assert_eq!(
            strip_url_prefix("https://krabarena.com/claims/foo-bar-baz"),
            "foo-bar-baz"
        );
        assert_eq!(
            strip_url_prefix("https://krabarena.org/claims/foo-bar-baz"),
            "foo-bar-baz"
        );
    }

    #[test]
    fn strip_url_prefix_passes_through_bare_slug() {
        assert_eq!(strip_url_prefix("foo-bar-baz"), "foo-bar-baz");
    }

    #[test]
    fn strip_url_prefix_passes_through_uuid() {
        let u = "b8009b83-3a42-47d0-bea5-844f44f17093";
        assert_eq!(strip_url_prefix(u), u);
    }

    #[test]
    fn strip_url_prefix_handles_anchor_and_query() {
        assert_eq!(
            strip_url_prefix("https://krabarena.org/battles/foo-bar#claims"),
            "foo-bar"
        );
        assert_eq!(
            strip_url_prefix("https://krabarena.org/claims/foo-bar?cb=1"),
            "foo-bar"
        );
        assert_eq!(
            strip_url_prefix("https://krabarena.org/battles/foo-bar/"),
            "foo-bar"
        );
    }

    #[test]
    fn strip_url_prefix_test_env_host() {
        assert_eq!(
            strip_url_prefix("https://krabarena-api-abc123-uc.a.run.app/battles/foo"),
            "foo"
        );
    }

    #[test]
    fn strip_url_prefix_trims_whitespace() {
        assert_eq!(
            strip_url_prefix("  https://krabarena.org/claims/foo  "),
            "foo"
        );
    }
}
