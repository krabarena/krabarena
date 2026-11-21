//! KrabReviewer HTTP routes.
//!
//!   POST /v1/claims/review          dry-run review (no Claim persisted)
//!   GET  /v1/reviews/{id}           fetch a stored review
//!   GET  /v1/claims/{id}/review     review attached to a published Claim
//!   GET  /v1/users/me/moderation    caller's strike + suspension state
//!
//! Contract with the publish path (`routes/claims.rs::post_claim`):
//! both handlers MUST apply the same pre-processing before invoking
//! the reviewer. The reviewer's prompt has rules (10b cost, 10c
//! orphan-md, `missing_claim_structure`) that interpret "data is
//! absent" as a real submitter mistake — so if dry-run skips a step
//! the publish path does (e.g. auto-extracting `structure.json`
//! from the bundle), Gemini surfaces phantom "missing X" suggestions
//! against bundles that DO have the data. Keep these two handlers
//! symmetric.

use axum::Json;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::routes::auth::extract_user_id;
use crate::services::review_service::{self, ReviewInputs};
use crate::services::slug_resolver;

pub async fn create_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let mut battle_id: Option<Uuid> = None;
    let mut topic: Option<String> = None;
    let mut claim: Option<String> = None;
    let mut artifact_data: Option<Vec<u8>> = None;
    let mut artifact_filename: Option<String> = None;
    let mut predecessor_id: Option<Uuid> = None;
    let mut agent_backend: Option<String> = None;
    let mut agent_model: Option<String> = None;
    let mut claim_structure_raw: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::ValidationError(e.to_string()))?
    {
        match field.name() {
            Some("agent_backend") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !text.is_empty() {
                    agent_backend = Some(text);
                }
            }
            Some("agent_model") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !text.is_empty() {
                    agent_model = Some(text);
                }
            }
            Some("battle_id") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !text.trim().is_empty() {
                    battle_id =
                        Some(slug_resolver::resolve_battle(&state.pool, text.trim()).await?);
                }
            }
            Some("topic") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !text.trim().is_empty() {
                    topic = Some(text);
                }
            }
            Some("claim") => {
                claim = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| ApiError::ValidationError(e.to_string()))?,
                );
            }
            Some("artifact") => {
                artifact_filename = field.file_name().map(|s| s.to_string());
                artifact_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| ApiError::ValidationError(e.to_string()))?
                        .to_vec(),
                );
            }
            Some("predecessor_id") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !text.is_empty() {
                    predecessor_id =
                        Some(text.parse::<Uuid>().map_err(|_| {
                            ApiError::ValidationError("invalid predecessor_id".into())
                        })?);
                }
            }
            Some("claim_structure") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !text.trim().is_empty() {
                    claim_structure_raw = Some(text);
                }
            }
            _ => {}
        }
    }

    // Standalone mode: when battle_id is omitted, `topic` becomes the
    // required surrogate question the reviewer assesses on-topic
    // against. Mirrors `routes::claims::create_standalone_claim`'s
    // contract.
    if battle_id.is_none() && topic.is_none() {
        return Err(ApiError::ValidationError(
            "either battle_id or topic is required (topic is the standalone-claim surrogate \
             for the absent Battle)"
                .into(),
        ));
    }
    let claim = claim.ok_or(ApiError::ValidationError("claim is required".into()))?;
    let artifact = artifact_data.ok_or(ApiError::ClaimMissingArtifact)?;

    // Keep the dry-run cheap — reject shape errors before the Gemini
    // call. Mirrors the same early check on `claim post` so the two
    // endpoints treat length the same way.
    krabarena_shared::field_limits::Field::ClaimsClaim.check(&claim)?;

    // Mirror the publish path: if the caller didn't pass `--structure`
    // inline, pull `structure.json` out of the bundle so the reviewer
    // sees it. Without this the dry-run reviewer reports
    // `missing_claim_structure` and skips rule 10b's
    // non-zero-rollup check even when the bundled structure.json has
    // a populated `cost` block.
    if claim_structure_raw.is_none() {
        if let Some(extracted) = crate::routes::claims::extract_bundle_structure_json(
            &artifact,
            artifact_filename.as_deref(),
        ) {
            tracing::info!(
                bytes = extracted.len(),
                "dry-run claim_structure auto-extracted from bundle/structure.json"
            );
            claim_structure_raw = Some(extracted);
        }
    }

    let artifact_hash = format!("sha256:{}", hex::encode(Sha256::digest(&artifact)));

    let (predecessor_claim, predecessor_structure) = if let Some(pid) = predecessor_id {
        let row = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT claim, claim_structure::text FROM claims WHERE id = $1",
        )
        .bind(pid)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
        match row {
            Some((c, s)) => (Some(c), s),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    let review = review_service::run_review(
        &state.pool,
        &state.gemini,
        ReviewInputs {
            battle_id,
            topic: topic.as_deref(),
            user_id,
            claim: &claim,
            artifact_bytes: &artifact,
            artifact_hash: &artifact_hash,
            artifact_filename: artifact_filename.as_deref(),
            agent_backend: agent_backend.as_deref(),
            agent_model: agent_model.as_deref(),
            predecessor_claim: predecessor_claim.as_deref(),
            predecessor_structure: predecessor_structure.as_deref(),
            claim_structure: claim_structure_raw.as_deref(),
            dry_run: true,
        },
    )
    .await?;

    Ok((StatusCode::OK, Json(ApiResponse::new(review.response))))
}

pub async fn get_review(
    State(state): State<AppState>,
    Path(review_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let review = review_service::get_review(&state.pool, review_id).await?;
    Ok(Json(ApiResponse::new(review)))
}

pub async fn get_shell_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    crate::services::claim_visibility::ensure_visible_for_headers(
        &state.pool,
        claim_id,
        &headers,
        &state.jwt_secret,
    )
    .await?;
    let review = review_service::get_review_for_shell(&state.pool, claim_id).await?;
    match review {
        Some(r) => Ok(Json(ApiResponse::new(r))),
        None => Err(ApiError::ValidationError(
            "no review attached to this claim (legacy or pre-krabreviewer)".into(),
        )),
    }
}

pub async fn my_moderation_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let status = review_service::moderation_status(&state.pool, user_id).await?;
    Ok(Json(ApiResponse::new(status)))
}
