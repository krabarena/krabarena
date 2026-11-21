//! Lightweight Gemini sanity-check on every verification (verify or
//! refute) before it lands in the `verifications` table.
//!
//! Why: a Verify with `output_hash="asdf"`, `duration_ms=0`, and
//! `environment=""` is technically well-formed (the route's
//! deterministic checks just require the fields be non-empty for
//! Confirmed), but it's clearly fake. A Refute pointing at a typo
//! when the claim is about latency is irrelevant. Both should be
//! caught before they pollute the verify/refute counts.
//!
//! Cost: one Gemini call (~700 prompt + ~200 completion ≈ $0.0001
//! per verification). No agentic loop, no tool use. The prompt is
//! deliberately short so cost stays linear with traffic.
//!
//! Verdict semantics (mirrors the claim reviewer):
//!   - `pass`:  evidence is plausible, refutation is on-topic.
//!   - `warn`:  evidence is thin or refutation is loosely related.
//!     Verification still commits; the review row is the audit
//!     trail.
//!   - `block`: clearly fake (zero-duration, gibberish hash, empty
//!     env) or clearly off-topic. Route returns 422 with the
//!     review attached, exactly like the claim block path does.

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::services::gemini_service::{self, GeminiClient, Usage};

/// Confirmed = the verifier ran the methodology and got the same
/// result. Disconfirmed = they ran it and got a contradicting
/// result. Used to switch the prompt wording — for Confirmed we
/// look at execution evidence, for Disconfirmed at the failure
/// reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationVerdict {
    Confirmed,
    Disconfirmed,
}

impl VerificationVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationVerdict::Confirmed => "confirmed",
            VerificationVerdict::Disconfirmed => "disconfirmed",
        }
    }

    /// Map to the `claim_reviews.review_target` discriminator.
    /// Confirmed verifications land as `verify`, disconfirmed as
    /// `refute` — that lets cost roll-ups split spend by verdict
    /// without joining `verifications` back in.
    pub fn review_target(&self) -> &'static str {
        match self {
            VerificationVerdict::Confirmed => "verify",
            VerificationVerdict::Disconfirmed => "refute",
        }
    }
}

/// Outcome the route hands back. Mirrors the shape used elsewhere
/// (verdict enum + summary + suggestions list) so the 422 block
/// payload looks just like the claim-review block payload.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationReviewResult {
    pub verdict: String, // pass | warn | block
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
    pub model: String,
    #[serde(skip)]
    pub usage: Usage,
}

/// Gemini's structured output. Kept private — we project to
/// `VerificationReviewResult` after parsing.
#[derive(Debug, Deserialize, Default)]
struct GeminiVerificationOutcome {
    #[serde(default)]
    verdict: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    suggestions: Vec<String>,
}

