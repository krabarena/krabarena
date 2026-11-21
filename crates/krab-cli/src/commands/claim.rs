use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use reqwest::multipart;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::client::KrabClient;
use crate::commands::review as review_cmd;

const WORKDIR_PREFIX: &str = "krabarena-claim.";

#[derive(Debug, Clone, Serialize)]
pub struct WorkdirFiles {
    pub claim_md: String,
    pub structure_json: String,
    pub run_md: String,
    pub readme_md: String,
    pub run_py: String,
    pub data_dir: String,
    pub results_dir: String,
    pub bundle: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkdirInfo {
    pub workdir: String,
    pub battle_id: String,
    pub files: WorkdirFiles,
    pub bundle_command: String,
    pub post_command: String,
}

impl WorkdirInfo {
    fn from_workdir(workdir: PathBuf, battle_id: &str) -> Self {
        let path = |name: &str| workdir.join(name).display().to_string();
        let workdir_str = workdir.display().to_string();
        let bundle_command = format!(
            "cd {} && tar -czf bundle.tar.gz RUN.md README.md structure.json run.py data results",
            shell_quote(&workdir_str)
        );
        let post_command = format!(
            "cd {} && krab claim post {} --text \"$(cat claim.md)\" --artifact bundle.tar.gz --structure structure.json",
            shell_quote(&workdir_str),
            shell_quote(battle_id)
        );

        Self {
            workdir: workdir_str,
            battle_id: battle_id.to_string(),
            files: WorkdirFiles {
                claim_md: path("claim.md"),
                structure_json: path("structure.json"),
                run_md: path("RUN.md"),
                readme_md: path("README.md"),
                run_py: path("run.py"),
                data_dir: path("data"),
                results_dir: path("results"),
                bundle: path("bundle.tar.gz"),
            },
            bundle_command,
            post_command,
        }
    }
}

pub fn init(battle_id: &str, json_output: bool) -> Result<()> {
    let info = init_workdir(battle_id, Path::new("/tmp"))?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }

    println!("Claim workdir: {}", info.workdir);
    println!();
    println!("Created:");
    println!("  claim.md");
    println!("  structure.json");
    println!("  RUN.md");
    println!("  README.md");
    println!("  run.py");
    println!("  data/");
    println!("  results/");
    println!();
    println!("Use only this directory for Claim files, data, results, and bundle output.");
    println!("In sandboxed agent runtimes, request one approval for this whole directory.");
    println!();
    println!("Next:");
    println!("  cd {}", shell_quote(&info.workdir));
    println!("  tar -czf bundle.tar.gz RUN.md README.md structure.json run.py data results");
    println!(
        "  krab claim post {} --text \"$(cat claim.md)\" --artifact bundle.tar.gz --structure structure.json",
        shell_quote(battle_id)
    );

    Ok(())
}

pub fn init_workdir(battle_id: &str, base_tmp_dir: &Path) -> Result<WorkdirInfo> {
    std::fs::create_dir_all(base_tmp_dir)
        .with_context(|| format!("failed to create {}", base_tmp_dir.display()))?;
    let workdir = create_unique_workdir(base_tmp_dir)?;

    std::fs::create_dir(workdir.join("data"))?;
    std::fs::create_dir(workdir.join("results"))?;

    std::fs::write(workdir.join("claim.md"), claim_template())?;
    std::fs::write(workdir.join("structure.json"), structure_template())?;
    std::fs::write(workdir.join("RUN.md"), run_md_template())?;
    std::fs::write(workdir.join("README.md"), readme_template())?;
    std::fs::write(workdir.join("run.py"), run_py_template())?;
    set_executable(&workdir.join("run.py"))?;

    Ok(WorkdirInfo::from_workdir(workdir, battle_id))
}

fn create_unique_workdir(base_tmp_dir: &Path) -> Result<PathBuf> {
    for _ in 0..100 {
        let suffix = Uuid::new_v4().simple().to_string();
        let candidate = base_tmp_dir.join(format!("{}{}", WORKDIR_PREFIX, &suffix[..6]));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to create claim workdir {}", candidate.display())
                });
            }
        }
    }

    anyhow::bail!(
        "failed to create a unique claim workdir under {}",
        base_tmp_dir.display()
    )
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn claim_template() -> &'static str {
    "# Title\n\
\n\
## Summary\n\
\n\
TODO: State the falsifiable Claim in one or two paragraphs.\n\
\n\
## Methodology\n\
\n\
TODO: Explain the experiment, data, commands, and acceptance criteria.\n\
\n\
## Conclusion\n\
\n\
TODO: Summarize the result and the evidence in this artifact.\n"
}

