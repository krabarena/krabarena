//! Instant notification emails (since 0.43.0).
//!
//! The notifications feed (`activity_service::user_notifications`) is
//! pull-only: agents see it via `krab notifications`, humans via
//! `/notifications`. This module adds a push channel — one short email
//! per notification-worthy event, sent to the recipient's
//! `users.email`.
//!
//! ## Hook
//!
//! Every trigger site calls [`emit`] right after its write commits:
//!
//! | Event                          | Call site                                         | Recipient      |
//! | ------------------------------ | ------------------------------------------------- | -------------- |
//! | `response` (verify / refute)   | `verification_service::create_verification`       | Claim author   |
//! | `comment` on a Claim           | `comment_service::create_comment`                 | Claim author   |
//! | `battle_claim`                 | `claim_service::create_claim` (Battle-attached)   | Battle author  |
//! | `battle_comment`               | `comment_service::create_battle_comment`          | Battle author  |
//! | `claim_review`                 | `claim_service::create_claim` (has `review_id`)   | Claim author   |
//! | `reply` (since 0.43.6)         | both comment create paths (has `parent_id`)       | Parent author  |
//! | `mention` (since 0.43.6)       | both comment create paths (`@handle` resolved)    | Mentioned user |
//!
//! A new comment produces at most one email per recipient:
//! [`comment_created_events`] prefers `reply` over `mention` over the
//! plain `comment` / `battle_comment`, mirroring the feed's dedupe.
//!
//! `battle_summary` stays feed/digest-only — it fires after every
//! Claim and would double the `battle_claim` mail.
//!
//! [`emit`] is fire-and-forget (spawns a task, never fails the caller),
//! mirroring `webhook_dispatch::dispatch_event`. The services keep a
//! `&PgPool`-only signature: the mailer + config are installed once at
//! startup via [`install`]. Nothing is installed in tests or when
//! Mailgun isn't configured, so [`emit`] is then a no-op.
//!
//! ## Who gets mail
//!
//! [`eligibility`] skips the recipient when they are the actor, have no
//! email, are a bot (`kind = 'bot'` or `auth_method = 'bot'`), are
//! suspended, have the digest set to `off`, or turned instant emails
//! off (`user_notification_prefs.instant_email = false`, migration
//! 070). An optional allowlist (`NOTIFICATION_EMAIL_ALLOWLIST`)
//! restricts delivery further.
//!
//! ## Throttle
//!
//! [`throttle_decision`]: at most one email per
//! `(user, kind, target)` per [`SAME_TARGET_WINDOW_MINUTES`] and at
//! most [`HOURLY_CAP`] per user per hour. Every send / throttle /
//! failure is recorded in `notification_email_log` (migration 070),
//! which is also the throttle's source of truth. The check-and-reserve
//! runs under a per-user transaction-scoped advisory lock so two
//! concurrent events can't both pass the check. Throttled events are
//! not lost — they stay in the feed, and every email points at
//! `krab notifications` for the full list.
//!
//! ## Dev / test safety
//!
//! Sending requires all of: Mailgun configured (`MAILGUN_API_KEY` +
//! `MAILGUN_DOMAIN`), and `NOTIFICATION_EMAILS_ENABLED=true`. Local dev
//! and the `krabarena-test` Cloud Run service set neither Mailgun nor
//! the flag (the test deploy pins it to `false`), and unit / DB tests
//! never call [`install`] — they drive [`deliver`] with a fake
//! [`NotificationMailer`].

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::activity_service::notification_kind;
use crate::services::email_layout::{self, EmailImage, EmailPage, Field, Tone};
use crate::services::webhook_payloads::{battle_url, claim_title};

/// Same-target throttle window: one email per (user, kind, target).
pub const SAME_TARGET_WINDOW_MINUTES: i64 = 10;
/// Per-user cap on instant notification emails per rolling hour.
pub const HOURLY_CAP: i64 = 10;
/// Max characters of the quoted excerpt in an email.
pub const EXCERPT_MAX_CHARS: usize = 200;
/// Max characters of a title inside the subject line.
const SUBJECT_TITLE_MAX_CHARS: usize = 80;
/// HMAC namespace for the one-click "stop instant emails" link
/// (`unsubscribe::sign_kind`).
pub const UNSUB_TOKEN_KIND: &str = "instant";

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// A notification-worthy write. Carries only ids — [`deliver`] loads the
/// rest after the triggering transaction committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationEvent {
    /// A Verify or Refute was recorded (`verifications.id`).
    Verification { verification_id: Uuid },
    /// A Comment on a Claim (`comments.id`).
    ClaimComment { comment_id: Uuid },
    /// A Claim was posted to a Battle (`claims.id`).
    BattleClaim { claim_id: Uuid },
    /// A Comment on a Battle (`comments.id`).
    BattleComment { comment_id: Uuid },
    /// KrabReviewer's verdict attached to a freshly posted Claim
    /// (`claims.id`; the review is `claims.review_id`).
    ClaimReview { claim_id: Uuid },
    /// A Comment replying to another Comment (`comments.id` of the
    /// reply; recipient = the parent comment's author). Since 0.43.6.
    CommentReply { comment_id: Uuid },
    /// A Comment that @mentions `user_id` (the recipient). Since 0.43.6.
    CommentMention { comment_id: Uuid, user_id: Uuid },
}

impl NotificationEvent {
    /// Feed kind this event corresponds to (`notification_kind::*`).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Verification { .. } => notification_kind::RESPONSE,
            Self::ClaimComment { .. } => notification_kind::COMMENT,
            Self::BattleClaim { .. } => notification_kind::BATTLE_CLAIM,
            Self::BattleComment { .. } => notification_kind::BATTLE_COMMENT,
            Self::ClaimReview { .. } => notification_kind::CLAIM_REVIEW,
            Self::CommentReply { .. } => notification_kind::REPLY,
            Self::CommentMention { .. } => notification_kind::MENTION,
        }
    }

    /// Id of the row that triggered the event (logged for audit).
    pub fn event_id(&self) -> Uuid {
        match *self {
            Self::Verification { verification_id } => verification_id,
            Self::ClaimComment { comment_id }
            | Self::BattleComment { comment_id }
            | Self::CommentReply { comment_id }
            | Self::CommentMention { comment_id, .. } => comment_id,
            Self::BattleClaim { claim_id } | Self::ClaimReview { claim_id } => claim_id,
        }
    }
}

/// Events a newly inserted Claim triggers: `battle_claim` when it is
/// attached to a Battle and not private, `claim_review` when a
/// KrabReviewer review is linked. Pure so the trigger rule is testable.
pub fn claim_created_events(
    claim_id: Uuid,
    battle_id: Option<Uuid>,
    visibility: Option<&str>,
    review_id: Option<Uuid>,
) -> Vec<NotificationEvent> {
    let mut out = Vec::new();
    if battle_id.is_some() && visibility != Some("private") {
        out.push(NotificationEvent::BattleClaim { claim_id });
    }
    if review_id.is_some() {
        out.push(NotificationEvent::ClaimReview { claim_id });
    }
    out
}

/// Whether a new comment sits on a Claim or a Battle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentTargetKind {
    Claim,
    Battle,
}

/// Events a newly inserted Comment triggers — at most one per
/// recipient, by priority:
///
/// 1. `reply` → the parent comment's author;
/// 2. `mention` → each mentioned user not already covered;
/// 3. `comment` / `battle_comment` → the Claim / Battle author, unless
///    they already got a `reply` or `mention` for this comment.
///
/// The actor never gets a reply / mention event (the plain target
/// event is still emitted and dropped by [`eligibility`] as before).
/// Pure so the routing rule is testable.
pub fn comment_created_events(
    comment_id: Uuid,
    actor_id: Uuid,
    target: CommentTargetKind,
    target_author: Option<Uuid>,
    parent_author: Option<Uuid>,
    mentioned: &[Uuid],
) -> Vec<NotificationEvent> {
    let mut out = Vec::new();
    let mut covered: Vec<Uuid> = vec![actor_id];
    if let Some(p) = parent_author
        && !covered.contains(&p)
    {
        out.push(NotificationEvent::CommentReply { comment_id });
        covered.push(p);
    }
    for &user_id in mentioned {
        if !covered.contains(&user_id) {
            out.push(NotificationEvent::CommentMention {
                comment_id,
                user_id,
            });
            covered.push(user_id);
        }
    }
    let target_covered = target_author.is_some_and(|a| a != actor_id && covered.contains(&a));
    if !target_covered {
        out.push(match target {
            CommentTargetKind::Claim => NotificationEvent::ClaimComment { comment_id },
            CommentTargetKind::Battle => NotificationEvent::BattleComment { comment_id },
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Runtime (mailer + config), installed once at startup
// ---------------------------------------------------------------------------

/// One outgoing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingEmail {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
    pub unsubscribe_url: String,
}

/// Transport seam. Production uses [`EmailService`](crate::services::email_service::EmailService)
/// (Mailgun); tests use an in-memory fake.
pub trait NotificationMailer: Send + Sync {
    fn send<'a>(
        &'a self,
        email: &'a OutgoingEmail,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

impl NotificationMailer for crate::services::email_service::EmailService {
    fn send<'a>(
        &'a self,
        email: &'a OutgoingEmail,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.send_html_with_unsubscribe(
                &email.to,
                &email.subject,
                &email.text,
                &email.html,
                &email.unsubscribe_url,
            )
            .await
            .map_err(|e| e.to_string())
        })
    }
}

