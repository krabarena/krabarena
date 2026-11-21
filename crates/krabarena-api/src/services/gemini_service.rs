//! Gemini Flash client for the KrabReviewer.
//!
//! This module holds the shared transport-level concerns — API key,
//! HTTP client, model name, error type — and the canonical rubric
//! (`CLAIM_REVIEW_SYSTEM_PROMPT`). The actual review loop lives in
//! [`agentic_reviewer`]; it calls `gemini.http_client()` to reuse this
//! connection pool.

use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use serde_json::Value;

/// `gemini-2.5-flash` (GA, $0.075/$0.30 per 1M input/output) — same
/// rate card as `gemini-3-flash-preview` but markedly more reliable
/// on long prompts (the preview consistently times out at 30-180s
/// on 50-100 KB summarizer inputs while 2.5-flash returns the same
/// shape in 5-15s). Switch back to the preview if/when it
/// stabilizes; the cost-estimation table treats them identically.
// Reverted to `gemini-2.5-flash` in 0.8.17. We tried lite in 0.8.15
// for the per-turn latency win (2-5s vs 10-30s), but our key gets
// rate-limited on lite far more aggressively than on flash — measured
// 22% of reviews hit HTTP 503 "high demand" on lite, vs ~0% on flash
// across the same workload. The 0.8.17 retry helper softens the lite
// failure mode, but flash just doesn't trip it. Latency cost vs lite:
// ~3-5s slower per turn; well within the 90s wall-clock cap given the
// stage-1 ≤ 4 / stage-2 ≤ 6 turn budget.
// Bumped flash → pro in 0.16.7 after a 3-run × 4-case dry-run study
// (GEPA reference-claims review, since removed): pro
// hit 12/12 `pass` with only info-level title-polish suggestions,
// vs flash's 6/12 `pass` rate where the remaining 6 were spurious
// `unverified_keywords` warns from flash mis-using the grep tool.
// Per-review cost stays under 2 cents at ~10K prompt + 2K output
// tokens; the reliability gain is worth the 16× rate multiplier for
// a single-call-per-publish workload.
pub const DEFAULT_MODEL: &str = "gemini-2.5-pro";
// Bumped 30s → 45s in 0.8.15 to absorb Gemini-flash-lite's
// occasional 30-40s response spikes on cumulative ~25 KB prompts.
// At 30s we were dropping ~22% of reviews on real published claims
// at the moment Gemini decided to be slow. The agentic loop's
// `max_wall_clock` (90s) caps total review time so a runaway
// per-call timeout can't compound.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

/// Token usage + estimated $ cost from a single Gemini call.
///
/// All Gemini call sites in this crate route through the same shape
/// so the persistence layer (`claim_reviews` row) sees the same
/// fields regardless of which reviewer made the call (claim,
/// battle, or verify/refute). Cost is stored as integer micro-USD
/// (1 USD = 1_000_000 µUSD) — no floats end up in the DB.
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cost_micros: i64,
}

impl Usage {
    /// Add another call's tokens onto this one. Used by the agentic
    /// reviewer to sum stage-1 + stage-2 token counts into one row.
    pub fn merge(&mut self, other: Usage) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.cost_micros = self.cost_micros.saturating_add(other.cost_micros);
    }
}

