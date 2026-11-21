//! Pure payload builders for webhook events (since 0.43.0).
//!
//! `webhook_dispatch::dispatch_event` is fire-and-forget and does its
//! own DB lookup inside a spawned task, so a handler-level test can't
//! observe "this path enqueued event X". Instead every dispatch site
//! routes through one of the functions below, which return the
//! `(event_name, data)` pair the site hands to `dispatch_event`. The
//! unit tests at the bottom of this file lock both halves: which event
//! name each input maps to, and the wire shape of `data`.
//!
//! Payload conventions (shared with `claim.posted`, see
//! `routes::claims::build_claim_webhook_payload`):
//!
//! - ids are stringified UUIDs, `null` when absent;
//! - every object carries a canonical public `url` on krabarena.com,
//!   built from the slug when one exists and the UUID otherwise;
//! - `title` is trimmed to [`TITLE_MAX_CHARS`] characters.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use krabarena_shared::webhook_events::{
    BATTLE_CREATED, CLAIM_REFUTED, CLAIM_VERIFIED, COMMENT_CREATED,
};

/// Public site origin used in payload URLs. Matches the hardcoded
/// origin in `models::claim::ClaimUrls` so a subscriber sees the same
/// URL in a webhook as in the REST API.
const PUBLIC_ORIGIN: &str = "https://krabarena.com";

/// Max characters of a Claim / Battle title carried in a payload.
pub const TITLE_MAX_CHARS: usize = 200;

/// Title of a Claim body: the first markdown H1 (`# Title`), or the
/// first line when there is no H1. Trimmed to [`TITLE_MAX_CHARS`]
/// characters (char-, not byte-based, so multi-byte text is safe).
pub fn claim_title(claim: &str) -> String {
    claim
        .lines()
        .find_map(|l| l.trim().strip_prefix("# "))
        .unwrap_or_else(|| claim.lines().next().unwrap_or("").trim())
        .trim()
        .chars()
        .take(TITLE_MAX_CHARS)
        .collect()
}

/// Canonical public URL of a Claim (slug when present, UUID otherwise).
pub fn claim_url(id: Uuid, slug: Option<&str>) -> String {
    let ident = slug
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string());
    format!("{PUBLIC_ORIGIN}/claims/{ident}")
}

/// Canonical public URL of a Battle (slug when present, UUID otherwise).
pub fn battle_url(id: Uuid, slug: Option<&str>) -> String {
    let ident = slug
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string());
    format!("{PUBLIC_ORIGIN}/battles/{ident}")
}

/// Map a stored verification verdict to its webhook event.
/// `confirmed` → `claim.verified`, `disconfirmed` → `claim.refuted`.
/// Anything else (the service rejects other values before insert, so
/// this is defensive) maps to `None` and no event fires.
pub fn verification_event(verdict: &str) -> Option<&'static str> {
    match verdict {
        "confirmed" => Some(CLAIM_VERIFIED),
        "disconfirmed" => Some(CLAIM_REFUTED),
        _ => None,
    }
}

/// Everything `claim.verified` / `claim.refuted` needs, captured at
/// the verification insert site (`verification_service::create_verification`).
#[derive(Debug, Clone)]
pub struct VerificationEventInput<'a> {
    pub verification_id: Uuid,
    /// Stored verdict: `confirmed` or `disconfirmed`.
    pub verdict: &'a str,
    pub verifier_id: Uuid,
    pub is_author_response: bool,
    pub agent_backend: Option<&'a str>,
    pub agent_model: Option<&'a str>,
    pub created_at: DateTime<Utc>,
    pub claim_id: Uuid,
    pub claim_slug: Option<&'a str>,
    pub claim_author_id: Uuid,
    pub claim_text: &'a str,
    pub battle_id: Option<Uuid>,
    pub battle_slug: Option<&'a str>,
}

/// Build the `(event, data)` pair for a recorded verification, or
/// `None` when the verdict doesn't map to an event.
pub fn verification_webhook(input: &VerificationEventInput<'_>) -> Option<(&'static str, Value)> {
    let event = verification_event(input.verdict)?;
    let outcome = if event == CLAIM_VERIFIED {
        "verified"
    } else {
        "refuted"
    };
    let data = json!({
        "verification": {
            "id": input.verification_id.to_string(),
            // Raw stored verdict (`confirmed` / `disconfirmed`) — the
            // same value `GET /v1/claims/{id}/verifications` returns.
            "verdict": input.verdict,
            // Public vocabulary (`verified` / `refuted`).
            "outcome": outcome,
            "author_id": input.verifier_id.to_string(),
            "is_author_response": input.is_author_response,
            "agent_backend": input.agent_backend,
            "agent_model": input.agent_model,
            "created_at": input.created_at.to_rfc3339(),
        },
        "claim": {
            "id": input.claim_id.to_string(),
            "slug": input.claim_slug,
            "battle_id": input.battle_id.map(|u| u.to_string()),
            "battle_slug": input.battle_slug,
            "author_id": input.claim_author_id.to_string(),
            "title": claim_title(input.claim_text),
            "url": claim_url(input.claim_id, input.claim_slug),
        }
    });
    Some((event, data))
}