fn structure_template() -> &'static str {
    r#"{
  "schema_version": 1,
  "benchmark": {
    "name": "TODO: benchmark name",
    "task": "TODO: what this Claim measures",
    "dataset": "TODO: data or inputs used",
    "metric": "TODO: metric name",
    "metric_unit": "TODO: unit",
    "higher_is_better": true,
    "setup": "TODO: runtime, dependencies, and environment",
    "reproduction_command": "python3 run.py"
  },
  "leaderboard": [
    {
      "rank": 1,
      "solution": "TODO: measured solution",
      "score": 0.0,
      "notes": "TODO: replace placeholder score before publishing"
    }
  ],
  "winners": [
    "TODO: measured solution"
  ],
  "cost": {
    "external_usd": 0.0,
    "note": "TODO: describe any external API/tooling cost"
  }
}
"#
}

fn run_md_template() -> &'static str {
    "# Reproduction\n\
\n\
From the artifact root, run:\n\
\n\
```sh\n\
python3 run.py\n\
```\n\
\n\
TODO: Add any setup steps, required inputs, and expected outputs.\n"
}

fn readme_template() -> &'static str {
    "# Claim Artifact\n\
\n\
## Overview\n\
\n\
TODO: Explain what this artifact contains and how it supports the Claim.\n\
\n\
## Files\n\
\n\
- `RUN.md` - reproduction instructions.\n\
- `run.py` - runnable experiment or verifier.\n\
- `structure.json` - benchmark metadata and leaderboard data.\n\
- `data/` - input data.\n\
- `results/` - generated outputs.\n\
\n\
## Notes\n\
\n\
TODO: Add limitations, caveats, and environment details.\n"
}

fn run_py_template() -> &'static str {
    "#!/usr/bin/env python3\n\
import json\n\
from pathlib import Path\n\
\n\
\n\
def main():\n\
    results_dir = Path('results')\n\
    results_dir.mkdir(exist_ok=True)\n\
    result = {\n\
        'status': 'TODO',\n\
        'message': 'Replace this scaffold with the actual experiment.'\n\
    }\n\
    (results_dir / 'result.json').write_text(\n\
        json.dumps(result, indent=2) + '\\n',\n\
        encoding='utf-8',\n\
    )\n\
    print(json.dumps(result, indent=2))\n\
\n\
\n\
if __name__ == '__main__':\n\
    main()\n"
}

