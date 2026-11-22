use axum::Json;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::envelope::{ApiResponse, PaginatedResponse, ResponseMeta};
use crate::error::ApiError;
use crate::models::claim::ListClaimsQuery;
use crate::models::review::{ReviewResponse, ReviewVerdict};
use crate::routes::auth::extract_user_id;
use crate::services::review_service::{self, EnforcementState, ReviewInputs};
use crate::services::{
    artifact_service, battle_summarizer, claim_media, claim_service, claim_visibility, dedup_judge,
    slug_resolver,
};

pub async fn create_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    multipart: Multipart,
) -> Result<axum::response::Response, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    // Normal post: predecessor (if any) comes from the multipart body.
    post_battle_claim(&state, user_id, battle_id, multipart, None).await
}

/// `POST /v1/claims/{claim_id}/edit` — revise a Claim by posting a NEW
/// version that supersedes the target. The new claim is created with
/// `predecessor_id = <target>` and the Battle inferred from the target
/// claim server-side, then run through the EXACT same create + review
/// path as a normal `claim post --predecessor`. Auth: the target's
/// author OR an admin (you can only edit your own claim).
///
/// Because the predecessor is forced to the named target, the reviewer
/// receives the predecessor's claim text + structure (so its
/// improvement / `NEEDS_PREDECESSOR` comparison and predecessor-diff
/// framing apply — that's the "is this an update, not a brand-new
/// claim" guard, reused rather than rebuilt). The semantic dedup judge
/// still runs; if it classifies the edit as an `improvement` of a
/// DIFFERENT existing claim than the named predecessor, the post is
/// blocked with `NEEDS_PREDECESSOR` pointing at that other claim — so
/// an "edit" that's really a new claim relative to its named
/// predecessor surfaces clearly.
pub async fn edit_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    multipart: Multipart,
) -> Result<axum::response::Response, ApiError> {
    let (user_id, role) = crate::routes::auth::extract_user_with_role(&headers, &state.jwt_secret)?;
    let target_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;

    // Resolve the target's author + battle. Read narrow columns (not
    // `SELECT *`) to sidestep the PgBouncer cached-plan trap (AGENTS.md
    // 0.8.20). A standalone claim (battle_id NULL) cannot be edited via
    // this endpoint — editing supersession is a Battle-claim concept.
    let target: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT author_id, battle_id FROM claims WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(target_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    let (target_author, target_battle) = target.ok_or(ApiError::ClaimNotFound)?;
    if target_author != user_id && role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let battle_id = target_battle.ok_or_else(|| {
        ApiError::ValidationError(
            "this claim has no Battle (standalone); `claim edit` only revises Battle-attached \
             claims. Adopt it into a Battle first, or post a fresh standalone claim."
                .into(),
        )
    })?;

    // Forced predecessor = the target. Any predecessor_id in the
    // multipart body is ignored; the named target wins.
    post_battle_claim(&state, user_id, battle_id, multipart, Some(target_id)).await
}

/// Shared create + review flow for a Battle-attached Claim. `create_claim`
/// (normal post) and `edit_claim` (post-as-new-version) both funnel
/// through here. `forced_predecessor`, when `Some`, overrides whatever
/// `predecessor_id` the multipart body carried — this is how `edit`
/// guarantees the new claim supersedes the named target.
async fn post_battle_claim(
    state: &AppState,
    user_id: Uuid,
    battle_id: Uuid,
    mut multipart: Multipart,
    forced_predecessor: Option<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    // Short-circuit on suspension / rate-limit before any work.
    if let Some(state_block) = review_service::enforcement_state(&state.pool, user_id).await? {
        return Err(enforcement_error(state_block));
    }

    let mut claim: Option<String> = None;
    let mut artifact_data: Option<Vec<u8>> = None;
    let mut artifact_filename: Option<String> = None;
    let mut private_artifact_data: Option<Vec<u8>> = None;
    let mut private_artifact_filename: Option<String> = None;
    let mut predecessor_id: Option<Uuid> = None;
    let mut agent_backend: Option<String> = None;
    let mut agent_model: Option<String> = None;
    let mut review_id: Option<Uuid> = None;
    let mut ignore_review = false;
    // Inline structured benchmark + leaderboard. Either supplied as a
    // multipart text field (CLI's --structure), or extracted by the
    // KrabReviewer from `structure.json` inside the artifact bundle.
    let mut claim_structure_raw: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::ValidationError(e.to_string()))?
    {
        match field.name() {
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
            Some("review_id") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !text.is_empty() {
                    review_id = Some(
                        text.parse::<Uuid>()
                            .map_err(|_| ApiError::ValidationError("invalid review_id".into()))?,
                    );
                }
            }
            Some("private_artifact") => {
                private_artifact_filename = field.file_name().map(|s| s.to_string());
                private_artifact_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| ApiError::ValidationError(e.to_string()))?
                        .to_vec(),
                );
            }
            Some("ignore_review") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                ignore_review = matches!(text.as_str(), "1" | "true" | "yes" | "on");
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

    // `claim edit` forces the predecessor to the named target,
    // overriding any `predecessor_id` the body happened to carry. For
    // a normal post `forced_predecessor` is None and the body value
    // (if any) stands.
    if let Some(forced) = forced_predecessor {
        predecessor_id = Some(forced);
    }

    let claim = claim.ok_or(ApiError::ValidationError("claim is required".into()))?;
    let artifact_data = artifact_data.ok_or(ApiError::ClaimMissingArtifact)?;

    // Validate the claim text shape BEFORE any side effect. Historically
    // the 5000-char check only fired inside `claim_service::create_claim`
    // — several layers down — so a too-long claim still ran the GCS
    // upload and persisted a `claim_reviews` row on the way there. The
    // leftover review row then confused subsequent retries (the response
    // body included the user's claim text, looking like a successful
    // persist to callers). Fail fast here so over-limit text costs
    // nothing.
    krabarena_shared::field_limits::Field::ClaimsClaim.check(&claim)?;

    let upload = artifact_service::upload_artifact(&state.blob_store_url, &artifact_data).await?;

    // Server-side duplicate pre-check (since 0.8.12). Catches the
    // failure mode where a caller posts the same artifact bundle
    // twice in quick succession — typically a CLI retry on a flaky
    // network or a misread "warned but published" verdict. The
    // existing reviewer-side dup detection in
    // `review_service::run_review` is bypassed when `--review-id`
    // reuses a cached review, so this lookup is the only line of
    // defense in the `--ignore-review --review-id <id>` retry path.
    //
    // Window is 24h to balance "honest re-post of an updated bundle"
    // (allowed if author actually changed the bundle, since hash
    // would differ) vs. "rapid retry storm" (blocked). Returns 409
    // with the existing claim's URL so the caller knows where the
    // earlier post went.
    if let Some((dup_id, dup_slug)) = sqlx::query_as::<_, (Uuid, Option<String>)>(
        "SELECT id, slug FROM claims \
         WHERE battle_id = $1 AND author_id = $2 AND artifact_hash = $3 \
           AND created_at > NOW() - INTERVAL '24 hours' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(battle_id)
    .bind(user_id)
    .bind(&upload.hash)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    {
        let url = format!(
            "https://krabarena.com/claims/{}",
            dup_slug.as_deref().unwrap_or(&dup_id.to_string())
        );
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": {
                    "code": "DUPLICATE_CLAIM",
                    "message": format!(
                        "you already posted this claim within the last 24h: {url}"
                    ),
                    "existing_claim_id": dup_id,
                    "existing_claim_url": url,
                }
            })),
        )
            .into_response());
    }

    // Two-stage semantic dedup pre-check (since 0.8.13). Complements
    // the hash-based dedup above — catches the case where someone
    // re-tarred the same conceptual claim with a different bundle
    // (different file mtime / order → different hash) but the claim
    // text is essentially the same. Operates per battle (not per
    // author) — a copy-paste from another agent's claim is also a
    // duplicate.
    //
    // Stage 1 (always): embed the new claim text via Gemini
    //   text-embedding-004 (768-dim, ~$0.0001/call), pgvector
    //   nearest-by-cosine top-3 across the battle, server-side.
    // Stage 2 (only when borderline): if top-1 sim ≥
    //   `HARD_DUPLICATE_THRESHOLD` (0.97) we block immediately
    //   without LLM. If top-1 sim ∈ [`SIMILARITY_LOWER_BOUND`
    //   (0.85), `HARD_DUPLICATE_THRESHOLD`) we ask Gemini Flash to
    //   classify (`duplicate` / `improvement` / `new`) — the model
    //   sees the new claim + all 3 candidates and decides.
    //
    // Verdict policy:
    //   `duplicate`   → 409 SEMANTIC_DUPLICATE_CLAIM, hard block.
    //   `improvement` AND author didn't already set --predecessor
    //                 to the related id → 409 NEEDS_PREDECESSOR with
    //                 the suggested id + the model's
    //                 `added_value_summary` so the author can
    //                 rewrite the title to highlight the delta.
    //   `improvement` AND author DID set --predecessor correctly
    //                 → proceed (author already linked it; trust
    //                 their title judgment).
    //   `new`         → proceed.
    //
    // The embedding is also kept around to store on the new claim
    // row, so future posts can dedup against it without re-embedding.
    let claim_embedding: Option<Vec<f32>> = if state.gemini.is_configured() {
        match state.gemini.embed_text(&claim, 768).await {
            Ok(emb) => Some(emb),
            Err(e) => {
                tracing::warn!(?e, "claim embedding failed; semantic dedup skipped");
                None
            }
        }
    } else {
        None
    };
    if let Some(ref emb) = claim_embedding {
        let candidates =
            // Exclude the forced predecessor: an `edit`/`--predecessor`
            // revision must not be blocked as a near-identical duplicate of
            // the very claim it supersedes (its own text is ~cosine 1.0).
            claim_service::nearest_neighbors_topk(&state.pool, Some(battle_id), emb, 3, forced_predecessor)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(?e, "nearest_neighbors_topk failed; semantic dedup skipped");
                    Vec::new()
                });
        let top_sim = candidates.first().map(|c| c.similarity).unwrap_or(0.0);
        if top_sim >= dedup_judge::HARD_DUPLICATE_THRESHOLD {
            // Trivial dup — skip the LLM call.
            let near = candidates
                .first()
                .expect("top_sim ≥ threshold implies non-empty");
            let url = format!(
                "https://krabarena.com/claims/{}",
                near.slug.as_deref().unwrap_or(&near.id.to_string())
            );
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({
                    "error": {
                        "code": "SEMANTIC_DUPLICATE_CLAIM",
                        "message": format!(
                            "this claim is semantically near-identical to {url} (cosine similarity {:.3}); duplicates are not allowed — comment on or refine the existing claim instead",
                            near.similarity
                        ),
                        "existing_claim_id": near.id,
                        "existing_claim_url": url,
                        "similarity": near.similarity,
                    }
                })),
            )
                .into_response());
        }
        if top_sim >= dedup_judge::SIMILARITY_LOWER_BOUND {
            match dedup_judge::judge(&state.gemini, &claim, &candidates).await {
                Ok(verdict) => {
                    use dedup_judge::DedupVerdictKind;
                    let related = verdict
                        .related_claim_id
                        .and_then(|rid| candidates.iter().find(|c| c.id == rid));
                    match (verdict.verdict, related) {
                        (DedupVerdictKind::Duplicate, Some(near)) => {
                            let url = format!(
                                "https://krabarena.com/claims/{}",
                                near.slug.as_deref().unwrap_or(&near.id.to_string())
                            );
                            return Ok((
                                StatusCode::CONFLICT,
                                Json(json!({
                                    "error": {
                                        "code": "SEMANTIC_DUPLICATE_CLAIM",
                                        "message": format!(
                                            "Gemini judge flagged this as a duplicate of {url} (similarity {:.3}); duplicates are not allowed — comment on or refine the existing claim instead",
                                            near.similarity
                                        ),
                                        "existing_claim_id": near.id,
                                        "existing_claim_url": url,
                                        "similarity": near.similarity,
                                        "rationale": verdict.rationale,
                                    }
                                })),
                            )
                                .into_response());
                        }
                        (DedupVerdictKind::Improvement, Some(near))
                            if predecessor_id != Some(near.id) =>
                        {
                            // Author hasn't linked this as a refinement.
                            // Force them to either set --predecessor or
                            // rewrite the title to advertise the delta.
                            let url = format!(
                                "https://krabarena.com/claims/{}",
                                near.slug.as_deref().unwrap_or(&near.id.to_string())
                            );
                            let added_value = verdict
                                .added_value_summary
                                .unwrap_or_else(|| "(no summary provided)".into());
                            return Ok((
                                StatusCode::CONFLICT,
                                Json(json!({
                                    "error": {
                                        "code": "NEEDS_PREDECESSOR",
                                        "message": format!(
                                            "this looks like an improvement of {url} (similarity {:.3}) but you did not link it. Re-post with `--predecessor {}` AND rewrite the title to highlight what's new (the model's read: {})",
                                            near.similarity, near.id, added_value
                                        ),
                                        "existing_claim_id": near.id,
                                        "existing_claim_url": url,
                                        "similarity": near.similarity,
                                        "added_value_summary": added_value,
                                        "rationale": verdict.rationale,
                                    }
                                })),
                            )
                                .into_response());
                        }
                        // `improvement` with predecessor already linked,
                        // or `new`, or any verdict whose related id we
                        // couldn't map back to a candidate (model
                        // hallucinated an id) — fall back to permissive.
                        _ => {}
                    }
                }
                Err(e) => {
                    tracing::warn!(?e, "dedup_judge call failed; falling back to permissive");
                }
            }
        }
    }

    // Upload private artifact if provided
    let private_upload = if let Some(ref pdata) = private_artifact_data {
        // Private artifacts go to the dedicated private store, never
        // the world-readable public bucket. See
        // `artifact_service::resolve_private_blob_store`.
        let pu = artifact_service::upload_private_artifact(
            state.private_blob_store_url.as_deref(),
            pdata,
        )
        .await?;
        Some(pu)
    } else {
        None
    };

    // Process inline media (![]() refs in body that point inside the
    // bundle). Runs AFTER the artifact upload (so we have the bytes)
    // and BEFORE run_review (so the reviewer sees the public URLs in
    // the rewritten body, not opaque bundle paths). Vision SafeSearch
    // failures fail closed — the claim is rejected, never accepted on
    // upstream failure.
    let processed_media = claim_media::process(
        &state.blob_store_url,
        &upload.hash,
        &artifact_data,
        artifact_filename.as_deref(),
        &claim,
    )
    .await?;
    let claim = processed_media.rewritten_body;
    let media_records = processed_media.records;

    // If the caller didn't pass `--structure` inline, try to pull
    // `structure.json` straight out of the bundle. Authors who follow
    // the canonical layout (RUN.md / README.md / structure.json at
    // the root) get the leaderboard table for free; only authors who
    // omit the file from the bundle still see the legacy fallback.
    //
    // CONTRACT: any new code path that invokes the KrabReviewer must
    // apply the same pre-processing as this handler — otherwise the
    // reviewer hallucinates "missing structure.json" / "missing cost
    // block" suggestions against bundles that DO have the data. The
    // dry-run handler at `routes/reviews.rs::create_review` mirrors
    // this block. If you add a new pre-processing step here (e.g.
    // CHANGELOG.md, manifest.yml, etc.), mirror it there too.
    if claim_structure_raw.is_none() {
        if let Some(extracted) =
            extract_bundle_structure_json(&artifact_data, artifact_filename.as_deref())
        {
            tracing::info!(
                bytes = extracted.len(),
                "claim_structure auto-extracted from bundle/structure.json"
            );
            claim_structure_raw = Some(extracted);
        }
    }

    // Run or fetch the review.  A caller-supplied `review_id` is honoured
    // only if it matches this exact artifact hash + battle + user — otherwise
    // it's a different submission and we re-review.
    // Pull both the claim text and the predecessor's `claim_structure`
    // (as raw JSON text) so the KrabReviewer can frame the new claim's
    // summary as a diff against the prior leaderboard. Stored as
    // JSONB; cast to text so we don't need an extra serde round-trip
    // on the way to the prompt.
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

    let review: ReviewResponse = if let Some(rid) = review_id {
        let fetched = review_service::get_review(&state.pool, rid).await?;
        let matches = fetched.battle_id == Some(battle_id)
            && fetched.user_id == user_id
            && fetched.artifact_hash == upload.hash
            && fetched.claim == claim;
        if matches {
            fetched
        } else {
            // Stale review_id → re-run.
            review_service::run_review(
                &state.pool,
                &state.gemini,
                ReviewInputs {
                    battle_id: Some(battle_id),
                    topic: None,
                    user_id,
                    claim: &claim,
                    artifact_bytes: &artifact_data,
                    artifact_hash: &upload.hash,
                    artifact_filename: artifact_filename.as_deref(),
                    predecessor_claim: predecessor_claim.as_deref(),
                    predecessor_structure: predecessor_structure.as_deref(),
                    claim_structure: claim_structure_raw.as_deref(),
                    agent_backend: agent_backend.as_deref(),
                    agent_model: agent_model.as_deref(),
                    dry_run: false,
                },
            )
            .await?
            .response
        }
    } else {
        review_service::run_review(
            &state.pool,
            &state.gemini,
            ReviewInputs {
                battle_id: Some(battle_id),
                topic: None,
                user_id,
                claim: &claim,
                artifact_bytes: &artifact_data,
                artifact_hash: &upload.hash,
                artifact_filename: artifact_filename.as_deref(),
                predecessor_claim: predecessor_claim.as_deref(),
                predecessor_structure: predecessor_structure.as_deref(),
                claim_structure: claim_structure_raw.as_deref(),
                agent_backend: agent_backend.as_deref(),
                agent_model: agent_model.as_deref(),
                dry_run: false,
            },
        )
        .await?
        .response
    };

    match ReviewVerdict::parse(&review.verdict).unwrap_or(ReviewVerdict::Pass) {
        ReviewVerdict::Block => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": {
                        "code": "CLAIM_BLOCKED",
                        "message": "KrabReviewer blocked this Claim; not overridable.",
                        "review": review,
                    }
                })),
            )
                .into_response());
        }
        ReviewVerdict::Warn if !ignore_review => {
            // Conflict: caller must revise or pass ignore_review.
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({
                    "error": {
                        "code": "CLAIM_NEEDS_REVISION",
                        "message": "KrabReviewer returned suggestions; revise and resubmit, or set ignore_review=true to publish with warnings on record.",
                        "review": review,
                    }
                })),
            )
                .into_response());
        }
        _ => {}
    }

    let highlights_json = if review.artifact_highlights.is_empty() {
        None
    } else {
        Some(serde_json::to_value(&review.artifact_highlights).unwrap_or(serde_json::Value::Null))
    };

    let warnings_ignored =
        ignore_review && ReviewVerdict::parse(&review.verdict) == Some(ReviewVerdict::Warn);

    // Parse the inline claim_structure once for two purposes:
    // 1. Store the raw JSON in `claims.claim_structure` (JSONB) so
    //    the UI can render the leaderboard table.
    // 2. Pull the rank-1 winners into the denormalized
    //    `claims.winners` TEXT[] for indexed querying.
    // Validation failures are intentionally NOT surfaced as errors
    // here — the KrabReviewer already converted them into warn-
    // severity suggestions on the review row, and the v0.7.x policy
    // is to publish the claim regardless. We just record what we
    // can; if `claim_structure_raw` is malformed we drop it and
    // leave both columns NULL so the UI shows the legacy fallback.
    let (claim_structure_json, winners_vec): (Option<serde_json::Value>, Vec<String>) =
        match claim_structure_raw.as_deref() {
            Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(value) => {
                    let winners: Vec<String> = value
                        .get("winners")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|w| w.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    (Some(value), winners)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "claim_structure not valid JSON; storing NULL");
                    (None, Vec::new())
                }
            },
            None => (None, Vec::new()),
        };

    // Pull the author-reported cost out of `structure.json`. Replaces
    // the README.md regex parser that v0.8.2 first shipped with — the
    // structure.json field is explicit, type-checked, and round-trips
    // back to the author. Server only fills the columns when the
    // structure carries a cost AND no PoW metadata path already set
    // them on the request (we never overwrite an explicit value).
    let extracted_cost = extract_cost_from_structure(claim_structure_json.as_ref());
    let tokens_consumed = extracted_cost.tokens;
    let price_cents = extracted_cost.price_cents;

    // Best-effort viral share layer: reserialize the reviewer's typed
    // `Presentation` back into JSON once so `CreateClaimParams` can
    // hold a `&serde_json::Value` reference into this local.
    let presentation_value = review
        .presentation
        .as_ref()
        .and_then(|p| serde_json::to_value(p).ok());

    let claim = claim_service::create_claim(
        &state.pool,
        claim_service::CreateClaimParams {
            battle_id: Some(battle_id),
            author_id: user_id,
            claim: &claim,
            artifact_hash: &upload.hash,
            artifact_url: &upload.url,
            artifact_size: upload.size,
            artifact_filename: artifact_filename.as_deref(),
            predecessor_id,
            agent_backend: agent_backend.as_deref(),
            agent_model: agent_model.as_deref(),
            review_id: Some(review.id),
            warnings_ignored,
            artifact_summary: review.artifact_summary.as_deref(),
            artifact_highlights: highlights_json.as_ref(),
            claim_structure: claim_structure_json.as_ref(),
            winners: &winners_vec,
            tokens_consumed,
            price_cents,
            claim_embedding: claim_embedding.as_deref(),
            topic: None,
            topic_tags: &[],
            visibility: "public",
            og_summary: review.og_summary.as_deref(),
            presentation: presentation_value.as_ref(),
        },
    )
    .await?;

    // Save private artifact reference if uploaded
    if let Some(ref pu) = private_upload {
        let _ = sqlx::query(
            "UPDATE claims SET private_artifact_url = $1, private_artifact_hash = $2, private_artifact_filename = $3 WHERE id = $4",
        )
        .bind(&pu.url)
        .bind(&pu.hash)
        .bind(private_artifact_filename.as_deref())
        .execute(&state.pool)
        .await;
    }

    // Persist claim_media rows after the claim row exists so the FK
    // resolves. The unique index on (claim_id, sha256) means a re-run
    // of a flaky request is idempotent.
    claim_media::persist(&state.pool, claim.id, &media_records).await?;

    // Fire `claim.posted` webhook to every active subscriber.
    // Spawn-internal so the author's response isn't blocked by
    // subscriber latency; failures are observability-only. Audience
    // follows the claim's visibility (private → author/admin subs only).
    crate::services::webhook_dispatch::dispatch_event_to(
        state.pool.clone(),
        krabarena_shared::webhook_events::CLAIM_POSTED,
        build_claim_webhook_payload(&claim),
        crate::services::webhook_dispatch::Audience::for_claim(
            Some(&claim.visibility),
            claim.author_id,
        ),
    );

    // --- 7. Battle executive summary refresh (incremental) --------
    //
    // Decoupled from review since 0.8.15. The reviewer no longer
    // produces `battle_executive_summary` as a side effect (that
    // forced every review to download every claim's bundle and
    // stuffed ~100 KB of cross-claim context into the prompt — slow
    // on every post). Instead, after every successful publish we
    // spawn a small async job that:
    //   1. Loads JUST this new claim's structure.json + README
    //   2. Asks Gemini to roll it into the previous summary
    //   3. Writes the result + bumps temperature + appends a
    //      history row
    //
    // The work is bounded (one bundle download, ~5 KB prompt) so it
    // finishes in ~10s without blocking the post. Failures here are
    // non-fatal: the claim is already published; the summary will
    // catch up on the next post or on a manual /regenerate-summary.
    let summary_state = state.clone();
    let gemini = state.gemini.clone();
    let new_claim_id = claim.id;
    tokio::spawn(async move {
        let pool = summary_state.pool.clone();
        match battle_summarizer::regenerate_summary(&pool, &gemini, battle_id, Some(new_claim_id))
            .await
        {
            Ok(regen) => {
                if let Err(e) = battle_summarizer::persist_summary(
                    &pool,
                    battle_id,
                    &regen.markdown,
                    regen.short_markdown.as_deref(),
                )
                .await
                {
                    tracing::warn!(?e, ?battle_id, "async summary persist failed");
                } else {
                    // Summary persisted → the battle card's content
                    // hash changed. Eagerly re-render + re-cache the OG
                    // card so the URL serves it immediately. We're
                    // already inside a spawned task, so warm inline.
                    // No-op when OG_RENDER_URL is unset; best-effort.
                    crate::routes::og::spawn_warm_battle_og_by_id(&summary_state, battle_id);
                }
            }
            Err(e) => tracing::warn!(?e, ?battle_id, "incremental summary update failed"),
        }
    });

    // --- 8. Keep the battle's competitor avatars/logos current --------
    //
    // A new (or edited) claim can introduce a new competitor in its
    // leaderboard. Backfill the battle's `solutions` so its logo +
    // brand color get derived right away — from the curated vendor map,
    // else the author's `website_url` — instead of waiting for a manual
    // admin call. Idempotent and light: only genuinely new names hit the
    // network (one favicon each). Fire-and-forget; failures are
    // observability-only (the page falls back to initials until the next
    // post or a manual backfill).
    {
        let bf_pool = state.pool.clone();
        let bf_http = state.proxy_http.clone();
        tokio::spawn(async move {
            if let Err(e) =
                crate::routes::admin::backfill_one_battle(&bf_pool, &bf_http, battle_id).await
            {
                tracing::warn!(?e, ?battle_id, "auto battle-solutions backfill failed");
            }
        });
    }

    Ok((StatusCode::CREATED, Json(ApiResponse::new(claim))).into_response())
}

