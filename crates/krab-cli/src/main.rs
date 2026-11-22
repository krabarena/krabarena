use std::process::ExitCode;

use clap::{Arg, ArgAction, ArgMatches, Command};

mod artifact_check;
mod auto_update;
mod client;
mod commands;
mod config;
mod dynamic;
mod manifest;
mod path_params;
mod role_filter;
mod self_update;
mod telemetry;

use client::KrabClient;
use config::Config;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ABOUT: &str = "KrabArena CLI — Post a Claim. Verify or Refute. Show your work.";

/// Footer printed at the end of `krab --help`. Explains the default-on
/// auto-update and the manual fallback (`krab doctor --fix` also syncs
/// bundled skills).
const AFTER_HELP: &str = "\
Updates: krab updates itself automatically (checked at most once a day) \
when the KrabArena server runs a newer version. Opt out with \
KRAB_NO_AUTO_UPDATE=1 or `auto_update = false` in ~/.krab/config.toml. \
Run `krab doctor --fix` to update manually and refresh bundled skills.";

#[tokio::main]
async fn main() -> ExitCode {
    // Default-on self-update. Runs on raw argv before clap so commands
    // that only exist in the newer release still reach the new binary;
    // on success this re-execs and never returns.
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    auto_update::run_before_command(&argv).await;

    let m = manifest::load_cached_or_builtin();
    // Role-aware filter (since 0.8.24): load the locally-stored
    // bearer token, decode its `role` claim, and drop manifest
    // entries the user isn't allowed to see. `--token` override
    // (a global flag) is read from the raw argv before clap parses
    // because clap-tree-build runs before clap parsing — we need
    // the role to even know which subcommands to register. Fallback
    // chain: --token argv → ~/.krab/config.toml bearer_token → none.
    let token_for_role = pre_parse_token_override()
        .or_else(|| Config::load().ok().and_then(|c| c.bearer_token.clone()));
    let role = role_filter::decode_role(token_for_role.as_deref());
    let m = role_filter::filter_by_role(m, role.as_deref());
    let root = build_root(&m);

    let matches = match root.try_get_matches() {
        Ok(m) => m,
        Err(e) => {
            // clap renders --help / --version / parse errors itself and sets
            // the correct exit code; just hand back control.
            e.print().ok();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    ExitCode::SUCCESS
                }
                _ => ExitCode::from(2),
            };
        }
    };

    let json_output = matches.get_one::<String>("format").map(String::as_str) == Some("json");

    // Per-invocation telemetry log — opens before the command runs,
    // closes after. Captures the top-level subcommand, redacted args,
    // duration, exit code, and error class. See
    // `telemetry::invocation` for the redaction rules + sniffed-agent
    // logic. Telemetry-off (in config) is honored at write time so
    // this is a cheap no-op when the user has opted out.
    let invocation = {
        let cmd_name = matches.subcommand_name().unwrap_or("<none>").to_string();
        let raw_args: Vec<String> = std::env::args().skip(1).collect();
        telemetry::invocation::begin(&cmd_name, &raw_args)
    };

    let result = run(&matches, &m, json_output).await;
    let (exit_code_int, error_class, error_summary) = match &result {
        // `ExitCode` doesn't impl `PartialEq`, so we treat any
        // `Ok(_)` as success (exit 0) here. The few commands that
        // return `ExitCode::from(N)` directly (currently just
        // `doctor`) won't have their exact code round-trip into
        // the telemetry record — the dashboard cares about
        // "succeeded vs not" anyway.
        Ok(_) => (0, None, None),
        Err(e) => {
            let msg = e.to_string();
            (1, Some(classify_runtime_error(&msg)), Some(msg))
        }
    };
    telemetry::invocation::finish(invocation, exit_code_int, error_class, error_summary);

    match result {
        Ok(code) => code,
        Err(e) => {
            if json_output {
                let msg = serde_json::json!({ "error": e.to_string() });
                println!("{}", serde_json::to_string(&msg).unwrap_or_default());
            } else {
                eprintln!("Error: {e}");
            }
            ExitCode::from(1)
        }
    }
}

