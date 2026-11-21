//! Benchmark short-label generator (0.28.x, PR-B).
//!
//! The Claims Wins matrix labels each column with the claim's
//! `claim_structure.benchmark.short_label` — a crisp, ≤~20-char
//! benchmark name ("SimpleQA-200", "You.com 50-q recall"). This module
//! is the agent that *picks* those labels: it reads every live claim's
//! benchmark name on a battle, asks Gemini for a tight label per claim
//! (steered by the battle's freeform `presentation_guidance` when set),
//! and writes the result back into `claim_structure`.
//!
//! Runs in two situations: as a backfill over an existing battle
//! (`POST /v1/battles/{id}/relabel`) and — once wired — alongside summary
//! regeneration so new claims get labels automatically. Persisted on the
//! claim so the render path stays a pure read (no per-request LLM call).
//!
//! Auth lives in the route handler; this module is content-only.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::services::gemini_service::GeminiClient;

/// Target label length the prompt asks for. The render path
/// (`shorten_benchmark_label`) caps display at ~20 chars regardless, but
/// telling the model keeps it from emitting a sentence.
const LABEL_CHAR_TARGET: usize = 20;

/// One claim's benchmark, as fed to the labeler.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimBenchmark {
    pub claim_id: Uuid,
    /// `claim_structure.benchmark.name` — the full benchmark name.
    pub benchmark_name: String,
    /// The claim's short statement, for disambiguation when two claims
    /// share a benchmark name but measure different cuts of it.
    pub claim: String,
}

/// Result of a relabel pass.
#[derive(Debug, Default, serde::Serialize)]
pub struct RelabelOutcome {
    pub labeled: usize,
    pub skipped_no_benchmark: usize,
    /// `claim_id → short_label` that were written.
    pub labels: HashMap<Uuid, String>,
}

/// Backfill `short_label` for every live claim on a battle. Loads the
/// benchmarks, asks Gemini for crisp labels, and persists each onto the
/// claim's `claim_structure`. Returns how many were labeled. A claim
/// with no `claim_structure.benchmark.name` is skipped (counted in
/// `skipped_no_benchmark`), not failed.
pub async fn relabel_battle(
    pool: &PgPool,
    gemini: &GeminiClient,
    battle_id: Uuid,
    guidance: Option<&str>,
) -> Result<RelabelOutcome, ApiError> {
    let benchmarks = load_battle_benchmarks(pool, battle_id).await?;
    if benchmarks.is_empty() {
        return Ok(RelabelOutcome::default());
    }

    let labels = generate_short_labels(gemini, &benchmarks, guidance).await?;

    let mut outcome = RelabelOutcome {
        labeled: 0,
        skipped_no_benchmark: 0,
        labels: HashMap::new(),
    };
    for b in &benchmarks {
        let Some(label) = labels.get(&b.claim_id) else {
            continue;
        };
        let label = clamp_label(label);
        if label.is_empty() {
            continue;
        }
        persist_label(pool, b.claim_id, &label).await?;
        outcome.labeled += 1;
        outcome.labels.insert(b.claim_id, label);
    }
    Ok(outcome)
}

/// Load every live claim on the battle that has a benchmark name.
async fn load_battle_benchmarks(
    pool: &PgPool,
    battle_id: Uuid,
) -> Result<Vec<ClaimBenchmark>, ApiError> {
    let rows: Vec<(Uuid, Option<String>, String)> = sqlx::query_as(
        "SELECT id, \
                claim_structure->'benchmark'->>'name' AS benchmark_name, \
                claim \
           FROM claims \
          WHERE battle_id = $1 \
            AND claim_structure IS NOT NULL \
            AND deleted_at IS NULL \
          ORDER BY created_at",
    )
    .bind(battle_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(rows
        .into_iter()
        .filter_map(|(claim_id, name, claim)| {
            let name = name?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(ClaimBenchmark {
                claim_id,
                benchmark_name: name,
                claim,
            })
        })
        .collect())
}

/// Write a single claim's `short_label` into its
/// `claim_structure.benchmark.short_label` via `jsonb_set` (creates the
/// key if missing). No-op if the claim somehow lost its structure.
async fn persist_label(pool: &PgPool, claim_id: Uuid, label: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE claims \
            SET claim_structure = jsonb_set( \
                    claim_structure, '{benchmark,short_label}', to_jsonb($1::text), true) \
          WHERE id = $2 AND claim_structure IS NOT NULL",
    )
    .bind(label)
    .bind(claim_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(())
}

/// Ask Gemini for one crisp label per claim. Returns `claim_id →
/// short_label`; claims the model omits simply don't appear.
async fn generate_short_labels(
    gemini: &GeminiClient,
    benchmarks: &[ClaimBenchmark],
    guidance: Option<&str>,
) -> Result<HashMap<Uuid, String>, ApiError> {
    let api_key = gemini
        .api_key()
        .ok_or_else(|| ApiError::ValidationError("GEMINI_API_KEY not configured".into()))?;
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        gemini.model(),
        api_key
    );
    let body = serde_json::json!({
        "system_instruction": { "parts": [{ "text": SYSTEM_PROMPT }] },
        "contents": [{ "role": "user", "parts": [{ "text": build_user_message(benchmarks, guidance) }] }],
        "generationConfig": { "temperature": 0.2, "responseMimeType": "application/json" },
    });
    let resp = gemini
        .http_client()
        .post(&url)
        .timeout(std::time::Duration::from_secs(120))
        .json(&body)
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ApiError::ValidationError(format!(
            "Gemini error {}: {}",
            status,
            text.chars().take(400).collect::<String>()
        )));
    }
    let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        ApiError::ValidationError(format!("Gemini response parse failed: {e} (body: {text})"))
    })?;
    let label_json = parsed
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::ValidationError(format!(
                "Gemini response missing text part (body: {})",
                text.chars().take(400).collect::<String>()
            ))
        })?;
    parse_label_response(label_json)
        .map_err(|e| ApiError::ValidationError(format!("label JSON parse failed: {e}")))
}