/// Env-driven switches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationEmailConfig {
    /// `NOTIFICATION_EMAILS_ENABLED=true` (exact). Default off.
    pub enabled: bool,
    /// `NOTIFICATION_EMAIL_ALLOWLIST` — comma-separated addresses or
    /// `@domain` suffixes. Empty = everyone eligible.
    pub allowlist: Vec<String>,
}

impl NotificationEmailConfig {
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let enabled = get("NOTIFICATION_EMAILS_ENABLED").as_deref() == Some("true");
        let allowlist = get("NOTIFICATION_EMAIL_ALLOWLIST")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        Self { enabled, allowlist }
    }

    /// True when `email` passes the allowlist (always, if it's empty).
    pub fn allows(&self, email: &str) -> bool {
        if self.allowlist.is_empty() {
            return true;
        }
        let email = email.trim().to_ascii_lowercase();
        self.allowlist.iter().any(|entry| {
            if entry.starts_with('@') {
                email.ends_with(entry.as_str())
            } else {
                email == *entry
            }
        })
    }
}

/// Everything [`deliver`] needs besides the pool.
#[derive(Clone)]
pub struct NotifierRuntime {
    pub mailer: Arc<dyn NotificationMailer>,
    pub config: NotificationEmailConfig,
    /// Absolute site origin for links (`EmailService::public_base_url`).
    pub public_base_url: String,
    /// HMAC key for unsubscribe / preferences links (`jwt_secret`).
    pub link_secret: String,
}

static RUNTIME: OnceLock<NotifierRuntime> = OnceLock::new();

/// Install the runtime at startup. Returns `false` (and installs
/// nothing) when the config is disabled, so [`emit`] stays a no-op.
pub fn install(runtime: NotifierRuntime) -> bool {
    if !runtime.config.enabled {
        return false;
    }
    RUNTIME.set(runtime).is_ok()
}

#[cfg(test)]
static RECORDED: std::sync::Mutex<Vec<NotificationEvent>> = std::sync::Mutex::new(Vec::new());

/// Test hook: events [`emit`] was called with, in order. Lets service
/// tests assert "this path enqueued an email" without a mailer.
#[cfg(test)]
pub fn recorded_events() -> Vec<NotificationEvent> {
    RECORDED.lock().unwrap().clone()
}

/// Fire-and-forget entry point for trigger sites. No-op unless
/// [`install`] succeeded. Errors are logged, never returned.
pub fn emit(pool: &PgPool, event: NotificationEvent) {
    #[cfg(test)]
    RECORDED.lock().unwrap().push(event);

    let Some(rt) = RUNTIME.get() else {
        return;
    };
    let pool = pool.clone();
    let rt = rt.clone();
    tokio::spawn(async move {
        match deliver(&pool, &rt, event).await {
            Ok(outcome) => tracing::debug!(?event, ?outcome, "notification_email.done"),
            Err(e) => tracing::warn!(?event, error = %e, "notification_email.failed"),
        }
    });
}

// ---------------------------------------------------------------------------
// Context + recipient loading
// ---------------------------------------------------------------------------

/// What happened, resolved from the DB.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct NotificationContext {
    pub recipient_id: Uuid,
    pub actor_id: Option<Uuid>,
    pub actor_name: Option<String>,
    pub verdict: Option<String>,
    pub excerpt: Option<String>,
    pub claim_id: Option<Uuid>,
    pub claim_slug: Option<String>,
    pub claim_head: Option<String>,
    pub battle_id: Option<Uuid>,
    pub battle_slug: Option<String>,
    pub battle_title: Option<String>,
    /// First chart on the claim: a moderated `claim_media` image, or
    /// the first `https` image in the claim markdown. Filled by
    /// [`attach_images`] at send time. `None` means "use the claim
    /// preview card" — the column is not part of the context query.
    #[sqlx(default)]
    pub claim_image_url: Option<String>,
    /// Editor-uploaded battle cover. Used when the email has no claim.
    #[sqlx(default)]
    pub battle_cover_url: Option<String>,
    #[sqlx(default)]
    pub battle_cover_alt: Option<String>,
}

const CTX_COLS: &str = "recipient_id, actor_id, actor_name, verdict, excerpt, \
     claim_id, claim_slug, claim_head, battle_id, battle_slug, battle_title";

fn context_sql(event: &NotificationEvent) -> String {
    let inner = match event {
        NotificationEvent::Verification { .. } => {
            "SELECT c.author_id AS recipient_id, v.author_id AS actor_id, u.display_name AS actor_name, \
                    v.verdict, COALESCE(v.failure_reason, v.environment) AS excerpt, \
                    c.id AS claim_id, c.slug AS claim_slug, substring(c.claim FROM 1 FOR 400) AS claim_head, \
                    b.id AS battle_id, b.slug AS battle_slug, b.title AS battle_title \
               FROM verifications v \
               JOIN claims c ON c.id = v.claim_id AND c.deleted_at IS NULL \
               LEFT JOIN battles b ON b.id = c.battle_id \
               LEFT JOIN users u ON u.id = v.author_id \
              WHERE v.id = $1"
        }
        NotificationEvent::ClaimComment { .. } => {
            "SELECT c.author_id, cm.author_id, u.display_name, NULL::text, cm.body, \
                    c.id, c.slug, substring(c.claim FROM 1 FOR 400), b.id, b.slug, b.title \
               FROM comments cm \
               JOIN claims c ON c.id = cm.claim_id AND c.deleted_at IS NULL \
               LEFT JOIN battles b ON b.id = c.battle_id \
               LEFT JOIN users u ON u.id = cm.author_id \
              WHERE cm.id = $1"
        }
        NotificationEvent::BattleClaim { .. } => {
            "SELECT b.author_id, c.author_id, u.display_name, NULL::text, c.og_summary, \
                    c.id, c.slug, substring(c.claim FROM 1 FOR 400), b.id, b.slug, b.title \
               FROM claims c \
               JOIN battles b ON b.id = c.battle_id AND b.deleted_at IS NULL \
               LEFT JOIN users u ON u.id = c.author_id \
              WHERE c.id = $1 AND c.deleted_at IS NULL \
                AND c.visibility IS DISTINCT FROM 'private'"
        }
        NotificationEvent::BattleComment { .. } => {
            "SELECT b.author_id, cm.author_id, u.display_name, NULL::text, cm.body, \
                    NULL::uuid, NULL::text, NULL::text, b.id, b.slug, b.title \
               FROM comments cm \
               JOIN battles b ON b.id = cm.battle_id AND b.deleted_at IS NULL \
               LEFT JOIN users u ON u.id = cm.author_id \
              WHERE cm.id = $1"
        }
        NotificationEvent::ClaimReview { .. } => {
            "SELECT c.author_id, NULL::uuid, NULL::text, rv.verdict, rv.summary, \
                    c.id, c.slug, substring(c.claim FROM 1 FOR 400), b.id, b.slug, b.title \
               FROM claims c \
               JOIN claim_reviews rv ON rv.id = c.review_id \
               LEFT JOIN battles b ON b.id = c.battle_id \
              WHERE c.id = $1 AND c.deleted_at IS NULL"
        }
        // Reply / mention: the comment may sit on a Claim (claim_* set,
        // battle from the Claim) or on a Battle (claim_* NULL).
        NotificationEvent::CommentReply { .. } => {
            "SELECT p.author_id, cm.author_id, u.display_name, NULL::text, cm.body, \
                    c.id, c.slug, substring(c.claim FROM 1 FOR 400), b.id, b.slug, b.title \
               FROM comments cm \
               JOIN comment_replies r ON r.comment_id = cm.id \
               JOIN comments p ON p.id = r.parent_id \
               LEFT JOIN claims c ON c.id = cm.claim_id \
               LEFT JOIN battles b ON b.id = COALESCE(c.battle_id, cm.battle_id) \
               LEFT JOIN users u ON u.id = cm.author_id \
              WHERE cm.id = $1 \
                AND (cm.claim_id IS NULL OR c.deleted_at IS NULL) \
                AND (cm.battle_id IS NULL OR b.deleted_at IS NULL)"
        }
        NotificationEvent::CommentMention { .. } => {
            "SELECT m.user_id, cm.author_id, u.display_name, NULL::text, cm.body, \
                    c.id, c.slug, substring(c.claim FROM 1 FOR 400), b.id, b.slug, b.title \
               FROM comments cm \
               JOIN comment_mentions m ON m.comment_id = cm.id AND m.user_id = $2 \
               LEFT JOIN claims c ON c.id = cm.claim_id \
               LEFT JOIN battles b ON b.id = COALESCE(c.battle_id, cm.battle_id) \
               LEFT JOIN users u ON u.id = cm.author_id \
              WHERE cm.id = $1 \
                AND (cm.claim_id IS NULL OR c.deleted_at IS NULL) \
                AND (cm.battle_id IS NULL OR b.deleted_at IS NULL)"
        }
    };
    format!(
        "SELECT {CTX_COLS} FROM ({inner}) AS ctx(recipient_id, actor_id, actor_name, verdict, excerpt, claim_id, claim_slug, claim_head, battle_id, battle_slug, battle_title)"
    )
}

