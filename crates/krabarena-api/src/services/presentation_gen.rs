//! Standalone backfill generator for the reviewer's viral-share layer
//! (`og_summary` + `presentation`).
//!
//! Context: since migration 067, every NEW Claim gets `og_summary` +
//! `presentation` written as a side effect of the KrabReviewer's
//! agentic review loop (Section F of `claim_review_prompt.txt`).
//! Claims posted before that shipped — and any claim whose
//! presentation an operator wants to refresh without re-running the
//! full (multi-turn, moderation-capable) review — need a lighter
//! path: one Gemini call, no moderation, no artifact re-download, no
//! suggestions.
//!
//! This module is that path. It reuses Section F of the canonical
//! reviewer prompt VERBATIM — extracted at runtime from
//! `gemini_service::CLAIM_REVIEW_SYSTEM_PROMPT` — so the backfill
//! generator can never drift from the reviewer's layout/grounding/tone
//! rules. Only the wrapper instructions (og_summary spec, output
//! envelope) are written here.
//!
//! Every result is passed through
//! `presentation_check::sanitize_presentation` before it reaches a
//! caller — the same deterministic grounding check the review-time
//! path enforces. Presentation is best-effort decoration: a Gemini
//! failure here is a normal `anyhow::Result` error, never a panic.

use serde::Deserialize;

use crate::AppState;
use crate::models::claim::ClaimRow;
use crate::models::review::Presentation;
use crate::services::gemini_service;
use crate::services::presentation_check;

/// Heading that opens the reviewer prompt's presentation rules.
const SECTION_F_START: &str = "F. PRESENTATION";
/// Sentinel that closes every lettered section in the reviewer
/// prompt; Section F's rules run from `SECTION_F_START` up to (but
/// not including) the first occurrence of this after it.
const SECTION_END: &str = "Return JSON only.";

/// Cap on the Claim markdown fed into the prompt. Claim bodies run a
/// few hundred to a few thousand chars in practice; 8 KB is generous
/// headroom without letting a pathological claim blow the prompt
/// budget.
const CLAIM_MARKDOWN_CAP_CHARS: usize = 8_000;
/// Cap on the serialized `claim_structure` JSON blob.
const STRUCTURE_CAP_CHARS: usize = 6_000;
/// `og_summary` hard cap per the reviewer spec (Section D).
const OG_SUMMARY_MAX_CHARS: usize = 200;

/// Extract Section F ("PRESENTATION") from the canonical reviewer
/// prompt: from the `F. PRESENTATION` heading up to (exclusive) the
/// next `Return JSON only.` sentinel. Returns `None` if either marker
/// is missing — callers surface that as a hard error rather than
/// silently falling back to a hand-copied (and driftable) rule set.
fn extract_section_f(prompt: &str) -> Option<&str> {
    let start = prompt.find(SECTION_F_START)?;
    let rest = &prompt[start..];
    let end = rest.find(SECTION_END)?;
    Some(rest[..end].trim())
}

/// Build the system prompt: Section F (verbatim, from the reviewer
/// prompt) wrapped in backfill-specific framing + the strict JSON
/// output envelope this module expects back.
fn build_system_prompt() -> anyhow::Result<String> {
    let section_f =
        extract_section_f(gemini_service::CLAIM_REVIEW_SYSTEM_PROMPT).ok_or_else(|| {
            anyhow::anyhow!(
                "presentation_gen: could not locate Section F markers \
             (\"{SECTION_F_START}\" .. \"{SECTION_END}\") in claim_review_prompt.txt \
             — the reviewer prompt changed shape; update the extraction markers"
            )
        })?;

    Ok(format!(
        "You are KrabReviewer's standalone presentation generator — a \
         backfill tool that (re)computes ONLY the viral-share fields for \
         a Claim that already exists on the platform. You are NOT \
         running a review: do not moderate, do not suggest edits, do \
         not question the claim's verdict or content. Your sole job is \
         to produce `og_summary` and `presentation` from the Claim \
         markdown and its `claim_structure` JSON (when provided) below.\n\
         \n\
         og_summary: <=200 chars, plain text, no markdown, no links. \
         Punchy social-preview copy for this Claim's page — should make \
         someone click. Return JSON null if you can't write a good one; \
         never invent facts not present in the Claim markdown or its \
         claim_structure.\n\
         \n\
         {section_f}\n\
         \n\
         Return STRICT JSON only — no markdown fences, no commentary — \
         matching exactly this shape:\n\
         {{\"og_summary\": \"...\" | null, \"presentation\": {{\"layout\": \
         \"...\", \"tweet_title\": \"...\", \"tweet_annotation\": \"...\", \
         \"hero_stat\": {{\"value\": \"...\", \"label\": \"...\"}} | null, \
         \"refutation\": {{\"claimed\": \"...\", \"measured\": \"...\"}} | \
         null}} | null}}\n\
         If you cannot ground every numeral you would write in the \
         Claim markdown or claim_structure JSON below (verbatim or as a \
         ratio of two grounded values), return `\"presentation\": null` \
         rather than guess — the server re-checks this deterministically \
         and drops the whole block on a single hallucinated number."
    ))
}

fn build_user_message(claim: &ClaimRow) -> String {
    let mut s = String::new();
    s.push_str("Claim markdown:\n");
    s.push_str(&truncate(&claim.claim, CLAIM_MARKDOWN_CAP_CHARS));
    s.push_str("\n\n");
    match &claim.claim_structure {
        Some(v) => {
            s.push_str("claim_structure JSON:\n");
            s.push_str(&truncate(
                &serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into()),
                STRUCTURE_CAP_CHARS,
            ));
        }
        None => {
            s.push_str(
                "(no claim_structure JSON on this claim — ground any \
                 numerals against the Claim markdown above; the server \
                 verifies against it too, so a numeral restated there \
                 is fine. If you can't ground a numeral against the \
                 Claim markdown, return \"presentation\": null.)",
            );
        }
    }
    s
}