/// Parse `usageMetadata` (Gemini v1beta `generateContent` response)
/// into a [`Usage`] enriched with cost. Missing / non-numeric fields
/// degrade gracefully to 0 — we never want a usage parse failure to
/// fail an otherwise-successful review.
pub fn parse_usage(payload: &Value, model: &str) -> Usage {
    let meta = &payload["usageMetadata"];
    let prompt_tokens = meta
        .get("promptTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let completion_tokens = meta
        .get("candidatesTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let cost_micros = estimate_cost_micros(model, prompt_tokens, completion_tokens);
    Usage {
        prompt_tokens,
        completion_tokens,
        cost_micros,
    }
}

/// Estimate the dollar cost of a single Gemini call in micro-USD.
///
/// Rates per 1M tokens (input / output):
///   - gemini-3-flash-preview      : $0.075 / $0.30
///   - gemini-2.5-flash            : $0.075 / $0.30
///   - gemini-flash-latest         : $0.075 / $0.30
///   - gemini-2.5-pro              : $1.25  / $5.00 (default since 0.16.7)
///   - gemini-pro-latest           : $1.25  / $5.00
///   - anything else               : 0 + warn (we want to notice)
///
/// All math is i64 µUSD. Rates are stored as µUSD per 1M tokens
/// (e.g. $0.075/M = 75_000 µUSD/M), so cost = tokens * rate /
/// 1_000_000. Small token counts will floor to 0 µUSD; that's fine,
/// the granularity at $0.000001 is already finer than we'll ever
/// surface to humans.
pub fn estimate_cost_micros(model: &str, prompt_tokens: u32, completion_tokens: u32) -> i64 {
    let (in_rate_per_m, out_rate_per_m): (i64, i64) = match model {
        "gemini-3-flash-preview" | "gemini-2.5-flash" | "gemini-flash-latest" => (75_000, 300_000),
        "gemini-2.5-pro" | "gemini-pro-latest" => (1_250_000, 5_000_000),
        other => {
            tracing::warn!(
                model = %other,
                "gemini.cost.unknown_model — cost recorded as 0; add a rate entry"
            );
            return 0;
        }
    };
    let prompt = (prompt_tokens as i64).saturating_mul(in_rate_per_m) / 1_000_000;
    let completion = (completion_tokens as i64).saturating_mul(out_rate_per_m) / 1_000_000;
    prompt.saturating_add(completion)
}

/// System prompt baked into every call.  This is the rubric the model
/// is coached against — moderation, quality, engagement.
pub const CLAIM_REVIEW_SYSTEM_PROMPT: &str = include_str!("claim_review_prompt.txt");

#[derive(Debug, thiserror::Error)]
pub enum GeminiError {
    #[error("gemini api key not configured")]
    NoApiKey,
    #[error("gemini request failed: {0}")]
    Transport(String),
    #[error("gemini returned non-success status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("gemini response parse failed: {0}")]
    Parse(String),
}

/// Strip secrets (and other query params, for good measure) from any
/// URL that appears inside an error message.
///
/// `reqwest::Error`'s `Display` impl includes the URL it was trying to
/// reach, which — for our Gemini calls — has the API key in the query
/// string (`...?key=AIzaSy...`). When that error gets bubbled up into
/// a `GeminiError::Transport(String)` and the upstream HTTP handler
/// returns it as `REVIEWER_UNAVAILABLE`, the key ends up in the
/// client-visible response body.
///
/// This helper rewrites every `https?://...?...` substring to drop
/// the query string entirely (`https://host/path` only). Used at every
/// reqwest-error stringification site in this module. Belt-and-
/// suspenders defence — the upstream `error.rs` handler also calls
/// this on the way out for any `ReviewerUnavailable` inner string.
pub(crate) fn redact_url_secrets(s: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // `https?://` + host + optional path, followed by `?…` up to
        // whitespace, ), ", or end-of-string. Greedy within the URL.
        regex::Regex::new(r#"(https?://[^\s)"]+?)\?[^\s)"]*"#).expect("static regex compiles")
    });
    re.replace_all(s, "$1").into_owned()
}

#[derive(Debug, Clone)]
pub struct BattleContext {
    pub title: String,
    pub description: String,
    pub status: String,
    /// Existing Claims on this Battle — powers the "contrast / position"
    /// engagement coaching.
    pub existing_claims: Vec<ExistingClaimBrief>,
    /// Each Claim's `structure.json` (compact projection — benchmark,
    /// leaderboard, winners, claim id + title), passed in as raw data
    /// so the reviewer's LLM can synthesize the cross-claim
    /// `battle_executive_summary` itself: de-dupe solution-name
    /// variants, normalize across metric scales, pick up to 5 top
    /// competitors and up to 5 top evals. We deliberately don't
    /// pre-aggregate server-side because the synthesis decisions
    /// (which Claude Code variant to merge, whether 87.6% beats 25/25,
    /// which evals are duplicates) belong to the model. Empty when no
    /// Claim on this Battle has a structure.json yet — the prompt
    /// falls back to narrative-only.
    pub claim_structures: Vec<ClaimStructureBrief>,
    /// The `battle_executive_summary` rendered after the prior
    /// review (or `None` for a Battle that has never been
    /// summarized). The reviewer's job is to roll the new Claim into
    /// this running state, not regenerate from scratch — keeps the
    /// editorial voice stable across summaries and lets the model
    /// reference what has already been said.
    pub previous_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExistingClaimBrief {
    pub id: String,
    pub claim: String,
}

/// One Claim's structured benchmark + leaderboard plus its
/// `README.md`, projected for the reviewer's cross-claim synthesis.
/// We pass both pieces because each carries different evidence: the
/// JSON has the canonical leaderboard rows the model aggregates
/// over, the README has the prose the author wrote about
/// methodology, edge cases, and what they think the result means.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimStructureBrief {
    pub claim_id: String,
    /// First markdown heading line of the Claim text — gives the
    /// LLM a stable handle for citing the claim in Notes.
    pub claim_title: String,
    /// The full `claim_structure` JSON value; the reviewer parses
    /// `benchmark`, `leaderboard`, and `winners` out of it.
    pub structure: serde_json::Value,
    /// `README.md` extracted from the Claim's artifact tarball
    /// (truncated by callers to keep prompt size bounded). `None`
    /// when the bundle didn't ship a README or the fetch failed.
    pub readme: Option<String>,
    /// Author's `$ effort` for this Claim (author USD + KrabReview
    /// USD). Powers the top-10-by-effort filter so the summary
    /// surfaces the most-invested work, not just the most-recent.
    pub effort_usd: f64,
}

#[derive(Clone)]
pub struct GeminiClient {
    http: Client,
    api_key: Option<String>,
    model: String,
}

impl GeminiClient {
    pub fn new(api_key: Option<String>) -> Self {
        // `pool_idle_timeout(15s)` evicts pooled connections faster
        // than reqwest's default (90s). Cloud Run instances often
        // sit idle for minutes; without this, the next post-idle
        // call reuses a dead connection and immediately fails with
        // `error sending request for url (...)`. The retry helper
        // (`post_json_with_retry`) catches what slips through.
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .pool_idle_timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());
        // Allow ops/dev to override the model at boot via env var
        // (e.g. trial gemini-2.5-pro before flipping the default).
        // Non-empty value wins; empty / unset falls through to the
        // compile-time DEFAULT_MODEL.
        let model = std::env::var("GEMINI_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self {
            http,
            api_key,
            model,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.api_key
            .as_deref()
            .is_some_and(|k| !k.trim().is_empty())
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref().filter(|k| !k.trim().is_empty())
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Expose the underlying HTTP client so `agentic_reviewer` can reuse
    /// the pool + REQUEST_TIMEOUT instead of standing up its own.
    pub fn http_client(&self) -> &Client {
        &self.http
    }

    /// Embed `text` via Gemini's `gemini-embedding-001` model,
    /// truncated to `dims` dimensions via matryoshka.
    ///
    /// Returns a `dims`-element float vector. Today's callers:
    ///   · claim-post dedup pre-check passes `dims=768` to match the
    ///     `claim_embedding vector(768)` column (migration 023).
    ///   · search ingestion passes `dims=768` to match the
    ///     `search_embedding vector(768)` columns (migration 051 —
    ///     0.19.3 swapped from 1536-dim OpenAI to 768-dim Gemini for
    ///     pricing + provider consistency).
    ///
    /// `gemini-embedding-001` natively emits 3072 dims and supports
    /// matryoshka truncation to any smaller dim — cosine similarities
    /// stay stable, so a 768-dim vector is good enough for both
    /// product surfaces at a fraction of the storage / index cost.
    /// Cheap (~$0.0001 / call at Google list price) so we can afford
    /// to run on every claim post + write-path search index write.
    pub async fn embed_text(&self, text: &str, dims: u32) -> Result<Vec<f32>, GeminiError> {
        let api_key = self.api_key().ok_or(GeminiError::NoApiKey)?;
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-001:embedContent?key={api_key}"
        );
        let body = serde_json::json!({
            "model": "models/gemini-embedding-001",
            "content": { "parts": [{ "text": text }] },
            "outputDimensionality": dims,
        });
        let resp =
            post_json_with_retry(&self.http, &url, &body, "embed", "gemini-embedding-001").await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GeminiError::Status {
                status: status.as_u16(),
                body,
            });
        }
        let json: Value = resp
            .json()
            .await
            .map_err(|e| GeminiError::Parse(e.to_string()))?;
        let values = json
            .get("embedding")
            .and_then(|e| e.get("values"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| GeminiError::Parse("missing embedding.values".into()))?;
        values
            .iter()
            .map(|v| {
                v.as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| GeminiError::Parse("non-float in embedding values".into()))
            })
            .collect()
    }
}