pub async fn get_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let claim = claim_service::get_claim(&state.pool, claim_id).await?;
    // Private standalone claims (since migration 035) are visible to
    // the author + admins only. Anyone else gets a 404 (same shape
    // as a missing slug — don't leak that the resource exists).
    let viewer = claim_visibility::viewer_from_headers(&headers, &state.jwt_secret);
    if !claim_visibility::can_view(Some(&claim.visibility), claim.author_id, &viewer) {
        return Err(ApiError::ClaimNotFound);
    }
    Ok(Json(ApiResponse::new(claim)))
}

/// DELETE /v1/claims/{claim_id}
///
/// Hard-deletes a Claim and its dependent rows. Authorised for the
/// Claim's author OR any admin. Refused when the Claim has any
/// engagement (verifications, comments, votes) — those rows belong
/// to other users and an author shouldn't be able to silently erase
/// the public record they generated. Admins can override only by
/// first removing the engagement rows manually (we deliberately do
/// not provide an "admin force" flag to keep the audit trail intact).
///
/// Cascade: `claim_media` already has `ON DELETE CASCADE`. The other
/// dependent tables don't, so we sweep them in the same transaction:
///   - `purchases.claim_id` (NOT NULL FK) → DELETE
///   - `proxy_logs.claim_id` (nullable FK) → SET NULL (preserve usage
///     log for billing / abuse forensics; the claim FK was just for
///     attribution)
///   - `token_transactions.ref_claim_id` (nullable FK) → SET NULL
///   - `battle_summary_history.claim_id` (nullable FK) → SET NULL
///   - `claims.predecessor_id` (self-FK) → SET NULL on any claim that
///     used this one as predecessor (preserves the dependent claim;
///     loses the lineage pointer)
///
/// Used today for one-off hygiene (clearing accidental dups from CLI
/// retries pre-0.8.12 dedup pre-check). Should be a rare operation.
pub async fn delete_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    let (user_id, role) = crate::routes::auth::extract_user_with_role(&headers, &state.jwt_secret)?;
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;

    let claim_row: Option<(Uuid,)> = sqlx::query_as("SELECT author_id FROM claims WHERE id = $1")
        .bind(claim_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let author_id = claim_row.ok_or(ApiError::ClaimNotFound)?.0;
    if author_id != user_id && role != "admin" {
        return Err(ApiError::Forbidden);
    }

    // Engagement check — refuse rather than silently erase other
    // users' content. Returns 409 Conflict (semantically correct for
    // "the request is fine but the resource state forbids it") rather
    // than 400 ValidationError (which is for malformed inputs).
    let engagement: (i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM verifications WHERE claim_id = $1), \
           (SELECT COUNT(*) FROM comments WHERE claim_id = $1), \
           (SELECT COUNT(*) FROM votes WHERE claim_id = $1)",
    )
    .bind(claim_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    if engagement.0 + engagement.1 + engagement.2 > 0 {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": {
                    "code": "CLAIM_HAS_ENGAGEMENT",
                    "message": format!(
                        "claim has engagement (verifications: {}, comments: {}, votes: {}); \
                         delete is refused — redact the content instead",
                        engagement.0, engagement.1, engagement.2
                    ),
                    "verifications": engagement.0,
                    "comments": engagement.1,
                    "votes": engagement.2,
                }
            })),
        )
            .into_response());
    }

    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    // Belt-and-suspenders: claim_media already cascades on FK, but
    // also clear it explicitly in the same tx so a future migration
    // dropping CASCADE doesn't silently leak rows.
    sqlx::query("DELETE FROM claim_media WHERE claim_id = $1")
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    sqlx::query("DELETE FROM purchases WHERE claim_id = $1")
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    sqlx::query("UPDATE proxy_logs SET claim_id = NULL WHERE claim_id = $1")
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    sqlx::query("UPDATE token_transactions SET ref_claim_id = NULL WHERE ref_claim_id = $1")
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    sqlx::query("UPDATE battle_summary_history SET claim_id = NULL WHERE claim_id = $1")
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    sqlx::query("UPDATE claims SET predecessor_id = NULL WHERE predecessor_id = $1")
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    sqlx::query("DELETE FROM claims WHERE id = $1")
        .bind(claim_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        claim_id = %claim_id,
        deleted_by = %user_id,
        deleter_role = %role,
        "claim.deleted"
    );

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /v1/claims` — create a **standalone** Claim, i.e. a Claim
/// posted against a topic that doesn't yet have a curated Battle.
/// Companion to the Battle-scoped `POST /v1/battles/{id}/claims`
/// which keeps its current shape (and remains the canonical path
/// when the author knows which Battle to attach to).
///
/// Multipart fields:
///   - `claim` (required) — the claim text. Same 5000-char cap.
///   - `artifact` (required) — the runnable artifact bundle.
///   - `topic` (required) — short question the author is answering
///     (the surrogate for the Battle title).
///   - `topic_tags` (required) — comma-separated tags; at least one.
///   - `agent_backend` / `agent_model` — optional identity strings.
///   - `claim_structure` — optional inline `structure.json`.
///
/// Visibility on insert is forced to `'private'` — the new claim is
/// only visible to its author + admins. An editor promotes it to
/// `'public'` via `POST /v1/admin/claims/{id}/promote` before it
/// shows up on `/claims/standalone`. The author can later adopt
/// the claim into a Battle (`POST /v1/claims/{id}/adopt`), which
/// flips visibility to `'adopted'`.
pub async fn create_standalone_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<axum::response::Response, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    if let Some(state_block) = review_service::enforcement_state(&state.pool, user_id).await? {
        return Err(enforcement_error(state_block));
    }

    let mut claim: Option<String> = None;
    let mut artifact_data: Option<Vec<u8>> = None;
    let mut artifact_filename: Option<String> = None;
    let mut agent_backend: Option<String> = None;
    let mut agent_model: Option<String> = None;
    let mut topic: Option<String> = None;
    let mut topic_tags_raw: Option<String> = None;
    let mut claim_structure_raw: Option<String> = None;
    let mut ignore_review = false;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::ValidationError(e.to_string()))?
    {
        match field.name() {
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
            Some("topic") => {
                let t = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !t.trim().is_empty() {
                    topic = Some(t);
                }
            }
            Some("topic_tags") => {
                let t = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !t.trim().is_empty() {
                    topic_tags_raw = Some(t);
                }
            }
            Some("agent_backend") => {
                let t = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !t.is_empty() {
                    agent_backend = Some(t);
                }
            }
            Some("agent_model") => {
                let t = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !t.is_empty() {
                    agent_model = Some(t);
                }
            }
            Some("claim_structure") => {
                let t = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                if !t.trim().is_empty() {
                    claim_structure_raw = Some(t);
                }
            }
            Some("ignore_review") => {
                let t = field
                    .text()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?;
                ignore_review = matches!(t.as_str(), "1" | "true" | "yes" | "on");
            }
            _ => {}
        }
    }

    let claim = claim.ok_or(ApiError::ValidationError("claim is required".into()))?;
    let artifact_data = artifact_data.ok_or(ApiError::ClaimMissingArtifact)?;
    let topic = topic.ok_or(ApiError::ValidationError(
        "topic is required for standalone claims (--no-battle)".into(),
    ))?;
    let topic_tags_raw = topic_tags_raw.ok_or(ApiError::ValidationError(
        "topic_tags is required for standalone claims (one or more comma-separated tags)".into(),
    ))?;
    let topic_tags: Vec<String> = topic_tags_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if topic_tags.is_empty() {
        return Err(ApiError::ValidationError(
            "topic_tags must contain at least one non-empty tag".into(),
        ));
    }

    krabarena_shared::field_limits::Field::ClaimsClaim.check(&claim)?;

    let upload = artifact_service::upload_artifact(&state.blob_store_url, &artifact_data).await?;

    // Process inline images in the claim body (bundle-relative refs).
    // Same pipeline the Battle-attached path uses — failures here are
    // closed (vision SafeSearch issues reject the post).
    let processed_media = claim_media::process(
        &state.blob_store_url,
        &upload.hash,
        &artifact_data,
        artifact_filename.as_deref(),
        &claim,
    )
    .await?;
    let claim_body = processed_media.rewritten_body;
    let media_records = processed_media.records;

    // Run the reviewer with `battle_id = None`. The reviewer's
    // standalone branch drops to structure + on-topic + quality only.
    let review = review_service::run_review(
        &state.pool,
        &state.gemini,
        ReviewInputs {
            battle_id: None,
            topic: Some(&topic),
            user_id,
            claim: &claim_body,
            artifact_bytes: &artifact_data,
            artifact_hash: &upload.hash,
            artifact_filename: artifact_filename.as_deref(),
            predecessor_claim: None,
            predecessor_structure: None,
            claim_structure: claim_structure_raw.as_deref(),
            agent_backend: agent_backend.as_deref(),
            agent_model: agent_model.as_deref(),
            dry_run: false,
        },
    )
    .await?
    .response;

    match ReviewVerdict::parse(&review.verdict).unwrap_or(ReviewVerdict::Pass) {
        ReviewVerdict::Block => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": {
                        "code": "CLAIM_BLOCKED",
                        "message": "KrabReviewer blocked this Claim; not overridable.",
                        "review": review,
                    }
                })),
            )
                .into_response());
        }
        ReviewVerdict::Warn if !ignore_review => {
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({
                    "error": {
                        "code": "CLAIM_NEEDS_REVISION",
                        "message": "KrabReviewer returned suggestions; revise and resubmit, or set ignore_review=true to publish with warnings on record.",
                        "review": review,
                    }
                })),
            )
                .into_response());
        }
        _ => {}
    }

    let highlights_json = if review.artifact_highlights.is_empty() {
        None
    } else {
        Some(serde_json::to_value(&review.artifact_highlights).unwrap_or(serde_json::Value::Null))
    };
    let warnings_ignored =
        ignore_review && ReviewVerdict::parse(&review.verdict) == Some(ReviewVerdict::Warn);

    let (claim_structure_json, winners_vec): (Option<serde_json::Value>, Vec<String>) =
        match claim_structure_raw.as_deref() {
            Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(value) => {
                    let winners: Vec<String> = value
                        .get("winners")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|w| w.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    (Some(value), winners)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "claim_structure not valid JSON; storing NULL");
                    (None, Vec::new())
                }
            },
            None => (None, Vec::new()),
        };
    let extracted_cost = extract_cost_from_structure(claim_structure_json.as_ref());

    // Best-effort viral share layer — see the Battle-attached path
    // above for why this is reserialized into a local.
    let presentation_value = review
        .presentation
        .as_ref()
        .and_then(|p| serde_json::to_value(p).ok());

    let claim = claim_service::create_claim(
        &state.pool,
        claim_service::CreateClaimParams {
            battle_id: None,
            author_id: user_id,
            claim: &claim_body,
            artifact_hash: &upload.hash,
            artifact_url: &upload.url,
            artifact_size: upload.size,
            artifact_filename: artifact_filename.as_deref(),
            predecessor_id: None,
            agent_backend: agent_backend.as_deref(),
            agent_model: agent_model.as_deref(),
            review_id: Some(review.id),
            warnings_ignored,
            artifact_summary: review.artifact_summary.as_deref(),
            artifact_highlights: highlights_json.as_ref(),
            claim_structure: claim_structure_json.as_ref(),
            winners: &winners_vec,
            tokens_consumed: extracted_cost.tokens,
            price_cents: extracted_cost.price_cents,
            claim_embedding: None,
            topic: Some(&topic),
            topic_tags: &topic_tags,
            visibility: "private",
            og_summary: review.og_summary.as_deref(),
            presentation: presentation_value.as_ref(),
        },
    )
    .await?;

    claim_media::persist(&state.pool, claim.id, &media_records).await?;

    // Standalone claims fire the same `claim.posted` event. Some
    // subscribers care about every Claim regardless of Battle
    // attachment; we let them filter on `battle_id == null` if they
    // only want Battle-attached ones. Standalone claims land with
    // visibility='private', so they are delivered ONLY to
    // subscriptions owned by the author or by a platform admin (e.g.
    // KrabChecker) — never announced to arbitrary subscribers.
    crate::services::webhook_dispatch::dispatch_event_to(
        state.pool.clone(),
        krabarena_shared::webhook_events::CLAIM_POSTED,
        build_claim_webhook_payload(&claim),
        crate::services::webhook_dispatch::Audience::for_claim(
            Some(&claim.visibility),
            claim.author_id,
        ),
    );

    Ok((StatusCode::CREATED, Json(ApiResponse::new(claim))).into_response())
}

/// `POST /v1/claims/{claim_id}/adopt` — move a Claim into a Battle
/// after the fact. Auth: claim author OR admin. Body:
/// `{ "battle_id": "<uuid or slug>" }`. Idempotent on re-adoption
/// into the same Battle; rejected (400) when the Claim is already
/// adopted into a different Battle.
///
/// Also re-checks the adopted claim against the destination
/// Battle's guidelines via a soft-warn KrabReviewer pass (`dry_run`
/// — no strike accrual, no enforcement) so an off-topic adoption
/// is visible in the response without blocking the move.
#[derive(Debug, serde::Deserialize)]
pub struct AdoptClaimRequest {
    pub battle_id: String,
}

pub async fn adopt_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Json(req): Json<AdoptClaimRequest>,
) -> Result<axum::response::Response, ApiError> {
    let (user_id, role) = crate::routes::auth::extract_user_with_role(&headers, &state.jwt_secret)?;
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let destination = slug_resolver::resolve_battle(&state.pool, &req.battle_id).await?;

    // Author-or-admin gate. Read `author_id` cheaply (avoid `SELECT *`
    // to sidestep the PgBouncer cached-plan trap — see AGENTS.md 0.8.20).
    let author_id: Uuid = sqlx::query_scalar("SELECT author_id FROM claims WHERE id = $1")
        .bind(claim_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::ClaimNotFound)?;
    if author_id != user_id && role != "admin" {
        return Err(ApiError::Forbidden);
    }

    let claim = claim_service::adopt_claim(&state.pool, claim_id, destination).await?;

    // Soft re-review against the destination Battle. Best-effort —
    // a transient Gemini blip shouldn't roll back an otherwise-valid
    // adoption. The result is included in the response so callers
    // can surface "this claim looks off-topic for the destination
    // battle" without blocking.
    let review_hint: Option<serde_json::Value> =
        match artifact_service::download_artifact(&claim.artifact_url).await {
            Ok(artifact_bytes) => {
                match review_service::run_review(
                    &state.pool,
                    &state.gemini,
                    ReviewInputs {
                        battle_id: Some(destination),
                        topic: claim.topic.as_deref(),
                        user_id,
                        claim: &claim.claim,
                        artifact_bytes: &artifact_bytes,
                        artifact_hash: &claim.artifact_hash,
                        artifact_filename: claim.artifact_filename.as_deref(),
                        predecessor_claim: None,
                        predecessor_structure: None,
                        claim_structure: None,
                        agent_backend: claim.agent_backend.as_deref(),
                        agent_model: claim.agent_model.as_deref(),
                        // dry-run so an off-topic adoption logs the verdict
                        // but doesn't accrue strikes / enforcement.
                        dry_run: true,
                    },
                )
                .await
                {
                    Ok(out) => Some(serde_json::json!({
                        "verdict": out.response.verdict,
                        "summary": out.response.summary,
                    })),
                    Err(e) => {
                        tracing::warn!(?e, "adopt re-review failed; continuing");
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(?e, "adopt re-review artifact fetch failed");
                None
            }
        };

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "claim": claim,
            "review_hint": review_hint,
        }))),
    )
        .into_response())
}

/// `POST /v1/admin/claims/{claim_id}/promote` — admin-only flip of
/// `visibility` on a standalone claim between `'public'` and
/// `'private'`. Adopted claims (visibility='adopted') and
/// Battle-attached claims are rejected — their visibility belongs
/// to the adoption / battle relationship, not the admin promote
/// surface.
#[derive(Debug, serde::Deserialize)]
pub struct PromoteClaimRequest {
    pub visibility: String,
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn promote_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Json(req): Json<PromoteClaimRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = crate::routes::auth::require_admin(&headers, &state.jwt_secret)?;
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let claim = claim_service::set_visibility(&state.pool, claim_id, &req.visibility).await?;
    tracing::info!(
        actor = %actor,
        claim_id = %claim_id,
        visibility = %req.visibility,
        reason = ?req.reason.as_deref(),
        "admin.claim.visibility_changed"
    );
    Ok((StatusCode::OK, Json(ApiResponse::new(claim))))
}

/// `GET /v1/claims` — list latest Claims globally. This is the JSON
/// variant of the home page's latest-claim activity source and the CLI
/// shape KrabAgent uses for newest-first polling. Private standalone
/// Claims are included only for their author or admins.
pub async fn list_latest_claims(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListClaimsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let viewer = crate::routes::auth::extract_user_with_role(&headers, &state.jwt_secret).ok();
    let limit = query.limit.unwrap_or(20);
    let (items, pagination) =
        claim_service::list_latest_claims(&state.pool, viewer, query.cursor.as_deref(), limit)
            .await?;

    Ok(Json(PaginatedResponse {
        data: crate::models::claim::claims_list_json(items, query.compact.unwrap_or(false)),
        pagination,
        meta: ResponseMeta::generate(),
    }))
}

/// `GET /v1/claims/standalone` — list public standalone Claims
/// (battle_id IS NULL AND visibility='public'). Companion to the
/// HTML page at `/claims/standalone`; this is the JSON variant the
/// e2e test hits.
pub async fn list_standalone_claims(
    State(state): State<AppState>,
    Query(query): Query<ListClaimsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = query.limit.unwrap_or(50);
    let (items, pagination) =
        claim_service::list_standalone_claims(&state.pool, query.cursor.as_deref(), limit).await?;
    Ok(Json(PaginatedResponse {
        data: crate::models::claim::claims_list_json(items, query.compact.unwrap_or(false)),
        pagination,
        meta: ResponseMeta::generate(),
    }))
}

pub async fn list_claims(
    State(state): State<AppState>,
    Path(battle_id_raw): Path<String>,
    Query(query): Query<ListClaimsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let limit = query.limit.unwrap_or(20);
    let (items, pagination) = claim_service::list_claims(
        &state.pool,
        battle_id,
        query.sort.as_deref(),
        query.cursor.as_deref(),
        limit,
    )
    .await?;

    Ok(Json(PaginatedResponse {
        data: crate::models::claim::claims_list_json(items, query.compact.unwrap_or(false)),
        pagination,
        meta: ResponseMeta::generate(),
    }))
}

pub async fn get_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let claim = claim_service::get_claim(&state.pool, claim_id).await?;
    let viewer = claim_visibility::viewer_from_headers(&headers, &state.jwt_secret);
    if !claim_visibility::can_view(Some(&claim.visibility), claim.author_id, &viewer) {
        return Err(ApiError::ClaimNotFound);
    }
    let data = artifact_service::download_artifact(&claim.artifact_url).await?;

    // Determine filename from: stored filename, hash, or fallback
    let filename = claim
        .artifact_filename
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| {
            let ext = if data.starts_with(&[0x1f, 0x8b]) {
                "tar.gz"
            } else if data.starts_with(b"PK") {
                "zip"
            } else if data.starts_with(b"#!") || std::str::from_utf8(&data).is_ok() {
                "txt"
            } else {
                "bin"
            };
            format!("artifact-{}.{}", &claim.artifact_hash[..12], ext)
        });

    let disposition = format!("attachment; filename=\"{}\"", filename);
    Ok((
        StatusCode::OK,
        [
            (
                "content-type".to_string(),
                "application/octet-stream".to_string(),
            ),
            ("content-disposition".to_string(), disposition),
        ],
        data,
    ))
}

pub async fn get_lineage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let viewer = claim_visibility::viewer_from_headers(&headers, &state.jwt_secret);
    claim_visibility::ensure_visible(&state.pool, claim_id, &viewer).await?;
    let lineage: Vec<_> = claim_service::get_lineage(&state.pool, claim_id)
        .await?
        .into_iter()
        // A visible head can still have private predecessors — drop
        // the ones this viewer may not see rather than 404ing the chain.
        .filter(|c| claim_visibility::can_view(Some(&c.visibility), c.author_id, &viewer))
        .collect();
    Ok(Json(ApiResponse::new(lineage)))
}

/// `GET /v1/claims/{claim_id}/private-artifact` — the ONLY way to read
/// a Claim's private artifact. Private artifacts live in a separate,
/// non-public bucket (`PRIVATE_BLOB_STORE_URL`); this route checks
/// the caller is the claim author, the parent Battle's author, or an
/// admin, then streams the object from storage with the service
/// account's credentials. Everyone else (including anonymous callers)
/// gets a 404 — same shape as "no private artifact" so the existence
/// of one doesn't leak.
pub async fn get_private_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    let viewer = claim_visibility::viewer_from_headers(&headers, &state.jwt_secret);
    if viewer.is_none() {
        return Err(ApiError::Unauthorized);
    }
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    /// (claim author, visibility, private artifact url, filename, battle author)
    type PrivateArtifactRow = (
        Uuid,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Uuid>,
    );
    let row: Option<PrivateArtifactRow> = sqlx::query_as(
        "SELECT c.author_id, c.visibility, c.private_artifact_url, \
                    c.private_artifact_filename, b.author_id \
               FROM claims c LEFT JOIN battles b ON b.id = c.battle_id \
              WHERE c.id = $1 AND c.deleted_at IS NULL",
    )
    .bind(claim_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    let (author_id, visibility, url, filename, battle_author) =
        row.ok_or(ApiError::ClaimNotFound)?;
    if !claim_visibility::can_view(visibility.as_deref(), author_id, &viewer)
        || !claim_visibility::can_view_private_artifact(author_id, battle_author, &viewer)
    {
        return Err(ApiError::ClaimNotFound);
    }
    let url = url
        .filter(|u| !u.is_empty())
        .ok_or_else(|| ApiError::NotFound("claim has no private artifact".into()))?;

    let body = artifact_service::stream_private_artifact(&url).await?;
    let filename = filename
        .filter(|f| !f.is_empty())
        .map(|f| f.replace(['"', '\\', '\r', '\n'], "_"))
        .unwrap_or_else(|| "private-artifact.bin".into());
    tracing::info!(claim = %claim_id, "private_artifact.download");
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .header(
            "content-disposition",
            format!("attachment; filename=\"{filename}\""),
        )
        .header("cache-control", "private, no-store")
        .body(body)
        .map_err(|e| ApiError::Internal(e.into()))
}

/// Build the `data.claim` block that ships in every `claim.posted`
/// webhook envelope. Kept narrow on purpose — subscribers don't need
/// the full ClaimResponse (no reviewer cost, no votes, no
/// embeddings), and a stable narrow contract is easier to evolve.
/// External callers (KrabChecker, …) depend on this shape; bump the
/// docs in `docs/webhooks.md` if it changes.
pub(crate) fn build_claim_webhook_payload(
    claim: &crate::models::claim::ClaimResponse,
) -> serde_json::Value {
    // Title = first markdown H1 line (`# Title`) or the first
    // non-empty line — same heuristic the slug generator uses.
    // Trimmed to 200 chars so subscribers can store it in a short
    // column without pre-trimming themselves.
    let title = crate::services::webhook_payloads::claim_title(&claim.claim);
    serde_json::json!({
        "claim": {
            "id": claim.id.to_string(),
            "slug": claim.slug,
            "battle_id": claim.battle_id.map(|u| u.to_string()),
            // Battle slug is not on ClaimResponse; subscribers can
            // look it up via `GET /v1/battles/{id}` if they need
            // the URL. Reserved key so we can fill it in later
            // without a contract bump.
            "battle_slug": serde_json::Value::Null,
            "author_id": claim.author_id.to_string(),
            "title": title,
            "url": claim.urls.web_url,
            "visibility": claim.visibility,
        }
    })
}

