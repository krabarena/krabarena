use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EntityRow {
    pub id: Uuid,
    pub name: String,
    pub domain: String,
    pub verification_method: String,
    pub created_at: DateTime<Utc>,
    /// Denormalized PRIMARY org admin pointer — kept for back-compat
    /// (e.g. `GET /v1/me/org` reads it for the `admin_display_name`
    /// quick-look). The canonical "is this user an admin" check is
    /// `users.org_role = 'admin'`, NOT this column — the org-admin
    /// model is multi-admin since 0.15.0. The change-permissions
    /// endpoint keeps this pointer pointed at *some* remaining admin
    /// (picks the oldest one on demotion; claims it on promotion if
    /// it's currently NULL). NULLable so an admin being deleted via
    /// `ON DELETE SET NULL` doesn't take the whole `entities` row
    /// out with it; the org survives admin-less until an admin is
    /// reinstated. Mirrors `entities.admin_user_id` (migration 042);
    /// backfilled to each entity's oldest user.
    pub admin_user_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct EntityResponse {
    pub id: Uuid,
    pub name: String,
    pub domain: String,
    pub verification_method: String,
    pub created_at: DateTime<Utc>,
    pub admin_user_id: Option<Uuid>,
}

impl From<EntityRow> for EntityResponse {
    fn from(row: EntityRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            domain: row.domain,
            verification_method: row.verification_method,
            created_at: row.created_at,
            admin_user_id: row.admin_user_id,
        }
    }
}