/// POST `body` JSON to `url` via `client`, retrying transient
/// failures with exponential backoff. The retry policy targets the
/// failure modes Gemini's API actually surfaces in practice:
///
///   - `is_connect()` errors: cheap (fail in ms), retry — these
///     are the stale-pool race that bites Cloud Run cold starts.
///   - HTTP 503 / 429 responses: Gemini's load balancer literally
///     says "high demand, retry later" with these statuses (since
///     0.8.17 — caught when reproducing E2E flake locally). Retry
///     with longer backoff since they reflect real upstream load.
///   - HTTP 4xx (other) / 5xx (other) / timeouts: pass through
///     unchanged. 4xx is our fault; non-503 5xx is uncommon and
///     the response body has actionable detail; timeouts mean
///     Gemini is genuinely slow and retrying just stacks waits.
///
/// Worst-case wall time: 3 attempts × max(connect-fail-ms, ~200ms
/// gemini-503-response) + backoffs (300ms + 800ms + 2000ms) =
/// ~3.5s for retried failures, or REQUEST_TIMEOUT (45s) once for
/// timeouts. Bounded under the 90s wall-clock cap.
///
/// All four Gemini call sites (agentic_reviewer stage 1+2,
/// embed_text, dedup_judge) share this helper.
pub async fn post_json_with_retry(
    client: &Client,
    url: &str,
    body: &Value,
    purpose: &'static str,
    model: &str,
) -> Result<reqwest::Response, GeminiError> {
    use crate::services::gemini_audit;
    const MAX_RETRIES: u32 = 3;

    // One request_id per logical call so retries can be GROUP BY'd
    // back together. attempt_no in the audit row is 1-indexed
    // (matches the human-readable retry log lines).
    let request_id = uuid::Uuid::new_v4();
    let model_owned = model.to_string();

    for attempt in 0..=MAX_RETRIES {
        let started = std::time::Instant::now();
        match client.post(url).json(body).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let elapsed_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                // Gemini 503 (UNAVAILABLE / "high demand") and 429
                // (rate limit) are explicitly transient — retry.
                // Everything else (200, 4xx, other 5xx) returns now.
                if (status == 503 || status == 429) && attempt < MAX_RETRIES {
                    // Longer backoff than for connect errors —
                    // upstream is busy, give it time. 300ms / 800ms
                    // / 2000ms.
                    let backoff_ms = match attempt {
                        0 => 300,
                        1 => 800,
                        _ => 2000,
                    };
                    // Structured fields below let Cloud Logging
                    // build a log-based metric for the 503/429 rate.
                    // Filter: `jsonPayload.event = "gemini.transient_retry"`.
                    tracing::warn!(
                        event = "gemini.transient_retry",
                        attempt = attempt + 1,
                        backoff_ms,
                        status,
                        kind = "upstream_status",
                        "gemini upstream {status}, retrying"
                    );
                    gemini_audit::record(
                        request_id,
                        (attempt + 1) as i32,
                        purpose,
                        model_owned.clone(),
                        gemini_audit::status::TRANSIENT_RETRY,
                        Some(status as i32),
                        Some("upstream_status"),
                        None,
                        elapsed_ms,
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                // Terminal: success or non-retried 4xx/5xx.
                let row_status = if resp.status().is_success() {
                    gemini_audit::status::SUCCESS
                } else {
                    gemini_audit::status::NON_2XX_TERMINAL
                };
                gemini_audit::record(
                    request_id,
                    (attempt + 1) as i32,
                    purpose,
                    model_owned.clone(),
                    row_status,
                    Some(status as i32),
                    if resp.status().is_success() {
                        None
                    } else {
                        Some("upstream_status")
                    },
                    None,
                    elapsed_ms,
                );
                return Ok(resp);
            }
            Err(e) if attempt < MAX_RETRIES && e.is_connect() => {
                let elapsed_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                let backoff_ms = 200u64 << attempt; // 200, 400, 800
                tracing::warn!(
                    event = "gemini.transient_retry",
                    attempt = attempt + 1,
                    backoff_ms,
                    error = %e,
                    kind = "connect",
                    "gemini connect error, retrying"
                );
                gemini_audit::record(
                    request_id,
                    (attempt + 1) as i32,
                    purpose,
                    model_owned.clone(),
                    gemini_audit::status::TRANSIENT_RETRY,
                    None,
                    Some("connect"),
                    Some(e.to_string()),
                    elapsed_ms,
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
            // Timeout retry — ONCE per call, with a TIGHTER
            // per-attempt timeout (15s vs the client's 45s default)
            // so the retry can't compound into a multi-minute wall.
            // (since 0.8.18) Brings CI flap rate from ~30% to <5%
            // by recovering from single-turn slow Gemini responses
            // while bounding worst-case wall time at 45s + 15s = 60s
            // per call.
            Err(e)
                if attempt < MAX_RETRIES
                    && e.is_timeout()
                    && /* only ONE timeout retry per call */ attempt == 0 =>
            {
                let elapsed_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                tracing::warn!(
                    event = "gemini.transient_retry",
                    attempt = attempt + 1,
                    error = %e,
                    kind = "timeout",
                    retry_timeout_ms = 15_000,
                    "gemini call timed out, retrying once with 15s budget"
                );
                gemini_audit::record(
                    request_id,
                    (attempt + 1) as i32,
                    purpose,
                    model_owned.clone(),
                    gemini_audit::status::TRANSIENT_RETRY,
                    None,
                    Some("timeout"),
                    Some(e.to_string()),
                    elapsed_ms,
                );
                // Override the request timeout on this single retry.
                let retry_started = std::time::Instant::now();
                let retry_resp = client
                    .post(url)
                    .json(body)
                    .timeout(std::time::Duration::from_secs(15))
                    .send()
                    .await;
                let retry_elapsed =
                    retry_started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                match retry_resp {
                    Ok(r) => {
                        let s = r.status().as_u16();
                        let row_status = if r.status().is_success() {
                            gemini_audit::status::SUCCESS
                        } else {
                            gemini_audit::status::NON_2XX_TERMINAL
                        };
                        gemini_audit::record(
                            request_id,
                            (attempt + 2) as i32,
                            purpose,
                            model_owned.clone(),
                            row_status,
                            Some(s as i32),
                            if r.status().is_success() {
                                None
                            } else {
                                Some("upstream_status")
                            },
                            None,
                            retry_elapsed,
                        );
                        return Ok(r);
                    }
                    Err(retry_err) => {
                        tracing::error!(
                            event = "gemini.retry_exhausted",
                            error = %retry_err,
                            kind = "timeout",
                            "gemini timeout retry also failed"
                        );
                        // Audit log gets the raw error (server-side
                        // only); client-facing GeminiError must be
                        // redacted so `?key=AIzaSy…` never reaches a
                        // REVIEWER_UNAVAILABLE response body.
                        gemini_audit::record(
                            request_id,
                            (attempt + 2) as i32,
                            purpose,
                            model_owned.clone(),
                            gemini_audit::status::TRANSPORT_ERROR,
                            None,
                            Some("timeout"),
                            Some(retry_err.to_string()),
                            retry_elapsed,
                        );
                        return Err(GeminiError::Transport(redact_url_secrets(
                            &retry_err.to_string(),
                        )));
                    }
                }
            }
            Err(e) => {
                let elapsed_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                // Catch-all: non-retryable transport error (TLS,
                // dns, etc., OR a timeout/connect past the retry
                // budget). Terminal, return Err.
                let error_kind: &'static str = if e.is_timeout() {
                    "timeout"
                } else if e.is_connect() {
                    "connect"
                } else {
                    "transport"
                };
                gemini_audit::record(
                    request_id,
                    (attempt + 1) as i32,
                    purpose,
                    model_owned.clone(),
                    gemini_audit::status::TRANSPORT_ERROR,
                    None,
                    Some(error_kind),
                    Some(e.to_string()),
                    elapsed_ms,
                );
                return Err(GeminiError::Transport(redact_url_secrets(&e.to_string())));
            }
        }
    }
    // Exhausted retries — make one more attempt with no retry,
    // propagate the result. If THIS one also fails, surface a
    // structured ERROR log so Cloud Logging picks it up as a
    // distinct "retry budget exhausted" signal vs the transient
    // retries above.
    let final_started = std::time::Instant::now();
    let resp = match client.post(url).json(body).send().await {
        Ok(r) => r,
        Err(e) => {
            let elapsed_ms = final_started.elapsed().as_millis().min(i32::MAX as u128) as i32;
            tracing::error!(
                event = "gemini.retry_exhausted",
                error = %e,
                kind = "transport",
                "gemini retries exhausted with transport error"
            );
            gemini_audit::record(
                request_id,
                (MAX_RETRIES + 2) as i32,
                purpose,
                model_owned.clone(),
                gemini_audit::status::EXHAUSTED,
                None,
                Some("transport"),
                Some(e.to_string()),
                elapsed_ms,
            );
            return Err(GeminiError::Transport(redact_url_secrets(&e.to_string())));
        }
    };
    let status = resp.status().as_u16();
    let final_elapsed = final_started.elapsed().as_millis().min(i32::MAX as u128) as i32;
    if status == 503 || status == 429 {
        tracing::error!(
            event = "gemini.retry_exhausted",
            status,
            kind = "upstream_status",
            "gemini retries exhausted, upstream still {status}"
        );
        gemini_audit::record(
            request_id,
            (MAX_RETRIES + 2) as i32,
            purpose,
            model_owned.clone(),
            gemini_audit::status::EXHAUSTED,
            Some(status as i32),
            Some("upstream_status"),
            None,
            final_elapsed,
        );
    } else {
        let row_status = if resp.status().is_success() {
            gemini_audit::status::SUCCESS
        } else {
            gemini_audit::status::NON_2XX_TERMINAL
        };
        gemini_audit::record(
            request_id,
            (MAX_RETRIES + 2) as i32,
            purpose,
            model_owned.clone(),
            row_status,
            Some(status as i32),
            if resp.status().is_success() {
                None
            } else {
                Some("upstream_status")
            },
            None,
            final_elapsed,
        );
    }
    Ok(resp)
}

/// Cosine similarity of two float vectors. Returns NaN if either
/// is the zero vector. Used by the claim post handler's semantic
/// dedup pre-check; pulled out so it's unit-testable without
/// touching the network or DB.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return f32::NAN;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return f32::NAN;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_secrets_strips_api_key_query_strings() {
        // The original real-world leak (fermatix-2 report #19): full
        // `?key=AIzaSy…` ended up in the client-visible error body.
        let raw = "gemini request failed: error sending request for url \
                   (https://generativelanguage.googleapis.com/v1beta/models/\
                   gemini-2.5-flash:generateContent?key=AIzaSyFAKE00000000000000000000000000000)";
        let out = redact_url_secrets(raw);
        assert!(!out.contains("AIzaSy"), "redaction left API key: {out}");
        assert!(!out.contains("?key="), "redaction left ?key= prefix: {out}");
        assert!(
            out.contains(
                "https://generativelanguage.googleapis.com/v1beta/models/\
                 gemini-2.5-flash:generateContent"
            ),
            "redaction stripped too much, host/path missing: {out}"
        );
    }

    #[test]
    fn redact_url_secrets_handles_multiple_urls_and_trailing_parens() {
        let raw =
            "two: https://a.example/x?key=secret1 and (https://b.example/y?token=secret2) done";
        let out = redact_url_secrets(raw);
        assert!(!out.contains("secret1"));
        assert!(!out.contains("secret2"));
        assert!(out.contains("https://a.example/x"));
        assert!(out.contains("https://b.example/y"));
    }

    #[test]
    fn redact_url_secrets_noop_on_clean_text() {
        let raw = "plain prose with no urls";
        assert_eq!(redact_url_secrets(raw), raw);
    }

    #[test]
    fn redact_url_secrets_leaves_urls_without_query_intact() {
        let raw = "see https://example.com/path for details";
        assert_eq!(redact_url_secrets(raw), raw);
    }

    #[test]
    fn system_prompt_encodes_rubric_sections() {
        // Guardrail: regressions on the system prompt usually come from
        // someone accidentally gutting the rubric. Pin the section
        // anchors here.
        for anchor in [
            "MODERATION",
            "QUALITY",
            "HARD-FLOOR",
            "ENGAGEMENT",
            "ARTIFACT SUMMARY",
            "Hacker News",
        ] {
            assert!(
                CLAIM_REVIEW_SYSTEM_PROMPT.contains(anchor),
                "system prompt missing `{anchor}`"
            );
        }
    }

    #[test]
    fn is_configured_reflects_key() {
        let none = GeminiClient::new(None);
        assert!(!none.is_configured());
        let blank = GeminiClient::new(Some("   ".into()));
        assert!(!blank.is_configured());
        let set = GeminiClient::new(Some("abc".into()));
        assert!(set.is_configured());
    }

    #[test]
    fn estimate_cost_known_models() {
        // 1M prompt + 1M completion at the Flash rate is
        // $0.075 + $0.30 = $0.375 = 375_000 µUSD.
        let cost = estimate_cost_micros("gemini-3-flash-preview", 1_000_000, 1_000_000);
        assert_eq!(cost, 375_000, "1M prompt + 1M completion at Flash rate");
        // 2.5-flash + flash-latest share rates.
        assert_eq!(
            estimate_cost_micros("gemini-2.5-flash", 1_000_000, 1_000_000),
            375_000
        );
        assert_eq!(
            estimate_cost_micros("gemini-flash-latest", 1_000_000, 1_000_000),
            375_000
        );
    }

    #[test]
    fn estimate_cost_typical_review() {
        // 10_000 prompt + 5_000 completion at the Flash rate:
        //   prompt:     10_000 * 75_000  / 1M =   750 µUSD
        //   completion:  5_000 * 300_000 / 1M = 1_500 µUSD
        // total = 2_250 µUSD = $0.00225
        let cost = estimate_cost_micros("gemini-3-flash-preview", 10_000, 5_000);
        assert_eq!(cost, 2_250);
    }

    #[test]
    fn estimate_cost_unknown_model_is_zero() {
        assert_eq!(estimate_cost_micros("unknown-model-x", 1000, 1000), 0);
    }

    #[test]
    fn estimate_cost_zero_tokens() {
        assert_eq!(estimate_cost_micros("gemini-3-flash-preview", 0, 0), 0);
    }

    #[test]
    fn parse_usage_extracts_token_counts() {
        let payload = serde_json::json!({
            "candidates": [],
            "usageMetadata": {
                "promptTokenCount": 1234,
                "candidatesTokenCount": 567
            }
        });
        let usage = parse_usage(&payload, "gemini-3-flash-preview");
        assert_eq!(usage.prompt_tokens, 1234);
        assert_eq!(usage.completion_tokens, 567);
        // 1234 * 75_000  / 1M  =  92  µUSD (floor)
        // 567  * 300_000 / 1M  = 170  µUSD (floor)
        // total = 262 µUSD = $0.000262
        assert_eq!(usage.cost_micros, 262);
    }

    #[test]
    fn parse_usage_handles_missing_metadata() {
        let payload = serde_json::json!({"candidates": []});
        let usage = parse_usage(&payload, "gemini-3-flash-preview");
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.cost_micros, 0);
    }

    #[test]
    fn usage_merge_sums_fields() {
        let mut a = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            cost_micros: 250,
        };
        a.merge(Usage {
            prompt_tokens: 200,
            completion_tokens: 75,
            cost_micros: 500,
        });
        assert_eq!(a.prompt_tokens, 300);
        assert_eq!(a.completion_tokens, 125);
        assert_eq!(a.cost_micros, 750);
    }
}