/// Pull a top-level `structure.json` out of the artifact bundle and
/// return it as a raw JSON string. Used as a fallback when the caller
/// didn't pass `--structure` inline. Returns `None` when the bundle
/// has no `structure.json`, when it isn't valid UTF-8, or when the
/// bytes don't parse as JSON. Failures are silent — the legacy
/// fallback path stays unchanged.
pub(crate) fn extract_bundle_structure_json(
    artifact_bytes: &[u8],
    artifact_filename: Option<&str>,
) -> Option<String> {
    use crate::services::review_tools::FileMap;
    let file_map = FileMap::from_tar_gz(artifact_bytes)
        .ok()
        .or_else(|| Some(FileMap::from_single_file(artifact_bytes, artifact_filename)))?;
    // `tar -czf bundle.tar.gz .` produces `./structure.json`; bare
    // tar with explicit files produces `structure.json`.
    let bytes = file_map
        .read_bytes("structure.json")
        .or_else(|| file_map.read_bytes("./structure.json"))?;
    let text = std::str::from_utf8(bytes).ok()?;
    // Validate it parses; an unparseable structure.json is the same
    // failure mode as a missing one — fall back to NULL persistence.
    serde_json::from_str::<serde_json::Value>(text).ok()?;
    Some(text.to_string())
}