/// Run the lightweight verify/refute review.
///
/// On `GEMINI_API_KEY` not set or Gemini transport hiccups, returns
/// a synthetic `pass` outcome with `model = "none"` — verifications
/// shouldn't break when the reviewer is unavailable, since the
/// route's deterministic field checks already catch the obvious
/// cases.
#[allow(clippy::too_many_arguments)]
pub async fn review_verification(
    gemini: &GeminiClient,
    claim_text: &str,
    verdict: VerificationVerdict,
    environment: Option<&str>,
    output_hash: Option<&str>,
    duration_ms: Option<i32>,
    failure_reason: Option<&str>,
    conditions: Option<&str>,
    reproduction_steps: Option<&str>,
) -> Result<VerificationReviewResult, ApiError> {
    let api_key = match gemini.api_key() {
        Some(k) => k.to_string(),
        None => {
            return Ok(VerificationReviewResult {
                verdict: "pass".into(),
                summary:
                    "GEMINI_API_KEY not set; verification reviewed by deterministic checks only."
                        .into(),
                suggestions: Vec::new(),
                model: "none".into(),
                usage: Usage::default(),
            });
        }
    };

    let prompt = build_prompt(
        claim_text,
        verdict,
        environment,
        output_hash,
        duration_ms,
        failure_reason,
        conditions,
        reproduction_steps,
    );

    let body = serde_json::json!({
        "systemInstruction": {
            "parts": [{
                "text": "You are KrabReviewer, sanity-checking a single \
                        verification or refutation against the claim it \
                        responds to. Return strict JSON only, no \
                        commentary outside the JSON."
            }]
        },
        "contents": [{
            "role": "user",
            "parts": [{ "text": prompt }]
        }],
        "generationConfig": {
            "temperature": 0.2,
            "responseMimeType": "application/json",
        }
    });

    let model = gemini_service::DEFAULT_MODEL;
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    let resp = match gemini.http_client().post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            // Reviewer unavailable should NOT block a legitimate
            // verification. Log + synth-pass.
            tracing::warn!(error = %e, "verification.review.transport_error.synth_pass");
            return Ok(VerificationReviewResult {
                verdict: "pass".into(),
                summary: format!("KrabReviewer unavailable ({e}); verification accepted."),
                suggestions: Vec::new(),
                model: "none".into(),
                usage: Usage::default(),
            });
        }
    };

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            status = status,
            body_preview = %body.chars().take(200).collect::<String>(),
            "verification.review.gemini_error.synth_pass"
        );
        return Ok(VerificationReviewResult {
            verdict: "pass".into(),
            summary: format!("KrabReviewer returned {status}; verification accepted."),
            suggestions: Vec::new(),
            model: "none".into(),
            usage: Usage::default(),
        });
    }

    let payload: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "verification.review.parse_error.synth_pass");
            return Ok(VerificationReviewResult {
                verdict: "pass".into(),
                summary: format!("KrabReviewer parse failure ({e}); verification accepted."),
                suggestions: Vec::new(),
                model: "none".into(),
                usage: Usage::default(),
            });
        }
    };
    let usage = gemini_service::parse_usage(&payload, model);
    let text = payload["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("{}");
    let parsed: GeminiVerificationOutcome = serde_json::from_str(text).unwrap_or_default();

    let verdict_str = match parsed.verdict.as_deref() {
        Some("block") => "block",
        Some("warn") => "warn",
        _ => "pass",
    }
    .to_string();

    let summary = parsed
        .summary
        .unwrap_or_else(|| "KrabReviewer accepted the verification.".into());

    Ok(VerificationReviewResult {
        verdict: verdict_str,
        summary,
        suggestions: parsed.suggestions,
        model: model.to_string(),
        usage,
    })
}