async fn load_context(
    pool: &PgPool,
    event: &NotificationEvent,
) -> Result<Option<NotificationContext>, sqlx::Error> {
    let sql = context_sql(event);
    let q = sqlx::query_as::<_, NotificationContext>(&sql).bind(event.event_id());
    // The mention query is the only one with a second parameter (the
    // mentioned user) — binding it elsewhere would be a protocol error.
    let q = match event {
        NotificationEvent::CommentMention { user_id, .. } => q.bind(*user_id),
        _ => q,
    };
    q.fetch_optional(pool).await
}

/// Recipient row + preferences.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RecipientInfo {
    pub user_id: Uuid,
    pub email: Option<String>,
    pub kind: Option<String>,
    pub auth_method: Option<String>,
    pub suspended_until: Option<DateTime<Utc>>,
    pub digest_frequency: Option<String>,
    pub instant_email: Option<bool>,
}

async fn load_recipient(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<RecipientInfo>, sqlx::Error> {
    sqlx::query_as::<_, RecipientInfo>(
        "SELECT u.id AS user_id, u.email, u.kind, u.auth_method, u.suspended_until, \
                p.digest_frequency, n.instant_email \
           FROM users u \
           LEFT JOIN user_email_preferences p ON p.user_id = u.id \
           LEFT JOIN user_notification_prefs n ON n.user_id = u.id \
          WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// Why a recipient gets no email.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The recipient performed the action themselves.
    Actor,
    NoEmail,
    Bot,
    Suspended,
    /// `user_email_preferences.digest_frequency = 'off'`.
    DigestOff,
    /// `user_notification_prefs.instant_email = false`.
    InstantOff,
    /// Address not in `NOTIFICATION_EMAIL_ALLOWLIST`.
    NotAllowlisted,
}

/// Recipient filter. Pure — the policy in one place.
pub fn eligibility(
    r: &RecipientInfo,
    actor_id: Option<Uuid>,
    now: DateTime<Utc>,
    config: &NotificationEmailConfig,
) -> Result<String, SkipReason> {
    if actor_id == Some(r.user_id) {
        return Err(SkipReason::Actor);
    }
    let email = r
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| e.contains('@'))
        .ok_or(SkipReason::NoEmail)?;
    if r.kind.as_deref() == Some("bot") || r.auth_method.as_deref() == Some("bot") {
        return Err(SkipReason::Bot);
    }
    if r.suspended_until.is_some_and(|t| t > now) {
        return Err(SkipReason::Suspended);
    }
    if r.digest_frequency.as_deref() == Some("off") {
        return Err(SkipReason::DigestOff);
    }
    if r.instant_email == Some(false) {
        return Err(SkipReason::InstantOff);
    }
    if !config.allows(email) {
        return Err(SkipReason::NotAllowlisted);
    }
    Ok(email.to_string())
}

/// Why an otherwise-eligible email was held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleReason {
    /// Already emailed about this (kind, target) in the last 10 minutes.
    SameTarget,
    /// Already sent `HOURLY_CAP` emails to this user in the last hour.
    HourlyCap,
}

impl ThrottleReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::SameTarget => "same_target",
            Self::HourlyCap => "hourly_cap",
        }
    }
}

/// Throttle rule. `same_target_recent` = sent rows for this
/// `(user, kind, target)` in the last [`SAME_TARGET_WINDOW_MINUTES`];
/// `sent_last_hour` = sent rows for this user in the last hour.
pub fn throttle_decision(same_target_recent: i64, sent_last_hour: i64) -> Option<ThrottleReason> {
    if same_target_recent > 0 {
        Some(ThrottleReason::SameTarget)
    } else if sent_last_hour >= HOURLY_CAP {
        Some(ThrottleReason::HourlyCap)
    } else {
        None
    }
}

/// Throttle key target: the Battle for Battle-scoped kinds (a burst of
/// new Claims on one Battle collapses to one email), the Claim otherwise.
pub fn throttle_target(kind: &str, ctx: &NotificationContext) -> Option<Uuid> {
    match kind {
        notification_kind::BATTLE_CLAIM | notification_kind::BATTLE_COMMENT => ctx.battle_id,
        _ => ctx.claim_id.or(ctx.battle_id),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Rendered subject + bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedEmail {
    pub subject: String,
    pub text: String,
    pub html: String,
}

fn truncate(s: &str, max: usize) -> String {
    crate::services::activity_service::one_line_excerpt(s, max)
}

/// Absolute web link for the event's target (Claim when there is one,
/// Battle otherwise) on `base`.
pub fn target_link(ctx: &NotificationContext, base: &str) -> String {
    let base = base.trim_end_matches('/');
    match (ctx.claim_id, ctx.battle_id) {
        (Some(id), _) => {
            let ident = ctx
                .claim_slug
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.to_string());
            format!("{base}/claims/{ident}")
        }
        (None, Some(id)) => {
            // Reuse the canonical builder, then rebase onto `base`.
            let canonical = battle_url(id, ctx.battle_slug.as_deref());
            let path = canonical
                .split_once("/battles/")
                .map(|(_, p)| p)
                .unwrap_or_default();
            format!("{base}/battles/{path}")
        }
        (None, None) => format!("{base}/notifications"),
    }
}

/// Battle page for the event, when the event is tied to a Battle.
pub fn battle_link(ctx: &NotificationContext, base: &str) -> Option<String> {
    let id = ctx.battle_id?;
    let base = base.trim_end_matches('/');
    let canonical = battle_url(id, ctx.battle_slug.as_deref());
    let path = canonical
        .split_once("/battles/")
        .map(|(_, p)| p)
        .unwrap_or_default();
    Some(format!("{base}/battles/{path}"))
}

/// Display width of a hero inside the 600px card (28px padding each
/// side, and a 2px frame around the picture).
const HERO_WIDTH: u32 = 540;
/// 1200×630 Open Graph card at [`HERO_WIDTH`]. Claim charts don't
/// store pixel size; the same box is the Outlook fallback, and clients
/// that honor `height:auto` use the file's real ratio.
const OG_HERO_HEIGHT: u32 = 284;

/// Picture for the card. A claim chart (CDN or markdown) wins, then
/// the claim's social card (`/og/claims/{slug}`), then the battle
/// cover, then the battle's social card. Sign-in mail has no context
/// and does not come through here.
pub fn hero_image(ctx: &NotificationContext, base_url: &str) -> Option<EmailImage> {
    let claim = ctx
        .claim_head
        .as_deref()
        .map(claim_title)
        .filter(|title| !title.is_empty())
        .unwrap_or_default();
    let battle = ctx
        .battle_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("");

    if ctx.claim_id.is_some()
        && let Some(src) = ctx
            .claim_image_url
            .as_deref()
            .filter(|url| email_layout::is_public_image_url(url))
    {
        return Some(EmailImage {
            src: src.trim().to_string(),
            alt: figure_alt("Chart for", &claim, "Chart from this claim"),
            width: HERO_WIDTH,
            height: OG_HERO_HEIGHT,
        });
    }
    if ctx.claim_id.is_some()
        && let Some(src) = og_url(base_url, "claims", ctx.claim_id, ctx.claim_slug.as_deref())
    {
        return Some(EmailImage {
            src,
            alt: figure_alt("Preview of", &claim, "KrabArena claim preview"),
            width: HERO_WIDTH,
            height: OG_HERO_HEIGHT,
        });
    }
    if let Some(src) = ctx
        .battle_cover_url
        .as_deref()
        .filter(|url| email_layout::is_public_image_url(url))
    {
        let alt = ctx
            .battle_cover_alt
            .as_deref()
            .map(str::trim)
            .filter(|alt| !alt.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| figure_alt("Cover image for", battle, "Battle cover image"));
        return Some(EmailImage {
            src: src.trim().to_string(),
            alt,
            width: HERO_WIDTH,
            height: OG_HERO_HEIGHT,
        });
    }
    if ctx.battle_id.is_some()
        && let Some(src) = og_url(
            base_url,
            "battles",
            ctx.battle_id,
            ctx.battle_slug.as_deref(),
        )
    {
        return Some(EmailImage {
            src,
            alt: figure_alt("Preview of", battle, "KrabArena battle preview"),
            width: HERO_WIDTH,
            height: OG_HERO_HEIGHT,
        });
    }
    None
}

fn figure_alt(prefix: &str, title: &str, fallback: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        fallback.to_string()
    } else {
        format!("{prefix} {}", truncate(title, 120))
    }
}

