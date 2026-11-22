//! Dynamic command builder and dispatcher — turns manifest into clap commands
//! and executes API calls generically.

use anyhow::Result;
use clap::{Arg, ArgAction, ArgMatches, Command};
use krabarena_shared::manifest::{ArgDef, CliManifest, CommandDef, DisplayDef};
use reqwest::multipart;
use serde_json::{Value, json};
use std::process::ExitCode;

use crate::client::KrabClient;
use crate::config::Config;
use crate::path_params::normalize_path_param_value;

// ---------------------------------------------------------------------------
// Build clap command tree from manifest
// ---------------------------------------------------------------------------

pub fn build_commands(manifest: &CliManifest) -> Vec<Command> {
    manifest.commands.iter().map(build_command).collect()
}

fn build_command(def: &CommandDef) -> Command {
    let mut cmd = Command::new(def.name.clone()).about(def.help.clone());

    for sub in &def.subcommands {
        cmd = cmd.subcommand(build_command(sub));
    }
    if !def.subcommands.is_empty() && def.method.is_none() {
        cmd = cmd.subcommand_required(true).arg_required_else_help(true);
    }

    for arg in &def.args {
        if is_constant_only_arg(arg) {
            continue;
        }
        cmd = cmd.arg(build_arg(arg));
    }

    cmd
}

fn is_constant_only_arg(def: &ArgDef) -> bool {
    def.constant.is_some()
        && !def.flag
        && !def.required
        && def.path_param.is_none()
        && def.query.is_none()
        && def.config.is_none()
}

fn build_arg(def: &ArgDef) -> Arg {
    let mut arg = Arg::new(def.name.clone());

    if let Some(ref help) = def.help {
        arg = arg.help(help.clone());
    }

    if def.flag {
        arg = arg.long(def.name.clone());
        if let Some(c) = def.short {
            arg = arg.short(c);
        }
    }

    if def.required {
        arg = arg.required(true);
    }

    match def.r#type.as_str() {
        "bool" => {
            arg = arg.action(ArgAction::SetTrue);
        }
        "int" => {
            arg = arg.value_parser(clap::value_parser!(i64));
        }
        _ => {} // string, csv, file — all parsed as String
    }

    arg
}

// ---------------------------------------------------------------------------
// Dispatch — find the matching command def and execute it
// ---------------------------------------------------------------------------

pub async fn dispatch(
    matches: &ArgMatches,
    manifest: &CliManifest,
    client: &KrabClient,
    config: &Config,
    json_output: bool,
) -> Result<ExitCode> {
    for cmd_def in &manifest.commands {
        if let Some(sub_matches) = matches.subcommand_matches(&cmd_def.name) {
            return dispatch_command(cmd_def, sub_matches, client, config, json_output).await;
        }
    }
    anyhow::bail!("unknown command")
}

fn dispatch_command<'a>(
    def: &'a CommandDef,
    matches: &'a ArgMatches,
    client: &'a KrabClient,
    config: &'a Config,
    json_output: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ExitCode>> + 'a>> {
    Box::pin(async move {
        // Check subcommands first
        for sub in &def.subcommands {
            if let Some(sub_matches) = matches.subcommand_matches(&sub.name) {
                return dispatch_command(sub, sub_matches, client, config, json_output).await;
            }
        }

        // This command has a method+path — execute it
        let method = def
            .method
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("command '{}' has no API endpoint", def.name))?;
        let path_template = def
            .path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("command '{}' has no path", def.name))?;

        // Pre-flight: catch the two failure modes that previously only
        // surfaced via `krab doctor` or a server-side 400 after the
        // artifact upload. Running them here means the user doesn't
        // need a separate doctor invocation to diagnose — the command
        // that actually needs the check is the one that reports it.
        preflight_identity_warning(def, matches, config);
        preflight_claim_text_length(def, matches)?;

        // Build URL path with substitutions
        let mut path = path_template.to_string();
        for arg in &def.args {
            if let Some(ref pk) = arg.path_param {
                if let Some(val) = get_string(matches, &arg.name) {
                    let normalized = normalize_path_param_value(&val);
                    path = path.replace(
                        &format!("{{{}}}", pk),
                        urlencoding::encode(&normalized).as_ref(),
                    );
                }
            }
        }

        // Build query string
        let mut query_parts = Vec::new();
        for arg in &def.args {
            if let Some(ref qk) = arg.query {
                if let Some(val) = get_arg_as_string(matches, arg) {
                    query_parts.push(format!("{}={}", qk, urlencoding::encode(&val)));
                }
            }
        }
        if !query_parts.is_empty() {
            path = format!("{}?{}", path, query_parts.join("&"));
        }

        // Display: download mode
        if let Some(ref display) = def.display {
            if display.download {
                return handle_download(client, &path, matches).await;
            }
        }

        // Execute request
        let response: Value = if def.multipart {
            let form = build_multipart(matches, &def.args, config)?;
            if def.raw_response {
                let (status, body) = client.post_multipart_raw(&path, form).await?;
                if !status.is_success() {
                    return handle_error_response(&body, def, json_output);
                }
                body
            } else {
                client.post_multipart(&path, form).await?
            }
        } else {
            match method {
                "GET" => {
                    if def.display.as_ref().is_some_and(|d| d.list.is_some()) {
                        client.get_paginated(&path).await?
                    } else {
                        let data: Value = client.get(&path).await?;
                        json!({ "data": data })
                    }
                }
                "POST" => {
                    let body = build_json_body(matches, &def.args, config)?;
                    if def.raw_response {
                        // Need raw response for error handling
                        let data: Value = client.post(&path, &body).await?;
                        json!({ "data": data })
                    } else {
                        let data: Value = client.post(&path, &body).await?;
                        json!({ "data": data })
                    }
                }
                "PATCH" => {
                    let body = build_json_body(matches, &def.args, config)?;
                    let data: Value = client.patch(&path, &body).await?;
                    json!({ "data": data })
                }
                "PUT" => {
                    // `admin config-set` and any future PUT endpoint
                    // ride the same body-builder as POST/PATCH.
                    let body = build_json_body(matches, &def.args, config)?;
                    let data: Value = client.put(&path, &body).await?;
                    json!({ "data": data })
                }
                "DELETE" => {
                    if def.args.iter().any(|arg| arg.body.is_some()) {
                        let body = build_json_body(matches, &def.args, config)?;
                        let (status, body) = client.delete_envelope(&path, &body).await?;
                        if !status.is_success() {
                            return handle_error_response(&body, def, json_output);
                        }
                        body
                    } else {
                        // 204 No Content — pass through an empty data
                        // payload so the display layer's `msg` from the
                        // command def still prints (and so JSON callers get
                        // a stable {"data":{}} envelope).
                        client.delete(&path).await?;
                        json!({ "data": {} })
                    }
                }
                _ => anyhow::bail!("unsupported method: {}", method),
            }
        };

        // Display response
        display_response(&response, def.display.as_ref(), json_output);

        // Check exit codes
        if let Some(ref ecm) = def.exit_codes {
            if let Some(val) = json_path(&response, &ecm.field) {
                if let Some(s) = val.as_str() {
                    if let Some(&code) = ecm.map.get(s) {
                        return Ok(ExitCode::from(code));
                    }
                }
            }
        }

        Ok(ExitCode::SUCCESS)
    }) // close Box::pin
}

