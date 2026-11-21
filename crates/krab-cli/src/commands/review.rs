//! `krab claim review` + `krab claim suggestions` — KrabReviewer UX.
//!
//! Also owns the rendering of a review to a human-readable block, reused by
//! `krab claim drop` when the dry-run surfaces suggestions.

use std::process::ExitCode;

use anyhow::Result;
use reqwest::multipart;
use serde_json::Value;

use crate::client::KrabClient;

/// Exit code used when the KrabReviewer returned warnings and the caller
/// did not pass `--ignore-review` / `--accept-review`. Matches the plan.
pub const EXIT_NEEDS_REVISION: u8 = 2;
/// Exit code used when the KrabReviewer returned a moderation block.
pub const EXIT_BLOCKED: u8 = 3;

/// Explicit dry-run of the KrabReviewer without publishing.
#[allow(dead_code)]
pub async fn dry_run(
    client: &KrabClient,
    battle_id: &str,
    claim: &str,
    artifact_path: &str,
    predecessor_id: Option<&str>,
    json_output: bool,
) -> Result<ExitCode> {
    let artifact_data = std::fs::read(artifact_path)?;
    let file_name = std::path::Path::new(artifact_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact")
        .to_string();

    let mut form = multipart::Form::new()
        .text("battle_id", battle_id.to_string())
        .text("claim", claim.to_string())
        .part(
            "artifact",
            multipart::Part::bytes(artifact_data).file_name(file_name),
        );
    if let Some(p) = predecessor_id {
        form = form.text("predecessor_id", p.to_string());
    }

    let review: Value = client.post_multipart("/v1/claims/review", form).await?;

    Ok(print_review_and_exit(&review, json_output))
}

/// Re-fetch a prior review by ID.
#[allow(dead_code)]
pub async fn get(client: &KrabClient, review_id: &str, json_output: bool) -> Result<ExitCode> {
    let review: Value = client.get(&format!("/v1/reviews/{}", review_id)).await?;
    Ok(print_review_and_exit(&review, json_output))
}

/// Print the review payload, return the exit-code the CLI should use.
pub fn print_review_and_exit(review: &Value, json_output: bool) -> ExitCode {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(review).unwrap_or_default()
        );
    } else {
        print_human(review);
    }
    match review["verdict"].as_str() {
        Some("block") => ExitCode::from(EXIT_BLOCKED),
        Some("warn") => ExitCode::from(EXIT_NEEDS_REVISION),
        _ => ExitCode::SUCCESS,
    }
}

fn print_human(review: &Value) {
    let verdict = review["verdict"].as_str().unwrap_or("?");
    let summary = review["summary"].as_str().unwrap_or("");
    let review_id = review["id"].as_str().unwrap_or("");

    let banner = match verdict {
        "block" => "BLOCKED",
        "warn" => "warn",
        "pass" => "pass",
        other => other,
    };
    // Intentionally don't surface the model identifier — it's an
    // internal implementation detail that changes when we swap
    // providers, and users don't need to care. The model name is still
    // in the DB row (`model_name`) and in the JSON output for
    // debuggability.
    eprintln!("\n🦀 KrabReviewer · {}", banner);
    if !summary.is_empty() {
        eprintln!("{}", summary);
    }

    if verdict == "block" {
        eprintln!("\nCannot publish — this verdict is not overridable.");
        if let Some(flags) = review["moderation_flags"].as_array() {
            for f in flags {
                let code = f["code"].as_str().unwrap_or("?");
                let reason = f["reason"].as_str().unwrap_or("");
                eprintln!("  • [{}] {}", code, reason);
            }
        }
        return;
    }

    if let Some(summary) = review["artifact_summary"].as_str() {
        if !summary.is_empty() {
            eprintln!("\nArtifact summary");
            for line in wrap(summary, 78) {
                eprintln!("  {}", line);
            }
        }
    }
    if let Some(highlights) = review["artifact_highlights"].as_array() {
        for h in highlights {
            let label = h["label"].as_str().unwrap_or("?");
            let detail = h["detail"].as_str().unwrap_or("");
            eprintln!("  • {:<9} {}", format!("{}:", label), detail);
        }
    }

    if let Some(suggestions) = review["suggestions"].as_array() {
        if !suggestions.is_empty() {
            eprintln!("\nSuggestions");
            for (i, s) in suggestions.iter().enumerate() {
                let sev = s["severity"].as_str().unwrap_or("info");
                let cat = s["category"].as_str().unwrap_or("?");
                let msg = s["message"].as_str().unwrap_or("");
                let fix = s["suggestion"].as_str().unwrap_or("");
                eprintln!("  {}. [{} · {}]  {}", i + 1, sev, cat, msg);
                if !fix.is_empty() {
                    eprintln!("       → {}", fix);
                }
            }
        }
    }

    eprintln!("\nReview ID: {}", review_id);
    if verdict == "warn" {
        eprintln!(
            "\nNext:\n  Revise and rerun `krab claim drop` (auto-reviews again),\n  or publish as-is with `--ignore-review` (warnings stored on record)."
        );
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.len() + word.len() + 1 > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}
