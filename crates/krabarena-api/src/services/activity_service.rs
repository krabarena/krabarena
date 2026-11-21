use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::envelope::PaginationMeta;
use crate::error::ApiError;
use crate::models::claim::not_superseded;
use crate::pagination::{Cursor, decode_cursor, finalize_page};

/// One row of a contributor's cross-surface activity feed (the
/// `/v1/agents/{id}` "Recent activity" list). `kind` discriminates
/// battle / claim / confirmed / refuted / comment; `title` + `detail`
/// are pre-formatted for display.
#[derive(Debug, Serialize)]
pub struct UserActivityItem {
    pub kind: String,
    pub id: Uuid,
    pub title: String,
    pub detail: String,
    pub created_at: DateTime<Utc>,
}

/// Paginated contributor activity across battles, claims, verifications
/// and comments authored by `user_id`, newest first.
///
/// Replaces the old five-separate-queries-then-merge-in-memory approach
/// (which silently truncated prolific contributors to ~100 items) with
/// a single keyset-paginated `UNION ALL` on `(created_at, id)`. Each leg
/// is backed by an `(author_id, created_at DESC)` index (migration 059).
/// Title/detail formatting mirrors the previous per-kind shaping; text
/// is truncated with SQL `substring` (character-, not byte-based).
pub async fn user_activity(
    pool: &PgPool,
    user_id: Uuid,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<UserActivityItem>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cur = decode_cursor(cursor);
    let superseded = not_superseded("c");

    // Every leg projects a common `(kind, id, title, detail, created_at)`
    // shape and applies the same `(created_at, id)` keyset predicate.
    let rows = sqlx::query_as::<_, (String, Uuid, String, String, DateTime<Utc>)>(&format!(
        r#"
        SELECT 'battle' AS kind, id, title,
               status AS detail, created_at
        FROM battles
        WHERE author_id = $1 AND deleted_at IS NULL
          AND ($2::timestamptz IS NULL OR created_at < $2
               OR (created_at = $2 AND id < $3))
        UNION ALL
        SELECT 'claim' AS kind, c.id, substring(c.claim FROM 1 FOR 120),
               c.verify_count || ' confirmed, ' || c.refute_count || ' refuted', c.created_at
        FROM claims c
        WHERE c.author_id = $1 AND c.deleted_at IS NULL {superseded}
          AND ($2::timestamptz IS NULL OR c.created_at < $2
               OR (c.created_at = $2 AND c.id < $3))
        UNION ALL
        SELECT 'confirmed' AS kind, id,
               'Confirmed in ' || COALESCE(duration_ms, 0) || 'ms',
               substring(COALESCE(environment, '') FROM 1 FOR 80), created_at
        FROM verifications
        WHERE author_id = $1 AND verdict = 'confirmed'
          AND ($2::timestamptz IS NULL OR created_at < $2
               OR (created_at = $2 AND id < $3))
        UNION ALL
        SELECT 'refuted' AS kind, id,
               substring(COALESCE(failure_reason, '') FROM 1 FOR 120), '', created_at
        FROM verifications
        WHERE author_id = $1 AND verdict = 'disconfirmed'
          AND ($2::timestamptz IS NULL OR created_at < $2
               OR (created_at = $2 AND id < $3))
        UNION ALL
        SELECT 'comment' AS kind, id, substring(body FROM 1 FOR 120), '', created_at
        FROM comments
        WHERE author_id = $1
          AND ($2::timestamptz IS NULL OR created_at < $2
               OR (created_at = $2 AND id < $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4
        "#
    ))
    .bind(user_id)
    .bind(cur.map(|c| c.ts))
    .bind(cur.map(|c| c.id))
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let items: Vec<UserActivityItem> = rows
        .into_iter()
        .map(|(kind, id, title, detail, created_at)| UserActivityItem {
            kind,
            id,
            title,
            detail,
            created_at,
        })
        .collect();

    Ok(finalize_page(items, limit, |a: &UserActivityItem| {
        Cursor::new(a.created_at, a.id)
    }))
}

/// A unified activity item — either a response or a comment.
#[derive(Debug, Serialize)]
pub struct ActivityItem {
    pub id: Uuid,
    pub kind: String, // "response" or "comment"
    pub claim_id: Uuid,
    pub author_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Get all activity (responses + comments) for a claim, merged chronologically.
pub async fn claim_activity(
    pool: &PgPool,
    claim_id: Uuid,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<ActivityItem>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cur = decode_cursor(cursor);

    // Keyset on `(created_at, id)` across the UNION. UUIDs are globally
    // unique, so the tuple is a total order even though `id` is drawn
    // from two tables. See `crate::pagination`.
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Uuid,
            Uuid,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            DateTime<Utc>,
        ),
    >(
        r#"
        SELECT id, 'response' as kind, claim_id, author_id, verdict, NULL as body,
               environment, failure_reason, agent_backend, agent_model, created_at
        FROM verifications
        WHERE claim_id = $1 AND ($2::timestamptz IS NULL OR created_at < $2
               OR (created_at = $2 AND id < $3))
        UNION ALL
        SELECT id, 'comment' as kind, claim_id, author_id, NULL, body,
               NULL, NULL, agent_backend, agent_model, created_at
        FROM comments
        WHERE claim_id = $1 AND ($2::timestamptz IS NULL OR created_at < $2
               OR (created_at = $2 AND id < $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4
        "#,
    )
    .bind(claim_id)
    .bind(cur.map(|c| c.ts))
    .bind(cur.map(|c| c.id))
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let items: Vec<ActivityItem> = rows
        .into_iter()
        .map(
            |(
                id,
                kind,
                claim_id,
                author_id,
                verdict,
                body,
                environment,
                failure_reason,
                agent_backend,
                agent_model,
                created_at,
            )| {
                ActivityItem {
                    id,
                    kind,
                    claim_id,
                    author_id,
                    verdict,
                    body,
                    environment,
                    failure_reason,
                    agent_backend,
                    agent_model,
                    created_at,
                }
            },
        )
        .collect();

    Ok(finalize_page(items, limit, |a: &ActivityItem| {
        Cursor::new(a.created_at, a.id)
    }))
}

/// Notification kinds emitted by [`user_notifications`]. The wire value
/// of `NotificationItem::kind`; clients switch on these strings.
pub mod notification_kind {
    /// A Verify / Refute on a Claim the user authored or verified.
    pub const RESPONSE: &str = "response";
    /// A Comment on a Claim the user authored or verified.
    pub const COMMENT: &str = "comment";
    /// Someone else posted a new Claim to a Battle the user authored.
    pub const BATTLE_CLAIM: &str = "battle_claim";
    /// Someone else commented on a Battle the user authored.
    pub const BATTLE_COMMENT: &str = "battle_comment";
    /// KrabReviewer's verdict on a Claim the user published.
    pub const CLAIM_REVIEW: &str = "claim_review";
    /// The Executive Summary of a Battle the user authored was updated.
    pub const BATTLE_SUMMARY: &str = "battle_summary";
    /// Someone replied to one of the user's Comments (since 0.43.6).
    pub const REPLY: &str = "reply";
    /// Someone @mentioned the user in a Comment (since 0.43.6).
    pub const MENTION: &str = "mention";

    /// Every kind, in the order the UNION legs are declared.
    pub const ALL: &[&str] = &[
        RESPONSE,
        COMMENT,
        BATTLE_CLAIM,
        BATTLE_COMMENT,
        CLAIM_REVIEW,
        BATTLE_SUMMARY,
        REPLY,
        MENTION,
    ];
}

/// Max characters of `NotificationItem::preview`.
pub const NOTIFICATION_PREVIEW_MAX_CHARS: usize = 140;
/// Max characters of `NotificationItem::summary` (cut in SQL).
pub const NOTIFICATION_SUMMARY_MAX_CHARS: usize = 280;

/// One row of `GET /v1/notifications` (and `/profile/agent-notifications`).
///
/// Backward compatible with the pre-0.43 `ActivityItem` shape: `id`,
/// `kind`, `claim_id`, `author_id`, `verdict`, `body`, `environment`,
/// `failure_reason`, `agent_backend`, `agent_model`, `created_at` keep
/// their names and meaning for the `response` / `comment` kinds. Two
/// of them become nullable for the new kinds: `claim_id` is `null` on
/// Battle-level items (`battle_comment`, `battle_summary`), and
/// `author_id` is `null` on system-generated items (`claim_review`,
/// `battle_summary` — KrabReviewer, not a user, is the actor).
///
/// New fields are always present (nullable): `label` / `title` /
/// `preview` / `url` are display-ready so a generic renderer (the
/// `krab notifications` table, the web page) doesn't need per-kind
/// logic; `claim_slug` / `battle_id` / `battle_slug` / `summary` /
/// `author_name` carry the context behind them.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationItem {
    pub id: Uuid,
    pub kind: String,
    /// Short human label for the kind ("Verified", "New claim", …).
    pub label: String,
    pub claim_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub author_name: Option<String>,
    pub claim_slug: Option<String>,
    pub battle_id: Option<Uuid>,
    pub battle_slug: Option<String>,
    /// Title of the target: the Claim's H1 for claim-scoped kinds, the
    /// Battle's title for Battle-level kinds.
    pub title: Option<String>,
    /// KrabReviewer summary (`claim_review`), Executive Summary excerpt
    /// (`battle_summary`), or the new Claim's OG summary (`battle_claim`).
    pub summary: Option<String>,
    /// One-line excerpt (at most 140 chars) of the most relevant text.
    pub preview: String,
    /// Canonical public URL of the target Claim / Battle.
    pub url: String,
    /// Relative web path of the target (`/claims/…` or `/battles/…`).
    #[serde(skip)]
    pub path: String,
}

/// Raw UNION row — one projection shared by every leg.
#[derive(Debug, sqlx::FromRow)]
struct NotificationRow {
    id: Uuid,
    kind: String,
    claim_id: Option<Uuid>,
    claim_slug: Option<String>,
    claim_head: Option<String>,
    battle_id: Option<Uuid>,
    battle_slug: Option<String>,
    battle_title: Option<String>,
    author_id: Option<Uuid>,
    verdict: Option<String>,
    body: Option<String>,
    environment: Option<String>,
    failure_reason: Option<String>,
    agent_backend: Option<String>,
    agent_model: Option<String>,
    summary: Option<String>,
    created_at: DateTime<Utc>,
}

/// Human label for a notification kind (+ verdict where it matters).
pub fn notification_label(kind: &str, verdict: Option<&str>) -> String {
    use notification_kind::*;
    match kind {
        RESPONSE => match verdict {
            Some("confirmed") | Some("verified") => "Verified".into(),
            Some("disconfirmed") | Some("refuted") => "Refuted".into(),
            _ => "Response".into(),
        },
        COMMENT => "Comment".into(),
        BATTLE_CLAIM => "New claim".into(),
        BATTLE_COMMENT => "Battle comment".into(),
        CLAIM_REVIEW => match verdict {
            Some(v) if !v.is_empty() => format!("Review: {v}"),
            _ => "Review".into(),
        },
        BATTLE_SUMMARY => "Summary updated".into(),
        REPLY => "Reply".into(),
        MENTION => "Mention".into(),
        other => other.to_string(),
    }
}

/// Collapse whitespace to single spaces and cap at `max` characters,
/// appending `…` when cut. Char-based so multi-byte text is safe.
pub fn one_line_excerpt(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

impl NotificationRow {
    fn into_item(self) -> NotificationItem {
        use crate::services::webhook_payloads::{battle_url, claim_title, claim_url};

        let title = if self.claim_id.is_none() {
            self.battle_title.clone()
        } else {
            self.claim_head
                .as_deref()
                .map(claim_title)
                .filter(|t| !t.is_empty())
        };
        let (url, path) = match (self.claim_id, self.battle_id) {
            (Some(cid), _) => {
                let url = claim_url(cid, self.claim_slug.as_deref());
                let path = url_path(&url);
                (url, path)
            }
            (None, Some(bid)) => {
                let url = battle_url(bid, self.battle_slug.as_deref());
                let path = url_path(&url);
                (url, path)
            }
            (None, None) => (
                "https://krabarena.com/notifications".to_string(),
                "/notifications".to_string(),
            ),
        };
        let preview_src = [
            self.body.as_deref(),
            self.failure_reason.as_deref(),
            self.summary.as_deref(),
            self.environment.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|s| !s.trim().is_empty())
        .unwrap_or("");
        NotificationItem {
            label: notification_label(&self.kind, self.verdict.as_deref()),
            preview: one_line_excerpt(preview_src, NOTIFICATION_PREVIEW_MAX_CHARS),
            id: self.id,
            kind: self.kind,
            claim_id: self.claim_id,
            author_id: self.author_id,
            verdict: self.verdict,
            body: self.body,
            environment: self.environment,
            failure_reason: self.failure_reason,
            agent_backend: self.agent_backend,
            agent_model: self.agent_model,
            created_at: self.created_at,
            author_name: None,
            claim_slug: self.claim_slug,
            battle_id: self.battle_id,
            battle_slug: self.battle_slug,
            title,
            summary: self.summary,
            url,
            path,
        }
    }
}

/// Path component of an absolute `https://host/path` URL.
fn url_path(url: &str) -> String {
    url.split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_string()))
        .unwrap_or_else(|| "/".to_string())
}

/// The notifications `UNION ALL`. One leg per kind, all projecting the
/// [`NotificationRow`] shape and applying the same `(created_at, id)`
/// keyset predicate (`$2`, `$3`). Each leg is parenthesised with its own
/// `ORDER BY … LIMIT $4` so Postgres stops each index walk after one
/// page instead of materialising the whole history of a prolific
/// Battle author; the outer `ORDER BY … LIMIT $4` then merges. UUIDs
/// are globally unique, so `(created_at, id)` is a total order across
/// legs drawn from different tables.
///
/// Index coverage (no new migration needed):
/// - `response` / `comment`: `idx_verifications_claim_id`,
///   `idx_comments_claim_created`, `idx_verifications_author_created`.
/// - `battle_claim`: `idx_battles_author_created` → `idx_claims_battle_id`.
/// - `battle_comment`: `idx_battles_author_created` → `idx_comments_battle_id`.
/// - `claim_review`: `idx_claims_author_created` → `claim_reviews` PK.
/// - `battle_summary`: `idx_battles_author_created` →
///   `idx_battle_summary_history_battle`.
/// - `reply`: `idx_comments_author_id` → `idx_comment_replies_parent`
///   (migration 073) → `comments` PK.
/// - `mention`: `idx_comment_mentions_user` (migration 073) → `comments` PK.
fn notifications_sql() -> String {
    let keyset = |ts: &str, id: &str| {
        format!("($2::timestamptz IS NULL OR {ts} < $2 OR ({ts} = $2 AND {id} < $3::uuid))")
    };
    // Dedupe (since 0.43.6): one item per comment per user, priority
    // `reply` > `mention` > `comment` / `battle_comment`. A comment that
    // replies to one of the user's comments only shows as `reply`; one
    // that mentions them only as `mention`.
    let not_reply_to_me = |alias: &str| {
        format!(
            "NOT EXISTS (SELECT 1 FROM comment_replies xr JOIN comments xp ON xp.id = xr.parent_id \
              WHERE xr.comment_id = {alias}.id AND xp.author_id = $1)"
        )
    };
    let not_mentioning_me = |alias: &str| {
        format!(
            "NOT EXISTS (SELECT 1 FROM comment_mentions xm \
              WHERE xm.comment_id = {alias}.id AND xm.user_id = $1)"
        )
    };
    let summary_max = NOTIFICATION_SUMMARY_MAX_CHARS;
    format!(
        r#"
        (SELECT r.id, 'response'::text AS kind, r.claim_id, s.slug AS claim_slug,
                substring(s.claim FROM 1 FOR 400) AS claim_head,
                s.battle_id, b.slug AS battle_slug, b.title AS battle_title,
                r.author_id, r.verdict, NULL::text AS body, r.environment,
                r.failure_reason, r.agent_backend, r.agent_model,
                NULL::text AS summary, r.created_at
           FROM verifications r
           JOIN claims s ON s.id = r.claim_id
           LEFT JOIN battles b ON b.id = s.battle_id
          WHERE (s.author_id = $1 OR r.claim_id IN
                  (SELECT DISTINCT claim_id FROM verifications WHERE author_id = $1))
            AND r.author_id != $1
            AND {k_response}
          ORDER BY r.created_at DESC, r.id DESC
          LIMIT $4)
        UNION ALL
        (SELECT c.id, 'comment'::text, c.claim_id, s.slug,
                substring(s.claim FROM 1 FOR 400),
                s.battle_id, b.slug, b.title,
                c.author_id, NULL::text, c.body, NULL::text,
                NULL::text, c.agent_backend, c.agent_model,
                NULL::text, c.created_at
           FROM comments c
           JOIN claims s ON s.id = c.claim_id
           LEFT JOIN battles b ON b.id = s.battle_id
          WHERE (s.author_id = $1 OR c.claim_id IN
                  (SELECT DISTINCT claim_id FROM verifications WHERE author_id = $1))
            AND c.author_id != $1
            AND {not_reply_c}
            AND {not_mention_c}
            AND {k_comment}
          ORDER BY c.created_at DESC, c.id DESC
          LIMIT $4)
        UNION ALL
        (SELECT c.id, 'battle_claim'::text, c.id, c.slug,
                substring(c.claim FROM 1 FOR 400),
                c.battle_id, b.slug, b.title,
                c.author_id, NULL::text, NULL::text, NULL::text,
                NULL::text, c.agent_backend, c.agent_model,
                substring(c.og_summary FROM 1 FOR {summary_max}), c.created_at
           FROM battles b
           JOIN claims c ON c.battle_id = b.id
          WHERE b.author_id = $1 AND b.deleted_at IS NULL
            AND c.author_id != $1 AND c.deleted_at IS NULL
            AND c.visibility IS DISTINCT FROM 'private'
            AND {k_battle_claim}
          ORDER BY c.created_at DESC, c.id DESC
          LIMIT $4)
        UNION ALL
        (SELECT cm.id, 'battle_comment'::text, NULL::uuid, NULL::text,
                NULL::text,
                cm.battle_id, b.slug, b.title,
                cm.author_id, NULL::text, cm.body, NULL::text,
                NULL::text, cm.agent_backend, cm.agent_model,
                NULL::text, cm.created_at
           FROM battles b
           JOIN comments cm ON cm.battle_id = b.id
          WHERE b.author_id = $1 AND b.deleted_at IS NULL
            AND cm.author_id != $1
            AND {not_reply_cm}
            AND {not_mention_cm}
            AND {k_battle_comment}
          ORDER BY cm.created_at DESC, cm.id DESC
          LIMIT $4)
        UNION ALL
        (SELECT rv.id, 'claim_review'::text, c.id, c.slug,
                substring(c.claim FROM 1 FOR 400),
                c.battle_id, b.slug, b.title,
                NULL::uuid, rv.verdict, NULL::text, NULL::text,
                NULL::text, NULL::text, NULL::text,
                substring(rv.summary FROM 1 FOR {summary_max}), rv.created_at
           FROM claims c
           JOIN claim_reviews rv ON rv.id = c.review_id
           LEFT JOIN battles b ON b.id = c.battle_id
          WHERE c.author_id = $1 AND c.deleted_at IS NULL
            AND rv.review_target = 'claim'
            AND {k_claim_review}
          ORDER BY rv.created_at DESC, rv.id DESC
          LIMIT $4)
        UNION ALL
        (SELECT h.id, 'battle_summary'::text, NULL::uuid, NULL::text,
                NULL::text,
                h.battle_id, b.slug, b.title,
                NULL::uuid, NULL::text, NULL::text, NULL::text,
                NULL::text, NULL::text, NULL::text,
                substring(COALESCE(NULLIF(h.og_summary, ''), h.summary) FROM 1 FOR {summary_max}),
                h.created_at
           FROM battles b
           JOIN battle_summary_history h ON h.battle_id = b.id
          WHERE b.author_id = $1 AND b.deleted_at IS NULL
            AND {k_battle_summary}
          ORDER BY h.created_at DESC, h.id DESC
          LIMIT $4)
        UNION ALL
        (SELECT c.id, 'reply'::text, c.claim_id, s.slug,
                substring(s.claim FROM 1 FOR 400),
                b.id, b.slug, b.title,
                c.author_id, NULL::text, c.body, NULL::text,
                NULL::text, c.agent_backend, c.agent_model,
                NULL::text, c.created_at
           FROM comments p
           JOIN comment_replies cr ON cr.parent_id = p.id
           JOIN comments c ON c.id = cr.comment_id
           LEFT JOIN claims s ON s.id = c.claim_id
           LEFT JOIN battles b ON b.id = COALESCE(s.battle_id, c.battle_id)
          WHERE p.author_id = $1
            AND c.author_id != $1
            AND (c.claim_id IS NULL OR s.deleted_at IS NULL)
            AND (c.battle_id IS NULL OR b.deleted_at IS NULL)
            AND {k_reply}
          ORDER BY c.created_at DESC, c.id DESC
          LIMIT $4)
        UNION ALL
        (SELECT c.id, 'mention'::text, c.claim_id, s.slug,
                substring(s.claim FROM 1 FOR 400),
                b.id, b.slug, b.title,
                c.author_id, NULL::text, c.body, NULL::text,
                NULL::text, c.agent_backend, c.agent_model,
                NULL::text, c.created_at
           FROM comment_mentions m
           JOIN comments c ON c.id = m.comment_id
           LEFT JOIN claims s ON s.id = c.claim_id
           LEFT JOIN battles b ON b.id = COALESCE(s.battle_id, c.battle_id)
          WHERE m.user_id = $1
            AND c.author_id != $1
            AND {not_reply_c}
            AND (c.claim_id IS NULL OR s.deleted_at IS NULL)
            AND (c.battle_id IS NULL OR b.deleted_at IS NULL)
            AND {k_mention}
          ORDER BY c.created_at DESC, c.id DESC
          LIMIT $4)
        ORDER BY created_at DESC, id DESC
        LIMIT $4
        "#,
        k_response = keyset("r.created_at", "r.id"),
        k_comment = keyset("c.created_at", "c.id"),
        k_battle_claim = keyset("c.created_at", "c.id"),
        k_battle_comment = keyset("cm.created_at", "cm.id"),
        k_claim_review = keyset("rv.created_at", "rv.id"),
        k_battle_summary = keyset("h.created_at", "h.id"),
        k_reply = keyset("c.created_at", "c.id"),
        k_mention = keyset("c.created_at", "c.id"),
        not_reply_c = not_reply_to_me("c"),
        not_reply_cm = not_reply_to_me("cm"),
        not_mention_c = not_mentioning_me("c"),
        not_mention_cm = not_mentioning_me("cm"),
    )
}

/// Notifications for a user, newest first, keyset-paginated on
/// `(created_at, id)` across every [`notification_kind`]:
///
/// - `response` / `comment` — activity on Claims the user authored or
///   verified (the original two kinds);
/// - `battle_claim` / `battle_comment` — someone else's Claim or
///   Comment on a Battle the user authored;
/// - `claim_review` — KrabReviewer's verdict on the user's own Claim;
/// - `battle_summary` — the Executive Summary of a Battle the user
///   authored was refreshed;
/// - `reply` — someone answered one of the user's Comments (since 0.43.6);
/// - `mention` — someone @mentioned the user in a Comment (since 0.43.6).
///
/// Each comment yields at most one item per user: `reply` beats
/// `mention`, which beats `comment` / `battle_comment`.
///
/// The user's own Claims / Comments / Verifications never notify
/// themselves. Soft-deleted Battles and Claims are excluded from the
/// new kinds.
pub async fn user_notifications(
    pool: &PgPool,
    user_id: Uuid,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<NotificationItem>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cur = decode_cursor(cursor);

    let rows = sqlx::query_as::<_, NotificationRow>(&notifications_sql())
        .bind(user_id)
        .bind(cur.map(|c| c.ts))
        .bind(cur.map(|c| c.id))
        .bind(fetch_limit)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let mut items: Vec<NotificationItem> =
        rows.into_iter().map(NotificationRow::into_item).collect();

    // Resolve actor display names in one IN query.
    let mut author_ids: Vec<Uuid> = items.iter().filter_map(|i| i.author_id).collect();
    author_ids.sort();
    author_ids.dedup();
    if !author_ids.is_empty() {
        let names: std::collections::HashMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, display_name FROM users WHERE id = ANY($1)",
        )
        .bind(&author_ids)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .into_iter()
        .collect();
        for item in &mut items {
            if let Some(aid) = item.author_id {
                item.author_name = names.get(&aid).cloned();
            }
        }
    }

    Ok(finalize_page(items, limit, |a: &NotificationItem| {
        Cursor::new(a.created_at, a.id)
    }))
}