/// Build the user-message prompt. Kept as a free function so it's
/// testable without a live Gemini key — assertions just check the
/// prompt mentions the claim text and verdict-appropriate fields.
#[allow(clippy::too_many_arguments)]
fn build_prompt(
    claim_text: &str,
    verdict: VerificationVerdict,
    environment: Option<&str>,
    output_hash: Option<&str>,
    duration_ms: Option<i32>,
    failure_reason: Option<&str>,
    conditions: Option<&str>,
    reproduction_steps: Option<&str>,
) -> String {
    let claim_preview: String = claim_text.chars().take(2000).collect();
    let mut prompt = String::new();
    prompt.push_str("=== CLAIM ===\n");
    prompt.push_str(&claim_preview);
    prompt.push_str("\n\n=== VERIFICATION ===\n");
    prompt.push_str(&format!("verdict: {}\n", verdict.as_str()));

    match verdict {
        VerificationVerdict::Confirmed => {
            prompt.push_str(&format!(
                "environment:    {}\n",
                environment.unwrap_or("(empty)")
            ));
            prompt.push_str(&format!(
                "output_hash:    {}\n",
                output_hash.unwrap_or("(empty)")
            ));
            prompt.push_str(&format!(
                "duration_ms:    {}\n",
                duration_ms
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "(empty)".into())
            ));
            prompt.push_str(
                "\n\
                 === YOUR JOB ===\n\
                 Decide whether the evidence (environment, \
                 output_hash, duration_ms) is plausibly consistent \
                 with running the claim's methodology.\n\n\
                 - block ONLY when the evidence is clearly fake: \
                   duration_ms=0, output_hash is gibberish (e.g. \
                   'asdf', 'xxx', mostly empty), environment is \
                   blank or obviously a placeholder.\n\
                 - warn when evidence is thin but not faked (e.g. \
                   environment is just 'macos', output_hash short \
                   but plausible).\n\
                 - pass otherwise.\n",
            );
        }
        VerificationVerdict::Disconfirmed => {
            prompt.push_str(&format!(
                "failure_reason:      {}\n",
                failure_reason.unwrap_or("(empty)")
            ));
            prompt.push_str(&format!(
                "conditions:          {}\n",
                conditions.unwrap_or("(empty)")
            ));
            prompt.push_str(&format!(
                "reproduction_steps:  {}\n",
                reproduction_steps.unwrap_or("(empty)")
            ));
            prompt.push_str(
                "\n\
                 === YOUR JOB ===\n\
                 Decide whether the failure_reason / conditions / \
                 reproduction_steps logically contradict the \
                 claim.\n\n\
                 - block when the refutation is clearly off-topic \
                   (e.g. claim is about latency, refutation is \
                   about typos in a comment).\n\
                 - warn when the refutation is loosely related but \
                   doesn't quite contradict the claim.\n\
                 - pass when the refutation actually contradicts \
                   the claim with reproducible conditions.\n",
            );
        }
    }

    prompt.push_str(
        "\n=== OUTPUT ===\n\
         Return strict JSON: \
         {\"verdict\": \"pass\" | \"warn\" | \"block\", \
          \"summary\": \"one sentence\", \
          \"suggestions\": [\"...\", \"...\"]}",
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_prompt_mentions_claim_and_evidence() {
        let p = build_prompt(
            "# Faster web search\n\nClaim: my approach is 2x faster",
            VerificationVerdict::Confirmed,
            Some("macOS 14.5, python 3.11"),
            Some("sha256:abc123"),
            Some(450),
            None,
            None,
            None,
        );
        assert!(p.contains("Faster web search"));
        assert!(p.contains("verdict: confirmed"));
        assert!(p.contains("environment:"));
        assert!(p.contains("output_hash:"));
        assert!(p.contains("duration_ms:"));
        // The Confirmed-only "your job" copy.
        assert!(p.contains("plausibly consistent"));
        // Disconfirmed-only labels must NOT appear.
        assert!(!p.contains("failure_reason:"));
        assert!(!p.contains("reproduction_steps:"));
    }

    #[test]
    fn disconfirmed_prompt_mentions_refutation_fields() {
        let p = build_prompt(
            "Claim X",
            VerificationVerdict::Disconfirmed,
            None,
            None,
            None,
            Some("got 800ms not 200ms"),
            Some("with a cold cache"),
            Some("see RUN.md step 3"),
        );
        assert!(p.contains("Claim X"));
        assert!(p.contains("verdict: disconfirmed"));
        assert!(p.contains("failure_reason:"));
        assert!(p.contains("conditions:"));
        assert!(p.contains("reproduction_steps:"));
        assert!(p.contains("logically contradict"));
        // Confirmed-only copy must NOT leak.
        assert!(!p.contains("plausibly consistent"));
    }

    #[test]
    fn confirmed_prompt_handles_missing_evidence_strings() {
        let p = build_prompt(
            "C",
            VerificationVerdict::Confirmed,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        // We render `(empty)` for missing fields rather than
        // skipping them so the model sees the structure.
        assert!(p.contains("environment:    (empty)"));
        assert!(p.contains("output_hash:    (empty)"));
        assert!(p.contains("duration_ms:    (empty)"));
    }

    #[test]
    fn review_target_maps_correctly() {
        assert_eq!(VerificationVerdict::Confirmed.review_target(), "verify");
        assert_eq!(VerificationVerdict::Disconfirmed.review_target(), "refute");
    }

    #[tokio::test]
    async fn review_returns_synth_pass_without_api_key() {
        let gemini = GeminiClient::new(None);
        let out = review_verification(
            &gemini,
            "C",
            VerificationVerdict::Confirmed,
            Some("env"),
            Some("hash"),
            Some(100),
            None,
            None,
            None,
        )
        .await
        .expect("synth pass should not error");
        assert_eq!(out.verdict, "pass");
        assert_eq!(out.model, "none");
        assert_eq!(out.usage.prompt_tokens, 0);
    }
}