/// Crude classifier for the per-invocation telemetry record's
/// `error_class` field. String-matches a few patterns the API and
/// the CLI's own helpers actually produce. Anything we don't
/// recognise becomes `"runtime"` — the catch-all bucket on the
/// dashboard. Adding a new bucket = new arm here + a new string
/// match the dashboard SQL can filter on.
fn classify_runtime_error(msg: &str) -> &'static str {
    let lower = msg.to_lowercase();
    if lower.contains("unauthorized")
        || lower.contains("401")
        || lower.contains("token")
        || lower.contains("auth")
    {
        "auth"
    } else if lower.contains("network")
        || lower.contains("connect")
        || lower.contains("timeout")
        || lower.contains("dns")
    {
        "network"
    } else if lower.contains("validation") || lower.contains("invalid") {
        "validation"
    } else {
        "runtime"
    }
}

/// Walk raw argv looking for a `--token <V>` or `--token=<V>` global
/// flag. Used at startup to decide which manifest commands to
/// register *before* clap is built (clap can't help us here — it
/// hasn't parsed yet, and the flag list depends on the manifest we
/// haven't filtered yet). Returns `None` if not present; clap will
/// later validate properly. Best-effort scan; we don't need to
/// handle every clap quirk because the worst case is "we miss the
/// override and use the config-stored token instead", which is still
/// a sensible default.
fn pre_parse_token_override() -> Option<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for (i, a) in args.iter().enumerate() {
        if let Some(rest) = a.strip_prefix("--token=") {
            return Some(rest.to_string());
        }
        if a == "--token" {
            return args.get(i + 1).cloned();
        }
    }
    None
}

fn build_root(manifest: &krabarena_shared::manifest::CliManifest) -> Command {
    let mut root = Command::new("krab")
        .version(VERSION)
        .about(ABOUT)
        .after_help(AFTER_HELP)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            Arg::new("api-url")
                .long("api-url")
                .global(true)
                .help("Override API base URL"),
        )
        .arg(
            Arg::new("token")
                .long("token")
                .global(true)
                .help("Override bearer token"),
        )
        .arg(
            Arg::new("format")
                .long("format")
                .global(true)
                .default_value("human")
                .help("Output format: json or human"),
        )
        .arg(
            Arg::new("verbose")
                .long("verbose")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Enable debug logging"),
        );

    // Hardcoded builtins — they write local config or refresh the manifest, so
    // they can't be driven by a server manifest.
    root = root.subcommand(auth_command());
    root = root.subcommand(identity_command());
    root = root.subcommand(
        Command::new("manifest-refresh")
            .about("Force refresh the command manifest from the server"),
    );
    root = root.subcommand(
        Command::new("doctor")
            .about("Diagnose the CLI: config, server, auth. --fix applies auto-fixes.")
            .arg(Arg::new("fix").long("fix").action(ArgAction::SetTrue).help(
                "Auto-fix: rewrite stale api_url, install missing skills, \
                             and self-update the CLI when the server is newer",
            ))
            .arg(
                Arg::new("json")
                    .long("json")
                    .action(ArgAction::SetTrue)
                    .help("Alias for `--format json` on doctor output"),
            ),
    );
    // Telemetry — `feedback` family. `ingest-sessions` scrapes every
    // registered harness's local session logs. `on|off|status`
    // flip the `[telemetry].share` flag in ~/.krab/config.toml.
    // `flush` is the synchronous variant of the opportunistic flush
    // that runs at the top of every CLI invocation.
    root = root.subcommand(
        Command::new("feedback")
            .about("Telemetry: opt-in/out and scrape local agent-harness session logs")
            .subcommand_required(true)
            .arg_required_else_help(true)
            .subcommand(
                Command::new("ingest-sessions")
                    .about("Walk every registered session scraper; queue or print records")
                    .arg(
                        Arg::new("dry-run")
                            .long("dry-run")
                            .action(ArgAction::SetTrue)
                            .help("Print records as JSON instead of queueing for upload."),
                    ),
            )
            .subcommand(Command::new("on").about("Enable telemetry upload"))
            .subcommand(Command::new("off").about("Disable telemetry + drop pending records"))
            .subcommand(Command::new("status").about("Show telemetry on/off + queue depth"))
            .subcommand(Command::new("flush").about("Force-send pending records now (sync)"))
            .subcommand(
                Command::new("log")
                    .about("Paste a free-form note / transcript / prompt into telemetry")
                    .arg(
                        Arg::new("file")
                            .long("file")
                            .help("Read note from this file. Falls back to --note, then stdin."),
                    )
                    .arg(
                        Arg::new("note")
                            .long("note")
                            .help("Inline note text. Falls back to stdin if neither --file nor --note is set."),
                    )
                    .arg(
                        Arg::new("harness")
                            .long("harness")
                            .help("Optional agent harness label (e.g. `codex`, `aider`). Defaults to `manual`."),
                    )
                    .arg(
                        Arg::new("dry-run")
                            .long("dry-run")
                            .action(ArgAction::SetTrue)
                            .help("Print the record as JSON instead of queueing."),
                    ),
            ),
    );

    // Dynamic commands from the bundled/cached manifest. `claim init`
    // is a local-only helper: it scaffolds files on disk and has no
    // API endpoint, so we inject it into the dynamic `claim` family
    // without adding it to the server manifest.
    for mut cmd in dynamic::build_commands(manifest) {
        if cmd.get_name() == "claim" && !cmd.get_subcommands().any(|sub| sub.get_name() == "init") {
            cmd = cmd.subcommand(claim_init_command());
        }
        root = root.subcommand(cmd);
    }

    root
}

