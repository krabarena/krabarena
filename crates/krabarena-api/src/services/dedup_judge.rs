//! Gemini-Flash dedup judge for the claim post handler.
//!
//! Given a new claim's text and the top-K vector-nearest existing
//! claims on the same battle, ask Gemini Flash to classify the
//! relationship as one of:
//!
//! - `duplicate` — same conclusion / experiment as an existing
//!   claim. Action: block. Suggest the user delete or comment on
//!   the existing one instead.
//! - `improvement` — extends / refines an existing claim's
//!   leaderboard or methodology. Action: warn unless the new claim
//!   already references that claim via `--predecessor` and the
//!   title clearly advertises the new value.
//! - `new` — independent claim sharing only surface vocabulary with
//!   the neighbours. Action: proceed.
//!
//! This module owns the prompt + the JSON schema + the parsing.
//! The pgvector top-K query lives in `claim_service`; the policy
//! decision (which verdict translates to which HTTP code) lives in
//! `routes::claims`.

use serde::Deserialize;
use uuid::Uuid;

use crate::services::claim_service::DedupCandidate;
use crate::services::gemini_service::{self, GeminiClient, GeminiError};

/// Cosine similarities below this are treated as "definitely
/// independent" — the LLM judge is never invoked. Catches cases
/// where the only shared signal is generic vocabulary on the
/// battle's topic.
///
/// Tuned for `gemini-embedding-001` truncated to 768 dimensions:
/// genuine paraphrases of the same conclusion sit around 0.83-0.90,
/// same-topic-different-angle around 0.70-0.80, surface-vocabulary
/// matches under 0.70.
pub const SIMILARITY_LOWER_BOUND: f32 = 0.78;

/// Cosine similarities at or above this are treated as "definitely
/// the same claim" — block immediately without burning a Gemini
/// call. Tuned for `gemini-embedding-001` @ 768 dims: identical text
/// gives ~1.00, trivially edited copies sit ≥ 0.95.
pub const HARD_DUPLICATE_THRESHOLD: f32 = 0.95;

/// Verdict from Gemini Flash. Matches the `responseSchema` we ask
/// the model for; deserialised straight off the wire.
#[derive(Debug, Clone, Deserialize)]
pub struct DedupVerdict {
    pub verdict: DedupVerdictKind,
    /// The candidate the new claim relates to. Required when
    /// `verdict` is `duplicate` or `improvement`; ignored when `new`.
    /// Parsed back into a real `Uuid` after the LLM round-trip.
    #[serde(default)]
    pub related_claim_id: Option<Uuid>,
    /// One sentence on what the new claim contributes beyond
    /// `related_claim_id`. Populated for `improvement` so the
    /// suggestion can quote it back to the author.
    #[serde(default)]
    pub added_value_summary: Option<String>,
    /// One sentence rationale — not surfaced to the user but useful
    /// in logs when verdicts disagree with author intent.
    #[serde(default)]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DedupVerdictKind {
    Duplicate,
    Improvement,
    New,
}

const SYSTEM_PROMPT: &str = "You are KrabReviewer's deduplication judge. \
You will be shown a NEW CLAIM and up to 3 CANDIDATE claims on the same battle, \
ranked by embedding similarity. Decide whether the NEW CLAIM is:\n\
\n\
  - `duplicate`   — same experiment, same conclusion, no meaningful \
                    new information. Reposting the same finding under a \
                    fresh wrapper is a duplicate.\n\
  - `improvement` — extends or refines a CANDIDATE: bigger sample, \
                    new sampler, new ranking, new methodology, retraction, \
                    bug fix. The author is moving the leaderboard forward.\n\
  - `new`         — shares only surface vocabulary with the candidates; \
                    independent question or independent answer.\n\
\n\
Return strict JSON only. No commentary outside the JSON.";