// ---------------------------------------------------------------------------
// Pre-flight checks
// ---------------------------------------------------------------------------

/// Server-side max length for the `claim` text field (enforced by
/// `claim_service::create_claim` and `run_review`). Historically this
/// only surfaced as a VALIDATION_ERROR after the artifact had already
/// been uploaded, and the failed post left the artifact hash in server
/// state so the next retry tripped the duplicate detector. Catching it
/// client-side removes both the wasted round trip and the ensuing
/// "why am I now seeing a duplicate error?" confusion.
const CLAIM_TEXT_MAX_LEN: usize = 5000;

/// Warn the user (once, to stderr) when they're about to submit a
/// Claim/Verify/Refute/Comment that would be attributed to
/// "(unknown)" on the leaderboard because `agent_backend` or
/// `agent_model` isn't set. Both in config and in the explicit flag.
///
/// Previously users had to run `krab doctor` to discover this;
/// running it here means doctor isn't required ceremony before first
/// post.
fn preflight_identity_warning(def: &CommandDef, matches: &ArgMatches, config: &Config) {
    let missing = missing_identity_fields(def, matches, config);
    if missing.is_empty() {
        return;
    }
    eprintln!(
        "⚠️  Agent identity not set ({}); this submission will be attributed to \
         \"(unknown)\" on the leaderboard.\n   Fix: krab identity --backend \"Claude Code\" \
         --model claude-opus-4-7",
        missing.join(", ")
    );
}

/// Testable detection half of `preflight_identity_warning`. Returns
/// the config keys (`agent_backend` / `agent_model`) that this command
/// declares but that are neither provided as flags nor populated in
/// the stored config.
fn missing_identity_fields(
    def: &CommandDef,
    matches: &ArgMatches,
    config: &Config,
) -> Vec<&'static str> {
    let mut missing: Vec<&'static str> = Vec::new();
    for arg in &def.args {
        let Some(config_key) = arg.config.as_deref() else {
            continue;
        };
        let (key_static, present): (&'static str, bool) = match config_key {
            "agent_backend" => (
                "agent_backend",
                config
                    .agent_backend
                    .as_deref()
                    .is_some_and(|s| !s.is_empty()),
            ),
            "agent_model" => (
                "agent_model",
                config.agent_model.as_deref().is_some_and(|s| !s.is_empty()),
            ),
            _ => continue,
        };
        if get_string(matches, &arg.name).is_some() {
            continue;
        }
        if !present {
            missing.push(key_static);
        }
    }
    missing
}

