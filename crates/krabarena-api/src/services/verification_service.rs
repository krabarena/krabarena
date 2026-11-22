use sqlx::PgPool;
use uuid::Uuid;

use crate::envelope::PaginationMeta;
use crate::error::ApiError;
use crate::models::verification::{CreateResponseRequest, VerificationResponse, VerificationRow};
use crate::pagination::{Cursor, decode_cursor, finalize_page};
use crate::services::battle_service;
use crate::services::gemini_service::GeminiClient;
use crate::services::verification_review_service::{self, VerificationVerdict};

/// Bundle returned by `create_verification` when a Gemini review
/// ran. The route surfaces this in the success-path JSON so callers
/// can see the review (including `pass` and `warn` rationales)
/// without a separate fetch.
#[derive(Debug, serde::Serialize)]
pub struct VerificationCreateOutput {
    pub verification: VerificationResponse,
    /// `None` when the review was synth-passed (no API key, transport
    /// hiccup) — the route returns the verification alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<crate::services::verification_review_service::VerificationReviewResult>,
}

/// 422-style failure: the Gemini review blocked this verification.
/// Mirrors the claim-block path.
#[derive(Debug, thiserror::Error)]
#[error("KrabReviewer blocked this verification")]
pub struct VerificationBlocked {
    pub review: crate::services::verification_review_service::VerificationReviewResult,
}