/// `responseSchema` for Gemini's structured-output mode. Mirrors
/// [`Presentation`] / [`crate::models::review::HeroStat`] /
/// [`crate::models::review::Refutation`] field-for-field.
fn response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "og_summary": { "type": "string", "nullable": true },
            "presentation": {
                "type": "object",
                "nullable": true,
                "properties": {
                    "layout": {
                        "type": "string",
                        "enum": ["h2h", "race", "frontier", "myth_buster"],
                    },
                    "tweet_title": { "type": "string" },
                    "tweet_annotation": { "type": "string" },
                    "hero_stat": {
                        "type": "object",
                        "nullable": true,
                        "properties": {
                            "value": { "type": "string" },
                            "label": { "type": "string" },
                        },
                        "required": ["value", "label"],
                    },
                    "refutation": {
                        "type": "object",
                        "nullable": true,
                        "properties": {
                            "claimed": { "type": "string" },
                            "measured": { "type": "string" },
                        },
                        "required": ["claimed", "measured"],
                    },
                },
                "required": ["layout", "tweet_title", "tweet_annotation"],
            },
        },
    })
}

#[derive(Debug, Deserialize)]
struct GeneratedFields {
    #[serde(default)]
    og_summary: Option<String>,
    #[serde(default)]
    presentation: Option<Presentation>,
}

/// Regenerate `og_summary` + `presentation` for one existing Claim via
/// a single Gemini call (NOT the full agentic review loop).
///
/// The returned `Presentation` (if any) has already been through
/// [`presentation_check::sanitize_presentation`] — callers must NOT
/// re-sanitize, and can persist the result directly.
///
/// Errors (missing API key, transport failure, malformed model
/// output) are returned as plain `anyhow::Error` — this is a
/// best-effort backfill tool, not a hot path; the caller decides
/// whether/how to surface a failure to an admin.
pub async fn generate_presentation_for_claim(
    state: &AppState,
    claim: &ClaimRow,
) -> anyhow::Result<(Option<String>, Option<Presentation>)> {
    let gemini = &state.gemini;
    let api_key = gemini
        .api_key()
        .ok_or_else(|| anyhow::anyhow!("GEMINI_API_KEY not configured on this server"))?;

    let system_prompt = build_system_prompt()?;
    let user_msg = build_user_message(claim);
    let model = gemini.model();

    let body = serde_json::json!({
        "systemInstruction": { "parts": [{ "text": system_prompt }] },
        "contents": [{ "role": "user", "parts": [{ "text": user_msg }] }],
        "generationConfig": {
            "temperature": 0.2,
            "responseMimeType": "application/json",
            "responseSchema": response_schema(),
        },
    });

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
    );

    let resp = gemini_service::post_json_with_retry(
        gemini.http_client(),
        &url,
        &body,
        "presentation_gen",
        model,
    )
    .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Gemini error {status}: {}",
            text.chars().take(400).collect::<String>()
        );
    }

    let payload: serde_json::Value = resp.json().await?;
    let text = payload
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Gemini response missing candidates[0].content.parts[0].text (body: {})",
                serde_json::to_string(&payload).unwrap_or_default()
            )
        })?;

    let mut parsed: GeneratedFields = serde_json::from_str(text)
        .map_err(|e| anyhow::anyhow!("Gemini response parse failed: {e} (body: {text})"))?;

    if let Some(og) = parsed.og_summary.take() {
        parsed.og_summary = Some(truncate_chars(og.trim(), OG_SUMMARY_MAX_CHARS));
    }

    let structure_raw = claim
        .claim_structure
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default());

    let presentation = parsed.presentation.and_then(|p| {
        presentation_check::sanitize_presentation(p, structure_raw.as_deref(), Some(&claim.claim))
    });

    Ok((parsed.og_summary, presentation))
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push_str("\n…[truncated]");
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_f_extracts_from_the_real_prompt() {
        let section = extract_section_f(gemini_service::CLAIM_REVIEW_SYSTEM_PROMPT)
            .expect("Section F markers must be present in claim_review_prompt.txt");
        assert!(section.starts_with(SECTION_F_START));
        assert!(section.contains("GROUNDING (hard rule)"));
        // The exclusive end marker itself must NOT leak into the
        // extracted section — otherwise the model sees a stray
        // "Return JSON only." mid-instruction that isn't ours.
        assert!(!section.contains(SECTION_END));
    }

    #[test]
    fn build_system_prompt_embeds_section_f_and_json_envelope() {
        let prompt = build_system_prompt().expect("extraction must succeed against the real file");
        assert!(prompt.contains("F. PRESENTATION"));
        assert!(prompt.contains("\"og_summary\""));
        assert!(prompt.contains("\"presentation\""));
    }

    #[test]
    fn extract_section_f_none_when_markers_missing() {
        assert!(extract_section_f("no markers here").is_none());
        assert!(extract_section_f("F. PRESENTATION but no end sentinel").is_none());
    }

    #[test]
    fn truncate_caps_to_max_chars() {
        let s = "a".repeat(3000);
        let out = truncate(&s, 2000);
        assert!(out.starts_with(&"a".repeat(2000)));
        assert!(out.contains("[truncated]"));
    }

    #[test]
    fn truncate_chars_caps_og_summary() {
        let s = "x".repeat(300);
        let out = truncate_chars(&s, OG_SUMMARY_MAX_CHARS);
        assert!(out.chars().count() <= OG_SUMMARY_MAX_CHARS);
        assert!(out.ends_with('…'));
    }
}