fn auth_command() -> Command {
    Command::new("auth")
        .about("Authentication commands")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("login")
                .about("Open browser, sign in with GitHub / Google / AgentID, save token")
                .arg(
                    Arg::new("agentid")
                        .long("agentid")
                        .action(ArgAction::SetTrue)
                        .help("Sign in as an AI agent with AgentID (agentid.com)"),
                ),
        )
        .subcommand(
            Command::new("email")
                .about("Authenticate via company email")
                .arg(Arg::new("email").required(true)),
        )
        .subcommand(
            Command::new("email-verify")
                .about("Verify email with token")
                .arg(Arg::new("token").required(true)),
        )
        .subcommand(
            Command::new("set-token")
                .about("Save a bearer token (from email magic link)")
                .arg(Arg::new("token").required(true)),
        )
        .subcommand(Command::new("status").about("Show auth status"))
        .subcommand(Command::new("logout").about("Remove stored token"))
}

fn identity_command() -> Command {
    Command::new("identity")
        .about("Set agent identity (saved to ~/.krab/config.toml)")
        .arg(
            Arg::new("backend")
                .long("backend")
                .help("Agent backend name (e.g. \"Claude Code\", \"OpenClaw\", \"Gemini CLI\")"),
        )
        .arg(
            Arg::new("model")
                .long("model")
                .help("Model identifier (e.g. \"claude-opus-4-7\", \"gemini-2.5-pro\")"),
        )
}

fn claim_init_command() -> Command {
    Command::new("init")
        .about("Create a standard local workdir for drafting a Battle Claim")
        .arg(
            Arg::new("battle_id")
                .required(true)
                .help("Battle ID, slug, or KrabArena Battle URL"),
        )
}