pub async fn create_verification(
    pool: &PgPool,
    gemini: &GeminiClient,
    claim_id: Uuid,
    author_id: Uuid,
    req: CreateResponseRequest,
) -> Result<Result<VerificationCreateOutput, VerificationBlocked>, ApiError> {
    let verdict = req.verdict.as_str();
    if verdict != "confirmed" && verdict != "disconfirmed" {
        return Err(ApiError::ValidationError(
            "verdict must be 'confirmed' or 'disconfirmed'".into(),
        ));
    }

    // Validate fields based on verdict
    if verdict == "confirmed" {
        if req.environment.as_ref().is_none_or(|s| s.is_empty()) {
            return Err(ApiError::MissingExecutionEvidence);
        }
        if req.output_hash.as_ref().is_none_or(|s| s.is_empty()) {
            return Err(ApiError::MissingExecutionEvidence);
        }
        if req.duration_ms.is_none_or(|d| d <= 0) {
            return Err(ApiError::MissingExecutionEvidence);
        }
    } else {
        if req.failure_reason.as_ref().is_none_or(|s| s.is_empty()) {
            return Err(ApiError::MissingFailureReason);
        }
        if req.conditions.as_ref().is_none_or(|s| s.is_empty()) {
            return Err(ApiError::ValidationError("conditions required".into()));
        }
        if req.reproduction_steps.as_ref().is_none_or(|s| s.is_empty()) {
            return Err(ApiError::ValidationError(
                "reproduction_steps required".into(),
            ));
        }
    }

    // Load claim text + author + battle so we can (a) authorize, (b)
    // run the Gemini review against the actual claim body, (c) link
    // the review row back to the same battle as the claim.
    // `battle_id` is Optional since migration 035 (standalone claims).
    // Battle-derived downstream logic (status recalc + temperature
    // refresh) is gated on the value being Some below.
    // The two slugs ride along for the `claim.verified` /
    // `claim.refuted` webhook payload (since 0.43.0) so the dispatch
    // doesn't cost a second round-trip.
    /// (author_id, battle_id, claim text, claim slug, battle slug, visibility)
    type ClaimLookup = (
        Uuid,
        Option<Uuid>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let claim_row: Option<ClaimLookup> = sqlx::query_as(
        "SELECT c.author_id, c.battle_id, c.claim, c.slug, b.slug, c.visibility \
             FROM claims c LEFT JOIN battles b ON b.id = c.battle_id \
             WHERE c.id = $1",
    )
    .bind(claim_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    let (claim_author, battle_id, claim_text, claim_slug, battle_slug, claim_visibility) =
        claim_row.ok_or(ApiError::ClaimNotFound)?;

    let is_author_response = claim_author == author_id;

    // Gemini sanity-check before we commit. A `block` short-circuits
    // and returns 422 with the review attached. `warn`/`pass` fall
    // through; the review row is persisted alongside the verification
    // for the audit trail and cost roll-ups.
    let verdict_enum = if verdict == "confirmed" {
        VerificationVerdict::Confirmed
    } else {
        VerificationVerdict::Disconfirmed
    };
    let review = verification_review_service::review_verification(
        gemini,
        &claim_text,
        verdict_enum,
        req.environment.as_deref(),
        req.output_hash.as_deref(),
        req.duration_ms,
        req.failure_reason.as_deref(),
        req.conditions.as_deref(),
        req.reproduction_steps.as_deref(),
    )
    .await?;

    if review.verdict == "block" {
        // Persist the block as a claim_reviews row so the audit
        // trail captures it even though no verification was created.
        // verification_id stays NULL because the verification was
        // never inserted.
        let _ = persist_verification_review(
            pool,
            battle_id,
            author_id,
            &claim_text,
            verdict_enum,
            None,
            &review,
        )
        .await;
        return Ok(Err(VerificationBlocked { review }));
    }
    let agent_backend = req.agent_backend.filter(|s| !s.is_empty());
    let agent_model = req.agent_model.filter(|s| !s.is_empty());

    // Wrap the verification INSERT and the matching `claims.*_count`
    // bump in a single transaction so we can't observe a verification
    // without its corresponding count tick (or vice versa) if a write
    // fails mid-flight. The count column name is whitelisted above.
    let count_col = if verdict == "confirmed" {
        "verify_count"
    } else {
        "refute_count"
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let row = sqlx::query_as::<_, VerificationRow>(
        r#"
        INSERT INTO verifications (claim_id, author_id, verdict, environment, output_hash, duration_ms,
                              failure_reason, conditions, reproduction_steps,
                              is_author_response, agent_backend, agent_model)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
        RETURNING *
        "#,
    )
    .bind(claim_id)
    .bind(author_id)
    .bind(verdict)
    .bind(&req.environment)
    .bind(&req.output_hash)
    .bind(req.duration_ms)
    .bind(&req.failure_reason)
    .bind(&req.conditions)
    .bind(&req.reproduction_steps)
    .bind(is_author_response)
    .bind(&agent_backend)
    .bind(&agent_model)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    // Bump `updated_at` alongside the count — battle pages sort
    // claims by updated_at DESC since 0.8.20, and a fresh
    // verification/refutation is engagement worth surfacing.
    sqlx::query(&format!(
        "UPDATE claims SET {} = {} + 1, updated_at = NOW() WHERE id = $1",
        count_col, count_col
    ))
    .bind(claim_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    // Reward the claim author when a THIRD PARTY confirms their claim.
    // A decoupled credit (see services::payout_service): 1 Toncoin per
    // verification, capped at 5 per claim counting pending+paid only (a
    // rejected payout frees the slot). Never for self-verifications and
    // never for refutes. The claim row was just locked by the UPDATE
    // above, so the cap check is race-safe against concurrent
    // verifications of the same claim. Idempotent on the verification id,
    // and part of this same transaction so the credit can't outlive a
    // rolled-back verification.
    if verdict == "confirmed" && !is_author_response {
        const PER_VERIFICATION: i64 = 1;
        const PER_CLAIM_CAP: i64 = 5;
        let credited = crate::services::payout_service::subject_credited_total(
            &mut *tx,
            "claim_verified",
            "claim",
            claim_id,
        )
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
        if credited + PER_VERIFICATION <= PER_CLAIM_CAP {
            let verifier_name: Option<(String,)> =
                sqlx::query_as("SELECT display_name FROM users WHERE id = $1")
                    .bind(author_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| ApiError::Internal(e.into()))?;
            let who = verifier_name
                .map(|(name,)| name)
                .unwrap_or_else(|| "another agent".to_string());
            crate::services::payout_service::credit(
                &mut *tx,
                crate::services::payout_service::CreditRequest {
                    user_id: claim_author,
                    amount: PER_VERIFICATION,
                    event_type: "claim_verified",
                    subject_type: Some("claim"),
                    subject_id: Some(claim_id),
                    context: serde_json::json!({
                        "verification_id": row.id,
                        "verifier_user_id": author_id,
                    }),
                    reason: format!("Verification of your claim by {who}"),
                    dedup_key: Some(format!("claim_verified:{}", row.id)),
                },
            )
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        }
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    // Recalculate battle status + temperature when attached. For
    // standalone claims (no Battle), both are a no-op.
    if let Some(bid) = battle_id {
        battle_service::recalculate_hole_status(pool, bid).await?;
        if let Ok(events) = crate::services::temperature::fetch_battle_events(pool, bid).await {
            let temp = crate::services::temperature::compute(&events, chrono::Utc::now());
            let _ = sqlx::query("UPDATE battles SET temperature = $1 WHERE id = $2")
                .bind(temp)
                .bind(bid)
                .execute(pool)
                .await;
        }
    }

    // Persist the review row now that we have a verification_id.
    // Best-effort — a failure here is logged but doesn't roll back
    // the verification (the verification is the user-visible artifact,
    // the review is metadata).
    if let Err(e) = persist_verification_review(
        pool,
        battle_id,
        author_id,
        &claim_text,
        verdict_enum,
        Some(row.id),
        &review,
    )
    .await
    {
        tracing::warn!(error = %e, "verification.review.persist_failed");
    }

    // Fire `claim.verified` / `claim.refuted` (since 0.43.0). Same
    // fire-and-forget + HMAC + delivery bookkeeping as `claim.posted`:
    // `dispatch_event` spawns internally, so subscriber latency never
    // reaches the verifier's response. This is the single insert path
    // for verifications — the REST route, and every admin/MCP surface
    // that records a verify/refute, go through `create_verification`.
    if let Some((event, payload)) = crate::services::webhook_payloads::verification_webhook(
        &crate::services::webhook_payloads::VerificationEventInput {
            verification_id: row.id,
            verdict,
            verifier_id: author_id,
            is_author_response,
            agent_backend: row.agent_backend.as_deref(),
            agent_model: row.agent_model.as_deref(),
            created_at: row.created_at,
            claim_id,
            claim_slug: claim_slug.as_deref(),
            claim_author_id: claim_author,
            claim_text: &claim_text,
            battle_id,
            battle_slug: battle_slug.as_deref(),
        },
    ) {
        // The payload carries the claim text — a private claim's
        // verdict goes only to its author's / admins' subscriptions.
        crate::services::webhook_dispatch::dispatch_event_to(
            pool.clone(),
            event,
            payload,
            crate::services::webhook_dispatch::Audience::for_claim(
                claim_visibility.as_deref(),
                claim_author,
            ),
        );
    }
    // Instant email to the Claim author (since 0.43.0).
    crate::services::notification_email::emit(
        pool,
        crate::services::notification_email::NotificationEvent::Verification {
            verification_id: row.id,
        },
    );

    tracing::info!(
        response_id = %row.id,
        claim_id = %claim_id,
        verdict = verdict,
        author_id = %author_id,
        review_verdict = %review.verdict,
        review_cost_micros = review.usage.cost_micros,
        "response.recorded"
    );

    let review_for_response = if review.model == "none" {
        None
    } else {
        Some(review)
    };
    Ok(Ok(VerificationCreateOutput {
        verification: VerificationResponse::from(row),
        review: review_for_response,
    }))
}

/// Insert a `claim_reviews` row for a verify/refute review. Used on
/// both the block path (before the verification exists,
/// `verification_id=None`) and the success path
/// (`verification_id=Some(id)`).
async fn persist_verification_review(
    pool: &PgPool,
    battle_id: Option<Uuid>,
    user_id: Uuid,
    claim_text: &str,
    verdict_kind: VerificationVerdict,
    verification_id: Option<Uuid>,
    review: &crate::services::verification_review_service::VerificationReviewResult,
) -> Result<(), sqlx::Error> {
    // For verify/refute reviews we don't have an artifact upload.
    // `claim` carries the claim body the verification was about,
    // `artifact_hash`/`artifact_size` get sentinel values so the
    // NOT NULL constraints from migration 005 are satisfied.
    let suggestions_json =
        serde_json::to_value(&review.suggestions).unwrap_or(serde_json::Value::Array(Vec::new()));
    let prompt_tokens = if review.model == "none" {
        None
    } else {
        Some(review.usage.prompt_tokens.min(i32::MAX as u32) as i32)
    };
    let completion_tokens = if review.model == "none" {
        None
    } else {
        Some(review.usage.completion_tokens.min(i32::MAX as u32) as i32)
    };
    let cost_micros = if review.model == "none" {
        None
    } else {
        Some(review.usage.cost_micros)
    };

    sqlx::query(
        r#"
        INSERT INTO claim_reviews (
            battle_id, user_id, claim, artifact_hash, artifact_size,
            model, verdict, summary, suggestions, moderation_flags,
            prompt_tokens, completion_tokens, cost_micros,
            review_target, verification_id
        )
        VALUES ($1, $2, $3, '', 0,
                $4, $5, $6, $7, '[]'::jsonb,
                $8, $9, $10,
                $11, $12)
        "#,
    )
    .bind(battle_id)
    .bind(user_id)
    .bind(claim_text)
    .bind(&review.model)
    .bind(&review.verdict)
    .bind(&review.summary)
    .bind(&suggestions_json)
    .bind(prompt_tokens)
    .bind(completion_tokens)
    .bind(cost_micros)
    .bind(verdict_kind.review_target())
    .bind(verification_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub fn normalize_verdict_filter(raw: Option<&str>) -> Result<Option<&'static str>, ApiError> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    krabarena_shared::domain::Verdict::from_db(&raw.to_ascii_lowercase())
        .map(|v| Some(v.as_db()))
        .ok_or_else(|| {
            ApiError::ValidationError(format!(
                "verdict must be one of: verified, refuted (aliases: confirmed, disconfirmed; got '{raw}')"
            ))
        })
}

pub async fn list_verifications(
    pool: &PgPool,
    claim_id: Uuid,
    verdict: Option<&str>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<VerificationResponse>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cur = decode_cursor(cursor);

    // Keyset on `(created_at, id)` — see `crate::pagination`.
    let rows = sqlx::query_as::<_, VerificationRow>(
        r#"
        SELECT * FROM verifications
        WHERE claim_id = $1
          AND ($2::text IS NULL OR verdict = $2)
          AND ($3::timestamptz IS NULL OR created_at < $3
               OR (created_at = $3 AND id < $4))
        ORDER BY created_at DESC, id DESC
        LIMIT $5
        "#,
    )
    .bind(claim_id)
    .bind(verdict)
    .bind(cur.map(|c| c.ts))
    .bind(cur.map(|c| c.id))
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let items: Vec<VerificationResponse> =
        rows.into_iter().map(VerificationResponse::from).collect();
    Ok(finalize_page(items, limit, |v: &VerificationResponse| {
        Cursor::new(v.created_at, v.id)
    }))
}
