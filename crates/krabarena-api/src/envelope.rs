use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
    pub meta: ResponseMeta,
}

#[derive(Debug, Serialize)]
pub struct ApiErrorResponse {
    pub error: ApiErrorBody,
    pub meta: ResponseMeta,
}

#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ResponseMeta {
    pub request_id: String,
    pub timestamp: String,
}

#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub pagination: PaginationMeta,
    pub meta: ResponseMeta,
}

#[derive(Debug, Serialize)]
pub struct PaginationMeta {
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

impl ResponseMeta {
    pub fn new(request_id: String) -> Self {
        Self {
            request_id,
            timestamp: Utc::now().to_rfc3339(),
        }
    }

    pub fn generate() -> Self {
        Self::new(Uuid::new_v4().to_string())
    }
}

impl<T: Serialize> ApiResponse<T> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            meta: ResponseMeta::generate(),
        }
    }
}