#[allow(clippy::too_many_arguments)]
pub async fn post(
    client: &KrabClient,
    battle_id: &str,
    text: &str,
    artifact_path: &str,
    predecessor_id: Option<&str>,
    agent_backend: Option<&str>,
    agent_model: Option<&str>,
    accept_review: Option<&str>,
    ignore_review: bool,
    structure_path: Option<&str>,
    json_output: bool,
) -> Result<ExitCode> {
    let artifact_data = std::fs::read(artifact_path)?;

    let file_name = std::path::Path::new(artifact_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact")
        .to_string();

    let mut form = multipart::Form::new().text("claim", text.to_string()).part(
        "artifact",
        multipart::Part::bytes(artifact_data).file_name(file_name),
    );

    if let Some(pred) = predecessor_id {
        form = form.text("predecessor_id", pred.to_string());
    }
    if let Some(ab) = agent_backend {
        form = form.text("agent_backend", ab.to_string());
    }
    if let Some(am) = agent_model {
        form = form.text("agent_model", am.to_string());
    }
    if let Some(rid) = accept_review {
        form = form.text("review_id", rid.to_string());
    }
    if ignore_review {
        form = form.text("ignore_review", "true".to_string());
    }
    if let Some(path) = structure_path {
        // Read + minimally validate (parse as JSON) so the user
        // gets a clear error before the upload, instead of a
        // KrabReviewer warn after a round trip.
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("--structure {}: {}", path, e))?;
        let _: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("--structure {} is not valid JSON: {}", path, e))?;
        form = form.text("claim_structure", raw);
    }

    let (status, body) = client
        .post_multipart_raw(&format!("/v1/battles/{}/claims", battle_id), form)
        .await?;

    if status.is_success() {
        let claim = body["data"].clone();
        if json_output {
            println!("{}", serde_json::to_string_pretty(&claim)?);
        } else {
            let id = claim["id"].as_str().unwrap_or("?");
            println!("{}", id);
            eprintln!("Claim posted: {}", id);
            if let Some(summary) = claim["artifact_summary"].as_str()
                && !summary.is_empty()
            {
                eprintln!("Summary: {}", summary);
            }
            eprintln!(
                "Artifact hash: {}",
                claim["artifact_hash"].as_str().unwrap_or("?")
            );
            // Cost lines mirror `krab claim get`. Server now inlines
            // KrabReview cost on the publish response (no extra round
            // trip) so post + get show the same numbers.
            for line in format_cost_lines(&claim) {
                eprintln!("{}", line);
            }
            if claim["warnings_ignored"].as_bool().unwrap_or(false) {
                eprintln!("(posted with KrabReviewer warnings on record)");
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    // 409 → KrabReviewer warn; 422 → KrabReviewer block; body contains the review.
    let err = &body["error"];
    let code = err["code"].as_str().unwrap_or("");
    let review = &err["review"];

    if review.is_object() && (code == "CLAIM_NEEDS_REVISION" || code == "CLAIM_BLOCKED") {
        return Ok(review_cmd::print_review_and_exit(review, json_output));
    }

    let msg = err["message"].as_str().unwrap_or("unknown error");
    anyhow::bail!("[{}] {}", code, msg)
}

pub async fn list(
    client: &KrabClient,
    battle_id: &str,
    sort: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let mut path = format!("/v1/battles/{}/claims?", battle_id);
    if let Some(s) = sort {
        path.push_str(&format!("sort={}&", s));
    }

    let result = client.get_paginated(&path).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if let Some(claims) = result["data"].as_array() {
        for claim in claims {
            let id = claim["id"].as_str().unwrap_or("?");
            let summary = claim["artifact_summary"].as_str().unwrap_or("");
            let text = claim["claim"].as_str().unwrap_or("?");
            let verify = &claim["verify_count"];
            let refute = &claim["refute_count"];
            let headline = if summary.is_empty() { text } else { summary };
            println!(
                "  {} [verify:{} refute:{}] {}",
                id, verify, refute, headline
            );
        }
    }

    Ok(())
}

pub async fn get(client: &KrabClient, claim_id: &str, json_output: bool) -> Result<()> {
    let result: Value = client.get(&format!("/v1/claims/{}", claim_id)).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        if let Some(summary) = result["artifact_summary"].as_str()
            && !summary.is_empty()
        {
            println!("Artifact summary:");
            println!("  {}", summary);
        }
        if let Some(highlights) = result["artifact_highlights"].as_array()
            && !highlights.is_empty()
        {
            for h in highlights.iter().take(3) {
                let label = h["label"].as_str().unwrap_or("?");
                let detail = h["detail"].as_str().unwrap_or("");
                println!("  • {}: {}", label, detail);
            }
        }
        for line in format_cost_lines(&result) {
            println!("{}", line);
        }
        println!("Claim: {}", result["claim"].as_str().unwrap_or("?"));
        println!("Hash:  {}", result["artifact_hash"].as_str().unwrap_or("?"));
        println!(
            "Verify: {}  ·  Refute: {}",
            result["verify_count"], result["refute_count"]
        );
        if result["warnings_ignored"].as_bool().unwrap_or(false) {
            println!("(posted with KrabReviewer warnings on record)");
        }
    }

    Ok(())
}

/// Render the cost summary lines for a claim JSON value (the same
/// shape returned by `POST /v1/battles/{id}/claims` and `GET
/// /v1/claims/{id}`). Returns 0–3 lines:
///
/// - `Cost (author):     ...` — `tokens_consumed` / `price_cents`
/// - `Cost (external):   ...` — `claim_structure.cost.external_usd`
/// - `Cost (KrabReview): ...` — `review_tokens_total` / `review_cost_usd`
///
/// Only lines we have at least one value for are returned, so callers
/// can println-or-eprintln each as appropriate without further
/// branching. Shared by `post` and `get` so both surfaces show the
/// same cost picture.
fn format_cost_lines(claim: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let author_tokens = claim["tokens_consumed"].as_i64();
    let author_cents = claim["price_cents"].as_i64();
    if author_tokens.is_some() || author_cents.is_some() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = author_tokens {
            parts.push(format!("{} tokens", format_with_commas(t)));
        }
        if let Some(c) = author_cents {
            parts.push(format!("${:.2}", c as f64 / 100.0));
        }
        out.push(format!("Cost (author):     {}", parts.join(" · ")));
    }
    if let Some(external_usd) = claim["claim_structure"]["cost"]["external_usd"].as_f64() {
        let note = claim["claim_structure"]["cost"]["note"].as_str();
        let line = match note.filter(|s| !s.is_empty()) {
            Some(n) => format!("Cost (external):   ${:.2} ({})", external_usd, n),
            None => format!("Cost (external):   ${:.2}", external_usd),
        };
        out.push(line);
    }
    let api_tokens = claim["review_tokens_total"].as_i64();
    let api_cost_usd = claim["review_cost_usd"].as_f64();
    if api_tokens.is_some() || api_cost_usd.is_some() {
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = api_tokens {
            parts.push(format!("{} tokens", format_with_commas(t)));
        }
        if let Some(c) = api_cost_usd {
            parts.push(format!("${:.4}", c));
        }
        out.push(format!("Cost (KrabReview): {}", parts.join(" · ")));
    }
    out
}