fn og_url(base: &str, kind: &str, id: Option<Uuid>, slug: Option<&str>) -> Option<String> {
    let base = base.trim().trim_end_matches('/');
    if !email_layout::is_public_image_url(&format!("{base}/")) {
        return None;
    }
    let ident = slug
        .map(str::trim)
        .filter(|segment| is_url_segment(segment))
        .map(str::to_string)
        .or_else(|| id.map(|id| id.to_string()))?;
    Some(format!("{base}/og/{kind}/{ident}"))
}

fn is_url_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// First `![alt](https://...)` in a claim body. Bundle-relative paths
/// are rewritten to CDN URLs before this runs; anything that is not
/// public https is skipped.
pub fn first_https_markdown_image(markdown: &str) -> Option<String> {
    let mut rest = markdown;
    while let Some(start) = rest.find("![") {
        rest = &rest[start + 2..];
        let alt_end = rest.find(']')?;
        let after_alt = &rest[alt_end..];
        if !after_alt.starts_with("](") {
            rest = &rest[alt_end + 1..];
            continue;
        }
        let mut url = after_alt[2..].trim_start();
        let end = url
            .find(|c: char| c == ')' || c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(url.len());
        url = url[..end].trim();
        if email_layout::is_public_image_url(url) {
            return Some(url.to_string());
        }
        rest = &after_alt[2..];
    }
    None
}

/// Load the claim chart or the battle cover onto `ctx`. A lookup
/// failure must not block the email — the preview-card URL still works
/// from the ids already on the context.
async fn attach_images(pool: &PgPool, ctx: &mut NotificationContext) -> Result<(), sqlx::Error> {
    if let Some(claim_id) = ctx.claim_id {
        let media: Option<String> = sqlx::query_scalar(
            "SELECT public_url FROM claim_media \
              WHERE claim_id = $1 AND kind = 'image' \
                AND public_url LIKE 'https://%' \
              ORDER BY created_at ASC \
              LIMIT 1",
        )
        .bind(claim_id)
        .fetch_optional(pool)
        .await?;
        if let Some(url) = media.filter(|url| email_layout::is_public_image_url(url)) {
            ctx.claim_image_url = Some(url);
            return Ok(());
        }
        let body: Option<String> = sqlx::query_scalar(
            "SELECT substring(claim FROM 1 FOR 20000) FROM claims \
              WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(claim_id)
        .fetch_optional(pool)
        .await?;
        if let Some(url) = body.as_deref().and_then(first_https_markdown_image) {
            ctx.claim_image_url = Some(url);
        }
        return Ok(());
    }
    if let Some(battle_id) = ctx.battle_id {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT cover_image_url, cover_image_alt FROM battles \
              WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(battle_id)
        .fetch_optional(pool)
        .await?;
        if let Some((url, alt)) = row
            && url
                .as_deref()
                .is_some_and(email_layout::is_public_image_url)
        {
            ctx.battle_cover_url = url;
            ctx.battle_cover_alt = alt.filter(|text| !text.trim().is_empty());
        }
    }
    Ok(())
}

/// Build the email. All user-supplied text is truncated and, in the
/// HTML part, escaped. No secrets, only a short excerpt of comments.
/// Layout lives in `email_layout` so every notification kind shares
/// the same branded shell. Subject lines are unchanged.
pub fn render_email(
    kind: &str,
    ctx: &NotificationContext,
    base_url: &str,
    unsubscribe_url: &str,
    prefs_url: &str,
) -> RenderedEmail {
    let actor = ctx
        .actor_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Someone".to_string());
    let claim_title_s = ctx
        .claim_head
        .as_deref()
        .map(claim_title)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "your claim".to_string());
    let battle_title_s = ctx
        .battle_title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "your battle".to_string());
    let ct = truncate(&claim_title_s, SUBJECT_TITLE_MAX_CHARS);
    let bt = truncate(&battle_title_s, SUBJECT_TITLE_MAX_CHARS);

    let (subject, headline) = match kind {
        notification_kind::RESPONSE => {
            let verb = match ctx.verdict.as_deref() {
                Some("disconfirmed") => "refuted",
                Some("confirmed") => "verified",
                _ => "responded to",
            };
            (
                format!("{actor} {verb} your claim \u{201c}{ct}\u{201d}"),
                format!("{actor} {verb} your claim \u{201c}{ct}\u{201d}."),
            )
        }
        notification_kind::COMMENT => (
            format!("{actor} commented on your claim \u{201c}{ct}\u{201d}"),
            format!("{actor} commented on your claim \u{201c}{ct}\u{201d}."),
        ),
        notification_kind::BATTLE_CLAIM => (
            format!("New claim on your battle \u{201c}{bt}\u{201d}"),
            format!(
                "{actor} posted a new claim \u{201c}{ct}\u{201d} to your battle \u{201c}{bt}\u{201d}."
            ),
        ),
        notification_kind::BATTLE_COMMENT => (
            format!("{actor} commented on your battle \u{201c}{bt}\u{201d}"),
            format!("{actor} commented on your battle \u{201c}{bt}\u{201d}."),
        ),
        notification_kind::REPLY => {
            let on = comment_target_phrase(ctx, &ct, &bt);
            (
                format!("{actor} replied to your comment on {on}"),
                format!("{actor} replied to your comment on {on}."),
            )
        }
        notification_kind::MENTION => {
            let on = comment_target_phrase(ctx, &ct, &bt);
            (
                format!("{actor} mentioned you on {on}"),
                format!("{actor} mentioned you in a comment on {on}."),
            )
        }
        notification_kind::CLAIM_REVIEW => {
            let verdict = ctx.verdict.clone().unwrap_or_else(|| "reviewed".into());
            (
                format!("KrabReviewer: {verdict} on your claim \u{201c}{ct}\u{201d}"),
                format!("KrabReviewer's verdict on your claim \u{201c}{ct}\u{201d}: {verdict}."),
            )
        }
        other => (
            format!("KrabArena notification: {other}"),
            format!("New activity on KrabArena ({other})."),
        ),
    };
    let subject = format!("[KrabArena] {subject}");
    let excerpt = ctx
        .excerpt
        .as_deref()
        .map(|e| truncate(e, EXCERPT_MAX_CHARS))
        .filter(|e| !e.is_empty());
    let claim_href = ctx.claim_id.map(|_| target_link(ctx, base_url));
    let battle_href = battle_link(ctx, base_url);
    let (kicker, tone) = kicker_for(kind, ctx.verdict.as_deref());

    let mut fields = Vec::new();
    if let Some(title) = ctx
        .battle_title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        fields.push(Field::text("BATTLE", truncate(title, BODY_TITLE_MAX_CHARS)));
    }
    if let Some(title) = ctx
        .claim_head
        .as_deref()
        .map(claim_title)
        .filter(|t| !t.is_empty())
    {
        fields.push(Field::text("CLAIM", truncate(&title, BODY_TITLE_MAX_CHARS)));
    }
    let author = if kind == notification_kind::CLAIM_REVIEW {
        "KrabReviewer".to_string()
    } else {
        actor
    };
    fields.push(Field::text("AUTHOR", author));
    if let Some(excerpt) = excerpt {
        fields.push(Field::quote(excerpt_label(kind), excerpt));
    }

    let (primary_label, primary_href, secondary_label, secondary_href) =
        match (claim_href, battle_href) {
            (Some(claim), Some(battle)) => (
                Some("VIEW CLAIM".to_string()),
                Some(claim),
                Some("View the battle".to_string()),
                Some(battle),
            ),
            (Some(claim), None) => (Some("VIEW CLAIM".to_string()), Some(claim), None, None),
            (None, Some(battle)) => (Some("VIEW BATTLE".to_string()), Some(battle), None, None),
            (None, None) => (
                Some("VIEW NOTIFICATIONS".to_string()),
                Some(target_link(ctx, base_url)),
                None,
                None,
            ),
        };

    let rendered = email_layout::render(&EmailPage {
        preheader: headline.clone(),
        kicker: kicker.to_string(),
        tone,
        headline,
        fields,
        primary_label,
        primary_href,
        secondary_label,
        secondary_href,
        note: Some(
            "See all your notifications from the CLI with `krab notifications`.".to_string(),
        ),
        reason: reason_for(kind).to_string(),
        footer_links: vec![
            (
                "Stop instant notification emails".to_string(),
                unsubscribe_url.to_string(),
            ),
            ("Notification settings".to_string(), prefs_url.to_string()),
        ],
        logo_src: email_layout::logo_url(base_url),
        hero: hero_image(ctx, base_url),
    });

    RenderedEmail {
        subject,
        text: rendered.text,
        html: rendered.html,
    }
}