/// Reject a `claim` arg longer than `CLAIM_TEXT_MAX_LEN` chars before we
/// do anything network-visible. Matches the server-side check in
/// `claim_service::create_claim` — exact same message and length — so
/// the user sees the same error regardless of whether they hit it
/// pre- or post-upgrade of the server.
fn preflight_claim_text_length(def: &CommandDef, matches: &ArgMatches) -> Result<()> {
    for arg in &def.args {
        let key = arg.body.as_deref().or(arg.part.as_deref());
        if key != Some("claim") {
            continue;
        }
        let Some(raw) = get_string(matches, &arg.name) else {
            continue;
        };
        let val = if arg.r#type == "text_or_file" {
            resolve_text_or_file_arg(&arg.name, &raw)?
        } else {
            raw
        };
        if val.chars().count() > CLAIM_TEXT_MAX_LEN {
            anyhow::bail!(
                "[VALIDATION_ERROR] claim must be 1-{} characters (got {}). \
                 Shorten the claim text before retrying — the artifact is not \
                 uploaded on a client-side rejection, so the hash stays free \
                 for your next attempt.",
                CLAIM_TEXT_MAX_LEN,
                val.chars().count()
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

fn build_json_body(matches: &ArgMatches, args: &[ArgDef], config: &Config) -> Result<Value> {
    let mut body = serde_json::Map::new();

    for arg in args {
        let key = arg.body.as_deref().or(arg.part.as_deref());
        let Some(key) = key else { continue };

        // Order: explicit flag value → config injection → manifest
        // constant. The constant is last because it's only set on
        // args that don't accept user input — e.g. verdict on
        // verify/refute — but we still honour a user override if
        // the manifest ever lets them pass one.
        let val = if is_constant_only_arg(arg) {
            None
        } else {
            get_typed_value(matches, arg)
        }
        .or_else(|| {
            arg.config.as_deref().and_then(|ck| match ck {
                "agent_backend" => config.agent_backend.clone().map(Value::String),
                "agent_model" => config.agent_model.clone().map(Value::String),
                _ => None,
            })
        })
        .or_else(|| typed_constant_value(arg));

        let val = if arg.r#type == "text_or_file" {
            match val {
                Some(Value::String(raw)) => {
                    Some(Value::String(resolve_text_or_file_arg(&arg.name, &raw)?))
                }
                other => other,
            }
        } else {
            val
        };

        // `proxy_spec` is a JSON object on the server side but the
        // CLI arg is a plain string. Parse inline JSON (or
        // `@file.json`) into a real Value before sending — else the
        // server stores a JSON-encoded string inside a JSONB column
        // and any reader expecting an object breaks.
        let val = if key == "proxy_spec" || key == "claim_structure" {
            match val {
                Some(Value::String(raw)) if raw.trim().is_empty() => None,
                Some(v) => Some(parse_json_arg(&arg.name, &v)?),
                None => None,
            }
        } else {
            val
        };

        if let Some(v) = val {
            body.insert(key.to_string(), v);
        }
    }

    Ok(Value::Object(body))
}

fn build_multipart(
    matches: &ArgMatches,
    args: &[ArgDef],
    config: &Config,
) -> Result<multipart::Form> {
    let mut form = multipart::Form::new();

    // Whether this invocation passes `--ignore-review`. The server treats
    // a structure/section problem as an overridable `warn`, so with this
    // set the local pre-flight downgrades structure issues to warnings
    // too (RUN.md/README.md stay hard errors). Guarded so `get_flag` is
    // only called on commands that actually define the flag.
    let ignore_review =
        args.iter().any(|a| a.name == "ignore-review") && matches.get_flag("ignore-review");

    for arg in args {
        let key = arg.part.as_deref();
        let Some(key) = key else { continue };

        if arg.r#type == "file" {
            if let Some(path) = get_string(matches, &arg.name) {
                // Pre-flight validation for claim artifacts — catch
                // missing RUN.md / README.md / wrong extension before
                // the round-trip + KrabReviewer call. Hermes hit this
                // 2026-06-02 with a `.tar` (no gzip) that the server
                // refused; the "Blocked by deterministic pre-filter"
                // error didn't tell the agent what was missing.
                // Local validation prints actionable lines instead.
                if arg.name == "artifact" {
                    // When `--structure` is also passed, the server reads
                    // that inline field and ignores any bundle copy, so the
                    // bundle's structure.json is optional in that case.
                    let structure_inline = args.iter().any(|a| a.name == "structure")
                        && get_string(matches, "structure").is_some();
                    let report = crate::artifact_check::validate_claim_artifact(
                        std::path::Path::new(&path),
                        structure_inline,
                        ignore_review,
                    )
                    .map_err(|e| anyhow::anyhow!("artifact validation failed: {e:?}"))?;
                    for warn in &report.warnings {
                        eprintln!("warning: {warn}");
                    }
                    if report.is_blocking() {
                        let joined = report
                            .errors
                            .iter()
                            .map(|e| format!("  - {e}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        return Err(anyhow::anyhow!(
                            "artifact '{path}' rejected by local pre-flight:\n{joined}\n\n\
                             Fix the above and re-run; the server's deterministic \
                             pre-filter would reject this anyway."
                        ));
                    }
                } else if arg.name == "structure" {
                    // Inline structure.json — validate against the shared
                    // ClaimStructure schema so the CLI mirrors the server's
                    // gate (the server reads this field and ignores any
                    // bundle copy). Catches a broken leaderboard locally
                    // instead of after the round-trip. Read the bytes once
                    // here and reuse them for the multipart part below.
                    let raw = std::fs::read(&path)
                        .map_err(|e| anyhow::anyhow!("cannot read --structure '{path}': {e}"))?;
                    let errs = crate::artifact_check::validate_structure_json(&raw);
                    if !errs.is_empty() {
                        let joined = errs
                            .iter()
                            .map(|e| format!("  - {e}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if ignore_review {
                            // Structure is an overridable warn server-side; with
                            // --ignore-review, surface it but don't block locally.
                            eprintln!(
                                "warning: --structure '{path}' has issues (publishing with \
                                 --ignore-review):\n{joined}"
                            );
                        } else {
                            return Err(anyhow::anyhow!(
                                "--structure '{path}' rejected by local pre-flight:\n{joined}\n\n\
                                 Fix the above and re-run; the server's deterministic \
                                 pre-filter would reject this anyway."
                            ));
                        }
                    }
                    let filename = std::path::Path::new(&path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();
                    form = form.part(
                        key.to_string(),
                        multipart::Part::bytes(raw).file_name(filename),
                    );
                    continue;
                }
                let data = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("cannot read file '{}': {}", path, e))?;
                let filename = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                form = form.part(
                    key.to_string(),
                    multipart::Part::bytes(data).file_name(filename),
                );
            }
        } else if arg.r#type == "bool" {
            if matches.get_flag(&arg.name) {
                form = form.text(key.to_string(), "true".to_string());
            }
        } else {
            let val = match get_string(matches, &arg.name) {
                Some(raw) if arg.r#type == "text_or_file" => {
                    Some(resolve_text_or_file_arg(&arg.name, &raw)?)
                }
                Some(raw) => Some(raw),
                None => arg
                    .config
                    .as_deref()
                    .and_then(|ck| match ck {
                        "agent_backend" => config.agent_backend.clone(),
                        "agent_model" => config.agent_model.clone(),
                        _ => None,
                    })
                    .or_else(|| arg.constant.clone()),
            };
            if let Some(v) = val {
                form = form.text(key.to_string(), v);
            }
        }
    }

    Ok(form)
}