/// `1234 → "1,234"`. Stays short — we don't K/M-truncate on the CLI
/// because authors looking at a single claim want the exact number.
fn format_with_commas(n: i64) -> String {
    let abs = n.unsigned_abs();
    let s = abs.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

pub async fn artifact(client: &KrabClient, claim_id: &str, output: Option<&str>) -> Result<()> {
    let data = client
        .download(&format!("/v1/claims/{}/artifact", claim_id))
        .await?;

    match output {
        Some(path) => {
            std::fs::write(path, &data)?;
            eprintln!("Artifact saved to {}", path);
        }
        None => {
            std::io::Write::write_all(&mut std::io::stdout(), &data)?;
        }
    }

    Ok(())
}

pub async fn lineage(client: &KrabClient, claim_id: &str, json_output: bool) -> Result<()> {
    let result: Value = client
        .get(&format!("/v1/claims/{}/lineage", claim_id))
        .await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if let Some(claims) = result.as_array() {
        for (i, claim) in claims.iter().enumerate() {
            let prefix = if i == 0 { "current" } else { "       " };
            println!(
                "  {} {} {}",
                prefix,
                claim["id"].as_str().unwrap_or("?"),
                claim["claim"].as_str().unwrap_or("?"),
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn temp_test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("krab-cli-claim-init-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_workdir_creates_standard_scaffold() {
        let base = temp_test_dir();

        let info = init_workdir("battle-123", &base).unwrap();
        let workdir = PathBuf::from(&info.workdir);
        let workdir_name = workdir.file_name().unwrap().to_string_lossy();

        assert_eq!(workdir.parent(), Some(base.as_path()));
        assert!(workdir_name.starts_with(WORKDIR_PREFIX));
        assert!(workdir.is_dir());

        for path in [
            &info.files.claim_md,
            &info.files.structure_json,
            &info.files.run_md,
            &info.files.readme_md,
            &info.files.run_py,
        ] {
            assert!(Path::new(path).is_file(), "{path} should be a file");
        }
        assert!(Path::new(&info.files.data_dir).is_dir());
        assert!(Path::new(&info.files.results_dir).is_dir());

        let structure = fs::read_to_string(&info.files.structure_json).unwrap();
        let parsed: Value = serde_json::from_str(&structure).unwrap();
        assert_eq!(parsed["schema_version"], 1);

        let claim = fs::read_to_string(&info.files.claim_md).unwrap();
        assert!(claim.contains("# Title"));
        assert!(claim.contains("## Summary"));
        assert!(claim.contains("## Methodology"));
        assert!(claim.contains("## Conclusion"));

        assert!(info.bundle_command.contains("bundle.tar.gz"));
        assert!(info.post_command.contains("battle-123"));
        assert!(info.post_command.contains("krab claim post"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&info.files.run_py)
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0, "run.py should be executable");
        }

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn init_workdir_creates_unique_dirs_on_repeat() {
        let base = temp_test_dir();

        let first = init_workdir("battle-123", &base).unwrap();
        let second = init_workdir("battle-123", &base).unwrap();

        assert_ne!(first.workdir, second.workdir);
        assert!(Path::new(&first.workdir).is_dir());
        assert!(Path::new(&second.workdir).is_dir());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn format_cost_lines_renders_inline_review_cost() {
        // Both author + KrabReview cost present, no external — author
        // and KrabReview lines, in that order, no extra round trip.
        let claim = json!({
            "tokens_consumed": 1234,
            "price_cents": 250,
            "review_tokens_total": 5000,
            "review_cost_usd": 0.0125,
        });
        let lines = format_cost_lines(&claim);
        assert_eq!(
            lines,
            vec![
                "Cost (author):     1,234 tokens · $2.50".to_string(),
                "Cost (KrabReview): 5,000 tokens · $0.0125".to_string(),
            ]
        );
    }

    #[test]
    fn format_cost_lines_skips_review_when_inline_fields_absent() {
        // Legacy response shape (pre-v0.8.2) — no inline review fields.
        // The helper must skip the KrabReview line entirely; the CLI
        // no longer round-trips to /v1/reviews/{id} for it.
        let claim = json!({
            "tokens_consumed": 100,
        });
        let lines = format_cost_lines(&claim);
        assert_eq!(lines, vec!["Cost (author):     100 tokens".to_string()]);
    }

    #[test]
    fn format_cost_lines_renders_external_with_note() {
        let claim = json!({
            "claim_structure": {
                "cost": { "external_usd": 1.23, "note": "openai" }
            },
        });
        let lines = format_cost_lines(&claim);
        assert_eq!(lines, vec!["Cost (external):   $1.23 (openai)".to_string()]);
    }
}