/// Card titles can be a little longer than the subject line.
const BODY_TITLE_MAX_CHARS: usize = 180;

fn kicker_for(kind: &str, verdict: Option<&str>) -> (&'static str, Tone) {
    match kind {
        notification_kind::RESPONSE => match verdict {
            Some("disconfirmed") => ("REFUTED", Tone::Red),
            Some("confirmed") => ("VERIFIED", Tone::Green),
            _ => ("RESPONSE", Tone::Cyan),
        },
        notification_kind::COMMENT | notification_kind::BATTLE_COMMENT => ("COMMENT", Tone::Cyan),
        notification_kind::BATTLE_CLAIM => ("NEW CLAIM", Tone::Green),
        notification_kind::REPLY => ("REPLY", Tone::Cyan),
        notification_kind::MENTION => ("MENTION", Tone::Gold),
        notification_kind::CLAIM_REVIEW => ("REVIEW", Tone::Orange),
        _ => ("NOTICE", Tone::Orange),
    }
}

fn excerpt_label(kind: &str) -> &'static str {
    match kind {
        notification_kind::BATTLE_CLAIM => "THESIS",
        notification_kind::RESPONSE => "NOTE",
        notification_kind::CLAIM_REVIEW => "REVIEW",
        notification_kind::COMMENT
        | notification_kind::BATTLE_COMMENT
        | notification_kind::REPLY
        | notification_kind::MENTION => "COMMENT",
        _ => "DETAILS",
    }
}

fn reason_for(kind: &str) -> &'static str {
    match kind {
        notification_kind::RESPONSE => {
            "You're receiving this because someone verified or refuted a claim you posted."
        }
        notification_kind::COMMENT => {
            "You're receiving this because someone commented on a claim you posted."
        }
        notification_kind::BATTLE_CLAIM => {
            "You're receiving this because a new claim was posted to a battle you created."
        }
        notification_kind::BATTLE_COMMENT => {
            "You're receiving this because someone commented on a battle you created."
        }
        notification_kind::REPLY => {
            "You're receiving this because someone replied to your comment."
        }
        notification_kind::MENTION => {
            "You're receiving this because someone mentioned you in a comment."
        }
        notification_kind::CLAIM_REVIEW => {
            "You're receiving this because KrabReviewer finished reviewing a claim you posted."
        }
        _ => "You're receiving this because of activity on KrabArena tied to your account.",
    }
}

/// "the claim “…”" / "the battle “…”" for reply / mention emails —
/// the comment sits on a Claim when `claim_id` is set, else a Battle.
fn comment_target_phrase(ctx: &NotificationContext, claim_t: &str, battle_t: &str) -> String {
    if ctx.claim_id.is_some() {
        format!("the claim \u{201c}{claim_t}\u{201d}")
    } else {
        format!("the battle \u{201c}{battle_t}\u{201d}")
    }
}

/// One-click "stop instant emails" link for `user_id`.
pub fn unsubscribe_link(base: &str, user_id: Uuid, secret: &str) -> String {
    let token = crate::web::unsubscribe::sign_kind(UNSUB_TOKEN_KIND, user_id, secret);
    format!(
        "{}/email/unsub/instant?u={user_id}&t={token}",
        base.trim_end_matches('/')
    )
}

/// Signed link to the preferences page (same token as the digest
/// email's "Manage preferences" link).
pub fn prefs_link(base: &str, user_id: Uuid, secret: &str) -> String {
    let token = crate::web::unsubscribe::sign(user_id, secret);
    format!(
        "{}/profile/email-preferences?u={user_id}&t={token}",
        base.trim_end_matches('/')
    )
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// Result of one [`deliver`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The triggering row is gone / not notification-worthy.
    NoContext,
    Skipped(SkipReason),
    Throttled(ThrottleReason),
    Sent {
        to: String,
    },
    Failed(String),
}

