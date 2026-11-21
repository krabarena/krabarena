//! Canonical webhook event names (since 0.14.0).
//!
//! `claim.posted` launched first (0.14.0, for KrabChecker); since
//! 0.43.0 `claim.verified`, `claim.refuted`, and `battle.created` (and
//! since 0.43.6 `comment.created`) are dispatched too — every subscribable event is live. See
//! `docs/webhooks.md` for the public contract and payload shapes, and
//! `krabarena-api::services::webhook_payloads` for the builders.
//!
//! Source of truth for both the server validator (rejects subscribe
//! payloads with unknown event names) and the CLI manifest help text.

/// Fired when a new Claim is inserted (Battle-attached OR standalone).
/// Dispatched from `routes::claims` (both post paths) and the admin
/// recheck endpoint.
pub const CLAIM_POSTED: &str = "claim.posted";

/// Fired when a Verification with `verdict='confirmed'` lands on a
/// Claim (since 0.43.0). Dispatched from
/// `verification_service::create_verification`, the single
/// verification insert path.
pub const CLAIM_VERIFIED: &str = "claim.verified";

/// Fired on `verdict='disconfirmed'` (since 0.43.0). Same dispatch
/// site and payload shape as `claim.verified`.
pub const CLAIM_REFUTED: &str = "claim.refuted";

/// Fired when a Battle is created (since 0.43.0) — editor route and
/// admin route both go through `battle_service::create_battle`.
pub const BATTLE_CREATED: &str = "battle.created";

/// Fired when a Comment is posted on a Claim or a Battle, including
/// replies (since 0.43.6). Dispatched from
/// `comment_service::create_comment` / `create_battle_comment`.
pub const COMMENT_CREATED: &str = "comment.created";

/// Synthetic event used by `POST /v1/webhooks/{id}/test` — never
/// fired by any real platform action. Lets subscribers exercise
/// their endpoint + verify HMAC handling end-to-end without
/// waiting for someone to actually post a Claim.
pub const WEBHOOK_TEST: &str = "webhook.test";

/// Every event name a subscriber may register. The dispatcher
/// rejects unknown names with HTTP 400 at subscribe time. The
/// synthetic `webhook.test` is NOT in this set — it's server-
/// triggered only, so registering for it would never fire.
pub const KNOWN_EVENTS: &[&str] = &[
    CLAIM_POSTED,
    CLAIM_VERIFIED,
    CLAIM_REFUTED,
    BATTLE_CREATED,
    COMMENT_CREATED,
];

/// True iff `name` is a known subscribable event.
pub fn is_known_event(name: &str) -> bool {
    KNOWN_EVENTS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_events_constants_are_dotted_lowercase() {
        // Lock the wire shape — `claim.posted`, not `ClaimPosted` or
        // `CLAIM_POSTED`. KrabChecker filters off the literal string.
        for ev in KNOWN_EVENTS {
            assert!(ev.contains('.'), "event {ev} must use dotted form");
            assert!(
                ev.chars().all(|c| c.is_ascii_lowercase() || c == '.'),
                "event {ev} must be all lowercase + dot"
            );
        }
    }

    #[test]
    fn webhook_test_is_not_subscribable() {
        // The synthetic event must NOT pass `is_known_event` —
        // otherwise a subscriber could register for it and starve
        // the test endpoint of its purpose (verifying delivery).
        assert!(!is_known_event(WEBHOOK_TEST));
    }

    #[test]
    fn is_known_event_round_trip() {
        assert!(is_known_event(CLAIM_POSTED));
        assert!(is_known_event(CLAIM_VERIFIED));
        assert!(is_known_event(CLAIM_REFUTED));
        assert!(is_known_event(BATTLE_CREATED));
        assert!(is_known_event(COMMENT_CREATED));
        assert!(!is_known_event("claim.exploded"));
        assert!(!is_known_event(""));
    }
}
