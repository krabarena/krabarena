//! Review orchestration.
//!
//! Flow:
//!   1. Load Battle context (title, description, existing claims).
//!   2. Run deterministic pre-filter (secrets regex, duplicate-hash, flood).
//!      If anything fires, emit a synthetic `block` review without calling
//!      Gemini.
//!   3. Otherwise, call Gemini Flash via [`gemini_service`].
//!   4. Resolve the verdict server-side (model's `moderation_flags` → block;
//!      any suggestion warn → warn; else pass).
//!   5. Persist the review row.
//!   6. On block: record a strike, evaluate [`moderation_policy`], and apply
//!      the enforcement action to the user (suspend / rate-limit).

use std::time::Instant;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::models::review::{
    ClaimReviewRow, Highlight, ModerationFlag, ModerationStatus, ModerationStrike,
    ModerationStrikeRow, RateLimitInfo, ReviewOutcome, ReviewResponse, ReviewVerdict,
    SuspensionInfo,
};
use crate::services::agentic_reviewer::{self, AgenticBudget, AgenticReviewInputs};
use crate::services::gemini_service::{
    BattleContext, ExistingClaimBrief, GeminiClient, GeminiError, Usage,
};
// Local empty-vec placeholder for the cross-claim context that
// `run_review` no longer loads (since 0.8.15).
use crate::services::gemini_service::ClaimStructureBrief;
use crate::services::moderation_policy::{self, Enforcement, FlagCode, PastStrike, Severity};
use crate::services::presentation_check;
use crate::services::review_tools::FileMap;
use krabarena_shared::claim_structure::{ClaimStructure, ParseError};

/// `og_summary` hard cap per the reviewer spec (Section D) — mirrors
/// `presentation_gen::OG_SUMMARY_MAX_CHARS`.
const OG_SUMMARY_MAX_CHARS: usize = 200;

/// Input shared by dry-run review and inline (publish-time) review.
pub struct ReviewInputs<'a> {
    /// `None` for standalone claims (no Battle context). The
    /// reviewer drops to a structure + quality + on-topic check
    /// against the claim's `topic` instead of the battle's
    /// rubric. See migration 035 + `crate::routes::claims`
    /// standalone-create handler.
    pub battle_id: Option<Uuid>,
    /// `topic` from a standalone claim — surfaced to the reviewer
    /// so the on-topic check has a target. `None` for
    /// Battle-attached claims.
    pub topic: Option<&'a str>,
    pub user_id: Uuid,
    pub claim: &'a str,
    pub artifact_bytes: &'a [u8],
    pub artifact_hash: &'a str,
    pub artifact_filename: Option<&'a str>,
    pub predecessor_claim: Option<&'a str>,
    /// Predecessor's structured benchmark + leaderboard (the JSON
    /// already stored on the row). Passed in pre-rendered so the
    /// reviewer can frame the new claim's `summary` and
    /// `battle_executive_summary` as a diff against the previous
    /// ranking instead of writing them in isolation.
    pub predecessor_structure: Option<&'a str>,
    /// Inline structured claim JSON (multipart `claim_structure`
    /// field, or the contents of `structure.json` from the artifact
    /// bundle). The pre-filter parses + validates this; a missing,
    /// unparseable, or field-rule-violating structure declines the
    /// publish deterministically (since 0.18.7 — see
    /// `check_claim_structure_and_sections`).
    pub claim_structure: Option<&'a str>,
    /// Agent metadata the submitter sent alongside the claim. Passed
    /// through to the reviewer so the "Submitter" block in the prompt
    /// reflects the agent actually submitting.
    pub agent_backend: Option<&'a str>,
    pub agent_model: Option<&'a str>,
    /// If true, skip strike recording and enforcement (dry-run mode).
    pub dry_run: bool,
}

/// Result of running a review — both the stored record and its resolved
/// verdict enum.
pub struct ReviewOutput {
    pub response: ReviewResponse,
    #[allow(dead_code)] // returned for future callers that branch on the enum
    pub verdict: ReviewVerdict,
}

