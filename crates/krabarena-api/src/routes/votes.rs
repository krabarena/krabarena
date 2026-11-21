use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::models::vote::CastVoteRequest;
use crate::services::{slug_resolver, vote_service, voter_service};

fn extract_voter_token(headers: &HeaderMap) -> Result<String, ApiError> {
    // Cookie session (new) is preferred; the legacy `x-voter-token`
    // header is the fallback during the migration window (and for
    // non-browser callers).
    if let Some(tok) = crate::web::cookies::get_cookie(headers, crate::web::cookies::SESSION) {
        return Ok(tok);
    }
    headers
        .get("x-voter-token")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .ok_or(ApiError::Unauthorized)
}

#[cfg(test)]
mod extract_token_tests {
    use super::extract_voter_token;
    use axum::http::HeaderMap;

    #[test]
    fn prefers_cookie_then_header() {
        let mut h = HeaderMap::new();
        h.insert("x-voter-token", "hdr".parse().unwrap());
        assert_eq!(extract_voter_token(&h).unwrap(), "hdr");
        h.insert("cookie", "ka_session=cook".parse().unwrap());
        assert_eq!(extract_voter_token(&h).unwrap(), "cook");
    }

    #[test]
    fn errors_when_neither_present() {
        assert!(extract_voter_token(&HeaderMap::new()).is_err());
    }
}

pub async fn cast_vote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Json(req): Json<CastVoteRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let token = extract_voter_token(&headers)?;
    let voter = voter_service::get_voter_by_session(&state.pool, &token)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let vote = vote_service::cast_vote(&state.pool, voter.id, claim_id, &req.vote_type).await?;
    Ok((StatusCode::OK, Json(ApiResponse::new(vote))))
}

pub async fn get_vote_counts(
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
    let (up, down) = vote_service::get_vote_counts(&state.pool, claim_id).await?;
    Ok(Json(ApiResponse::new(serde_json::json!({
        "claim_id": claim_id,
        "upvotes": up,
        "downvotes": down,
    }))))
}