/// Resolve, filter, throttle, render, send, log. Driven by [`emit`] in
/// production and directly (with a fake mailer) in tests.
pub async fn deliver(
    pool: &PgPool,
    rt: &NotifierRuntime,
    event: NotificationEvent,
) -> Result<DeliveryOutcome, sqlx::Error> {
    let kind = event.kind();
    let Some(ctx) = load_context(pool, &event).await? else {
        return Ok(DeliveryOutcome::NoContext);
    };
    let Some(recipient) = load_recipient(pool, ctx.recipient_id).await? else {
        return Ok(DeliveryOutcome::NoContext);
    };
    let to = match eligibility(&recipient, ctx.actor_id, Utc::now(), &rt.config) {
        Ok(to) => to,
        Err(reason) => return Ok(DeliveryOutcome::Skipped(reason)),
    };
    let Some(target_id) = throttle_target(kind, &ctx) else {
        return Ok(DeliveryOutcome::NoContext);
    };

    // Check + reserve under a per-user advisory lock so concurrent
    // events for the same user serialise on the throttle check.
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(format!("notification_email:{}", recipient.user_id))
        .execute(&mut *tx)
        .await?;
    let (same_target, last_hour): (i64, i64) = sqlx::query_as(
        "SELECT \
           COUNT(*) FILTER (WHERE kind = $2 AND target_id = $3 \
                              AND created_at > now() - make_interval(mins => $4::int)), \
           COUNT(*) FILTER (WHERE created_at > now() - interval '1 hour') \
           FROM notification_email_log \
          WHERE user_id = $1 AND status = 'sent' \
            AND created_at > now() - interval '1 hour'",
    )
    .bind(recipient.user_id)
    .bind(kind)
    .bind(target_id)
    .bind(SAME_TARGET_WINDOW_MINUTES as i32)
    .fetch_one(&mut *tx)
    .await?;

    let throttled = throttle_decision(same_target, last_hour);
    let status = if throttled.is_some() {
        "throttled"
    } else {
        "sent"
    };
    let log_id: Uuid = sqlx::query_scalar(
        "INSERT INTO notification_email_log (user_id, kind, target_id, event_id, status, reason) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(recipient.user_id)
    .bind(kind)
    .bind(target_id)
    .bind(event.event_id())
    .bind(status)
    .bind(throttled.map(|t| t.as_str()))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    if let Some(reason) = throttled {
        return Ok(DeliveryOutcome::Throttled(reason));
    }

    let unsub = unsubscribe_link(&rt.public_base_url, recipient.user_id, &rt.link_secret);
    let prefs = prefs_link(&rt.public_base_url, recipient.user_id, &rt.link_secret);
    let mut ctx = ctx;
    if let Err(error) = attach_images(pool, &mut ctx).await {
        tracing::warn!(%error, "notification_email.image_lookup_failed");
    }
    let rendered = render_email(kind, &ctx, &rt.public_base_url, &unsub, &prefs);
    let email = OutgoingEmail {
        to: to.clone(),
        subject: rendered.subject,
        text: rendered.text,
        html: rendered.html,
        unsubscribe_url: unsub,
    };
    match rt.mailer.send(&email).await {
        Ok(()) => {
            tracing::info!(user_id = %recipient.user_id, kind, "notification_email.sent");
            Ok(DeliveryOutcome::Sent { to })
        }
        Err(e) => {
            let _ = sqlx::query(
                "UPDATE notification_email_log SET status = 'failed', reason = $2 WHERE id = $1",
            )
            .bind(log_id)
            .bind(truncate(&e, 200))
            .execute(pool)
            .await;
            Ok(DeliveryOutcome::Failed(e))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn recipient() -> RecipientInfo {
        RecipientInfo {
            user_id: Uuid::from_u128(1),
            email: Some("author@example.com".into()),
            kind: Some("human".into()),
            auth_method: Some("email".into()),
            suspended_until: None,
            digest_frequency: None,
            instant_email: None,
        }
    }

    fn cfg() -> NotificationEmailConfig {
        NotificationEmailConfig {
            enabled: true,
            allowlist: vec![],
        }
    }

    fn ctx() -> NotificationContext {
        NotificationContext {
            recipient_id: Uuid::from_u128(1),
            actor_id: Some(Uuid::from_u128(2)),
            actor_name: Some("Verifier <Bot>".into()),
            verdict: Some("confirmed".into()),
            excerpt: Some("ran it on linux, same numbers".into()),
            claim_id: Some(Uuid::from_u128(3)),
            claim_slug: Some("rust-beats-go".into()),
            claim_head: Some("# Rust beats Go\n\n## Summary".into()),
            battle_id: Some(Uuid::from_u128(4)),
            battle_slug: Some("fastest-http".into()),
            battle_title: Some("Fastest HTTP server".into()),
            claim_image_url: None,
            battle_cover_url: None,
            battle_cover_alt: None,
        }
    }

    // --- recipient selection -------------------------------------------

    #[test]
    fn eligible_default_recipient_gets_mail() {
        let now = Utc::now();
        assert_eq!(
            eligibility(&recipient(), Some(Uuid::from_u128(2)), now, &cfg()),
            Ok("author@example.com".to_string())
        );
        // No prefs rows at all = defaults (daily digest, instant on).
        assert!(eligibility(&recipient(), None, now, &cfg()).is_ok());
    }

    #[test]
    fn actor_is_excluded() {
        let r = recipient();
        assert_eq!(
            eligibility(&r, Some(r.user_id), Utc::now(), &cfg()),
            Err(SkipReason::Actor)
        );
    }

    #[test]
    fn bots_are_excluded_by_kind_or_auth_method() {
        let mut r = recipient();
        r.kind = Some("bot".into());
        assert_eq!(
            eligibility(&r, None, Utc::now(), &cfg()),
            Err(SkipReason::Bot)
        );
        let mut r = recipient();
        r.auth_method = Some("bot".into());
        assert_eq!(
            eligibility(&r, None, Utc::now(), &cfg()),
            Err(SkipReason::Bot)
        );
    }

    #[test]
    fn opt_outs_are_respected() {
        let mut r = recipient();
        r.digest_frequency = Some("off".into());
        assert_eq!(
            eligibility(&r, None, Utc::now(), &cfg()),
            Err(SkipReason::DigestOff)
        );

        let mut r = recipient();
        r.digest_frequency = Some("weekly".into());
        assert!(eligibility(&r, None, Utc::now(), &cfg()).is_ok());

        let mut r = recipient();
        r.instant_email = Some(false);
        assert_eq!(
            eligibility(&r, None, Utc::now(), &cfg()),
            Err(SkipReason::InstantOff)
        );
    }

    #[test]
    fn missing_email_and_suspension_skip() {
        let mut r = recipient();
        r.email = None;
        assert_eq!(
            eligibility(&r, None, Utc::now(), &cfg()),
            Err(SkipReason::NoEmail)
        );
        let mut r = recipient();
        r.email = Some("not-an-address".into());
        assert_eq!(
            eligibility(&r, None, Utc::now(), &cfg()),
            Err(SkipReason::NoEmail)
        );

        let now = Utc::now();
        let mut r = recipient();
        r.suspended_until = Some(now + chrono::Duration::hours(1));
        assert_eq!(
            eligibility(&r, None, now, &cfg()),
            Err(SkipReason::Suspended)
        );
        r.suspended_until = Some(now - chrono::Duration::hours(1));
        assert!(eligibility(&r, None, now, &cfg()).is_ok());
    }

    #[test]
    fn allowlist_restricts_recipients() {
        let c = NotificationEmailConfig::from_lookup(|k| match k {
            "NOTIFICATION_EMAILS_ENABLED" => Some("true".into()),
            "NOTIFICATION_EMAIL_ALLOWLIST" => Some(" qa@krabarena.com , @example.org ".into()),
            _ => None,
        });
        assert!(c.enabled);
        assert!(c.allows("QA@krabarena.com"));
        assert!(c.allows("alice@example.org"));
        assert!(!c.allows("someone@example.com"));
        assert_eq!(
            eligibility(&recipient(), None, Utc::now(), &c),
            Err(SkipReason::NotAllowlisted)
        );
    }

    #[test]
    fn config_is_off_unless_exactly_true() {
        for v in [None, Some("1"), Some("TRUE"), Some("yes")] {
            let c = NotificationEmailConfig::from_lookup(|k| {
                (k == "NOTIFICATION_EMAILS_ENABLED")
                    .then(|| v.map(str::to_string))
                    .flatten()
            });
            assert!(!c.enabled, "{v:?} must not enable sending");
        }
    }

    #[test]
    fn install_refuses_disabled_config() {
        struct Never;
        impl NotificationMailer for Never {
            fn send<'a>(
                &'a self,
                _: &'a OutgoingEmail,
            ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
                Box::pin(async { panic!("must never send") })
            }
        }
        assert!(!install(NotifierRuntime {
            mailer: Arc::new(Never),
            config: NotificationEmailConfig::default(),
            public_base_url: "http://localhost".into(),
            link_secret: "s".into(),
        }));
        assert!(RUNTIME.get().is_none());
    }

    // --- throttling -----------------------------------------------------

    #[test]
    fn throttle_rules() {
        assert_eq!(throttle_decision(0, 0), None);
        assert_eq!(throttle_decision(0, HOURLY_CAP - 1), None);
        assert_eq!(throttle_decision(1, 0), Some(ThrottleReason::SameTarget));
        assert_eq!(
            throttle_decision(0, HOURLY_CAP),
            Some(ThrottleReason::HourlyCap)
        );
        assert_eq!(
            throttle_decision(2, HOURLY_CAP + 5),
            Some(ThrottleReason::SameTarget)
        );
    }

    #[test]
    fn throttle_target_groups_battle_kinds_by_battle() {
        let c = ctx();
        assert_eq!(
            throttle_target(notification_kind::BATTLE_CLAIM, &c),
            c.battle_id
        );
        assert_eq!(
            throttle_target(notification_kind::BATTLE_COMMENT, &c),
            c.battle_id
        );
        assert_eq!(throttle_target(notification_kind::RESPONSE, &c), c.claim_id);
        assert_eq!(throttle_target(notification_kind::COMMENT, &c), c.claim_id);
        assert_eq!(
            throttle_target(notification_kind::CLAIM_REVIEW, &c),
            c.claim_id
        );
    }

    // --- triggers -------------------------------------------------------

    #[test]
    fn claim_created_trigger_rules() {
        let id = Uuid::from_u128(7);
        let b = Some(Uuid::from_u128(8));
        let r = Some(Uuid::from_u128(9));
        assert_eq!(
            claim_created_events(id, b, Some("public"), r),
            vec![
                NotificationEvent::BattleClaim { claim_id: id },
                NotificationEvent::ClaimReview { claim_id: id }
            ]
        );
        // Standalone claim: review only.
        assert_eq!(
            claim_created_events(id, None, Some("private"), r),
            vec![NotificationEvent::ClaimReview { claim_id: id }]
        );
        // Private claim on a battle never reaches the battle author.
        assert!(claim_created_events(id, b, Some("private"), None).is_empty());
    }

    #[test]
    fn event_kinds_map_to_feed_kinds() {
        let u = Uuid::from_u128(1);
        assert_eq!(
            NotificationEvent::Verification { verification_id: u }.kind(),
            "response"
        );
        assert_eq!(
            NotificationEvent::ClaimComment { comment_id: u }.kind(),
            "comment"
        );
        assert_eq!(
            NotificationEvent::BattleClaim { claim_id: u }.kind(),
            "battle_claim"
        );
        assert_eq!(
            NotificationEvent::BattleComment { comment_id: u }.kind(),
            "battle_comment"
        );
        assert_eq!(
            NotificationEvent::ClaimReview { claim_id: u }.kind(),
            "claim_review"
        );
        for k in [
            "response",
            "comment",
            "battle_claim",
            "battle_comment",
            "claim_review",
        ] {
            assert!(notification_kind::ALL.contains(&k));
        }
    }

    #[test]
    fn every_event_has_context_sql() {
        let u = Uuid::from_u128(1);
        for ev in [
            NotificationEvent::Verification { verification_id: u },
            NotificationEvent::ClaimComment { comment_id: u },
            NotificationEvent::BattleClaim { claim_id: u },
            NotificationEvent::BattleComment { comment_id: u },
            NotificationEvent::ClaimReview { claim_id: u },
            NotificationEvent::CommentReply { comment_id: u },
            NotificationEvent::CommentMention {
                comment_id: u,
                user_id: u,
            },
        ] {
            let sql = context_sql(&ev);
            assert!(sql.contains("$1"), "{ev:?}");
            assert!(sql.starts_with("SELECT recipient_id"), "{ev:?}");
            // Only the mention query takes the recipient as `$2`.
            assert_eq!(
                sql.contains("$2"),
                matches!(ev, NotificationEvent::CommentMention { .. }),
                "{ev:?}"
            );
        }
    }

    // --- comment routing (reply / mention / comment dedupe) -------------

    #[test]
    fn reply_and_mention_kinds() {
        let u = Uuid::from_u128(1);
        assert_eq!(
            NotificationEvent::CommentReply { comment_id: u }.kind(),
            "reply"
        );
        let m = NotificationEvent::CommentMention {
            comment_id: u,
            user_id: Uuid::from_u128(2),
        };
        assert_eq!(m.kind(), "mention");
        assert_eq!(m.event_id(), u);
        assert!(notification_kind::ALL.contains(&"reply"));
        assert!(notification_kind::ALL.contains(&"mention"));
    }

    #[test]
    fn comment_routing_one_event_per_recipient() {
        let c = Uuid::from_u128(100);
        let actor = Uuid::from_u128(1);
        let claim_author = Uuid::from_u128(2);
        let parent_author = Uuid::from_u128(3);
        let other = Uuid::from_u128(4);
        use CommentTargetKind::*;
        use NotificationEvent as E;

        // Plain comment: only the target author event (unchanged).
        assert_eq!(
            comment_created_events(c, actor, Claim, Some(claim_author), None, &[]),
            vec![E::ClaimComment { comment_id: c }]
        );
        assert_eq!(
            comment_created_events(c, actor, Battle, Some(claim_author), None, &[]),
            vec![E::BattleComment { comment_id: c }]
        );

        // Reply to a third party: reply + comment to the claim author.
        assert_eq!(
            comment_created_events(
                c,
                actor,
                Claim,
                Some(claim_author),
                Some(parent_author),
                &[]
            ),
            vec![
                E::CommentReply { comment_id: c },
                E::ClaimComment { comment_id: c }
            ]
        );

        // Reply to the claim author: reply only (no double `comment`).
        assert_eq!(
            comment_created_events(c, actor, Claim, Some(claim_author), Some(claim_author), &[]),
            vec![E::CommentReply { comment_id: c }]
        );

        // Reply that also mentions the parent author → one `reply`.
        assert_eq!(
            comment_created_events(
                c,
                actor,
                Claim,
                Some(claim_author),
                Some(parent_author),
                &[parent_author]
            ),
            vec![
                E::CommentReply { comment_id: c },
                E::ClaimComment { comment_id: c }
            ]
        );

        // Mentioning the claim author → `mention`, not `comment`.
        assert_eq!(
            comment_created_events(c, actor, Claim, Some(claim_author), None, &[claim_author]),
            vec![E::CommentMention {
                comment_id: c,
                user_id: claim_author
            }]
        );

        // Self-mention and self-reply never notify the actor.
        assert_eq!(
            comment_created_events(
                c,
                actor,
                Claim,
                Some(claim_author),
                Some(actor),
                &[actor, other]
            ),
            vec![
                E::CommentMention {
                    comment_id: c,
                    user_id: other
                },
                E::ClaimComment { comment_id: c }
            ]
        );

        // Actor is the claim author: the target event is still emitted
        // (and skipped as `Actor` at delivery, as before).
        assert_eq!(
            comment_created_events(c, actor, Claim, Some(actor), None, &[other]),
            vec![
                E::CommentMention {
                    comment_id: c,
                    user_id: other
                },
                E::ClaimComment { comment_id: c }
            ]
        );
    }

    #[test]
    fn reply_and_mention_subjects() {
        let mut c = ctx();
        c.actor_name = Some("Bob".into());
        let e = render_email("reply", &c, "https://x", "u", "p");
        assert_eq!(
            e.subject,
            "[KrabArena] Bob replied to your comment on the claim \u{201c}Rust beats Go\u{201d}"
        );
        assert!(e.text.contains("https://x/claims/rust-beats-go"));
        let e = render_email("mention", &c, "https://x", "u", "p");
        assert!(e.subject.contains("Bob mentioned you on the claim"));

        // Battle-level comment.
        c.claim_id = None;
        c.claim_slug = None;
        c.claim_head = None;
        let e = render_email("mention", &c, "https://x", "u", "p");
        assert!(
            e.subject
                .contains("mentioned you on the battle \u{201c}Fastest HTTP server\u{201d}")
        );
        assert!(e.text.contains("https://x/battles/fastest-http"));
    }

    // --- rendering ------------------------------------------------------

    #[test]
    fn verification_email_renders_headline_link_and_cli_hint() {
        let e = render_email(
            "response",
            &ctx(),
            "https://krabarena.com/",
            "https://krabarena.com/email/unsub/instant?u=x&t=y",
            "https://krabarena.com/profile/email-preferences?u=x&t=z",
        );
        assert_eq!(
            e.subject,
            "[KrabArena] Verifier <Bot> verified your claim \u{201c}Rust beats Go\u{201d}"
        );
        assert!(
            e.text
                .contains("https://krabarena.com/claims/rust-beats-go")
        );
        assert!(e.text.contains("krab notifications"));
        assert!(e.text.contains("ran it on linux"));
        assert!(e.text.contains("/email/unsub/instant?u=x&t=y"));
        // HTML escapes user-controlled text.
        assert!(e.html.contains("Verifier &lt;Bot&gt;"));
        assert!(!e.html.contains("<Bot>"));
        assert!(
            e.html
                .contains("href=\"https://krabarena.com/claims/rust-beats-go\"")
        );
        assert!(e.html.contains("Stop instant notification emails"));
    }

    #[test]
    fn refute_and_review_and_battle_subjects() {
        let mut c = ctx();
        c.verdict = Some("disconfirmed".into());
        assert!(
            render_email("response", &c, "https://x", "u", "p")
                .subject
                .contains("refuted")
        );

        let mut c = ctx();
        c.actor_id = None;
        c.actor_name = None;
        c.verdict = Some("warn".into());
        let e = render_email("claim_review", &c, "https://x", "u", "p");
        assert!(e.subject.contains("KrabReviewer: warn"));

        let e = render_email("battle_claim", &ctx(), "https://x", "u", "p");
        assert!(
            e.subject
                .contains("New claim on your battle \u{201c}Fastest HTTP server\u{201d}")
        );
        assert!(e.text.contains("https://x/claims/rust-beats-go"));

        let mut c = ctx();
        c.claim_id = None;
        c.claim_slug = None;
        c.claim_head = None;
        let e = render_email("battle_comment", &c, "https://x", "u", "p");
        assert!(e.subject.contains("commented on your battle"));
        assert!(e.text.contains("https://x/battles/fastest-http"));
    }

    #[test]
    fn excerpt_is_short() {
        let mut c = ctx();
        c.excerpt = Some("word ".repeat(500));
        let e = render_email("comment", &c, "https://x", "u", "p");
        let quoted = e
            .text
            .lines()
            .find(|l| l.trim_start().starts_with('"'))
            .unwrap();
        assert!(quoted.chars().count() <= EXCERPT_MAX_CHARS + 6);
    }

    #[test]
    fn links_are_signed_and_verify() {
        let uid = Uuid::from_u128(42);
        let l = unsubscribe_link("https://krabarena.com/", uid, "secret");
        let token = l.split("t=").nth(1).unwrap();
        assert!(crate::web::unsubscribe::verify_kind(
            UNSUB_TOKEN_KIND,
            uid,
            token,
            "secret"
        ));
        // Not interchangeable with the digest unsubscribe token.
        assert!(!crate::web::unsubscribe::verify(uid, token, "secret"));
        let p = prefs_link("https://krabarena.com", uid, "secret");
        let ptoken = p.split("t=").nth(1).unwrap();
        assert!(crate::web::unsubscribe::verify(uid, ptoken, "secret"));
    }

    #[test]
    fn every_kind_uses_the_shared_branded_shell() {
        for kind in [
            "response",
            "comment",
            "battle_claim",
            "battle_comment",
            "reply",
            "mention",
            "claim_review",
            "other",
        ] {
            let e = render_email(
                kind,
                &ctx(),
                "https://krabarena.com",
                "https://krabarena.com/email/unsub/instant?u=1&t=2",
                "https://krabarena.com/profile/email-preferences?u=1&t=3",
            );
            assert!(e.subject.starts_with("[KrabArena] "), "{kind}");
            assert!(e.html.contains("KRAB"), "{kind}");
            assert!(e.html.contains("ARENA"), "{kind}");
            assert!(e.html.contains("max-width:600px"), "{kind}");
            assert!(e.html.contains("prefers-color-scheme: dark"), "{kind}");
            assert!(
                e.html
                    .contains(r#"name="color-scheme" content="light dark""#),
                "{kind}"
            );
            assert!(!e.html.contains("linear-gradient"), "{kind}");
            email_layout::contrast::assert_readable(kind, &e.html, 4.5);
            assert!(!e.html.contains("mix-blend-mode"), "{kind}");
            assert!(e.html.contains("receiving this because"), "{kind}");
            assert!(
                e.html.contains("Stop instant notification emails"),
                "{kind}"
            );
            assert!(e.html.contains("Notification settings"), "{kind}");
            assert!(e.text.contains("krab notifications"), "{kind}");
            assert!(
                e.text.contains("Stop instant notification emails:"),
                "{kind}"
            );
        }
    }

    #[test]
    fn battle_claim_email_shows_battle_claim_author_thesis_and_links() {
        let e = render_email(
            "battle_claim",
            &ctx(),
            "https://x",
            "https://x/unsub",
            "https://x/prefs",
        );
        assert!(e.html.contains("NEW CLAIM"));
        assert!(e.html.contains("VIEW CLAIM"));
        assert!(e.html.contains("href=\"https://x/claims/rust-beats-go\""));
        assert!(e.html.contains("View the battle"));
        assert!(e.html.contains("https://x/battles/fastest-http"));
        assert!(e.html.contains("Fastest HTTP server"));
        assert!(e.html.contains("Rust beats Go"));
        assert!(e.html.contains("THESIS"));
        assert!(e.html.contains("ran it on linux, same numbers"));
        assert!(e.html.contains("Verifier &lt;Bot&gt;"));
        assert!(e.html.contains("battle you created"));
        assert!(
            e.text
                .contains("VIEW CLAIM: https://x/claims/rust-beats-go")
        );
        assert!(
            e.text
                .contains("View the battle: https://x/battles/fastest-http")
        );
        assert!(e.text.contains("BATTLE\nFastest HTTP server"));
        assert!(e.text.contains("AUTHOR\nVerifier <Bot>"));
    }

    #[test]
    fn verdict_kickers_and_battle_only_cta() {
        let mut c = ctx();
        c.verdict = Some("confirmed".into());
        assert!(
            render_email("response", &c, "https://x", "u", "p")
                .html
                .contains("VERIFIED")
        );
        c.verdict = Some("disconfirmed".into());
        let refuted = render_email("response", &c, "https://x", "u", "p");
        assert!(refuted.html.contains("REFUTED"));
        assert!(refuted.html.contains(">NOTE<") || refuted.html.contains("NOTE"));

        c.claim_id = None;
        c.claim_slug = None;
        c.claim_head = None;
        let battle_only = render_email("battle_comment", &c, "https://x", "u", "p");
        assert!(battle_only.html.contains("VIEW BATTLE"));
        assert!(!battle_only.html.contains("VIEW CLAIM"));
        assert!(battle_only.html.contains("https://x/battles/fastest-http"));
        assert!(!battle_only.text.contains("/claims/"));
    }

    #[test]
    fn review_email_names_krabreviewer() {
        let mut c = ctx();
        c.actor_name = None;
        c.verdict = Some("warn".into());
        let e = render_email("claim_review", &c, "https://x", "u", "p");
        assert!(e.html.contains("REVIEW"));
        assert!(e.html.contains("KrabReviewer"));
        assert!(!e.html.contains(">Someone<"));
        assert!(e.subject.contains("KrabReviewer: warn"));
    }

    #[test]
    fn claim_email_uses_the_chart_or_the_claim_preview() {
        let chart = "https://cdn.example.com/bundles/quality_vs_cost.png";
        let mut c = ctx();
        c.claim_image_url = Some(chart.into());
        let e = render_email("battle_claim", &c, "https://krabarena.com", "u", "p");
        assert!(e.html.contains(chart));
        assert!(e.html.contains(r#"width="540""#));
        assert!(e.html.contains(r#"height="284""#));
        assert!(e.html.contains("alt=\"Chart for Rust beats Go\""));
        assert!(e.html.contains("apple-touch-icon.png"));
        assert!(!e.html.contains("/og/claims/"));
        assert!(e.text.contains(&format!("Image: {chart}")));

        c.claim_image_url = Some("javascript:alert(1)".into());
        let e = render_email("response", &c, "https://krabarena.com", "u", "p");
        assert!(!e.html.contains("javascript:"));
        assert!(
            e.html
                .contains("https://krabarena.com/og/claims/rust-beats-go")
        );
        assert!(e.html.contains("alt=\"Preview of Rust beats Go\""));

        c.claim_image_url = None;
        let review = render_email("claim_review", &c, "https://krabarena.com", "u", "p");
        assert!(
            review
                .html
                .contains("https://krabarena.com/og/claims/rust-beats-go")
        );
        assert!(review.html.contains("class=\"hero\""));
    }

    #[test]
    fn battle_email_uses_the_cover_or_the_battle_preview() {
        let mut c = ctx();
        c.claim_id = None;
        c.claim_slug = None;
        c.claim_head = None;
        c.claim_image_url = Some("https://cdn.example.com/ignored.png".into());
        let cover = "https://cdn.example.com/covers/web-access.png";
        c.battle_cover_url = Some(cover.into());
        c.battle_cover_alt = Some("Agents racing across a search API".into());
        let e = render_email("battle_comment", &c, "https://krabarena.com", "u", "p");
        assert!(e.html.contains(cover));
        assert!(e.html.contains("alt=\"Agents racing across a search API\""));
        assert!(!e.html.contains("/og/"));
        assert!(!e.html.contains("ignored.png"));

        c.battle_cover_url = Some("http://insecure.example/cover.png".into());
        let e = render_email("battle_comment", &c, "https://krabarena.com", "u", "p");
        assert!(
            e.html
                .contains("https://krabarena.com/og/battles/fastest-http")
        );
        assert!(e.html.contains("alt=\"Preview of Fastest HTTP server\""));
        assert!(!e.html.contains("http://insecure"));
    }

    #[test]
    fn markdown_image_scanner_takes_the_first_https_picture() {
        let body = "# Title\n\n![diagram](charts/local.png)\n\n![cost](https://cdn.example.com/a.png \"title\")\n";
        assert_eq!(
            first_https_markdown_image(body).as_deref(),
            Some("https://cdn.example.com/a.png")
        );
        assert!(first_https_markdown_image("no pictures here").is_none());
        assert!(first_https_markdown_image("![x](data:image/png;base64,aa)").is_none());
    }
}
