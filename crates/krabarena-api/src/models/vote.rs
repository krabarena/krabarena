use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct VoteRow {
    pub id: Uuid,
    pub voter_id: Uuid,
    pub claim_id: Uuid,
    pub vote_type: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct VoteResponse {
    pub id: Uuid,
    pub claim_id: Uuid,
    pub vote_type: String,
}

#[derive(Debug, Deserialize)]
pub struct CastVoteRequest {
    pub vote_type: String,
}

impl From<VoteRow> for VoteResponse {
    fn from(row: VoteRow) -> Self {
        Self {
            id: row.id,
            claim_id: row.claim_id,
            vote_type: row.vote_type,
        }
    }
}