/// Run the LLM judge on `(new_claim, candidates)`. `gemini` must be
/// configured; check `is_configured()` before calling.
pub async fn judge(
    gemini: &GeminiClient,
    new_claim: &str,
    candidates: &[DedupCandidate],
) -> Result<DedupVerdict, GeminiError> {
    let api_key = gemini.api_key().ok_or(GeminiError::NoApiKey)?;

    let prompt = build_user_prompt(new_claim, candidates);

    let body = serde_json::json!({
        "systemInstruction": {
            "parts": [{ "text": SYSTEM_PROMPT }]
        },
        "contents": [{
            "role": "user",
            "parts": [{ "text": prompt }]
        }],
        "generationConfig": {
            "temperature": 0.1,
            "responseMimeType": "application/json",
            "responseSchema": {
                "type": "object",
                "properties": {
                    "verdict":            { "type": "string", "enum": ["duplicate", "improvement", "new"] },
                    "related_claim_id":   { "type": "string", "nullable": true },
                    "added_value_summary": { "type": "string", "nullable": true },
                    "rationale":          { "type": "string", "nullable": true },
                },
                "required": ["verdict"],
            }
        }
    });

    let model = gemini_service::DEFAULT_MODEL;
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
    );
    let resp = gemini_service::post_json_with_retry(
        gemini.http_client(),
        &url,
        &body,
        "dedup_judge",
        model,
    )
    .await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(GeminiError::Status { status, body });
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| GeminiError::Parse(e.to_string()))?;
    let text = json
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| GeminiError::Parse("missing candidates[0].content.parts[0].text".into()))?;
    serde_json::from_str(text).map_err(|e| GeminiError::Parse(e.to_string()))
}

fn build_user_prompt(new_claim: &str, candidates: &[DedupCandidate]) -> String {
    let mut s = String::new();
    s.push_str("NEW CLAIM:\n");
    s.push_str(&truncate(new_claim, 2000));
    s.push_str("\n\n---\n");
    s.push_str("CANDIDATES (top nearest by embedding similarity, descending):\n");
    for (i, c) in candidates.iter().enumerate() {
        s.push_str(&format!(
            "\n[{n}] id={id} similarity={sim:.3}\n",
            n = i + 1,
            id = c.id,
            sim = c.similarity
        ));
        s.push_str(&truncate(&c.claim, 2000));
        s.push('\n');
    }
    s.push_str(
        "\n---\nReturn `{verdict, related_claim_id, added_value_summary, rationale}` as JSON. \
         Set `related_claim_id` to one of the candidate ids when verdict is `duplicate` or \
         `improvement`. Use the EXACT id string shown above; do not invent a uuid. \
         Set `added_value_summary` to a single sentence explaining what the new claim adds \
         beyond `related_claim_id` when verdict is `improvement`.",
    );
    s
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push_str("\n…[truncated]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_constants_are_well_ordered() {
        // Sanity: the lower bound must be strictly less than the
        // hard-dupe threshold, otherwise the borderline window
        // (where the LLM judge runs) is empty and we either
        // over-block or under-block. Clippy collapses
        // const-vs-const compares at compile time so we go through
        // a const_assert pattern instead of `assert!`.
        const _: () = assert!(SIMILARITY_LOWER_BOUND < HARD_DUPLICATE_THRESHOLD);
    }

    #[test]
    fn prompt_includes_all_candidates() {
        let cs = vec![
            DedupCandidate {
                id: Uuid::nil(),
                slug: None,
                claim: "# A".into(),
                author_id: Uuid::nil(),
                similarity: 0.91,
            },
            DedupCandidate {
                id: Uuid::from_u128(1),
                slug: None,
                claim: "# B".into(),
                author_id: Uuid::nil(),
                similarity: 0.87,
            },
        ];
        let p = build_user_prompt("# X", &cs);
        assert!(p.contains("[1]"));
        assert!(p.contains("[2]"));
        assert!(p.contains("# X"));
        assert!(p.contains("# A"));
        assert!(p.contains("# B"));
    }

    #[test]
    fn truncate_caps_to_max_chars() {
        let s = "a".repeat(3000);
        let out = truncate(&s, 2000);
        // 2000 chars + "\n…[truncated]" suffix
        assert!(out.starts_with(&"a".repeat(2000)));
        assert!(out.contains("[truncated]"));
    }
}
