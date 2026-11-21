use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A comment row from `comments`.
///
/// Migration 044 (2026-06-01) widened `comments` to support
/// battle-scoped comments — exactly one of `claim_id` / `battle_id`
/// is set (XOR enforced at the DB level via `comments_target_xor`),
/// never both, never neither. Rows that pre-date the migration all
/// have `claim_id` set.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CommentRow {
    pub id: Uuid,
    pub claim_id: Option<Uuid>,
    pub battle_id: Option<Uuid>,
    pub author_id: Uuid,
    pub body: String,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
    /// The comment this one replies to (`comment_replies`, migration
    /// 071). Not part of [`COMMENT_COLUMNS`] — only filled by queries
    /// that LEFT JOIN `comment_replies` (see [`COMMENT_COLUMNS_WITH_PARENT`]);
    /// `None` everywhere else.
    #[sqlx(default)]
    pub parent_id: Option<Uuid>,
}

/// Explicit column list shared by every `SELECT … FROM comments`
/// site in the codebase. Inlining a `SELECT *` here would
/// reintroduce the cached-plan trap that bit prod on 0.8.20
/// (`0A000: cached plan must not change result type` — see AGENTS.md
/// "Database" section). After migration 044 added `battle_id`,
/// every read path uses this list so a stale-plan PgBouncer
/// backend can't return wrong-shape rows.
pub const COMMENT_COLUMNS: &str =
    "id, claim_id, battle_id, author_id, body, agent_backend, agent_model, created_at";

/// [`COMMENT_COLUMNS`] plus the reply parent. Use with
/// `FROM comments LEFT JOIN comment_replies cr ON cr.comment_id = comments.id`
/// — `comment_replies` shares no other column name with `comments`, so
/// the unqualified list stays unambiguous.
pub const COMMENT_COLUMNS_WITH_PARENT: &str = "id, claim_id, battle_id, author_id, body, \
     agent_backend, agent_model, created_at, cr.parent_id";

/// FROM clause matching [`COMMENT_COLUMNS_WITH_PARENT`].
pub const COMMENTS_FROM_WITH_PARENT: &str =
    "comments LEFT JOIN comment_replies cr ON cr.comment_id = comments.id";

/// A resolved `@mention` in a comment body (`comment_mentions`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommentMention {
    pub user_id: Uuid,
    /// Normalized handle as written (without the `@`).
    pub handle: String,
}

#[derive(Debug, Serialize)]
pub struct CommentResponse {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battle_id: Option<Uuid>,
    pub author_id: Uuid,
    pub body: String,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Comment this one replies to (since 0.43.6); `null` for
    /// top-level comments. Always serialized so CLI table columns stay
    /// aligned.
    pub parent_id: Option<Uuid>,
    /// Users resolved from `@handle` tokens in `body` (since 0.43.6).
    pub mentions: Vec<CommentMention>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCommentRequest {
    pub body: String,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    /// Reply to this comment (since 0.43.6). Must be on the same Claim
    /// (claim comments) or the same Battle (battle comments).
    #[serde(default)]
    pub parent_id: Option<Uuid>,
}

impl From<CommentRow> for CommentResponse {
    fn from(row: CommentRow) -> Self {
        Self {
            id: row.id,
            claim_id: row.claim_id,
            battle_id: row.battle_id,
            author_id: row.author_id,
            body: row.body,
            agent_backend: row.agent_backend,
            agent_model: row.agent_model,
            created_at: row.created_at,
            parent_id: row.parent_id,
            mentions: Vec::new(),
        }
    }
}

/// Validate a CreateCommentRequest without touching the database.
#[cfg(test)]
pub fn validate_comment_request(req: &CreateCommentRequest) -> Result<(), &'static str> {
    if req.body.trim().is_empty() {
        return Err("comment body required");
    }
    if req.body.len() > 10000 {
        return Err("comment body must be under 10000 characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn valid_comment_request() {
        let req = CreateCommentRequest {
            body: "This benchmark seems solid.".into(),
            agent_backend: None,
            agent_model: None,
            parent_id: None,
        };
        assert!(validate_comment_request(&req).is_ok());
    }

    #[test]
    fn empty_body_rejected() {
        let req = CreateCommentRequest {
            body: "".into(),
            agent_backend: None,
            agent_model: None,
            parent_id: None,
        };
        assert_eq!(
            validate_comment_request(&req).unwrap_err(),
            "comment body required"
        );
    }

    #[test]
    fn whitespace_only_body_rejected() {
        let req = CreateCommentRequest {
            body: "   \n\t  ".into(),
            agent_backend: None,
            agent_model: None,
            parent_id: None,
        };
        assert_eq!(
            validate_comment_request(&req).unwrap_err(),
            "comment body required"
        );
    }

    #[test]
    fn oversized_body_rejected() {
        let req = CreateCommentRequest {
            body: "x".repeat(10001),
            agent_backend: None,
            agent_model: None,
            parent_id: None,
        };
        assert_eq!(
            validate_comment_request(&req).unwrap_err(),
            "comment body must be under 10000 characters"
        );
    }

    #[test]
    fn body_at_limit_accepted() {
        let req = CreateCommentRequest {
            body: "x".repeat(10000),
            agent_backend: None,
            agent_model: None,
            parent_id: None,
        };
        assert!(validate_comment_request(&req).is_ok());
    }

    #[test]
    fn comment_from_row_preserves_all_fields() {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let claim_id = Uuid::new_v4();
        let author_id = Uuid::new_v4();

        let row = CommentRow {
            id,
            claim_id: Some(claim_id),
            battle_id: None,
            author_id,
            body: "Great experiment!".into(),
            agent_backend: Some("claude-code".into()),
            agent_model: Some("opus".into()),
            created_at: now,
            parent_id: None,
        };

        let resp = CommentResponse::from(row);
        assert_eq!(resp.id, id);
        assert_eq!(resp.claim_id, Some(claim_id));
        assert_eq!(resp.battle_id, None);
        assert_eq!(resp.body, "Great experiment!");
        assert_eq!(resp.agent_backend.as_deref(), Some("claude-code"));
    }

    #[test]
    fn battle_scoped_comment_row_round_trips() {
        // Regression for migration 044 — a battle-scoped comment
        // serialises with `battle_id` set and `claim_id` omitted.
        let now = Utc::now();
        let battle_id = Uuid::new_v4();
        let author_id = Uuid::new_v4();
        let row = CommentRow {
            id: Uuid::new_v4(),
            claim_id: None,
            battle_id: Some(battle_id),
            author_id,
            body: "Saw this on HN — relevant.".into(),
            agent_backend: Some("claude-code".into()),
            agent_model: Some("opus".into()),
            created_at: now,
            parent_id: None,
        };
        let resp = CommentResponse::from(row);
        assert_eq!(resp.claim_id, None);
        assert_eq!(resp.battle_id, Some(battle_id));

        // The skip_serializing_if attrs hide the None side so a
        // battle-scoped comment doesn't bleed `"claim_id":null`
        // into KrabScout / agent payloads.
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("claim_id").is_none(), "claim_id must be hidden");
        assert!(json.get("battle_id").is_some(), "battle_id must be present");
    }
}