const SYSTEM_PROMPT: &str = "You name benchmark columns for a leaderboard table. \
For each claim you are given a UUID and the benchmark it measures, output a SHORT, crisp \
column label — the benchmark's name, NOT a description of the result. Rules: keep each \
label under ~20 characters; prefer the canonical benchmark name plus its key parameter \
(e.g. \"SimpleQA-200\", \"FRAMES-200\", \"You.com 50-q recall\"); never include the \
winning tool's name or the outcome; when several claims share a benchmark family, keep \
their labels distinct by their parameter (size, split, metric). Respond with ONLY a JSON \
object mapping each claim UUID (string) to its label (string). No prose, no markdown.";

/// Build the per-call user message: the optional battle guidance, then a
/// line per claim with its UUID, benchmark name, and claim statement.
fn build_user_message(benchmarks: &[ClaimBenchmark], guidance: Option<&str>) -> String {
    let mut msg = String::new();
    if let Some(g) = guidance.map(str::trim).filter(|g| !g.is_empty()) {
        msg.push_str("Battle presentation guidance (follow it for label style):\n");
        msg.push_str(g);
        msg.push_str("\n\n");
    }
    msg.push_str(&format!(
        "Label these {} benchmark{} (aim for ≤{LABEL_CHAR_TARGET} chars each):\n",
        benchmarks.len(),
        if benchmarks.len() == 1 { "" } else { "s" },
    ));
    for b in benchmarks {
        msg.push_str(&format!(
            "- {}\n    benchmark: {}\n    claim: {}\n",
            b.claim_id,
            b.benchmark_name,
            truncate(&b.claim, 160),
        ));
    }
    msg
}

/// Parse the model's JSON object (tolerating ```json fences) into a
/// `claim_id → label` map. Entries with an unparseable UUID or an empty
/// label are dropped, not fatal.
fn parse_label_response(raw: &str) -> Result<HashMap<Uuid, String>, serde_json::Error> {
    let cleaned = strip_code_fence(raw);
    let map: HashMap<String, String> = serde_json::from_str(cleaned)?;
    Ok(map
        .into_iter()
        .filter_map(|(k, v)| {
            let id = Uuid::parse_str(k.trim()).ok()?;
            let label = v.trim().to_string();
            if label.is_empty() {
                return None;
            }
            Some((id, label))
        })
        .collect())
}

/// Strip a leading/trailing markdown code fence (```json … ```), which
/// Gemini sometimes adds even with `responseMimeType: application/json`.
fn strip_code_fence(s: &str) -> &str {
    let t = s.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    t.strip_suffix("```").unwrap_or(t).trim()
}

/// Clamp a label to the display budget on a char boundary (defensive —
/// the model is asked to stay short, but never trust it).
fn clamp_label(label: &str) -> String {
    let label = label.trim();
    if label.chars().count() <= LABEL_CHAR_TARGET {
        return label.to_string();
    }
    let mut out: String = label.chars().take(LABEL_CHAR_TARGET - 1).collect();
    out.push('…');
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_label_response_maps_uuids_and_drops_junk() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let raw =
            format!("{{\"{id1}\": \"SimpleQA-200\", \"{id2}\": \"  \", \"not-a-uuid\": \"x\"}}");
        let out = parse_label_response(&raw).unwrap();
        assert_eq!(out.get(&id1).map(String::as_str), Some("SimpleQA-200"));
        assert!(!out.contains_key(&id2), "empty label dropped");
        assert_eq!(out.len(), 1, "bad uuid dropped");
    }

    #[test]
    fn parse_label_response_tolerates_code_fence() {
        let id = Uuid::new_v4();
        let raw = format!("```json\n{{\"{id}\": \"FRAMES-200\"}}\n```");
        let out = parse_label_response(&raw).unwrap();
        assert_eq!(out.get(&id).map(String::as_str), Some("FRAMES-200"));
    }

    #[test]
    fn clamp_label_caps_long_labels_on_char_boundary() {
        assert_eq!(clamp_label("SimpleQA-200"), "SimpleQA-200");
        let long = "An overly verbose benchmark description that runs on";
        let out = clamp_label(long);
        assert!(out.chars().count() <= LABEL_CHAR_TARGET);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn user_message_includes_guidance_and_each_claim() {
        let id = Uuid::new_v4();
        let benches = vec![ClaimBenchmark {
            claim_id: id,
            benchmark_name: "SimpleQA 200-question accuracy".into(),
            claim: "Exa beats You.com".into(),
        }];
        let msg = build_user_message(&benches, Some("  keep it terse  "));
        assert!(msg.contains("presentation guidance"));
        assert!(msg.contains("keep it terse"));
        assert!(msg.contains(&id.to_string()));
        assert!(msg.contains("SimpleQA 200-question accuracy"));
        // No guidance → no guidance preamble.
        let msg2 = build_user_message(&benches, None);
        assert!(!msg2.contains("presentation guidance"));
    }
}