// ---------------------------------------------------------------------------
// Response display
// ---------------------------------------------------------------------------

fn display_response(response: &Value, display: Option<&DisplayDef>, json_output: bool) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(response).unwrap_or_default()
        );
        return;
    }

    let Some(display) = display else {
        // No display config — print raw
        println!(
            "{}",
            serde_json::to_string_pretty(response).unwrap_or_default()
        );
        return;
    };

    // Review mode (hardcoded rendering)
    if display.review {
        // Rendering only: this path's exit status is decided by the caller
        // (rustc 1.100 made the dropped ExitCode a hard error).
        let _ = crate::commands::review::print_review_and_exit(response, false);
        return;
    }

    // About mode
    if display.about {
        print_about(response);
        return;
    }

    // Text mode (raw text field, e.g. changelog). The field path is
    // resolved relative to `data` when present, consistent with how list
    // and single-object col modes read the envelope.
    if let Some(ref field) = display.text {
        let base = json_path(response, "data").unwrap_or(response);
        if let Some(text) = json_path(base, field).and_then(|v| v.as_str()) {
            println!("{}", text);
        }
        return;
    }

    // Print ID to stdout. Strings render as-is; numbers / bools
    // stringify so a `display.id` pointing at e.g. a `subscriber_count`
    // count (`i64` on the wire) doesn't silently print nothing.
    if let Some(ref id_path) = display.id {
        if let Some(val) = json_path(response, id_path) {
            if let Some(s) = val.as_str() {
                println!("{}", s);
            } else if val.is_number() || val.is_boolean() {
                println!("{}", val);
            }
        }
    }

    // Print success message to stderr. Same number-fallback as the
    // ID block so `<msg>: <N>` works for numeric ids without falling
    // back to the literal `?` placeholder.
    if let Some(ref msg) = display.msg {
        if let Some(ref id_path) = display.id {
            if let Some(val) = json_path(response, id_path) {
                let rendered = val
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| val.to_string());
                eprintln!("{}: {}", msg, rendered);
            }
        } else {
            eprintln!("{}", msg);
        }
    }

    // List mode
    if let Some(ref list_path) = display.list {
        if let Some(items) = json_path(response, list_path).and_then(|v| v.as_array()) {
            if items.is_empty() {
                println!("  (none)");
            } else {
                for item in items {
                    let parts: Vec<String> = display
                        .cols
                        .iter()
                        .filter_map(|col| {
                            item.get(col).map(|v| match v {
                                Value::String(s) => s.clone(),
                                Value::Null => String::new(),
                                other => other.to_string(),
                            })
                        })
                        .collect();
                    println!("  {}", parts.join("\t"));
                }
            }
        }
    }

    // Single-object columns (for `get` commands).
    // Cols support dotted paths (e.g. `user.display_name`) via json_path so
    // responses with nested shape (like ContributorResponse { user, activity })
    // can reach inside.
    if display.list.is_none() && !display.cols.is_empty() {
        let data = json_path(response, "data").unwrap_or(response);
        for col in &display.cols {
            if let Some(val) = json_path(data, col) {
                let display_val = match val {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    Value::Null => continue,
                    other => other.to_string(),
                };
                let label = col.rsplit('.').next().unwrap_or(col);
                println!("{}: {}", label, display_val);
            }
        }
    }
}

fn handle_error_response(body: &Value, def: &CommandDef, json_output: bool) -> Result<ExitCode> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(body).unwrap_or_default());
    } else if let Some(ref display) = def.display {
        if display.review {
            // Review error — show the review from error.review
            if let Some(review) = body.get("error").and_then(|e| e.get("review")) {
                let _ = crate::commands::review::print_review_and_exit(review, false);
            } else {
                let code = body["error"]["code"].as_str().unwrap_or("ERROR");
                let msg = body["error"]["message"].as_str().unwrap_or("Unknown error");
                eprintln!("[{}] {}", code, msg);
            }
        } else {
            let code = body["error"]["code"].as_str().unwrap_or("ERROR");
            let msg = body["error"]["message"].as_str().unwrap_or("Unknown error");
            eprintln!("[{}] {}", code, msg);
        }
    }

    // Check exit code map
    if let Some(ref ecm) = def.exit_codes {
        if let Some(val) = json_path(body, &ecm.field) {
            if let Some(s) = val.as_str() {
                if let Some(&code) = ecm.map.get(s) {
                    return Ok(ExitCode::from(code));
                }
            }
        }
    }

    Ok(ExitCode::from(1))
}

