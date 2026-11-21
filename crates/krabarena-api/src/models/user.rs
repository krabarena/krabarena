use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub entity_id: Uuid,
    pub display_name: String,
    pub email: Option<String>,
    pub auth_method: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
    /// `'human'` (default) or `'bot'` — bots are service accounts (e.g.
    /// KrabChecker) provisioned by an org admin via the bot-creation
    /// endpoint (PR B). Mirrors `users.kind` (migration 042); the
    /// CHECK constraint at the DB level guarantees the value is one
    /// of the two literals, so we keep it as `String` here rather
    /// than introducing an enum that has to round-trip via sqlx.
    pub kind: String,
    /// Org-level role: `'admin'` or `'member'` (default `'member'`).
    /// Canonical "is this user an admin of their org" signal —
    /// `entities.admin_user_id` is now a denormalized primary-admin
    /// pointer kept for back-compat only. Mirrors `users.org_role`
    /// (migration 042); the CHECK constraint guarantees the value
    /// is one of the two literals.
    pub org_role: String,
    /// Optional public URL for the user's profile photo (migration
    /// 046, 0.18.1). NULL = use the initials-fallback avatar.
    /// Populated by `POST /v1/users/me/avatar`; updates are
    /// idempotent — each upload replaces the prior URL. Bots leave
    /// this NULL until an admin opts them in to a curated logo.
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub entity_id: Uuid,
    pub display_name: String,
    pub email: Option<String>,
    pub auth_method: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub kind: String,
    pub org_role: String,
    /// See [`UserRow::avatar_url`]. Serialised as `avatar_url` so the
    /// CLI / web UI can render a circular `<img>` when set and fall
    /// back to initials otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

impl From<UserRow> for UserResponse {
    fn from(row: UserRow) -> Self {
        Self {
            id: row.id,
            entity_id: row.entity_id,
            display_name: row.display_name,
            email: row.email,
            auth_method: row.auth_method,
            role: row.role,
            created_at: row.created_at,
            kind: row.kind,
            org_role: row.org_role,
            avatar_url: row.avatar_url,
        }
    }
}
