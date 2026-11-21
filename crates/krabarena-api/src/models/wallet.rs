use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A user's single crypto wallet (`UNIQUE user_id`). Currency + network
/// are stored as data so more networks can be added without a schema
/// change — see `services::crypto_networks`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WalletRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub address: String,
    pub currency: String,
    pub network: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