async fn handle_download(
    client: &KrabClient,
    path: &str,
    matches: &ArgMatches,
) -> Result<ExitCode> {
    let data = client.download(path).await?;
    if let Some(output) = matches.get_one::<String>("output") {
        std::fs::write(output, &data)?;
        eprintln!("Saved to {}", output);
    } else {
        use std::io::Write;
        std::io::stdout().write_all(&data)?;
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_string(matches: &ArgMatches, name: &str) -> Option<String> {
    matches.get_one::<String>(name).cloned()
}

fn resolve_text_or_file_arg(arg_name: &str, raw: &str) -> Result<String> {
    let Some(path) = raw.strip_prefix('@') else {
        return Ok(raw.to_string());
    };
    if path.is_empty() {
        anyhow::bail!("--{}: @file path is empty", arg_name);
    }
    std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(
            "--{} {}: cannot read UTF-8 text file '{}': {}",
            arg_name,
            raw,
            path,
            e
        )
    })
}

/// Extract an arg value as a string, respecting its declared type.
/// Used for URL query/path substitution, which always needs a `String`.
fn get_arg_as_string(matches: &ArgMatches, arg: &ArgDef) -> Option<String> {
    match arg.r#type.as_str() {
        "int" => matches.get_one::<i64>(&arg.name).map(|v| v.to_string()),
        "bool" => {
            if matches.get_flag(&arg.name) {
                Some("true".to_string())
            } else {
                None
            }
        }
        _ => get_string(matches, &arg.name),
    }
}

/// Turn a user-supplied JSON arg into a real `Value`:
/// - `@path/to/file.json` → read file, parse JSON
/// - inline JSON (`{…}` / `[…]` / primitives) → parse directly
///
fn parse_json_arg(arg_name: &str, v: &Value) -> Result<Value> {
    let Some(s) = v.as_str() else {
        return Ok(v.clone());
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(Value::String(String::new()));
    }
    let source: std::borrow::Cow<'_, str> = if let Some(path) = trimmed.strip_prefix('@') {
        if path.is_empty() {
            anyhow::bail!("--{}: @file path is empty", arg_name);
        }
        std::fs::read_to_string(path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "--{} {}: cannot read JSON file '{}': {}",
                    arg_name,
                    s,
                    path,
                    e
                )
            })?
            .into()
    } else {
        std::borrow::Cow::Borrowed(trimmed)
    };
    serde_json::from_str::<Value>(source.trim()).map_err(|e| {
        anyhow::anyhow!(
            "--{}: invalid JSON{}: {}",
            arg_name,
            if trimmed.starts_with('@') {
                " in @file"
            } else {
                ""
            },
            e
        )
    })
}

fn typed_constant_value(arg: &ArgDef) -> Option<Value> {
    let constant = arg.constant.as_deref()?;
    match arg.r#type.as_str() {
        "bool" => constant.parse::<bool>().ok().map(Value::Bool),
        "int" => constant.parse::<i64>().ok().map(|value| json!(value)),
        "csv" => Some(json!(
            constant.split(',').map(str::trim).collect::<Vec<&str>>()
        )),
        _ => Some(Value::String(constant.to_string())),
    }
}

fn get_typed_value(matches: &ArgMatches, arg: &ArgDef) -> Option<Value> {
    match arg.r#type.as_str() {
        "bool" => {
            if matches.get_flag(&arg.name) {
                Some(Value::Bool(true))
            } else {
                None
            }
        }
        "int" => matches.get_one::<i64>(&arg.name).map(|v| json!(*v)),
        "csv" => get_string(matches, &arg.name).map(|s| {
            let items: Vec<&str> = s.split(',').map(|s| s.trim()).collect();
            json!(items)
        }),
        _ => get_string(matches, &arg.name).map(Value::String),
    }
}