/// Entry point: run the full review pipeline and persist the outcome.
pub async fn run_review(
    pool: &PgPool,
    gemini: &GeminiClient,
    inputs: ReviewInputs<'_>,
) -> Result<ReviewOutput, ApiError> {
    if inputs.claim.trim().is_empty() {
        return Err(ApiError::ValidationError("claim is required".into()));
    }
    if inputs.artifact_bytes.is_empty() {
        return Err(ApiError::ClaimMissingArtifact);
    }

    let started = Instant::now();

    // --- 1. Load Battle + existing claims --------------------------------
    // For standalone claims (battle_id None), synthesise a minimal
    // BattleContext keyed off the claim's `topic` so the reviewer's
    // prompt still has a question to assess on-topic against. The
    // existing-claims list is empty since there's no Battle to scope
    // it to. The reviewer's no-battle path explicitly skips the
    // battle-rubric checks; see `agentic_reviewer::run_agentic_review`.
    let battle = match inputs.battle_id {
        Some(bid) => sqlx::query_as::<_, (String, String, String, Option<String>)>(
            "SELECT title, description, status, executive_summary FROM battles WHERE id = $1",
        )
        .bind(bid)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::BattleNotFound)?,
        None => {
            // Synthetic BattleContext for the standalone path. The
            // `title` shows up in the reviewer prompt as the question
            // the claim is answering; for a standalone claim that's
            // exactly `topic`.
            let topic = inputs.topic.unwrap_or("(no topic)");
            (
                topic.to_string(),
                "Standalone Claim — no curated Battle yet. The claim's `topic` field is the \
                 question; review structure + quality + on-topic only. Skip battle-rubric checks."
                    .to_string(),
                "standalone".to_string(),
                None,
            )
        }
    };

    let existing_claims = match inputs.battle_id {
        Some(bid) => sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, claim FROM claims WHERE battle_id = $1 ORDER BY created_at DESC LIMIT 10",
        )
        .bind(bid)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?,
        None => Vec::new(),
    };

    // Cross-claim context (top-N structures + READMEs) used to be
    // loaded here so the reviewer could produce
    // `battle_executive_summary` as a side effect of every claim
    // review. Decoupled in 0.8.15: review judges THIS claim against
    // the rubric only; the battle summary is updated separately by
    // an async `battle_summarizer::regenerate_summary(.., Some(id))`
    // call from the claim post handler after a successful publish.
    // Net win per review: ~10 GCS bundle downloads dropped + ~100 KB
    // of prompt context dropped.
    let claim_structures: Vec<ClaimStructureBrief> = Vec::new();

    // --- 2. Deterministic pre-filter ----------------------------------
    let mut pre_flags: Vec<ModerationFlag> = Vec::new();

    // Duplicate-artifact detection across the Battle. When found, we
    // fetch the colliding Claim's id + slug + author so the flag can
    // tell the user *which* Claim they're duplicating (and whether
    // it's their own earlier post or someone else's), with a direct
    // link. Without this, callers just see "byte-equivalent to an
    // existing Claim" and have to go hunt for it.
    // `IS NOT DISTINCT FROM` makes the `NULL = NULL` comparison true,
    // so a standalone claim (battle_id None) compares against the
    // other standalone rows. A Battle-attached post still scopes to
    // its Battle.
    let duplicate: Option<(Uuid, Option<String>, Uuid)> = sqlx::query_as(
        "SELECT id, slug, author_id FROM claims \
         WHERE battle_id IS NOT DISTINCT FROM $1 AND artifact_hash = $2 \
         ORDER BY created_at ASC LIMIT 1",
    )
    .bind(inputs.battle_id)
    .bind(inputs.artifact_hash)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    if let Some((dup_id, dup_slug, dup_author)) = duplicate {
        let link = format!(
            "https://krabarena.com/claims/{}",
            dup_slug.as_deref().unwrap_or(&dup_id.to_string())
        );
        let reason = if dup_author == inputs.user_id {
            format!("you have already posted this Claim: {link}")
        } else {
            format!("another agent already posted this Claim: {link}")
        };
        pre_flags.push(ModerationFlag {
            code: FlagCode::Duplicate.as_str().into(),
            reason,
            file: None,
            line: None,
            autofix_hint: Some(format!(
                "inspect {link}; modify the artifact to change the hash, or use \
                 --predecessor {dup_id} to post a refinement of it"
            )),
        });
    }

    // Author-flood counter (Claims by this user on this Battle in the last 1h).
    // For standalone claims, scopes to all standalone rows by the
    // same author (battle_id NULL).
    let author_recent_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM claims \
         WHERE battle_id IS NOT DISTINCT FROM $1 \
           AND author_id = $2 AND created_at > NOW() - INTERVAL '1 hour'",
    )
    .bind(inputs.battle_id)
    .bind(inputs.user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    if author_recent_count > 3 {
        pre_flags.push(ModerationFlag {
            code: FlagCode::Flooding.as_str().into(),
            reason: format!("{author_recent_count} Claims on this Battle in the last hour"),
            file: None,
            line: None,
            autofix_hint: Some("wait before submitting more Claims to this Battle".into()),
        });
    }

    // Regex secret scan — returns structured flags with line numbers and autofix hints.
    pre_flags.extend(scan_for_secrets(inputs.artifact_bytes));

    // Artifact structure check: every artifact bundle must include
    // `RUN.md` (exact reproduction steps) AND `README.md` (extended
    // report). Missing either is a Minor flag (honest author mistake,
    // trivially fixable) that still blocks the publish so the Claim
    // ships with the canonical structure. Runs on the already-bounded
    // FileMap, so a decompression bomb doesn't turn into a reviewer
    // OOM here.
    let filename_for_map = inputs.artifact_filename.unwrap_or("");
    let early_files = if filename_for_map.ends_with(".tar.gz") || filename_for_map.ends_with(".tgz")
    {
        // Surface extraction failures explicitly — silently dropping
        // the error and falling through to single-file wrapping makes
        // the downstream "missing README.md" flag misleading (the
        // README was present, but FileMap bailed on a size cap or
        // zip-bomb guard before walking past the oversized entry).
        // Bot authors hit this when they bundle raw upstream proxy
        // responses; clearer flags shave hours off the debug loop.
        match FileMap::from_tar_gz(inputs.artifact_bytes) {
            Ok(m) => Some(m),
            Err(e) => {
                pre_flags.push(crate::models::review::ModerationFlag {
                    code: "artifact_oversized".into(),
                    reason: format!(
                        "Artifact extraction failed: {e}. The reviewer applies \
                         per-file (5 MB), total-uncompressed (50 MB), \
                         compressed (50 MB), and entry-count (1000) caps to \
                         keep ingestion memory-safe. Trim the offending \
                         entry — for raw upstream proxy responses, keep just \
                         the per-query summary fields (query, latency_ms, \
                         answer_text) rather than the full response payload."
                    ),
                    file: None,
                    line: None,
                    autofix_hint: Some(
                        "Reduce the bundle's largest results/*.json (or \
                         similar) entry below 5 MB by dropping verbose \
                         provider internals (choices[].logprobs, tool_calls, \
                         system_fingerprint, etc.) and keeping only the \
                         per-row fields you'll actually cite as evidence."
                            .into(),
                    ),
                });
                None
            }
        }
    } else {
        Some(FileMap::from_single_file(
            inputs.artifact_bytes,
            inputs.artifact_filename,
        ))
    };
    if let Some(ref files) = early_files {
        pre_flags.extend(check_required_artifact_files(files));
    }

    // Deterministic structure + required-section gate (0.18.7). A Claim
    // with no machine-readable leaderboard, a malformed/invalid
    // `structure.json`, or missing required markdown sections trips this
    // here — before Gemini — so the *form* bar doesn't depend on a flaky
    // LLM pass. Kept SEPARATE from `pre_flags`: these are an OVERRIDABLE
    // `warn` (a publisher who passes `--ignore-review` can still ship a
    // form-imperfect Claim), whereas the real moderation pre-flags
    // (secrets, oversized, missing RUN.md/README.md) below are a hard,
    // non-overridable `block`. All structure codes are NOT in `FlagCode`
    // (same as `artifact_oversized`), so they never record a moderation
    // strike either way — broken form is an honest, fixable mistake, not
    // abuse. See `check_claim_structure_and_sections`.
    let structure_flags = check_claim_structure_and_sections(
        inputs.claim,
        inputs.claim_structure,
        early_files.as_ref(),
    );

    // Same source `check_claim_structure_and_sections` used above: the
    // inline `claim_structure` field, else `structure.json` from the
    // bundle FileMap. Captured here (before `early_files` goes out of
    // scope) so the presentation grounding check below can be run
    // against the identical raw string, not a re-derived one.
    let structure_raw: Option<String> = inputs
        .claim_structure
        .map(str::to_string)
        .or_else(|| early_files.as_ref().and_then(bundle_structure_raw));

    let mut outcome: ReviewOutcome;
    let model_name: String;
    // Gemini token usage + estimated $ cost. Populated only when we
    // actually call Gemini; the prefilter / synthetic-pass paths
    // record zeros so cost roll-ups don't double-count anything.
    let mut usage = Usage::default();

    if let Some((verdict, summary, flags)) = resolve_prefilter_verdict(pre_flags, structure_flags) {
        // Short-circuit on the deterministic pre-filter — no Gemini call.
        // (For an overridable `warn` the publish handler returns 409 unless
        // `--ignore-review` is set; an overridden form-imperfect Claim then
        // ships with the warnings on record and no AI summary — the author
        // opted into that.)
        outcome = ReviewOutcome {
            verdict: Some(verdict.into()),
            summary: Some(summary.into()),
            artifact_summary: None,
            artifact_highlights: Vec::new(),
            suggestions: Vec::new(),
            moderation_flags: flags,
            og_summary: None,
            presentation: None,
        };
        model_name = "prefilter".into();
    } else {
        // --- 3. Gemini call --------------------------------------------
        let battle_ctx = BattleContext {
            title: battle.0,
            description: battle.1,
            status: battle.2,
            existing_claims: existing_claims
                .into_iter()
                .map(|(id, claim)| ExistingClaimBrief {
                    id: id.to_string(),
                    claim,
                })
                .collect(),
            claim_structures,
            previous_summary: battle.3,
        };

        // Build an in-memory FileMap of the artifact. Tarballs get
        // safely extracted (bounded, no symlinks, no zip-slip); anything
        // else is wrapped as a single-entry map. The agentic reviewer
        // then decides for itself which files are worth reading.
        let filename = inputs.artifact_filename.unwrap_or("");
        let files = if filename.ends_with(".tar.gz") || filename.ends_with(".tgz") {
            match FileMap::from_tar_gz(inputs.artifact_bytes) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "tar.gz extraction failed; wrapping as single file");
                    FileMap::from_single_file(inputs.artifact_bytes, inputs.artifact_filename)
                }
            }
        } else {
            FileMap::from_single_file(inputs.artifact_bytes, inputs.artifact_filename)
        };

        // Join author + stats so the reviewer sees who is submitting
        // and what their track record looks like. Best-effort — if the
        // lookup fails we still run, just without the submitter block.
        // `agent_backend` / `agent_model` flow through `ReviewInputs`
        // separately; they're treated as plain claim variables, not
        // fields on `ClaimAuthor`.
        let author = super::claim_service::load_claim_author(pool, inputs.user_id)
            .await
            .ok()
            .flatten();

        let agentic_inputs = AgenticReviewInputs {
            battle: &battle_ctx,
            claim: inputs.claim,
            predecessor_claim: inputs.predecessor_claim,
            predecessor_structure: inputs.predecessor_structure,
            claim_structure: inputs.claim_structure,
            files: &files,
            duplicate_artifact_hash_match: false,
            author_recent_claim_count: author_recent_count as u32,
            author: author.as_ref(),
            agent_backend: inputs.agent_backend,
            agent_model: inputs.agent_model,
        };

        // Load the KrabReviewer rubric from DB (admin-configurable) or
        // fallback to compiled-in. The agent-loop layer then appends
        // the tool-use protocol and untrusted-content contract.
        let rubric = super::config_service::get_review_prompt(pool).await;

        match agentic_reviewer::run_agentic_review(
            gemini,
            agentic_inputs,
            &rubric,
            AgenticBudget::default(),
        )
        .await
        {
            Ok(result) => {
                outcome = result.outcome;
                usage = result.usage;
                model_name = gemini.model().to_string();
            }
            Err(GeminiError::NoApiKey) => {
                tracing::warn!("GEMINI_API_KEY not set — issuing synthetic pass review (dev mode)");
                outcome = synthetic_pass_outcome(inputs.artifact_filename);
                model_name = "none".into();
            }
            Err(e) => {
                // Gemini transport hiccups, parse errors, and "exhausted
                // turn budget" all land here. They're external-dependency
                // failures, not our bug — return 503 REVIEWER_UNAVAILABLE
                // so the CLI can present a "try again in a minute"
                // message and the alerting stops treating transient
                // upstream failures as our 500s. Downgrade to warn:
                // tracing::error! triggers prod alerts we don't want
                // to page on for every Gemini blip.
                tracing::warn!(error = %e, "agentic review unavailable");
                return Err(ApiError::ReviewerUnavailable(e.to_string()));
            }
        }
    }

    // --- 4. Resolve verdict -------------------------------------------
    let mut verdict = resolve_verdict(&outcome);

    // Effort floor (overridable warn). The home feed ranks by author-compute
    // "effort dollars" — structure.json `cost.usd` -> `claims.price_cents`. A
    // Claim that declares no own-compute cost/tokens registers as $0 and never
    // surfaces on the effort-ranked feed (a common oversight: reporting only
    // `external_usd`, which is aggregated separately and does NOT count as
    // effort). Applied HERE, after the full review resolved, so a missing
    // `cost` never short-circuits the substantive Gemini pass — an off-topic
    // Claim still hard-blocks on its merits. Records the flag for the author
    // and upgrades a clean `pass` to an overridable `warn`, but NEVER
    // downgrades a real block/warn.
    if declares_no_effort(structure_raw.as_deref()) {
        outcome.moderation_flags.push(ModerationFlag {
            code: "no_reported_effort".into(),
            reason: "structure.json cost.usd (and cost.tokens) is missing or zero, so this \
                     Claim declares no author-compute effort — it registers as $0 and won't \
                     surface on the effort-ranked feed. Report your own compute in cost.usd; \
                     external API spend goes in cost.external_usd (which does not count as \
                     effort)."
                .into(),
            file: Some("structure.json".into()),
            line: None,
            autofix_hint: Some(
                "set structure.json cost.usd to your own compute cost in USD (or cost.tokens); \
                 see https://krabarena.com/docs/claim-structure"
                    .into(),
            ),
        });
        if verdict == ReviewVerdict::Pass {
            verdict = ReviewVerdict::Warn;
        }
    }

    // --- 5. Persist ---------------------------------------------------
    let suggestions_json = serde_json::to_value(&outcome.suggestions).unwrap_or_default();
    let moderation_flags_json = serde_json::to_value(&outcome.moderation_flags).unwrap_or_default();
    let highlights_json = if outcome.artifact_highlights.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::to_value(&outcome.artifact_highlights).unwrap_or(serde_json::Value::Null)
    };

    let latency_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;

    // Persist cost only when we actually called Gemini. Prefilter
    // blocks and synthetic-pass dev-mode rows leave the cost columns
    // NULL so roll-ups don't credit them with token spend they
    // didn't incur.
    let (prompt_tokens_val, completion_tokens_val, cost_micros_val) =
        if model_name == "prefilter" || model_name == "none" {
            (None::<i32>, None::<i32>, None::<i64>)
        } else {
            (
                Some(usage.prompt_tokens.min(i32::MAX as u32) as i32),
                Some(usage.completion_tokens.min(i32::MAX as u32) as i32),
                Some(usage.cost_micros),
            )
        };

    // Deterministic grounding post-check (0.37.0): drop any presentation
    // block whose headline numbers aren't traceable back to
    // structure.json OR the claim markdown — best-effort decoration,
    // never a publish blocker.
    let had_presentation = outcome.presentation.is_some();
    outcome.presentation = outcome.presentation.take().and_then(|p| {
        presentation_check::sanitize_presentation(p, structure_raw.as_deref(), Some(inputs.claim))
    });
    if had_presentation && outcome.presentation.is_none() {
        tracing::warn!(
            battle_id = ?inputs.battle_id,
            user_id = %inputs.user_id,
            "presentation dropped: failed deterministic grounding check"
        );
    }

    let presentation_json = outcome
        .presentation
        .as_ref()
        .and_then(|p| serde_json::to_value(p).ok());

    // `og_summary` hard cap per the reviewer spec (Section D) — the
    // model is instructed to stay under this, but nothing upstream
    // enforces it before now. Same helper `presentation_gen` uses so
    // the two callers of this spec can't drift.
    let og_summary_capped = outcome
        .og_summary
        .as_deref()
        .map(|s| presentation_check::truncate_chars(s, OG_SUMMARY_MAX_CHARS));

    let row = sqlx::query_as::<_, ClaimReviewRow>(
        r#"
        INSERT INTO claim_reviews (
            battle_id, user_id, claim, artifact_hash, artifact_size, model, verdict,
            summary, artifact_summary, artifact_highlights, suggestions,
            moderation_flags, latency_ms,
            prompt_tokens, completion_tokens, cost_micros, review_target,
            og_summary, presentation
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                $14, $15, $16, 'claim', $17, $18)
        RETURNING *
        "#,
    )
    .bind(inputs.battle_id)
    .bind(inputs.user_id)
    .bind(inputs.claim)
    .bind(inputs.artifact_hash)
    .bind(inputs.artifact_bytes.len() as i64)
    .bind(&model_name)
    .bind(verdict.as_str())
    .bind(outcome.summary.as_deref())
    .bind(outcome.artifact_summary.as_deref())
    .bind(&highlights_json)
    .bind(&suggestions_json)
    .bind(&moderation_flags_json)
    .bind(latency_ms)
    .bind(prompt_tokens_val)
    .bind(completion_tokens_val)
    .bind(cost_micros_val)
    .bind(og_summary_capped.as_deref())
    .bind(&presentation_json)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    // --- 6. On block: strike + enforcement (publish only, not dry-run) -
    if verdict == ReviewVerdict::Block && !inputs.dry_run {
        if let Some(enforcement) =
            record_strikes_and_evaluate(pool, inputs.user_id, row.id, &outcome.moderation_flags)
                .await?
        {
            apply_enforcement(pool, inputs.user_id, enforcement).await?;
        }
    }

    // Summary write moved to the claim post handler (since 0.8.12) so
    // that --review-id retries — which bypass `run_review` and reuse
    // the cached `ReviewResponse` — also bump the executive summary +
    // temperature. Search for `battle_summarizer::persist_summary` in
    // `routes/claims.rs` for the single call site.

    tracing::info!(
        review_id = %row.id,
        battle_id = ?inputs.battle_id,
        user_id = %inputs.user_id,
        verdict = verdict.as_str(),
        model = %model_name,
        latency_ms = latency_ms,
        "claim.review.completed"
    );

    let response = ReviewResponse::from(row);
    Ok(ReviewOutput { response, verdict })
}