/// Everything `battle.created` needs, captured at the Battle insert
/// site (`battle_service::create_battle`, shared by the editor route
/// and the admin route).
#[derive(Debug, Clone)]
pub struct BattleEventInput<'a> {
    pub battle_id: Uuid,
    pub slug: Option<&'a str>,
    pub title: &'a str,
    pub author_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub status: &'a str,
    pub domain_tags: &'a [String],
    pub created_at: DateTime<Utc>,
}

/// Build the `(event, data)` pair for a newly created Battle.
pub fn battle_created_webhook(input: &BattleEventInput<'_>) -> (&'static str, Value) {
    let title: String = input.title.trim().chars().take(TITLE_MAX_CHARS).collect();
    let data = json!({
        "battle": {
            "id": input.battle_id.to_string(),
            "slug": input.slug,
            "title": title,
            "author_id": input.author_id.to_string(),
            "parent_id": input.parent_id.map(|u| u.to_string()),
            "status": input.status,
            "domain_tags": input.domain_tags,
            "url": battle_url(input.battle_id, input.slug),
            "created_at": input.created_at.to_rfc3339(),
        }
    });
    (BATTLE_CREATED, data)
}

/// Everything `comment.created` needs, captured at the two comment
/// insert sites (`comment_service::create_comment` /
/// `create_battle_comment`). Since 0.43.6.
#[derive(Debug, Clone)]
pub struct CommentEventInput<'a> {
    pub comment_id: Uuid,
    pub author_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub body: &'a str,
    pub claim_id: Option<Uuid>,
    pub battle_id: Option<Uuid>,
    /// Slug of the Claim (claim comments) or Battle (battle comments).
    pub target_slug: Option<&'a str>,
    pub mentioned_user_ids: &'a [Uuid],
    pub agent_backend: Option<&'a str>,
    pub agent_model: Option<&'a str>,
    pub created_at: DateTime<Utc>,
}

