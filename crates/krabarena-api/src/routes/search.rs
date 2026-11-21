//! `GET /v1/search` — hybrid semantic + lexical search across battles,
//! claims, and users.
//!
//! In AI mode (`ai=1`, the default) the query is embedded via Gemini
//! `gemini-embedding-001` (768-dim) and the cosine signal is mixed with
//! the pg_trgm lexical signal. With `ai=0` only the lexical (trigram)
//! signal runs — no embedding call. If the embedding fails in AI mode
//! the request degrades to lexical-only rather than erroring, and the
//! `degraded` flag in the response says so.
//!
//! Query embeddings are cached by `(provider, q)` in the embedding
//! service, so repeated queries don't re-hit the upstream API.

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::{Json, http::StatusCode};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::error::ApiError;
use crate::services::embedding_service::{self, global};
use crate::services::search_service::{self, DEFAULT_LIMIT, SearchKind};

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// The free-text query. Required; empty or missing returns 400.
    pub q: Option<String>,
    /// Optional kind filter — `battles` / `claims` / `users` / `all`.
    /// Defaults to `all` so the topnav box just works.
    pub kind: Option<String>,
    /// Optional 1..100 result cap. Defaults to [`DEFAULT_LIMIT`].
    pub limit: Option<i64>,
    /// AI (semantic) toggle. `1`/`true`/`on` (default) → semantic +
    /// lexical; `0`/`false`/`off` → lexical-only, no embedding call.
    pub ai: Option<String>,
}

/// Parse the `ai` toggle. Anything other than an explicit off-value is
/// treated as on, so the default (absent) is AI-enabled.
pub fn parse_ai_flag(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("0") | Some("false") | Some("off") | Some("no")
    )
}

pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let q = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::ValidationError("missing `q` query parameter".into()))?;
    let kind = SearchKind::parse(params.kind.as_deref());
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let want_ai = parse_ai_flag(params.ai.as_deref());

    let client = global();
    // In AI mode, embed the query — but only when a *live* provider is
    // configured. With the hash fallback (no GEMINI_API_KEY) the
    // "embedding" is noise, so using it would make AI-on diverge from
    // AI-off for no benefit; instead we run lexical-only, identical to
    // ai=0. On a live-provider failure we degrade to lexical-only too,
    // rather than 500-ing — a search box should always return something.
    let (embedding, degraded) = if want_ai && client.is_live() {
        match client.embed_query(q).await {
            Ok(e) => (Some(e), false),
            Err(e) => {
                tracing::warn!(error = %e, "search.embed_failed.degrading_to_lexical");
                (None, true)
            }
        }
    } else {
        (None, false)
    };

    let hits = search_service::search(
        &state.pool,
        &state.public_base_url,
        q,
        embedding.as_deref(),
        kind,
        limit,
    )
    .await?;

    let ai_used = embedding.is_some();
    Ok((
        StatusCode::OK,
        Json(json!({
            "data": hits,
            "provider": client.name(),
            "live": client.is_live() && ai_used,
            "ai": ai_used,
            "degraded": degraded,
            "kind": kind.as_str(),
            "query": q,
            "embedding_dim": embedding_service::EMBEDDING_DIM,
            "limit": limit,
        })),
    ))
}

/// Number of autocomplete suggestions returned by [`suggest`]. Small —
/// it's a typeahead dropdown, not a results page.
const SUGGEST_LIMIT: i64 = 8;

#[derive(Debug, Deserialize)]
pub struct SuggestQuery {
    pub q: Option<String>,
    pub kind: Option<String>,
    pub ai: Option<String>,
}

/// `GET /v1/search/suggest` — slim typeahead used by the search boxes.
///
/// Same retrieval as [`search`] but capped at [`SUGGEST_LIMIT`] and
/// projected to just `{ kind, title, url }` so the dropdown payload
/// stays tiny. A blank query returns an empty list (200, not 400) so
/// the client can fire on every keystroke without special-casing.
pub async fn suggest(
    State(state): State<AppState>,
    Query(params): Query<SuggestQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let q = params.q.as_deref().map(str::trim).unwrap_or("");
    if q.is_empty() {
        return Ok((StatusCode::OK, Json(json!({ "data": [] }))));
    }
    let kind = SearchKind::parse(params.kind.as_deref());
    let want_ai = parse_ai_flag(params.ai.as_deref());

    let client = global();
    let embedding = if want_ai && client.is_live() {
        match client.embed_query(q).await {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!(error = %e, "suggest.embed_failed.degrading_to_lexical");
                None
            }
        }
    } else {
        None
    };

    let hits = search_service::search(
        &state.pool,
        &state.public_base_url,
        q,
        embedding.as_deref(),
        kind,
        SUGGEST_LIMIT,
    )
    .await?;

    let data: Vec<_> = hits
        .into_iter()
        .map(|h| {
            json!({
                "kind": h.kind,
                "title": h.title,
                "url": h.url,
            })
        })
        .collect();
    Ok((StatusCode::OK, Json(json!({ "data": data }))))
}