/// Fetch a previously stored review by ID.
pub async fn get_review(pool: &PgPool, review_id: Uuid) -> Result<ReviewResponse, ApiError> {
    let row = sqlx::query_as::<_, ClaimReviewRow>("SELECT * FROM claim_reviews WHERE id = $1")
        .bind(review_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or_else(|| ApiError::ValidationError("review not found".into()))?;
    Ok(ReviewResponse::from(row))
}

/// Fetch the review attached to a published Claim, if any.
pub async fn get_review_for_shell(
    pool: &PgPool,
    claim_id: Uuid,
) -> Result<Option<ReviewResponse>, ApiError> {
    let review_id: Option<Uuid> = sqlx::query_scalar("SELECT review_id FROM claims WHERE id = $1")
        .bind(claim_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .flatten();
    match review_id {
        Some(id) => Ok(Some(get_review(pool, id).await?)),
        None => Ok(None),
    }
}

/// Full moderation status for `/v1/users/me/moderation`.
pub async fn moderation_status(pool: &PgPool, user_id: Uuid) -> Result<ModerationStatus, ApiError> {
    let strikes: Vec<ModerationStrikeRow> = sqlx::query_as(
        "SELECT * FROM moderation_strikes WHERE user_id = $1 ORDER BY created_at DESC LIMIT 50",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let user_state = sqlx::query_as::<
        _,
        (
            Option<chrono::DateTime<chrono::Utc>>,
            Option<String>,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    >(
        "SELECT suspended_until, suspension_reason, rate_limit_until FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let (suspended_until, suspension_reason, rate_limit_until) =
        user_state.unwrap_or((None, None, None));
    let now = Utc::now();

    Ok(ModerationStatus {
        strikes: strikes.into_iter().map(ModerationStrike::from).collect(),
        active_suspension: suspended_until
            .filter(|t| *t > now)
            .map(|until| SuspensionInfo {
                until,
                reason: suspension_reason.unwrap_or_else(|| "suspended".into()),
            }),
        active_rate_limit: rate_limit_until
            .filter(|t| *t > now)
            .map(|until| RateLimitInfo { until }),
    })
}

/// Check whether the user is currently suspended or rate-limited; returns
/// the machine-readable description if so.
pub async fn enforcement_state(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<EnforcementState>, ApiError> {
    let row = sqlx::query_as::<
        _,
        (
            Option<chrono::DateTime<chrono::Utc>>,
            Option<String>,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    >(
        "SELECT suspended_until, suspension_reason, rate_limit_until FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let Some((suspended_until, reason, rate_limit_until)) = row else {
        return Ok(None);
    };
    let now = Utc::now();
    if let Some(until) = suspended_until
        && until > now
    {
        return Ok(Some(EnforcementState::Suspended {
            until,
            reason: reason.unwrap_or_else(|| "suspended".into()),
        }));
    }
    if let Some(until) = rate_limit_until
        && until > now
    {
        // Build the "why" string at check time so the 400 we return
        // can tell the caller which strikes tripped the rule, not
        // just that one did. Looks like "5 minor (low_effort×3,
        // off_topic×2), 1 major (duplicate)".
        let strike_summary = summarize_recent_strikes(pool, user_id).await?;
        return Ok(Some(EnforcementState::RateLimited {
            until,
            strike_summary,
        }));
    }
    Ok(None)
}

/// Build a human-readable one-liner of the user's moderation strikes
/// in the last 24h for inclusion in rate-limit error messages.
/// Empty string if there are none (shouldn't happen when the user is
/// rate-limited, but handled for safety).
async fn summarize_recent_strikes(pool: &PgPool, user_id: Uuid) -> Result<String, ApiError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT severity, code FROM moderation_strikes \
         WHERE user_id = $1 AND created_at > NOW() - INTERVAL '24 hours'",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(render_strike_summary(&rows))
}

/// Pure renderer — split from the DB-fetching wrapper so it can be
/// unit-tested without a connection. Input is
/// `[(severity, code), …]`; output is a single line like
/// `"5 minor (low_effort×3, off_topic×2), 1 major (duplicate)"`.
fn render_strike_summary(rows: &[(String, String)]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    // Group: severity → code → count. BTreeMap gives deterministic
    // iteration order (alphabetical by code within each severity) for
    // stable test fixtures and log lines.
    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    for (sev, code) in rows {
        *buckets
            .entry(sev.as_str())
            .or_default()
            .entry(code.clone())
            .or_insert(0) += 1;
    }

    // Order: minor → major → severe, matching the escalation ladder.
    let order = ["minor", "major", "severe"];
    let parts: Vec<String> = order
        .iter()
        .filter_map(|sev| buckets.get(*sev).map(|codes| (sev, codes)))
        .map(|(sev, codes)| {
            let total: usize = codes.values().sum();
            let breakdown: Vec<String> = codes
                .iter()
                .map(|(code, count)| {
                    if *count == 1 {
                        code.clone()
                    } else {
                        format!("{code}×{count}")
                    }
                })
                .collect();
            format!("{total} {sev} ({})", breakdown.join(", "))
        })
        .collect();
    parts.join(", ")
}

#[derive(Debug, Clone)]
pub enum EnforcementState {
    Suspended {
        until: chrono::DateTime<chrono::Utc>,
        reason: String,
    },
    RateLimited {
        until: chrono::DateTime<chrono::Utc>,
        /// Precomputed summary like `"5 minor (low_effort×3, off_topic×2), 1 major (duplicate)"`
        /// of the user's last-24h strikes, for inclusion in the 400
        /// the route returns.
        strike_summary: String,
    },
}

// -- Helpers ----------------------------------------------------------------

/// A moderation flag code that the deterministic structure/section
/// pre-filter emits — an OVERRIDABLE form problem, not a hard block.
/// Anything NOT in this set (real moderation: secret/pii/…, plus the
/// non-strike-but-hard `artifact_oversized` and `MissingArtifactFile`)
/// is a hard block. New form codes must be added here to be overridable;
/// the safe default for an unknown code is to block.
fn is_overridable_form_flag(code: &str) -> bool {
    matches!(
        code,
        "missing_claim_structure"
            | "invalid_claim_structure"
            | "missing_required_section"
            | "malformed_title"
            | "no_reported_effort"
    ) || krabarena_shared::claim_structure::ValidationError::is_known_id(code)
}

/// True when the Claim ships a VALID structure.json that declares no
/// author-compute effort (`cost.usd` and `cost.tokens` both missing or
/// zero). Such a Claim registers as $0 and won't rank on the effort feed —
/// worth an overridable warn. Returns false when there's no structure or it
/// doesn't parse: those cases are already covered by the form gate's
/// `missing_claim_structure` / `invalid_claim_structure` warns, so flagging
/// effort on top would be redundant noise.
fn declares_no_effort(structure_raw: Option<&str>) -> bool {
    let Some(raw) = structure_raw else {
        return false;
    };
    match ClaimStructure::parse_and_validate(raw) {
        Ok(s) => {
            let usd = s.cost.as_ref().and_then(|c| c.usd).unwrap_or(0.0);
            let tokens = s.cost.as_ref().and_then(|c| c.tokens).unwrap_or(0);
            usd <= 0.0 && tokens == 0
        }
        Err(_) => false,
    }
}

fn resolve_verdict(outcome: &ReviewOutcome) -> ReviewVerdict {
    if !outcome.moderation_flags.is_empty() {
        // A moderation flag normally forces a hard block. Exception: the
        // deterministic structure/section gate emits OVERRIDABLE form
        // flags — if EVERY flag is one of those, it's a `warn` the
        // publisher can ship past with `--ignore-review`, not a block.
        // Any non-form flag (secret, pii, oversized, missing
        // RUN.md/README.md, or an LLM moderation flag) forces the block.
        let all_form = outcome
            .moderation_flags
            .iter()
            .all(|f| is_overridable_form_flag(&f.code));
        return if all_form {
            ReviewVerdict::Warn
        } else {
            ReviewVerdict::Block
        };
    }
    let has_warn = outcome.suggestions.iter().any(|s| s.severity == "warn");
    if has_warn {
        ReviewVerdict::Warn
    } else {
        match outcome.verdict.as_deref() {
            Some("block") => ReviewVerdict::Block,
            Some("warn") => ReviewVerdict::Warn,
            _ => ReviewVerdict::Pass,
        }
    }
}

fn synthetic_pass_outcome(artifact_filename: Option<&str>) -> ReviewOutcome {
    ReviewOutcome {
        verdict: Some("pass".into()),
        summary: Some(
            "KrabReviewer not configured (GEMINI_API_KEY missing); published without review."
                .into(),
        ),
        artifact_summary: artifact_filename.map(|n| format!("Artifact: {n}")),
        artifact_highlights: vec![Highlight {
            label: "KrabReviewer".into(),
            detail: "skipped (dev mode)".into(),
        }],
        suggestions: Vec::new(),
        moderation_flags: Vec::new(),
        og_summary: None,
        presentation: None,
    }
}

/// Enforce the canonical artifact structure: every bundle must
/// contain a `RUN.md` (exact reproduction steps — commands, versions,
/// expected runtime) and a `README.md` (the extended report — full
/// tables, raw numbers, analysis that didn't fit in the 5000-char
/// claim body). Missing either returns a Minor moderation flag with
/// a file-specific autofix hint. Case-insensitive match; top-level
/// only (RUN.md stashed in a subdir doesn't count, otherwise the
/// reader can't find it).
fn check_required_artifact_files(files: &FileMap) -> Vec<ModerationFlag> {
    let listing = files.list_files();
    let mut has_run = false;
    let mut has_readme = false;
    for entry in &listing {
        // Normalize tar-style "./RUN.md" to "RUN.md" before the
        // top-level check — `tar -czf bundle.tar.gz .` is the most
        // natural CLI invocation but it emits every entry with a
        // leading `./`, which the prior version of this scan saw as
        // a path separator and skipped, leading to a spurious
        // `missing_artifact_file` block. After the strip:
        //
        //   "./RUN.md"        → "RUN.md"        (top-level, counted)
        //   "./subdir/RUN.md" → "subdir/RUN.md" (still has `/`, skipped)
        //   "RUN.md"          → "RUN.md"        (unchanged)
        let normalized = entry
            .path
            .strip_prefix("./")
            .or_else(|| entry.path.strip_prefix(".\\"))
            .unwrap_or(&entry.path);

        // Top-level only (no `/` or `\` in the normalized path).
        if normalized.contains('/') || normalized.contains('\\') {
            continue;
        }
        let lower = normalized.to_ascii_lowercase();
        if lower == "run.md" {
            has_run = true;
        }
        if lower == "readme.md" {
            has_readme = true;
        }
    }

    let code = FlagCode::MissingArtifactFile.as_str().to_string();
    let mut flags = Vec::new();
    if !has_run {
        flags.push(ModerationFlag {
            code: code.clone(),
            reason: "artifact is missing RUN.md — the file that tells another agent exactly how to reproduce the result (commands, pinned versions, expected runtime). Every KrabArena artifact must include it.".into(),
            file: None,
            line: None,
            autofix_hint: Some(
                "add RUN.md to the bundle root with the exact commands to rerun the experiment; see https://krabarena.com/docs/claim-structure"
                    .into(),
            ),
        });
    }
    if !has_readme {
        flags.push(ModerationFlag {
            code,
            reason: "artifact is missing README.md — the extended report that backs the claim's Summary/Methodology/Conclusion with full tables, raw numbers, and analysis. Every KrabArena artifact must include it.".into(),
            file: None,
            line: None,
            autofix_hint: Some(
                "add README.md to the bundle root with the full report (the claim body is a 5000-char shop-window; put the details here)"
                    .into(),
            ),
        });
    }
    flags
}

/// Decide the deterministic pre-filter outcome from the two flag buckets,
/// or `None` when nothing tripped (the caller then runs Gemini).
///
///  - Any hard pre-flag (secret / oversized / missing RUN.md·README.md) →
///    a non-overridable `block`; structure/section flags are surfaced
///    alongside so the author sees everything in one pass.
///  - Else structure/section flags only → an overridable `warn` (a
///    publisher who passes `--ignore-review` can still ship).
///
/// Pure + total so the verdict policy is unit-tested directly (the live
/// `run_review` path needs a DB + Gemini, so it can't easily cover this).
/// Ordering matters: hard pre-flags MUST win over structure flags, or a
/// secret/oversized bundle would downgrade to an overridable warn.
fn resolve_prefilter_verdict(
    pre_flags: Vec<ModerationFlag>,
    structure_flags: Vec<ModerationFlag>,
) -> Option<(&'static str, &'static str, Vec<ModerationFlag>)> {
    if !pre_flags.is_empty() {
        let mut flags = pre_flags;
        flags.extend(structure_flags);
        Some(("block", "Blocked by deterministic pre-filter.", flags))
    } else if !structure_flags.is_empty() {
        Some((
            "warn",
            "Form issues from deterministic pre-filter (overridable).",
            structure_flags,
        ))
    } else {
        None
    }
}

/// Deterministic structure + required-section gate (0.18.7).
///
/// Two checks the form bar must not delegate to the LLM:
///
///   1. `structure.json` — must be present (inline `--structure` field
///      or `structure.json` at the bundle root) AND pass
///      [`ClaimStructure::parse_and_validate`]. A missing leaderboard,
///      unparseable JSON, or a field-rule violation (empty leaderboard,
///      duplicate rank, winner not in the table, empty setup/metric)
///      trips this. Before 0.18.7 these were soft `warn`s the LLM could
///      miss; now they're a deterministic, pre-LLM check.
///   2. The four required markdown sections — a level-1 `# Title` plus
///      non-empty `## Summary`, `## Methodology`, `## Conclusion`. Moved
///      off the flaky LLM pass (old prompt criterion 11) into a
///      deterministic check.
///
/// Severity: the caller (`run_review`) turns a non-empty result into an
/// **overridable `warn`** verdict — a publisher who passes
/// `--ignore-review` can still ship a form-imperfect Claim. (A real
/// moderation pre-flag — secret/oversized/missing RUN.md — co-occurring
/// promotes the whole outcome to a hard, non-overridable `block`.)
///
/// Every flag here carries a string `code` that is deliberately NOT a
/// [`FlagCode`] variant. `record_strikes_and_evaluate` parses the code
/// and silently skips unknown ones (same path as `artifact_oversized`),
/// so these gate the publish but never accrue a moderation strike —
/// broken form is an honest mistake, not abuse.
///
/// `inline_structure` is `inputs.claim_structure`: the multipart
/// `claim_structure` field, or the bundle's `structure.json` already
/// extracted + parse-checked by the caller (an unparseable bundle file
/// arrives here as `None`). We therefore also read the bundle's raw
/// `structure.json` bytes directly so a malformed-in-bundle file is
/// reported as `invalid_claim_structure`, not the vaguer
/// `missing_claim_structure`.
fn check_claim_structure_and_sections(
    claim_body: &str,
    inline_structure: Option<&str>,
    files: Option<&FileMap>,
) -> Vec<ModerationFlag> {
    let mut flags = Vec::new();

    let raw_structure: Option<String> = inline_structure
        .map(str::to_string)
        .or_else(|| files.and_then(bundle_structure_raw));

    match raw_structure {
        None => flags.push(ModerationFlag {
            code: "missing_claim_structure".into(),
            reason: "every Claim must ship a structure.json leaderboard — the \
                     machine-readable benchmark + ranking the UI renders and \
                     downstream agents read. Without it there is no Claim, only a \
                     write-up."
                .into(),
            file: None,
            line: None,
            autofix_hint: Some(
                "add structure.json at the bundle root (or pass --structure inline); \
                 see https://krabarena.com/docs/claim-structure"
                    .into(),
            ),
        }),
        Some(raw) => match ClaimStructure::parse_and_validate(&raw) {
            // A valid structure passes the form gate. The separate
            // effort-floor check (`declares_no_effort`) is applied AFTER the
            // full review resolves, so a missing `cost` never short-circuits
            // the Gemini pass — see the effort-floor step in `review_claim`.
            Ok(_) => {}
            Err(ParseError::Json(e)) => flags.push(ModerationFlag {
                code: "invalid_claim_structure".into(),
                reason: format!("structure.json is not valid JSON: {e}"),
                file: Some("structure.json".into()),
                line: None,
                autofix_hint: Some(
                    "fix the JSON syntax so it parses; see \
                     https://krabarena.com/docs/claim-structure"
                        .into(),
                ),
            }),
            Err(ParseError::Validation(errs)) => {
                for err in errs {
                    flags.push(ModerationFlag {
                        // Stable identifier from the shared validator
                        // (`empty_leaderboard`, `winner_not_in_leaderboard`,
                        // `non_falsifiable_setup`, `duplicate_rank`, …). Not a
                        // FlagCode, so it blocks without a strike.
                        code: err.id().into(),
                        reason: err.to_string(),
                        file: Some("structure.json".into()),
                        line: None,
                        autofix_hint: Some(
                            "correct the offending structure.json field; see \
                             https://krabarena.com/docs/claim-structure"
                                .into(),
                        ),
                    });
                }
            }
        },
    }

    flags.extend(check_required_sections(claim_body));
    flags
}

/// Pull the raw (possibly malformed) `structure.json` bytes out of a
/// bundle FileMap, as lossy UTF-8. Mirrors the dual `./`-prefixed lookup
/// `routes::claims::extract_bundle_structure_json` uses.
fn bundle_structure_raw(files: &FileMap) -> Option<String> {
    let bytes = files
        .read_bytes("structure.json")
        .or_else(|| files.read_bytes("./structure.json"))?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// The four required markdown sections, deterministically. A level-1
/// `# Title` must exist and be plain text (no markdown links / inline
/// code — the UI renders it verbatim); `## Summary`, `## Methodology`,
/// `## Conclusion` must each exist with non-empty content. One flag per
/// missing/empty/malformed item.
fn check_required_sections(body: &str) -> Vec<ModerationFlag> {
    let mut flags = Vec::new();
    let lines: Vec<&str> = body.lines().collect();
    let headings = heading_levels(&lines);

    // Title = the first level-1 heading outside any fenced code block.
    let title = headings.iter().flatten().find(|h| h.0 == 1).map(|h| h.1);
    match title {
        None => flags.push(missing_section_flag(
            "Title",
            "the claim body must open with a level-1 `# Title` line — the headline \
             the UI extracts. It must be plain text (no markdown, no links).",
        )),
        Some(title) if title_has_markup(title) => flags.push(ModerationFlag {
            code: "malformed_title".into(),
            reason: "the `# Title` headline must be plain text — no markdown links \
                     or inline code (the UI renders it verbatim)."
                .into(),
            file: None,
            line: None,
            autofix_hint: Some(
                "rewrite the `# Title` line as plain prose; move links/code into the \
                 body. See https://krabarena.com/docs/claim-structure"
                    .into(),
            ),
        }),
        Some(_) => {}
    }

    for section in ["Summary", "Methodology", "Conclusion"] {
        if !has_nonempty_section(&lines, &headings, section) {
            flags.push(missing_section_flag(
                section,
                &format!("the claim body must include a non-empty `## {section}` section"),
            ));
        }
    }

    flags
}

fn missing_section_flag(section: &str, reason: &str) -> ModerationFlag {
    ModerationFlag {
        code: "missing_required_section".into(),
        reason: reason.to_string(),
        file: None,
        line: None,
        autofix_hint: Some(format!(
            "add a `## {section}` section with content; the required structure is \
             `# Title` / `## Summary` / `## Methodology` / `## Conclusion`. See \
             https://krabarena.com/docs/claim-structure"
        )),
    }
}

/// A would-be headline that carries a markdown link (`[text](url)`) or
/// inline code (backtick) — the UI shows the title verbatim, so either
/// renders wrong. Kept deliberately narrow (links + code) to avoid
/// false-flagging legitimate punctuation in plain titles.
fn title_has_markup(title: &str) -> bool {
    title.contains("](") || title.contains('`')
}

/// Parse a markdown ATX heading line into `(level, trimmed-title)`.
/// Returns `None` for non-heading lines or hashes with no following
/// whitespace+text (e.g. `#nope`, a bare `##`).
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let title = rest.trim();
    if title.is_empty() {
        return None;
    }
    Some((hashes, title))
}

/// Per-line ATX heading info, index-aligned with `lines`. Lines inside a
/// fenced code block (opened/closed by a ``` or ~~~ run) are forced to
/// `None` so a `#`-leading code line (a shell comment, a diff hunk) is
/// never mistaken for a heading — which would otherwise prematurely end a
/// section or falsely satisfy the `# Title` check.
fn heading_levels<'a>(lines: &[&'a str]) -> Vec<Option<(usize, &'a str)>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut in_fence = false;
    for &line in lines {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            out.push(None);
        } else if in_fence {
            out.push(None);
        } else {
            out.push(parse_heading(line));
        }
    }
    out
}

/// Does an ATX heading title name one of the required sections? Keys off
/// the first word (trailing punctuation stripped) and accepts a `-s`
/// plural, so `## Summary of results`, `## Conclusions`, and
/// `## Methodology & setup` all match without authors being forced to
/// use the bare word.
fn heading_names_section(title: &str, name: &str) -> bool {
    let first = title.split_whitespace().next().unwrap_or("");
    let word = first.trim_end_matches(|c: char| !c.is_alphanumeric());
    word.eq_ignore_ascii_case(name) || word.eq_ignore_ascii_case(&format!("{name}s"))
}

/// True when a heading naming `name` is present AND its section carries at
/// least one non-blank line. The section runs until the next heading of
/// equal-or-higher level, so content nested under a deeper sub-heading
/// (`### Setup` under `## Methodology`) still counts. Headings inside
/// fenced code blocks are ignored (see [`heading_levels`]).
fn has_nonempty_section(lines: &[&str], headings: &[Option<(usize, &str)>], name: &str) -> bool {
    let mut i = 0;
    while i < lines.len() {
        let Some((level, title)) = headings[i] else {
            i += 1;
            continue;
        };
        if !heading_names_section(title, name) {
            i += 1;
            continue;
        }
        // Section body runs until the next heading whose level <= this
        // one; a deeper sub-heading stays part of the section.
        let mut j = i + 1;
        while j < lines.len() {
            if let Some((lvl, _)) = headings[j] {
                if lvl <= level {
                    break;
                }
            }
            if !lines[j].trim().is_empty() {
                return true;
            }
            j += 1;
        }
        // Heading present but empty — keep scanning for a later same-name
        // heading that does carry content.
        i = j;
    }
    false
}

/// Scan for the most dangerous credentials. Deterministic and cheap — this
/// runs before we spend a Gemini call. Returns structured ModerationFlags
/// with file/line info and autofix hints.
fn scan_for_secrets(bytes: &[u8]) -> Vec<ModerationFlag> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return vec![];
    };

    type LineMatcher = fn(&str) -> bool;
    let patterns: &[(&str, LineMatcher, &str)] = &[
        (
            "AWS access key",
            |l: &str| {
                l.contains("AKIA") && l.chars().filter(|c| c.is_ascii_alphanumeric()).count() >= 20
            },
            "replace with os.getenv('AWS_ACCESS_KEY_ID')",
        ),
        (
            "GCP private key block",
            |l: &str| {
                l.contains("-----BEGIN PRIVATE KEY-----")
                    || l.contains("-----BEGIN RSA PRIVATE KEY-----")
            },
            "load key from file or env var, never embed in artifact",
        ),
        (
            "SSH private key block",
            |l: &str| l.contains("-----BEGIN OPENSSH PRIVATE KEY-----"),
            "remove private key from artifact",
        ),
        (
            "OpenAI-style key (sk-…)",
            |l: &str| is_prefixed_secret(l, "sk-", 32),
            "replace with os.getenv('OPENAI_API_KEY')",
        ),
        (
            "Anthropic-style key (sk-ant-…)",
            |l: &str| is_prefixed_secret(l, "sk-ant-", 32),
            "replace with os.getenv('ANTHROPIC_API_KEY')",
        ),
        (
            "GitHub token (ghp_/gho_/ghu_…)",
            |l: &str| {
                ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]
                    .iter()
                    .any(|p| is_prefixed_secret(l, p, 30))
            },
            "replace with os.getenv('GITHUB_TOKEN')",
        ),
        (
            "Slack token (xox…)",
            |l: &str| {
                ["xoxb-", "xoxp-", "xoxa-", "xoxs-"]
                    .iter()
                    .any(|p| is_prefixed_secret(l, p, 20))
            },
            "replace with os.getenv('SLACK_TOKEN')",
        ),
        (
            "Exa API key",
            |l: &str| is_prefixed_secret(l, "exa-", 20),
            "replace with os.getenv('EXA_API_KEY')",
        ),
        (
            "Google/Gemini API key (AIzaSy…)",
            |l: &str| is_prefixed_secret(l, "AIzaSy", 30),
            "replace with os.getenv('GOOGLE_API_KEY')",
        ),
    ];

    let mut flags = vec![];
    for (lineno, line) in text.lines().enumerate() {
        for (name, pat, fix) in patterns {
            if pat(line) {
                flags.push(ModerationFlag {
                    code: "secret_detected".to_string(),
                    reason: format!("{name} at line {}", lineno + 1),
                    file: None,
                    line: Some(lineno as u32 + 1),
                    autofix_hint: Some(fix.to_string()),
                });
            }
        }
    }
    flags
}

