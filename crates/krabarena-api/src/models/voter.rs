use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct VoterRow {
    pub id: Uuid,
    pub provider: String,
    pub provider_id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Migration 068. `#[sqlx(default)]` so `SELECT v.* FROM voters`
    /// plans cached from before the column existed (and any row
    /// decoded against the pre-migration catalog) still decode —
    /// missing maps to the column default (`false`, i.e. still
    /// digest-eligible).
    #[sqlx(default)]
    pub digest_opt_out: bool,
}

#[derive(Debug, Serialize)]
pub struct VoterResponse {
    pub id: Uuid,
    pub display_name: String,
    pub provider: String,
    /// JWT minted on /auth/voter/me when the voter's email maps to a
    /// `users.role='admin'` row. Populated by the handler (which has
    /// DB + jwt_secret access); `From<VoterRow>` leaves it None. Lets
    /// the /login/callback bootstrap page promote the voter session
    /// into an admin UI session in the same fetch it already makes
    /// for the display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin_token: Option<String>,
}

impl From<VoterRow> for VoterResponse {
    fn from(row: VoterRow) -> Self {
        Self {
            id: row.id,
            display_name: row.display_name,
            provider: row.provider,
            admin_token: None,
        }
    }
}