/// Build the `(event, data)` pair for a newly posted Comment. `url`
/// points at the Claim (claim comments) or Battle (battle comments).
pub fn comment_created_webhook(input: &CommentEventInput<'_>) -> (&'static str, Value) {
    let url = match (input.claim_id, input.battle_id) {
        (Some(id), _) => claim_url(id, input.target_slug),
        (None, Some(id)) => battle_url(id, input.target_slug),
        (None, None) => PUBLIC_ORIGIN.to_string(),
    };
    let data = json!({
        "comment": {
            "id": input.comment_id.to_string(),
            "author_id": input.author_id.to_string(),
            "parent_id": input.parent_id.map(|u| u.to_string()),
            "body": input.body,
            "claim_id": input.claim_id.map(|u| u.to_string()),
            "battle_id": input.battle_id.map(|u| u.to_string()),
            "mentioned_user_ids": input
                .mentioned_user_ids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>(),
            "agent_backend": input.agent_backend,
            "agent_model": input.agent_model,
            "url": url,
            "created_at": input.created_at.to_rfc3339(),
        }
    });
    (COMMENT_CREATED, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_created_payload_shapes() {
        let mention = Uuid::from_u128(9);
        let mentions = [mention];
        let input = CommentEventInput {
            comment_id: Uuid::from_u128(1),
            author_id: Uuid::from_u128(2),
            parent_id: Some(Uuid::from_u128(3)),
            body: "agree with @bob",
            claim_id: Some(Uuid::from_u128(4)),
            battle_id: None,
            target_slug: Some("rust-beats-go"),
            mentioned_user_ids: &mentions,
            agent_backend: Some("claude-code"),
            agent_model: None,
            created_at: "2026-10-25T12:00:00Z".parse().unwrap(),
        };
        let (event, data) = comment_created_webhook(&input);
        assert_eq!(event, "comment.created");
        let c = &data["comment"];
        assert_eq!(c["parent_id"], Uuid::from_u128(3).to_string());
        assert_eq!(c["url"], "https://krabarena.com/claims/rust-beats-go");
        assert_eq!(c["battle_id"], Value::Null);
        assert_eq!(c["mentioned_user_ids"][0], mention.to_string());

        let battle = CommentEventInput {
            claim_id: None,
            battle_id: Some(Uuid::from_u128(5)),
            parent_id: None,
            target_slug: None,
            mentioned_user_ids: &[],
            ..input
        };
        let (_, data) = comment_created_webhook(&battle);
        assert_eq!(data["comment"]["parent_id"], Value::Null);
        assert_eq!(
            data["comment"]["url"],
            format!("https://krabarena.com/battles/{}", Uuid::from_u128(5))
        );
    }

    fn verification_input(verdict: &str) -> VerificationEventInput<'_> {
        VerificationEventInput {
            verification_id: Uuid::from_u128(1),
            verdict,
            verifier_id: Uuid::from_u128(2),
            is_author_response: false,
            agent_backend: Some("claude-code"),
            agent_model: None,
            created_at: "2026-10-25T12:00:00Z".parse().unwrap(),
            claim_id: Uuid::from_u128(3),
            claim_slug: Some("rust-beats-go"),
            claim_author_id: Uuid::from_u128(4),
            claim_text: "# Rust beats Go\n\n## Summary\nIt does.",
            battle_id: Some(Uuid::from_u128(5)),
            battle_slug: Some("fastest-http-server"),
        }
    }

    #[test]
    fn confirmed_verification_enqueues_claim_verified() {
        let (event, data) = verification_webhook(&verification_input("confirmed")).unwrap();
        assert_eq!(event, "claim.verified");
        assert_eq!(data["verification"]["verdict"], "confirmed");
        assert_eq!(data["verification"]["outcome"], "verified");
    }

    #[test]
    fn disconfirmed_verification_enqueues_claim_refuted() {
        let (event, data) = verification_webhook(&verification_input("disconfirmed")).unwrap();
        assert_eq!(event, "claim.refuted");
        assert_eq!(data["verification"]["verdict"], "disconfirmed");
        assert_eq!(data["verification"]["outcome"], "refuted");
    }

    #[test]
    fn unknown_verdict_enqueues_nothing() {
        assert!(verification_webhook(&verification_input("maybe")).is_none());
        assert_eq!(verification_event(""), None);
    }

    #[test]
    fn verification_payload_shape_matches_claim_posted_conventions() {
        let (_, data) = verification_webhook(&verification_input("confirmed")).unwrap();
        let v = &data["verification"];
        assert_eq!(v["id"], Uuid::from_u128(1).to_string());
        assert_eq!(v["author_id"], Uuid::from_u128(2).to_string());
        assert_eq!(v["is_author_response"], false);
        assert_eq!(v["agent_backend"], "claude-code");
        assert!(v["agent_model"].is_null());
        assert_eq!(v["created_at"], "2026-10-25T12:00:00+00:00");

        let c = &data["claim"];
        assert_eq!(c["id"], Uuid::from_u128(3).to_string());
        assert_eq!(c["slug"], "rust-beats-go");
        assert_eq!(c["battle_id"], Uuid::from_u128(5).to_string());
        assert_eq!(c["battle_slug"], "fastest-http-server");
        assert_eq!(c["author_id"], Uuid::from_u128(4).to_string());
        assert_eq!(c["title"], "Rust beats Go");
        assert_eq!(c["url"], "https://krabarena.com/claims/rust-beats-go");
    }

    #[test]
    fn standalone_claim_verification_has_null_battle_fields() {
        let mut input = verification_input("confirmed");
        input.battle_id = None;
        input.battle_slug = None;
        input.claim_slug = None;
        let (_, data) = verification_webhook(&input).unwrap();
        assert!(data["claim"]["battle_id"].is_null());
        assert!(data["claim"]["battle_slug"].is_null());
        // No slug → URL falls back to the UUID.
        assert_eq!(
            data["claim"]["url"],
            format!("https://krabarena.com/claims/{}", Uuid::from_u128(3))
        );
    }

    #[test]
    fn battle_created_payload_shape() {
        let tags = vec!["search".to_string(), "llm".to_string()];
        let (event, data) = battle_created_webhook(&BattleEventInput {
            battle_id: Uuid::from_u128(9),
            slug: Some("best-web-search-api"),
            title: "  Best web search API?  ",
            author_id: Uuid::from_u128(8),
            parent_id: None,
            status: "open",
            domain_tags: &tags,
            created_at: "2026-10-25T12:00:00Z".parse().unwrap(),
        });
        assert_eq!(event, "battle.created");
        let b = &data["battle"];
        assert_eq!(b["id"], Uuid::from_u128(9).to_string());
        assert_eq!(b["slug"], "best-web-search-api");
        assert_eq!(b["title"], "Best web search API?");
        assert_eq!(b["author_id"], Uuid::from_u128(8).to_string());
        assert!(b["parent_id"].is_null());
        assert_eq!(b["status"], "open");
        assert_eq!(b["domain_tags"], json!(["search", "llm"]));
        assert_eq!(
            b["url"],
            "https://krabarena.com/battles/best-web-search-api"
        );
    }

    #[test]
    fn every_wired_event_is_subscribable() {
        // A dispatched event a subscriber can't register for would be
        // dead on arrival — lock the two lists together.
        use krabarena_shared::webhook_events::is_known_event;
        for ev in [CLAIM_VERIFIED, CLAIM_REFUTED, BATTLE_CREATED] {
            assert!(is_known_event(ev), "{ev} must be subscribable");
        }
    }

    #[test]
    fn claim_title_prefers_h1_and_caps_length() {
        assert_eq!(claim_title("intro\n# Real title\nbody"), "Real title");
        assert_eq!(
            claim_title("  plain first line  \nmore"),
            "plain first line"
        );
        assert_eq!(claim_title(""), "");
        let long = format!("# {}", "é".repeat(500));
        assert_eq!(claim_title(&long).chars().count(), TITLE_MAX_CHARS);
    }
}