/// Author-reported cost pulled out of `structure.json`. All fields
/// are `None` when the structure had no `cost` block, no usable
/// fields, or values that would violate the column constraints
/// (negative dollars, zero cents — the `claims.price_cents` CHECK
/// constraint is `IS NULL OR > 0`). Token count is clamped at
/// `i32::MAX` since the column type is INTEGER.
///
/// `external_usd` is **not** persisted on a column; it stays in the
/// `claim_structure` JSONB and is summed at query time by the
/// agent / arena rollups. This keeps the column count down for a
/// soft-launch field that may evolve.
#[derive(Debug, Default, Clone, PartialEq)]
struct ExtractedCost {
    tokens: Option<i32>,
    price_cents: Option<i32>,
}

/// Read `cost.tokens` (u64) and `cost.usd` (f64) off the parsed
/// `structure.json` blob and convert them to the column types on
/// `claims` (tokens_consumed: INTEGER; price_cents: INTEGER, with
/// CHECK > 0). Out-of-range / negative values are silently dropped
/// — a malformed cost is the same failure mode as a missing one.
///
/// `cost.external_usd` is read by the rollup queries directly off
/// the JSONB; nothing to extract on this path.
fn extract_cost_from_structure(structure: Option<&serde_json::Value>) -> ExtractedCost {
    let mut out = ExtractedCost::default();
    let Some(cost) = structure.and_then(|v| v.get("cost")) else {
        return out;
    };

    if let Some(t) = cost.get("tokens").and_then(|v| v.as_u64()) {
        // Clamp to i32::MAX so the INTEGER column fits. A claim
        // legitimately exceeding 2.1B tokens is implausible; if it
        // ever happens we'll see it in the warn log and revisit.
        let clamped = t.min(i32::MAX as u64) as i32;
        if clamped > 0 {
            out.tokens = Some(clamped);
        }
    }

    if let Some(usd) = cost.get("usd").and_then(|v| v.as_f64()) {
        // Round to cents. Skip negatives + zero — the price_cents
        // CHECK constraint is `IS NULL OR > 0`, so a literal 0 must
        // stay NULL rather than violate the constraint.
        if usd.is_finite() && usd > 0.0 {
            let cents = (usd * 100.0).round();
            if cents > 0.0 && cents <= i32::MAX as f64 {
                out.price_cents = Some(cents as i32);
            }
        }
    }

    out
}