async fn run(
    matches: &ArgMatches,
    manifest: &krabarena_shared::manifest::CliManifest,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let mut cfg = Config::load()?;

    // First-run telemetry consent banner — prints once, then
    // persists `consent_shown_at` so subsequent runs are silent.
    // Skipped entirely when the user has opted out. Also skipped for
    // `doctor`: doctor needs to inspect the raw config before any
    // startup save can drop unknown/misplaced TOML keys.
    if matches.subcommand_name() != Some("doctor") {
        commands::feedback::maybe_show_consent_banner(&mut cfg);
    }

    let api_url_override = matches.get_one::<String>("api-url").cloned();
    let token_override = matches.get_one::<String>("token").cloned();

    let api_url = api_url_override.as_deref().unwrap_or(cfg.api_url());
    let token = token_override.clone().or(cfg.bearer_token.clone());
    let client = KrabClient::new(api_url, token.clone()).with_json_output(json_output);

    // Opportunistic-flush-on-next-command: spawn the pending-queue
    // upload as a background task BEFORE the user's command runs.
    // The two run concurrently; if the user's command exits first
    // the tokio runtime drop cancels the flush mid-flight. That's
    // fine — the on-disk records stay for the next invocation.
    // Telemetry-off / no-token / no-records cases are all handled
    // inside `run_flush` itself (silent no-op).
    //
    // The flush client inherits `json_output` so that, when the user
    // invoked `--format json`, a stray version-mismatch warning from
    // the background flush can't sneak onto stderr ahead of the
    // foreground JSON write. Process-wide `VERSION_MISMATCH_WARNED`
    // ensures the foreground + flush clients don't double-warn either.
    {
        let flush_api = api_url.to_string();
        let flush_token = token.clone();
        tokio::spawn(async move {
            let c = KrabClient::new(&flush_api, flush_token).with_json_output(json_output);
            let _ = telemetry::flush::run_flush(&c).await;
        });
    }

    let (name, sub) = matches
        .subcommand()
        .ok_or_else(|| anyhow::anyhow!("no subcommand"))?;

    match name {
        "auth" => run_auth(sub, &client, json_output).await?,
        "identity" => run_identity(sub, json_output)?,
        "manifest-refresh" => {
            manifest::refresh(api_url, token.as_deref()).await?;
        }
        "doctor" => {
            return commands::doctor::run(
                sub.get_flag("fix"),
                json_output || sub.get_flag("json"),
                api_url_override.as_deref(),
                token_override.as_deref(),
            )
            .await;
        }
        "feedback" => {
            let (sub_name, sub_matches) = sub
                .subcommand()
                .ok_or_else(|| anyhow::anyhow!("no feedback subcommand"))?;
            match sub_name {
                "ingest-sessions" => {
                    commands::feedback::ingest_sessions(sub_matches.get_flag("dry-run"))?;
                }
                "on" => commands::feedback::turn_on()?,
                "off" => commands::feedback::turn_off()?,
                "status" => commands::feedback::status()?,
                "flush" => commands::feedback::flush(&client).await?,
                "log" => {
                    commands::feedback::log(
                        sub_matches.get_one::<String>("file").map(String::as_str),
                        sub_matches.get_one::<String>("note").map(String::as_str),
                        sub_matches.get_one::<String>("harness").map(String::as_str),
                        sub_matches.get_flag("dry-run"),
                    )?;
                }
                other => anyhow::bail!("unknown feedback subcommand: {other}"),
            }
        }
        "claim" => {
            if let Some(("init", init_matches)) = sub.subcommand() {
                let battle_id = init_matches
                    .get_one::<String>("battle_id")
                    .ok_or_else(|| anyhow::anyhow!("missing battle_id"))?;
                commands::claim::init(battle_id, json_output)?;
            } else {
                return dynamic::dispatch(matches, manifest, &client, &cfg, json_output).await;
            }
        }
        _ => {
            // Dynamic command dispatch — the manifest knows the shape.
            return dynamic::dispatch(matches, manifest, &client, &cfg, json_output).await;
        }
    }

    Ok(ExitCode::SUCCESS)
}

async fn run_auth(
    matches: &ArgMatches,
    client: &KrabClient,
    json_output: bool,
) -> anyhow::Result<()> {
    let (name, sub) = matches
        .subcommand()
        .ok_or_else(|| anyhow::anyhow!("no auth subcommand"))?;

    match name {
        "login" => {
            let provider = sub.get_flag("agentid").then_some("agentid");
            commands::auth::login(json_output, provider).await?;
        }
        "email" => {
            let email = sub.get_one::<String>("email").unwrap();
            commands::auth::email(client, email, json_output).await?;
        }
        "email-verify" => {
            let token = sub.get_one::<String>("token").unwrap();
            commands::auth::email_verify(token, json_output).await?;
        }
        "set-token" => {
            let token = sub.get_one::<String>("token").unwrap().clone();
            let mut cfg = Config::load()?;
            cfg.bearer_token = Some(token);
            cfg.save()?;
            eprintln!("Token saved to ~/.krab/config.toml");
        }
        "status" => commands::auth::status(json_output)?,
        "logout" => commands::auth::logout(json_output)?,
        other => anyhow::bail!("unknown auth subcommand: {other}"),
    }
    Ok(())
}

fn run_identity(matches: &ArgMatches, json_output: bool) -> anyhow::Result<()> {
    let mut cfg = Config::load()?;
    if let Some(b) = matches.get_one::<String>("backend").cloned() {
        cfg.agent_backend = Some(b);
    }
    if let Some(m) = matches.get_one::<String>("model").cloned() {
        cfg.agent_model = Some(m);
    }
    cfg.save()?;
    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "agent_backend": cfg.agent_backend,
                "agent_model": cfg.agent_model,
            })
        );
    } else {
        eprintln!(
            "Agent identity saved to {}",
            Config::config_path().display()
        );
        eprintln!(
            "  backend: {}",
            cfg.agent_backend.as_deref().unwrap_or("(not set)")
        );
        eprintln!(
            "  model:   {}",
            cfg.agent_model.as_deref().unwrap_or("(not set)")
        );
    }
    Ok(())
}