fn is_prefixed_secret(line: &str, prefix: &str, min_tail: usize) -> bool {
    let Some(idx) = line.find(prefix) else {
        return false;
    };
    let tail: String = line[idx + prefix.len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    tail.len() >= min_tail
}

async fn record_strikes_and_evaluate(
    pool: &PgPool,
    user_id: Uuid,
    review_id: Uuid,
    flags: &[ModerationFlag],
) -> Result<Option<Enforcement>, ApiError> {
    if flags.is_empty() {
        return Ok(None);
    }

    let mut highest: Option<Severity> = None;

    // History — 60-day window is comfortably above the longest threshold (30d).
    let history_rows = sqlx::query_as::<_, (String, chrono::DateTime<chrono::Utc>)>(
        "SELECT severity, created_at FROM moderation_strikes \
         WHERE user_id = $1 AND created_at > NOW() - INTERVAL '60 days'",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let history: Vec<PastStrike> = history_rows
        .into_iter()
        .filter_map(|(sev, created_at)| {
            let s = match sev.as_str() {
                "minor" => Severity::Minor,
                "major" => Severity::Major,
                "severe" => Severity::Severe,
                _ => return None,
            };
            Some(PastStrike {
                severity: s,
                created_at,
            })
        })
        .collect();

    for flag in flags {
        let Some(code) = FlagCode::parse(&flag.code) else {
            continue;
        };
        let severity = moderation_policy::severity_for(code);
        sqlx::query(
            "INSERT INTO moderation_strikes (user_id, review_id, code, severity, reason) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(user_id)
        .bind(review_id)
        .bind(flag.code.as_str())
        .bind(severity.as_str())
        .bind(&flag.reason)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

        highest = Some(match highest {
            Some(Severity::Severe) => Severity::Severe,
            Some(Severity::Major) if severity == Severity::Severe => Severity::Severe,
            Some(existing) if rank(severity) > rank(existing) => severity,
            Some(existing) => existing,
            None => severity,
        });
    }

    Ok(highest.map(|s| moderation_policy::evaluate(s, Utc::now(), &history)))
}

fn rank(s: Severity) -> u8 {
    match s {
        Severity::Minor => 1,
        Severity::Major => 2,
        Severity::Severe => 3,
    }
}

async fn apply_enforcement(
    pool: &PgPool,
    user_id: Uuid,
    enforcement: Enforcement,
) -> Result<(), ApiError> {
    match enforcement {
        Enforcement::None => Ok(()),
        Enforcement::RateLimit { until, .. } => {
            sqlx::query("UPDATE users SET rate_limit_until = $1 WHERE id = $2")
                .bind(until)
                .bind(user_id)
                .execute(pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
            tracing::warn!(user_id = %user_id, until = %until, "user.rate_limited");
            Ok(())
        }
        Enforcement::Suspend { until, reason } => {
            sqlx::query(
                "UPDATE users SET suspended_until = $1, suspension_reason = $2 WHERE id = $3",
            )
            .bind(until)
            .bind(&reason)
            .bind(user_id)
            .execute(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
            tracing::warn!(user_id = %user_id, until = %until, reason = %reason, "user.suspended");
            Ok(())
        }
        Enforcement::PermanentSuspend { reason } => {
            // "Permanent" encoded as far-future timestamp.
            let far_future = Utc::now() + chrono::Duration::days(36_500);
            sqlx::query(
                "UPDATE users SET suspended_until = $1, suspension_reason = $2 WHERE id = $3",
            )
            .bind(far_future)
            .bind(&reason)
            .bind(user_id)
            .execute(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
            tracing::warn!(user_id = %user_id, reason = %reason, "user.suspended_permanent");
            Ok(())
        }
    }
}

// `load_claim_structures` was removed in 0.8.15 — the reviewer no
// longer needs cross-claim context. The summary update path in
// `battle_summarizer::regenerate_summary` owns claim-structure
// loading now (incrementally for the per-claim path; full top-10
// for the explicit /regenerate-summary admin endpoint).

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Minimal helper: build a tar.gz with the given top-level files.
    fn build_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (path, bytes) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, *bytes)
                    .expect("append");
            }
            builder.finish().expect("finish tar");
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).expect("gz write");
        gz.finish().expect("gz finish")
    }

    fn files_from(bundle: &[(&str, &[u8])]) -> FileMap {
        let gz = build_tar_gz(bundle);
        FileMap::from_tar_gz(&gz).expect("tar.gz should parse")
    }

    #[test]
    fn check_required_artifact_files_accepts_bundle_with_both() {
        let files = files_from(&[
            ("README.md", b"# Report"),
            ("RUN.md", b"# How to reproduce\n\npython run.py"),
            ("run.py", b"print('hi')"),
        ]);
        let flags = check_required_artifact_files(&files);
        assert!(flags.is_empty(), "expected no flags, got: {flags:?}");
    }

    #[test]
    fn check_required_artifact_files_flags_missing_run_md() {
        let files = files_from(&[("README.md", b"# Report"), ("run.py", b"print('hi')")]);
        let flags = check_required_artifact_files(&files);
        assert_eq!(flags.len(), 1, "expected one flag");
        assert_eq!(flags[0].code, "missing_artifact_file");
        assert!(
            flags[0].reason.contains("RUN.md"),
            "reason should name RUN.md: {}",
            flags[0].reason
        );
    }

    #[test]
    fn check_required_artifact_files_flags_missing_readme() {
        let files = files_from(&[
            ("RUN.md", b"# How to reproduce\n\npython run.py"),
            ("run.py", b"print('hi')"),
        ]);
        let flags = check_required_artifact_files(&files);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].code, "missing_artifact_file");
        assert!(flags[0].reason.contains("README.md"));
    }

    #[test]
    fn check_required_artifact_files_flags_both_when_both_missing() {
        let files = files_from(&[("run.py", b"print('hi')")]);
        let flags = check_required_artifact_files(&files);
        assert_eq!(flags.len(), 2);
        assert!(flags.iter().all(|f| f.code == "missing_artifact_file"));
        let reasons: String = flags.iter().map(|f| f.reason.as_str()).collect();
        assert!(reasons.contains("RUN.md"));
        assert!(reasons.contains("README.md"));
    }

    #[test]
    fn check_required_artifact_files_rejects_nested_files_as_substitute() {
        // README.md + RUN.md buried inside docs/ don't count — a reader
        // looking at the artifact root must find them.
        let files = files_from(&[
            ("docs/README.md", b"# Report"),
            ("docs/RUN.md", b"# How"),
            ("run.py", b"print('hi')"),
        ]);
        let flags = check_required_artifact_files(&files);
        assert_eq!(
            flags.len(),
            2,
            "nested README/RUN should not satisfy the top-level requirement"
        );
    }

    #[test]
    fn check_required_artifact_files_matches_case_insensitively() {
        // Authors use all-caps by convention, but also accept `Readme.md`
        // / `Run.md`. Case-insensitivity prevents a spurious reject.
        let files = files_from(&[
            ("Readme.md", b"# Report"),
            ("Run.md", b"# How to reproduce"),
        ]);
        let flags = check_required_artifact_files(&files);
        assert!(flags.is_empty(), "case should not matter: {flags:?}");
    }

    #[test]
    fn check_required_artifact_files_strips_tar_dot_slash_prefix() {
        // `tar -czf bundle.tar.gz .` (the most natural CLI invocation)
        // emits every entry with a leading `./`. Without normalization
        // the path-separator scan saw `./RUN.md` as nested and skipped
        // it, falsely flagging the bundle as missing both required
        // files. Real-world v0.7.0 usage hit this; regression test
        // ensures it stays fixed.
        let files = files_from(&[
            ("./README.md", b"# Report"),
            ("./RUN.md", b"# How to reproduce"),
            ("./run.py", b"print('hi')"),
        ]);
        let flags = check_required_artifact_files(&files);
        assert!(
            flags.is_empty(),
            "leading ./ on tar entries must not trigger missing-file flags: {flags:?}"
        );
    }

    #[test]
    fn check_required_artifact_files_still_rejects_nested_after_dot_slash_strip() {
        // Stripping the leading `./` must NOT promote files in nested
        // directories to top-level. `./docs/RUN.md` is still nested.
        let files = files_from(&[
            ("./docs/README.md", b"# Report"),
            ("./docs/RUN.md", b"# How to reproduce"),
            ("./run.py", b"print('hi')"),
        ]);
        let flags = check_required_artifact_files(&files);
        assert_eq!(
            flags.len(),
            2,
            "nested files must still be skipped after `./` strip: {flags:?}"
        );
    }

    #[test]
    fn resolve_verdict_prefers_moderation_flags() {
        let outcome = ReviewOutcome {
            verdict: Some("pass".into()),
            moderation_flags: vec![ModerationFlag {
                code: "secret_detected".into(),
                reason: "x".into(),
                file: None,
                line: None,
                autofix_hint: None,
            }],
            ..Default::default()
        };
        assert_eq!(resolve_verdict(&outcome), ReviewVerdict::Block);
    }

    #[test]
    fn resolve_verdict_promotes_warn_from_suggestions() {
        let outcome = ReviewOutcome {
            verdict: Some("pass".into()),
            suggestions: vec![crate::models::review::Suggestion {
                id: "s1".into(),
                severity: "warn".into(),
                category: "alignment".into(),
                message: "m".into(),
                suggestion: "s".into(),
                anchor: None,
                autofix_hint: None,
            }],
            ..Default::default()
        };
        assert_eq!(resolve_verdict(&outcome), ReviewVerdict::Warn);
    }

    #[test]
    fn resolve_verdict_passes_clean_review() {
        let outcome = ReviewOutcome {
            verdict: Some("pass".into()),
            suggestions: vec![crate::models::review::Suggestion {
                id: "s1".into(),
                severity: "info".into(),
                category: "engagement".into(),
                message: "m".into(),
                suggestion: "s".into(),
                anchor: None,
                autofix_hint: None,
            }],
            ..Default::default()
        };
        assert_eq!(resolve_verdict(&outcome), ReviewVerdict::Pass);
    }

    #[test]
    fn resolve_verdict_form_only_flags_are_overridable_warn() {
        // Regression for the 0.18.7 e2e failure: structure/section flags
        // are moderation_flags, but resolve_verdict must surface them as
        // an overridable `warn`, not a hard `block`.
        for code in [
            "missing_claim_structure",
            "invalid_claim_structure",
            "missing_required_section",
            "malformed_title",
            "empty_leaderboard",
            "winner_not_in_leaderboard",
        ] {
            let outcome = ReviewOutcome {
                verdict: Some("warn".into()),
                moderation_flags: vec![mk_flag(code)],
                ..Default::default()
            };
            assert_eq!(
                resolve_verdict(&outcome),
                ReviewVerdict::Warn,
                "{code} should be an overridable warn"
            );
        }
    }

    #[test]
    fn resolve_verdict_blocks_when_any_hard_flag_present() {
        // A hard flag mixed with form flags must still hard-block.
        let outcome = ReviewOutcome {
            verdict: Some("warn".into()),
            moderation_flags: vec![
                mk_flag("missing_claim_structure"),
                mk_flag("secret_detected"),
            ],
            ..Default::default()
        };
        assert_eq!(resolve_verdict(&outcome), ReviewVerdict::Block);
    }

    #[test]
    fn resolve_verdict_blocks_oversized_non_strike_flag() {
        // artifact_oversized is non-strike but NOT overridable — block.
        let outcome = ReviewOutcome {
            verdict: Some("block".into()),
            moderation_flags: vec![mk_flag("artifact_oversized")],
            ..Default::default()
        };
        assert_eq!(resolve_verdict(&outcome), ReviewVerdict::Block);
    }

    #[test]
    fn scan_detects_openai_key() {
        let body = b"#!/bin/bash\nexport OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz012345\n";
        let flags = scan_for_secrets(body);
        assert!(!flags.is_empty(), "should match sk- key");
        assert!(flags[0].reason.contains("OpenAI"));
        assert!(flags[0].autofix_hint.is_some());
        assert_eq!(flags[0].line, Some(2));
    }

    #[test]
    fn scan_detects_github_token() {
        let body = b"token=ghp_abcdefghijklmnopqrstuvwxyz012345\n";
        let flags = scan_for_secrets(body);
        assert!(!flags.is_empty(), "github token");
        assert!(flags[0].reason.contains("GitHub"));
    }

    #[test]
    fn scan_detects_private_key_block() {
        let body = b"\n-----BEGIN OPENSSH PRIVATE KEY-----\n...";
        let flags = scan_for_secrets(body);
        assert!(!flags.is_empty());
        assert!(flags[0].reason.contains("SSH"));
    }

    #[test]
    fn scan_ignores_short_tokens() {
        let body = b"sk-short\n";
        assert!(scan_for_secrets(body).is_empty());
    }

    #[test]
    fn strike_summary_empty_on_no_rows() {
        assert_eq!(render_strike_summary(&[]), "");
    }

    #[test]
    fn strike_summary_bundles_counts_per_code() {
        // The shape the user's account actually had when they got rate-
        // limited: 5 minor strikes across two codes + 1 major duplicate.
        let rows = vec![
            ("minor".into(), "low_effort".into()),
            ("minor".into(), "low_effort".into()),
            ("minor".into(), "low_effort".into()),
            ("minor".into(), "off_topic".into()),
            ("minor".into(), "off_topic".into()),
            ("major".into(), "duplicate".into()),
        ];
        assert_eq!(
            render_strike_summary(&rows),
            "5 minor (low_effort×3, off_topic×2), 1 major (duplicate)"
        );
    }

    #[test]
    fn strike_summary_elides_count_for_single() {
        let rows = vec![("minor".into(), "low_effort".into())];
        assert_eq!(render_strike_summary(&rows), "1 minor (low_effort)");
    }

    #[test]
    fn strike_summary_orders_severity_minor_major_severe() {
        let rows = vec![
            ("severe".into(), "policy_violation".into()),
            ("major".into(), "duplicate".into()),
            ("minor".into(), "low_effort".into()),
        ];
        assert_eq!(
            render_strike_summary(&rows),
            "1 minor (low_effort), 1 major (duplicate), 1 severe (policy_violation)"
        );
    }

    // --- Deterministic structure + required-section gate (0.18.7) ---

    const VALID_STRUCTURE: &str = r#"{
        "schema_version": 1,
        "benchmark": {
            "name": "WebSearch-SimpleQA-20", "task": "factoid recall",
            "dataset": "SimpleQA-20", "metric": "recall@10",
            "higher_is_better": true, "setup": "seed=42, top-10 snippets"
        },
        "leaderboard": [
            {"rank": 1, "solution": "Exa", "score": 90.0},
            {"rank": 2, "solution": "Keenable", "score": 65.0}
        ],
        "winners": ["Exa"],
        "cost": {"usd": 0.25, "tokens": 120000}
    }"#;

    const VALID_BODY: &str = "# Exa wins\n\n## Summary\nExa leads.\n\n\
        ## Methodology\nRan 20 queries, seed=42.\n\n## Conclusion\nExa is best.\n";

    fn codes(flags: &[ModerationFlag]) -> Vec<String> {
        flags.iter().map(|f| f.code.clone()).collect()
    }

    fn mk_flag(code: &str) -> ModerationFlag {
        ModerationFlag {
            code: code.into(),
            reason: "test".into(),
            file: None,
            line: None,
            autofix_hint: None,
        }
    }

    // --- resolve_prefilter_verdict (the warn/block decision) ---

    #[test]
    fn prefilter_verdict_none_when_no_flags() {
        assert!(resolve_prefilter_verdict(vec![], vec![]).is_none());
    }

    #[test]
    fn prefilter_verdict_structure_only_is_overridable_warn() {
        let (verdict, _, flags) =
            resolve_prefilter_verdict(vec![], vec![mk_flag("missing_claim_structure")]).unwrap();
        assert_eq!(verdict, "warn");
        assert_eq!(codes(&flags), vec!["missing_claim_structure"]);
    }

    #[test]
    fn prefilter_verdict_hard_flag_is_block() {
        let (verdict, _, flags) =
            resolve_prefilter_verdict(vec![mk_flag("secret_detected")], vec![]).unwrap();
        assert_eq!(verdict, "block");
        assert_eq!(codes(&flags), vec!["secret_detected"]);
    }

    #[test]
    fn prefilter_verdict_hard_flag_wins_over_structure() {
        // The load-bearing invariant: a secret/oversized bundle that ALSO
        // has form problems must hard-`block`, never downgrade to the
        // overridable form `warn`. Both flag sets are surfaced.
        let (verdict, _, flags) = resolve_prefilter_verdict(
            vec![mk_flag("secret_detected")],
            vec![mk_flag("missing_required_section")],
        )
        .unwrap();
        assert_eq!(verdict, "block");
        assert!(codes(&flags).contains(&"secret_detected".to_string()));
        assert!(codes(&flags).contains(&"missing_required_section".to_string()));
    }

    #[test]
    fn structure_gate_accepts_valid_inline_and_body() {
        let flags = check_claim_structure_and_sections(VALID_BODY, Some(VALID_STRUCTURE), None);
        assert!(flags.is_empty(), "expected no flags, got: {flags:?}");
    }

    #[test]
    fn structure_gate_reads_valid_structure_from_bundle() {
        // No inline structure, but the bundle ships a valid structure.json.
        let files = files_from(&[
            ("README.md", b"# Report"),
            ("RUN.md", b"# Run"),
            ("structure.json", VALID_STRUCTURE.as_bytes()),
        ]);
        let flags = check_claim_structure_and_sections(VALID_BODY, None, Some(&files));
        assert!(flags.is_empty(), "expected no flags, got: {flags:?}");
    }

    #[test]
    fn effort_floor_detects_missing_and_zero_cost() {
        // Applied AFTER the review resolves (not in the form gate), so a
        // missing cost never short-circuits the Gemini pass. VALID_STRUCTURE
        // declares real effort → false.
        assert!(!declares_no_effort(Some(VALID_STRUCTURE)));

        // Only external_usd (aggregated separately, not effort) → true.
        const EXTERNAL_ONLY: &str = r#"{
            "schema_version": 1,
            "benchmark": {
                "name": "WebSearch-SimpleQA-20", "task": "factoid recall",
                "dataset": "SimpleQA-20", "metric": "recall@10",
                "higher_is_better": true, "setup": "seed=42, top-10 snippets"
            },
            "leaderboard": [{"rank": 1, "solution": "Exa", "score": 90.0}],
            "winners": ["Exa"],
            "cost": {"external_usd": 0.05}
        }"#;
        assert!(declares_no_effort(Some(EXTERNAL_ONLY)));

        // No structure / unparseable → false (the form gate already warns).
        assert!(!declares_no_effort(None));
        assert!(!declares_no_effort(Some("{not json")));
    }

    #[test]
    fn structure_gate_flags_missing_structure() {
        let files = files_from(&[("README.md", b"# Report"), ("RUN.md", b"# Run")]);
        let flags = check_claim_structure_and_sections(VALID_BODY, None, Some(&files));
        assert_eq!(codes(&flags), vec!["missing_claim_structure"]);
    }

    #[test]
    fn structure_gate_flags_malformed_json_in_bundle() {
        // A present-but-unparseable bundle structure.json must read as
        // `invalid_claim_structure`, not the vaguer `missing_…`.
        let files = files_from(&[
            ("README.md", b"# Report"),
            ("RUN.md", b"# Run"),
            ("structure.json", b"not valid json {"),
        ]);
        let flags = check_claim_structure_and_sections(VALID_BODY, None, Some(&files));
        assert_eq!(codes(&flags), vec!["invalid_claim_structure"]);
    }

    #[test]
    fn structure_gate_flags_validation_errors_with_stable_ids() {
        // Empty leaderboard + winner not in leaderboard → the shared
        // validator's stable ids surface verbatim as flag codes.
        let bad = r#"{
            "schema_version": 1,
            "benchmark": {"name": "n", "task": "t", "dataset": "d",
                "metric": "m", "higher_is_better": true, "setup": "s"},
            "leaderboard": [],
            "winners": ["Ghost"]
        }"#;
        let flags = check_claim_structure_and_sections(VALID_BODY, Some(bad), None);
        let c = codes(&flags);
        assert!(c.contains(&"empty_leaderboard".to_string()), "{c:?}");
        assert!(
            c.contains(&"winner_not_in_leaderboard".to_string()),
            "{c:?}"
        );
    }

    #[test]
    fn structure_gate_flags_empty_setup_as_non_falsifiable() {
        let bad = r#"{
            "schema_version": 1,
            "benchmark": {"name": "n", "task": "t", "dataset": "d",
                "metric": "m", "higher_is_better": true, "setup": "   "},
            "leaderboard": [{"rank": 1, "solution": "A", "score": 1.0}],
            "winners": ["A"]
        }"#;
        let flags = check_claim_structure_and_sections(VALID_BODY, Some(bad), None);
        assert_eq!(codes(&flags), vec!["non_falsifiable_setup"]);
    }

    #[test]
    fn structure_gate_codes_are_not_strike_codes() {
        // Structural codes must NOT parse as FlagCode, so they block the
        // publish but never record a moderation strike (see
        // record_strikes_and_evaluate).
        for code in [
            "missing_claim_structure",
            "invalid_claim_structure",
            "missing_required_section",
            "malformed_title",
            "empty_leaderboard",
            "winner_not_in_leaderboard",
            "non_falsifiable_setup",
            "duplicate_rank",
        ] {
            assert!(
                FlagCode::parse(code).is_none(),
                "{code} must not be a strike-bearing FlagCode"
            );
        }
    }

    #[test]
    fn sections_gate_flags_missing_methodology() {
        let body = "# Title\n\n## Summary\nx\n\n## Conclusion\ny\n";
        let flags = check_required_sections(body);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].code, "missing_required_section");
        assert!(
            flags[0].reason.contains("Methodology"),
            "{}",
            flags[0].reason
        );
    }

    #[test]
    fn sections_gate_flags_empty_summary() {
        // Heading present but no content before the next heading.
        let body = "# Title\n\n## Summary\n\n## Methodology\nm\n\n## Conclusion\nc\n";
        let flags = check_required_sections(body);
        assert_eq!(codes(&flags), vec!["missing_required_section"]);
        assert!(flags[0].reason.contains("Summary"));
    }

    #[test]
    fn sections_gate_flags_missing_title() {
        // Only level-2 headings, no `# Title`.
        let body = "## Summary\nx\n\n## Methodology\nm\n\n## Conclusion\nc\n";
        let flags = check_required_sections(body);
        assert_eq!(codes(&flags), vec!["missing_required_section"]);
        assert!(flags[0].reason.contains("Title"));
    }

    #[test]
    fn sections_gate_accepts_well_formed_body() {
        assert!(check_required_sections(VALID_BODY).is_empty());
    }

    #[test]
    fn parse_heading_rejects_hash_without_space() {
        assert_eq!(parse_heading("#nope"), None);
        assert_eq!(parse_heading("##"), None);
        assert_eq!(parse_heading("plain text"), None);
        assert_eq!(parse_heading("# Title"), Some((1, "Title")));
        assert_eq!(parse_heading("##  Summary  "), Some((2, "Summary")));
    }

    #[test]
    fn sections_gate_accepts_content_under_subheadings() {
        // Regression: a section whose content lives under a deeper
        // sub-heading must NOT read as empty (a `###` stays part of the
        // enclosing `##` section).
        let body = "# Title\n\n## Summary\nWins.\n\n\
            ## Methodology\n### Setup\nRan 20 queries, seed=42.\n\n\
            ## Conclusion\nBest.\n";
        assert!(
            check_required_sections(body).is_empty(),
            "{:?}",
            check_required_sections(body)
        );
    }

    #[test]
    fn sections_gate_accepts_heading_name_variants() {
        // Extra trailing words and a plural must still match.
        let body = "# Title\n\n## Summary of results\nWins.\n\n\
            ## Methodology & setup\nRan it.\n\n## Conclusions\nBest.\n";
        assert!(
            check_required_sections(body).is_empty(),
            "{:?}",
            check_required_sections(body)
        );
    }

    #[test]
    fn sections_gate_ignores_hash_lines_inside_code_fences() {
        // A `#`-leading line inside a fenced block is not a heading, so
        // it neither ends the Methodology section nor stands in for the
        // `# Title` (this body has a real `# Title` too).
        let body = "# Title\n\n## Summary\nWins.\n\n\
            ## Methodology\n```sh\n# install deps\npip install foo\n```\n\n\
            ## Conclusion\nBest.\n";
        assert!(
            check_required_sections(body).is_empty(),
            "{:?}",
            check_required_sections(body)
        );
    }

    #[test]
    fn sections_gate_does_not_accept_code_fence_title() {
        // The only `#`-line is inside a code fence → no real `# Title`.
        let body = "```\n# Not a title\n```\n\n## Summary\nx\n\n\
            ## Methodology\nm\n\n## Conclusion\nc\n";
        let flags = check_required_sections(body);
        assert!(
            flags.iter().any(|f| f.reason.contains("Title")),
            "{flags:?}"
        );
    }

    #[test]
    fn sections_gate_flags_title_with_markdown_link() {
        let body = "# See [the benchmark](https://example.com)\n\n## Summary\nx\n\n\
            ## Methodology\nm\n\n## Conclusion\nc\n";
        let flags = check_required_sections(body);
        assert_eq!(codes(&flags), vec!["malformed_title"]);
    }
}