fn enforcement_error(state: EnforcementState) -> ApiError {
    match state {
        EnforcementState::Suspended { until, reason } => ApiError::ValidationError(format!(
            "account suspended until {} — {}",
            until.to_rfc3339(),
            reason
        )),
        EnforcementState::RateLimited {
            until,
            strike_summary,
        } => {
            // Historically this message was just `publishing
            // rate-limited until <timestamp>`, which sent everyone
            // off inventing plausible-sounding rules they'd tripped.
            // Include the actual strike breakdown + the rule that
            // fired so the caller can see what they did and what
            // will age out when.
            let base = format!("publishing rate-limited until {}", until.to_rfc3339());
            if strike_summary.is_empty() {
                ApiError::ValidationError(base)
            } else {
                ApiError::ValidationError(format!(
                    "{base} — triggered by {strike_summary} in the last 24h"
                ))
            }
        }
    }
}

#[cfg(test)]
mod extract_structure_tests {
    use super::*;
    use std::io::Write;

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            for (path, bytes) in files {
                let mut h = tar::Header::new_gnu();
                h.set_size(bytes.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, path, *bytes).expect("append");
            }
            b.finish().expect("finish");
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).expect("gz write");
        gz.finish().expect("gz finish")
    }

    #[test]
    fn extracts_top_level_structure_json() {
        let body = br#"{"schema_version":1,"benchmark":{"name":"x"},"leaderboard":[]}"#;
        let bundle = tar_gz(&[("structure.json", body)]);
        let out = extract_bundle_structure_json(&bundle, Some("a.tar.gz"))
            .expect("structure should be extracted");
        assert!(out.contains("\"schema_version\":1"));
    }

    #[test]
    fn handles_dot_slash_prefixed_entry() {
        // `tar -czf bundle.tar.gz .` produces ./structure.json paths.
        let body = br#"{"benchmark":{}}"#;
        let bundle = tar_gz(&[("./structure.json", body)]);
        let out = extract_bundle_structure_json(&bundle, Some("a.tar.gz"))
            .expect("should find ./structure.json");
        assert!(out.contains("\"benchmark\""));
    }

    #[test]
    fn returns_none_when_missing() {
        let bundle = tar_gz(&[("README.md", b"hi")]);
        assert!(extract_bundle_structure_json(&bundle, Some("a.tar.gz")).is_none());
    }

    #[test]
    fn returns_none_when_unparseable_json() {
        let bundle = tar_gz(&[("structure.json", b"not valid json {")]);
        // Falls back silently so the legacy-pill path still kicks in.
        assert!(extract_bundle_structure_json(&bundle, Some("a.tar.gz")).is_none());
    }

    #[test]
    fn returns_none_for_non_archive_artifact() {
        // A bare image as the artifact (FileMap::from_single_file
        // path) shouldn't surface a structure.json that isn't there.
        assert!(extract_bundle_structure_json(b"not a tar gz", Some("blob.bin")).is_none());
    }

    #[test]
    fn extract_cost_pulls_tokens_and_dollars() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"cost":{"tokens":32000,"usd":0.84,"note":"Claude Opus 4.7"}}"#,
        )
        .unwrap();
        let out = extract_cost_from_structure(Some(&v));
        assert_eq!(out.tokens, Some(32_000));
        // 0.84 * 100 = 84 cents (rounded).
        assert_eq!(out.price_cents, Some(84));
    }

    #[test]
    fn extract_cost_returns_default_when_block_absent() {
        let v: serde_json::Value = serde_json::from_str(r#"{"benchmark":{}}"#).unwrap();
        assert_eq!(
            extract_cost_from_structure(Some(&v)),
            ExtractedCost::default()
        );
        // Also for the no-structure case.
        assert_eq!(extract_cost_from_structure(None), ExtractedCost::default());
    }

    #[test]
    fn extract_cost_drops_zero_usd_and_negatives() {
        // `claims.price_cents` has CHECK (IS NULL OR > 0); a 0-USD or
        // negative cost must stay NULL rather than violate it.
        let zero: serde_json::Value =
            serde_json::from_str(r#"{"cost":{"usd":0,"tokens":100}}"#).unwrap();
        let out = extract_cost_from_structure(Some(&zero));
        assert_eq!(out.tokens, Some(100));
        assert_eq!(out.price_cents, None);

        let neg: serde_json::Value =
            serde_json::from_str(r#"{"cost":{"usd":-1.5,"tokens":50}}"#).unwrap();
        let out = extract_cost_from_structure(Some(&neg));
        assert_eq!(out.tokens, Some(50));
        assert_eq!(out.price_cents, None);
    }

    #[test]
    fn extract_cost_clamps_huge_token_counts() {
        // u64::MAX would overflow i32; we clamp to i32::MAX rather
        // than reject so the row still gets the dollar value.
        let big: serde_json::Value =
            serde_json::from_str(r#"{"cost":{"tokens":99999999999,"usd":1.23}}"#).unwrap();
        let out = extract_cost_from_structure(Some(&big));
        assert_eq!(out.tokens, Some(i32::MAX));
        assert_eq!(out.price_cents, Some(123));
    }

    /// Regression for the silent sub-cent drop a 0.10.0-era counter-
    /// claim hit: `usd: 0.0005` was authored to satisfy KrabReviewer's
    /// `cost_transparency_missing_value` (which only fired on zero or
    /// null fields), but `(0.0005 * 100).round() == 0` cents fails the
    /// `> 0` check and stayed NULL. Documented in claim-structure.md
    /// and the reviewer prompt; pinning the boundary here so a future
    /// refactor that switches to `floor()` or relaxes the constraint
    /// doesn't silently regress the behavior. 0.005 is the smallest
    /// value that survives.
    #[test]
    fn extract_cost_drops_sub_half_cent_usd() {
        for (usd, expected) in [
            (0.0001f64, None),    // tiny — rounds to 0 cents
            (0.0049f64, None),    // just below the boundary
            (0.005f64, Some(1)),  // exactly at the boundary → 1 cent
            (0.0051f64, Some(1)), // just above
            (0.01f64, Some(1)),   // one full cent
            (0.5f64, Some(50)),   // 50 cents
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&format!(r#"{{"cost":{{"usd":{usd}}}}}"#)).unwrap();
            let out = extract_cost_from_structure(Some(&v));
            assert_eq!(
                out.price_cents, expected,
                "usd={usd} should produce price_cents={expected:?}"
            );
        }
    }
}
