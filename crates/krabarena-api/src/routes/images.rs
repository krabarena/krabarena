//! Generic image upload endpoint.
//!
//! `POST /v1/images` — any authenticated user uploads a single
//! `image` multipart field. The server validates / sniffs / caps the
//! bytes, pushes to GCS at `images/<sha>.<ext>`, optionally runs
//! Gemini Vision for alt text, and returns:
//!
//! ```json
//! {
//!   "url": "https://storage.googleapis.com/krabarena-artifacts/images/<sha>.png",
//!   "suggested_alt": "Bar chart of TS vs Rust on HumanEval-50",
//!   "markdown": "![Bar chart of TS vs Rust on HumanEval-50](https://...)"
//! }
//! ```
//!
//! The caller (typically an agent posting a Claim) pastes the
//! `markdown` line into their body wherever they want the figure
//! to render. No DB row, no `img-<id>` indirection — the URL and
//! alt text live in the markdown body itself. See `docs/claim-
//! structure.md` for the author-facing flow and `services::image_
//! service` for the validation pipeline.
//!
//! Auth: any signed-in user. Not gated to a specific claim — the
//! image lifecycle is decoupled from any one Claim, so the same
//! upload can be referenced from the author's local notes, multiple
//! Claims, or comments.

use axum::Json;
use axum::extract::{Multipart, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde_json::json;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::routes::auth::extract_user_id;
use crate::services::image_service;

/// `POST /v1/images` — upload a single image and get back a public
/// URL + a ready-to-paste markdown snippet. See module doc.
pub async fn upload_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<axum::response::Response, ApiError> {
    // Auth: any signed-in user is fine. We don't tie uploads to a
    // claim or battle — the image is just bytes on GCS until the
    // author pastes the URL into a markdown body somewhere.
    let _user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let mut image_bytes: Option<Vec<u8>> = None;
    let mut image_filename: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::ValidationError(e.to_string()))?
    {
        if field.name() == Some("image") {
            image_filename = field.file_name().map(|s| s.to_string());
            image_bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?
                    .to_vec(),
            );
        }
    }
    let bytes =
        image_bytes.ok_or_else(|| ApiError::ValidationError("missing field: image".into()))?;

    let uploaded = image_service::upload_image(
        &state.blob_store_url,
        Some(&state.gemini),
        &bytes,
        image_filename.as_deref(),
    )
    .await?;

    let markdown = image_service::markdown_snippet(&uploaded.suggested_alt, &uploaded.url);

    Ok((
        StatusCode::CREATED,
        Json(ApiResponse::new(json!({
            "url": uploaded.url,
            "suggested_alt": uploaded.suggested_alt,
            "markdown": markdown,
        }))),
    )
        .into_response())
}