fn json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn print_about(response: &Value) {
    let data = json_path(response, "data").unwrap_or(response);

    if let Some(tagline) = data["tagline"].as_str() {
        println!("🦀 KrabArena\n{}\n", tagline);
    }
    if let Some(motto) = data["motto"].as_str() {
        println!("{}\n", motto);
    }

    // Vocab
    if let Some(vocab) = data["vocab"].as_array() {
        println!("Vocabulary");
        for v in vocab {
            let word = v["word"].as_str().unwrap_or("?");
            let def = v["definition"].as_str().unwrap_or("");
            let example = v["example"].as_str().unwrap_or("");
            println!("  {:<16} {}", word, def);
            if !example.is_empty() {
                println!("  {:<16} e.g. {}", "", example);
            }
        }
        println!();
    }

    // Mission
    if let Some(mission) = data["mission"].as_array() {
        println!("Mission");
        for m in mission {
            if let Some(s) = m.as_str() {
                println!("  {}", s);
            }
        }
        println!();
    }

    // FAQ
    if let Some(faq) = data["faq"].as_array() {
        println!("FAQ");
        for item in faq {
            let q = item["question"].as_str().unwrap_or("?");
            let a = item["answer"].as_str().unwrap_or("");
            println!("  Q: {}", q);
            println!("  A: {}\n", a);
        }
    }

    if let Some(sign_off) = data["sign_off"].as_str() {
        println!("{}", sign_off);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krabarena_shared::manifest::ArgDef;

    fn claim_post_def() -> CommandDef {
        CommandDef {
            name: "post".into(),
            method: Some("POST".into()),
            path: Some("/v1/battles/{battle_id}/claims".into()),
            multipart: true,
            args: vec![
                ArgDef {
                    name: "claim".into(),
                    flag: true,
                    part: Some("claim".into()),
                    ..Default::default()
                },
                ArgDef {
                    name: "agent-backend".into(),
                    flag: true,
                    config: Some("agent_backend".into()),
                    part: Some("agent_backend".into()),
                    ..Default::default()
                },
                ArgDef {
                    name: "agent-model".into(),
                    flag: true,
                    config: Some("agent_model".into()),
                    part: Some("agent_model".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn build_matches(def: &CommandDef, argv: Vec<&str>) -> ArgMatches {
        let cmd = build_command(def);
        let mut full = vec!["post"];
        full.extend(argv);
        cmd.try_get_matches_from(full).expect("parse")
    }

    #[test]
    fn text_length_preflight_rejects_long_claim() {
        let def = claim_post_def();
        let long = "a".repeat(5001);
        let m = build_matches(&def, vec!["--claim", &long]);
        let err =
            preflight_claim_text_length(&def, &m).expect_err("5001-char claim should be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("VALIDATION_ERROR") && msg.contains("1-5000"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn text_length_preflight_accepts_exactly_max() {
        let def = claim_post_def();
        let exactly = "a".repeat(5000);
        let m = build_matches(&def, vec!["--claim", &exactly]);
        assert!(
            preflight_claim_text_length(&def, &m).is_ok(),
            "5000-char claim should pass"
        );
    }

    #[test]
    fn text_or_file_reads_markdown_file() {
        let tmp = std::env::temp_dir().join(format!("krab-text-or-file-{}.md", std::process::id()));
        std::fs::write(&tmp, "# Title\n\n## Summary\nfile body\n").unwrap();
        let raw = format!("@{}", tmp.display());
        let resolved = resolve_text_or_file_arg("claim", &raw).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(resolved, "# Title\n\n## Summary\nfile body\n");
    }

    #[test]
    fn text_or_file_keeps_inline_text() {
        assert_eq!(
            resolve_text_or_file_arg("claim", "# Title\ninline").unwrap(),
            "# Title\ninline"
        );
    }

    #[test]
    fn text_or_file_missing_file_errors_before_request_build() {
        let err = resolve_text_or_file_arg("claim", "@/tmp/krab-missing-claim-file.md")
            .expect_err("missing @file must be a local error");
        assert!(
            err.to_string().contains("cannot read UTF-8 text file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn claim_text_preflight_reads_file_before_length_check() {
        let tmp = std::env::temp_dir().join(format!("krab-long-claim-{}.md", std::process::id()));
        std::fs::write(&tmp, "a".repeat(5001)).unwrap();

        let def = CommandDef {
            name: "post".into(),
            method: Some("POST".into()),
            multipart: true,
            args: vec![ArgDef {
                name: "claim".into(),
                flag: true,
                r#type: "text_or_file".into(),
                part: Some("claim".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let cmd = build_command(&def);
        let raw = format!("@{}", tmp.display());
        let m = cmd
            .try_get_matches_from(["post", "--claim", &raw])
            .expect("parse");
        let err = preflight_claim_text_length(&def, &m).expect_err("file content is too long");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            err.to_string().contains("got 5001"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn path_param_normalization_accepts_full_claim_and_battle_urls() {
        assert_eq!(
            normalize_path_param_value(
                "https://krabarena.com/claims/repo-count-rerun-shows-python?x=1#top"
            ),
            "repo-count-rerun-shows-python"
        );
        assert_eq!(
            normalize_path_param_value(
                "https://krabarena.org/battles/python-vs-typescript-vs-rust-vs-go/"
            ),
            "python-vs-typescript-vs-rust-vs-go"
        );
        assert_eq!(
            normalize_path_param_value("http://127.0.0.1:3000/battles/fab3f4b6#claims"),
            "fab3f4b6"
        );
        assert_eq!(
            normalize_path_param_value("plain-slug-or-uuid"),
            "plain-slug-or-uuid"
        );
    }

    #[test]
    fn text_length_preflight_ignores_commands_without_claim_arg() {
        // Any arg that isn't `claim` should not be checked by this pre-flight.
        let def = CommandDef {
            name: "noop".into(),
            args: vec![ArgDef {
                name: "description".into(),
                flag: true,
                body: Some("description".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let cmd = build_command(&def);
        let m = cmd
            .try_get_matches_from(["noop", "--description", &"x".repeat(10_000)])
            .expect("parse");
        assert!(preflight_claim_text_length(&def, &m).is_ok());
    }

    #[test]
    fn missing_identity_flagged_when_config_empty_and_no_flag() {
        let def = claim_post_def();
        let m = build_matches(&def, vec!["--claim", "ok"]);
        let cfg = Config {
            agent_backend: None,
            agent_model: Some(String::new()),
            ..Config::default()
        };
        let missing = missing_identity_fields(&def, &m, &cfg);
        assert_eq!(missing, vec!["agent_backend", "agent_model"]);
    }

    #[test]
    fn missing_identity_empty_when_flag_provided() {
        let def = claim_post_def();
        let m = build_matches(
            &def,
            vec![
                "--claim",
                "ok",
                "--agent-backend",
                "Claude Code",
                "--agent-model",
                "claude-opus-4-7",
            ],
        );
        let cfg = Config::default();
        let missing = missing_identity_fields(&def, &m, &cfg);
        assert!(
            missing.is_empty(),
            "flags should override empty config: {missing:?}"
        );
    }

    #[test]
    fn missing_identity_empty_when_config_populated() {
        let def = claim_post_def();
        let m = build_matches(&def, vec!["--claim", "ok"]);
        let cfg = Config {
            agent_backend: Some("Claude Code".into()),
            agent_model: Some("claude-opus-4-7".into()),
            ..Config::default()
        };
        assert!(missing_identity_fields(&def, &m, &cfg).is_empty());
    }

    #[test]
    fn build_json_body_injects_manifest_constant_for_verify_verdict() {
        // Mirrors the real `verify` manifest — verdict is a constant,
        // environment is a flag, agent-backend is config-injected.
        let def = CommandDef {
            name: "verify".into(),
            method: Some("POST".into()),
            args: vec![
                ArgDef {
                    name: "verdict".into(),
                    body: Some("verdict".into()),
                    constant: Some("confirmed".into()),
                    ..Default::default()
                },
                ArgDef {
                    name: "env".into(),
                    flag: true,
                    required: true,
                    body: Some("environment".into()),
                    ..Default::default()
                },
                ArgDef {
                    name: "agent-backend".into(),
                    flag: true,
                    config: Some("agent_backend".into()),
                    body: Some("agent_backend".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let cmd = build_command(&def);
        let m = cmd
            .try_get_matches_from(["verify", "--env", "macOS M2"])
            .expect("parse");
        let cfg = Config {
            agent_backend: Some("Claude Code".into()),
            ..Config::default()
        };
        let body = build_json_body(&m, &def.args, &cfg).unwrap();
        assert_eq!(body["verdict"], "confirmed");
        assert_eq!(body["environment"], "macOS M2");
        assert_eq!(body["agent_backend"], "Claude Code");
    }

    fn battle_edit_def() -> CommandDef {
        CommandDef {
            name: "edit".into(),
            method: Some("PATCH".into()),
            args: vec![
                ArgDef {
                    name: "proxy-spec".into(),
                    flag: true,
                    body: Some("proxy_spec".into()),
                    ..Default::default()
                },
                ArgDef {
                    name: "title".into(),
                    flag: true,
                    body: Some("title".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn verify_clap_rejects_invocation_missing_tokens_and_cost() {
        // 0.16.0 PR: --tokens and --cost are required at clap-parse
        // time. Before, the LLM that drives KrabChecker would emit a
        // `krab verify <id> --env ... --output-hash ... --duration ...`
        // without effort flags and the server happily wrote NULLs. Now
        // the local CLI binary refuses to send the request — which is
        // the only enforcement layer that can't be bypassed by an old
        // client (the server still accepts NULL for back-compat with
        // refute and pre-0.16.0 binaries in the wild).
        //
        // We pull the real `verify` command out of the shipped manifest
        // — not a hand-built fixture — so this test goes red the
        // moment someone reverts `required: true` on the builder side.
        let manifest = krabarena_shared::manifest_builder::build_manifest();
        let verify_def = manifest
            .commands
            .iter()
            .find(|c| c.name == "verify")
            .expect("manifest must ship a verify command");
        let cmd = build_command(verify_def);

        // Missing both --tokens and --cost: must error.
        let err = cmd
            .clone()
            .try_get_matches_from([
                "verify",
                "deadbeef-claim-id",
                "--env",
                "macOS",
                "--output-hash",
                "sha256:abc",
                "--duration",
                "1000",
            ])
            .expect_err("clap must reject verify without --tokens and --cost");
        let msg = err.to_string();
        assert!(
            msg.contains("--tokens") || msg.contains("tokens"),
            "expected clap to name the missing --tokens flag; got: {msg}"
        );

        // Missing only --cost: still must error (regression guard
        // against someone flipping just one of the two back to
        // optional).
        let err2 = cmd
            .clone()
            .try_get_matches_from([
                "verify",
                "deadbeef-claim-id",
                "--env",
                "macOS",
                "--output-hash",
                "sha256:abc",
                "--duration",
                "1000",
                "--tokens",
                "42",
            ])
            .expect_err("clap must reject verify without --cost");
        assert!(
            err2.to_string().contains("cost"),
            "expected clap to name the missing --cost flag; got: {err2}"
        );

        // Sanity: with every required flag, clap accepts.
        cmd.clone()
            .try_get_matches_from([
                "verify",
                "deadbeef-claim-id",
                "--env",
                "macOS",
                "--output-hash",
                "sha256:abc",
                "--duration",
                "1000",
                "--tokens",
                "42",
                "--cost",
                "1",
            ])
            .expect("verify with all required flags must parse");
    }

    #[test]
    fn proxy_spec_inline_json_parsed_into_object() {
        // Regression: previously, `--proxy-spec='{"services":{}}'` got
        // sent as a JSON string (literal `"{\"services\":{}}"`) inside
        // the wire body, so the server stored a string in a JSONB
        // column and any downstream reader broke.
        let def = battle_edit_def();
        let cmd = build_command(&def);
        let m = cmd
            .try_get_matches_from([
                "edit",
                "--proxy-spec",
                "{\"services\":{\"keenable\":{\"cost_per_1k_cents\":100}}}",
            ])
            .expect("parse");
        let body = build_json_body(&m, &def.args, &Config::default()).unwrap();
        assert!(
            body["proxy_spec"].is_object(),
            "expected object, got {:?}",
            body["proxy_spec"]
        );
        assert_eq!(
            body["proxy_spec"]["services"]["keenable"]["cost_per_1k_cents"],
            100
        );
    }

    #[test]
    fn proxy_spec_at_file_read_and_parsed() {
        let tmp = std::env::temp_dir().join("test-proxy-spec.json");
        std::fs::write(&tmp, r#"{"budget_cents": 5000}"#).unwrap();
        let def = battle_edit_def();
        let cmd = build_command(&def);
        let arg = format!("@{}", tmp.display());
        let m = cmd
            .try_get_matches_from(["edit", "--proxy-spec", &arg])
            .expect("parse");
        let body = build_json_body(&m, &def.args, &Config::default()).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(body["proxy_spec"]["budget_cents"], 5000);
    }

    #[test]
    fn proxy_spec_invalid_json_errors_locally() {
        let def = battle_edit_def();
        let cmd = build_command(&def);
        let m = cmd
            .try_get_matches_from(["edit", "--proxy-spec", "{not-json"])
            .expect("parse");
        let err = build_json_body(&m, &def.args, &Config::default())
            .expect_err("invalid JSON should fail before request build");
        assert!(
            err.to_string().contains("invalid JSON"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn proxy_spec_missing_at_file_errors_locally() {
        let def = battle_edit_def();
        let cmd = build_command(&def);
        let m = cmd
            .try_get_matches_from(["edit", "--proxy-spec", "@/tmp/krab-missing-proxy-spec.json"])
            .expect("parse");
        let err = build_json_body(&m, &def.args, &Config::default())
            .expect_err("missing @file should fail before request build");
        assert!(
            err.to_string().contains("cannot read JSON file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn claim_structure_invalid_at_file_errors_locally() {
        let tmp = std::env::temp_dir().join("test-claim-structure-invalid.json");
        std::fs::write(&tmp, "{not-json").unwrap();
        let def = CommandDef {
            name: "claim-update".into(),
            method: Some("PATCH".into()),
            args: vec![ArgDef {
                name: "structure".into(),
                flag: true,
                body: Some("claim_structure".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let cmd = build_command(&def);
        let arg = format!("@{}", tmp.display());
        let m = cmd
            .try_get_matches_from(["claim-update", "--structure", &arg])
            .expect("parse");
        let err = build_json_body(&m, &def.args, &Config::default())
            .expect_err("invalid @file JSON should fail before request build");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            err.to_string().contains("invalid JSON in @file"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn proxy_spec_omitted_when_flag_not_passed() {
        let def = battle_edit_def();
        let cmd = build_command(&def);
        let m = cmd
            .try_get_matches_from(["edit", "--title", "New title"])
            .expect("parse");
        let body = build_json_body(&m, &def.args, &Config::default()).unwrap();
        assert!(
            body.get("proxy_spec").is_none(),
            "proxy_spec should be omitted when the flag isn't passed"
        );
        assert_eq!(body["title"], "New title");
    }

    #[test]
    fn claim_structure_json_parsed_into_object() {
        let def = CommandDef {
            name: "claim-update".into(),
            method: Some("PATCH".into()),
            args: vec![ArgDef {
                name: "structure".into(),
                flag: true,
                body: Some("claim_structure".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let cmd = build_command(&def);
        let m = cmd
            .try_get_matches_from(["claim-update", "--structure", "{\"leaderboard\":[]}"])
            .expect("parse");
        let body = build_json_body(&m, &def.args, &Config::default()).unwrap();
        assert!(body["claim_structure"].is_object());
        assert_eq!(body["claim_structure"]["leaderboard"], json!([]));
    }

    #[test]
    fn json_body_text_or_file_reads_markdown_file() {
        let tmp = std::env::temp_dir().join("test-admin-claim.md");
        std::fs::write(&tmp, "# Title\n\n## Summary\nFrom file.").unwrap();
        let def = CommandDef {
            name: "claim-update".into(),
            method: Some("PATCH".into()),
            args: vec![ArgDef {
                name: "claim".into(),
                flag: true,
                r#type: "text_or_file".into(),
                body: Some("claim".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let cmd = build_command(&def);
        let raw = format!("@{}", tmp.display());
        let m = cmd
            .try_get_matches_from(["claim-update", "--claim", &raw])
            .expect("parse");
        let body = build_json_body(&m, &def.args, &Config::default()).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(body["claim"], "# Title\n\n## Summary\nFrom file.");
    }

    #[test]
    fn bool_constant_in_json_body_is_boolean() {
        let def = CommandDef {
            name: "unsuspend".into(),
            method: Some("PATCH".into()),
            args: vec![ArgDef {
                name: "lift_suspension".into(),
                r#type: "bool".into(),
                body: Some("lift_suspension".into()),
                constant: Some("true".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let cmd = build_command(&def);
        let m = cmd.try_get_matches_from(["unsuspend"]).expect("parse");
        let body = build_json_body(&m, &def.args, &Config::default()).unwrap();
        assert_eq!(body["lift_suspension"], true);
    }
}
