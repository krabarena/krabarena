//! `krab doctor` — diagnose a misbehaving CLI and auto-fix known issues.
//!
//! Checks, in order:
//!   - CLI binary version (just reported; compared against server later)
//!   - Config file present and readable
//!   - api_url set, and not a known-stale host (e.g. the legacy
//!     `krabbit-api-*.run.app` service predating the rebrand)
//!   - Bearer token present
//!   - Agent identity — the `agent_backend` and `agent_model` values
//!     that stamp Claims and Verifications so the leaderboard credits
//!     the right agent. Without these, posts are attributed to nobody.
//!   - Server reachable on `/health`
//!   - Server `version` field exists (legacy services omit it) and is
//!     compatible with this CLI
//!   - Manifest endpoint (`/v1/cli/manifest`) reachable and well-formed —
//!     the contract between CLI and server for dynamic command dispatch.
//!   - End-to-end battle list (when a bearer is set) — real GET through
//!     the LB + envelope + JSON parsing, asserting at least one battle
//!     is visible. Catches the failure modes that bit us in practice:
//!     legacy api_url serving HTML, missing data envelope, malformed
//!     auth header, query builder panicking on int flags.
//!   - Skills installation — when project-local or global agent
//!     directories exist, verify the single `krabarena` skill is present
//!     in the right `skills/` subdirectory (as of 0.11.4; older
//!     installs had four `krabarena-*` skills which collapsed into
//!     one). `install.sh` bundles it alongside the CLI; a missing
//!     skill directory usually means the user installed with
//!     `--skip-skills` or the release archive wasn't available at
//!     install time.
//!   - Skills content drift — for each installed skill, compare every
//!     file byte-for-byte against the matching entry in the latest
//!     release archive (`gs://…/latest/krabarena-skills-*.tar.gz`).
//!     Drift is reported as `[stale]` with a per-file `+X/-Y` line
//!     count summary. Catches the case where a release ships an updated
//!     `SKILL.md` but the user already has the skill installed, so the
//!     missing-then-pull path never fires.
//!
//! With `--fix`:
//!   - known-stale api_url hosts are rewritten to `https://krabarena.com`
//!     and the config is saved
//!   - missing skills are pulled from GCS and extracted in place (same
//!     archives `install.sh` uses — no need to re-run the full installer)
//!   - drifted skill files are overwritten with the archive copy in
//!     place (per-file, so user-added scratch files in the skill dir
//!     survive)
//!   - if the server's `/health` version is newer than the running CLI,
//!     the binary tar.gz is downloaded from GCS, sha256-verified, copied
//!     to a sibling staging file (`.krab.update`) in the install dir,
//!     chmod'd, and atomically `rename`-d over `current_exe()`. On Unix
//!     overwriting a running binary is safe: the kernel keeps the old
//!     mapping alive for this process and the next `krab` invocation
//!     picks up the new one. The atomic rename eliminates the
//!     "kill-mid-copy → half-written binary" failure mode the prior
//!     `fs::copy` direct overwrite was vulnerable to.
//!
//! Token *validity* isn't probed — there's no CLI-auth whoami endpoint,
//! and the browser-OAuth `/auth/voter/me` 401s against CLI bearer tokens
//! by design. If a real command like `krab battle list` works, the
//! token is fine.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::self_update::{
    GCS_LATEST, HttpFetcher, download_to, extract_tar_gz, semver_cmp, tempdir_for, verify_sha256,
};

const CANONICAL_URL: &str = "https://krabarena.com";

/// Hosts we know serve a stale pre-rebrand build. Matched by substring.
///
/// `krabarena.org` is the legacy apex from before the 2026-05-22 .com
/// flip. Reads still work via the 308 redirect middleware, but writes
/// 401 because most HTTP clients drop the `Authorization` header on
/// cross-host redirects (a deliberate security mitigation). Rewriting
/// any user's stored `api_url` to `https://krabarena.com` on next
/// `doctor --fix` saves them the silent-auth-failure debugging.
const STALE_HOSTS: &[&str] = &["krabbit-api-", "krabbit.keenable", "krabarena.org"];

#[derive(Debug, Deserialize)]
struct Health {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Default, Serialize)]
struct DoctorReport {
    status: String,
    problems: usize,
    checks: Vec<DoctorCheck>,
}

#[derive(Clone, Debug, Serialize)]
struct DoctorCheck {
    section: String,
    label: String,
    status: String,
    severity: String,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    harness: Option<String>,
    scope: String,
}

struct DoctorReporter {
    json_output: bool,
    current_section: String,
    report: DoctorReport,
}

impl DoctorReporter {
    fn new(json_output: bool) -> Self {
        Self {
            json_output,
            current_section: String::new(),
            report: DoctorReport {
                status: "ok".into(),
                ..Default::default()
            },
        }
    }

    fn section(&mut self, name: &str) {
        self.current_section = name.to_string();
        if !self.json_output {
            println!();
            println!("{}", name);
        }
    }

    fn ok(&mut self, label: &str, detail: &str) {
        self.record("ok", label, detail, "hard", "core", None);
    }

    fn warn(&mut self, label: &str, detail: &str) {
        self.record("warn", label, detail, "soft", "core", None);
    }

    fn fail(&mut self, label: &str, detail: &str) {
        self.record("fail", label, detail, "hard", "core", None);
    }

    fn fixed(&mut self, label: &str, detail: &str) {
        self.record("fixed", label, detail, "hard", "core", None);
    }

    fn skill_ok(
        &mut self,
        harness: &str,
        scope: &'static str,
        hard: bool,
        label: &str,
        detail: &str,
    ) {
        self.record(
            "ok",
            label,
            detail,
            if hard { "hard" } else { "soft" },
            scope,
            Some(harness),
        );
    }

    fn skill_warn(&mut self, harness: &str, scope: &'static str, label: &str, detail: &str) {
        self.record("warn", label, detail, "soft", scope, Some(harness));
    }

    fn skill_fail(
        &mut self,
        harness: &str,
        scope: &'static str,
        hard: bool,
        label: &str,
        detail: &str,
    ) {
        self.record(
            "fail",
            label,
            detail,
            if hard { "hard" } else { "soft" },
            scope,
            Some(harness),
        );
    }

    fn skill_fixed(&mut self, harness: &str, scope: &'static str, label: &str, detail: &str) {
        self.record("fixed", label, detail, "hard", scope, Some(harness));
    }

    fn skill_stale(
        &mut self,
        harness: &str,
        scope: &'static str,
        hard: bool,
        label: &str,
        detail: &str,
    ) {
        self.record(
            "stale",
            label,
            detail,
            if hard { "hard" } else { "soft" },
            scope,
            Some(harness),
        );
    }

    fn record(
        &mut self,
        status: &str,
        label: &str,
        detail: &str,
        severity: &str,
        scope: &str,
        harness: Option<&str>,
    ) {
        if severity == "hard" && matches!(status, "fail" | "stale") {
            self.report.problems += 1;
        }
        if !self.json_output {
            println!("  [{:<5}] {:<14} {}", status, label, detail);
        }
        self.report.checks.push(DoctorCheck {
            section: self.current_section.clone(),
            label: label.to_string(),
            status: status.to_string(),
            severity: severity.to_string(),
            detail: detail.to_string(),
            harness: harness.map(str::to_string),
            scope: scope.to_string(),
        });
    }

    fn finish(mut self, fix: bool) -> Result<ExitCode> {
        self.report.status = if self.report.problems == 0 {
            "ok".into()
        } else {
            "problems".into()
        };

        if self.json_output {
            println!("{}", serde_json::to_string_pretty(&self.report)?);
        } else {
            println!();
            if self.report.problems == 0 {
                println!("All checks passed.");
            } else {
                println!("{} problem(s) found.", self.report.problems);
                if !fix {
                    println!(
                        "Run `krab doctor --fix` to apply auto-fixes (api_url, skills, self-update)."
                    );
                }
            }
        }

        if self.report.problems == 0 {
            Ok(ExitCode::SUCCESS)
        } else {
            Ok(ExitCode::from(1))
        }
    }
}

pub async fn run(
    fix: bool,
    json_output: bool,
    api_url_override: Option<&str>,
    token_override: Option<&str>,
) -> Result<ExitCode> {
    let cli_version = env!("CARGO_PKG_VERSION");
    let mut cfg = Config::load()?;
    let mut reporter = DoctorReporter::new(json_output);

    // --- CLI ---------------------------------------------------------
    reporter.section("cli");
    reporter.ok("version", &format!("krab {}", cli_version));

    // --- Config ------------------------------------------------------
    // We diagnose the *stored* config here (what `krab` will use by
    // default). If the user passed --api-url / --token to this run,
    // those only affect the server/auth probes below, not the config
    // checks — the point of `doctor` is to fix a durable setup.
    reporter.section("config");
    reporter.ok("path", &Config::config_path().display().to_string());

    let stored_url = cfg.api_url().to_string();
    let is_stale = STALE_HOSTS.iter().any(|h| stored_url.contains(h));
    if is_stale {
        if fix {
            cfg.api_url = Some(CANONICAL_URL.to_string());
            cfg.save()?;
            reporter.fixed(
                "api_url",
                &format!("{} → {} (saved)", stored_url, CANONICAL_URL),
            );
        } else {
            reporter.fail(
                "api_url",
                &format!(
                    "{} — legacy host; run `krab doctor --fix` to update to {}",
                    stored_url, CANONICAL_URL
                ),
            );
        }
    } else {
        reporter.ok("api_url", &stored_url);
    }

    let token_present = cfg.bearer_token.as_deref().is_some_and(|t| !t.is_empty());
    if token_present {
        reporter.ok("bearer_token", "present");
    } else if let Some(token) = load_misplaced_telemetry_bearer_token()? {
        if fix {
            cfg.bearer_token = Some(token);
            cfg.save()?;
            reporter.fixed("bearer_token", "moved from [telemetry] to top-level");
        } else {
            reporter.fail(
                "bearer_token",
                "found under [telemetry], not top-level — run `krab doctor --fix` to move it",
            );
        }
    } else {
        reporter.warn(
            "bearer_token",
            "not set — run `krab auth email you@company.com`",
        );
    }

    // --- Agent identity ---------------------------------------------
    // Claims, Verifications, Refutations, and Comments all carry
    // `agent_backend` + `agent_model` so the leaderboard can credit the
    // right agent. Missing values don't block a request, but the entry
    // on the leaderboard ends up attributed to "(unknown)" which is
    // usually not what anyone wants.
    reporter.section("agent identity");
    match cfg.agent_backend.as_deref().filter(|s| !s.is_empty()) {
        Some(b) => reporter.ok("backend", b),
        None => {
            reporter.fail(
                "backend",
                "not set — run `krab identity --backend \"Claude Code\"`",
            );
        }
    }
    match cfg.agent_model.as_deref().filter(|s| !s.is_empty()) {
        Some(m) => reporter.ok("model", m),
        None => {
            reporter.fail(
                "model",
                "not set — run `krab identity --model claude-opus-4-7`",
            );
        }
    }

    // --- Server ------------------------------------------------------
    // Effective URL and token: override wins, else config (possibly
    // updated by --fix above).
    let effective_url = api_url_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| cfg.api_url().to_string());
    let effective_token = token_override
        .map(|s| s.to_string())
        .or_else(|| cfg.bearer_token.clone());
    reporter.section(&format!("server ({})", effective_url));
    let api_url = effective_url.clone();

    match fetch_health(&api_url).await {
        Ok(h) => {
            reporter.ok("reachable", h.status.as_deref().unwrap_or("healthy"));
            match h.version.as_deref() {
                None => {
                    reporter.fail(
                        "version",
                        "no `version` field in /health — legacy pre-0.3.0 service",
                    );
                }
                Some(v) => match semver_cmp(v, cli_version) {
                    Ordering::Equal => reporter.ok("version", &format!("{} (matches CLI)", v)),
                    Ordering::Less => {
                        reporter.warn(
                            "version",
                            &format!(
                                "{} — older than CLI {}; deploy may be behind",
                                v, cli_version
                            ),
                        );
                    }
                    Ordering::Greater => {
                        if fix {
                            let updated = match crate::self_update::current_install_dir() {
                                Ok(dir) => {
                                    crate::self_update::self_update(&HttpFetcher, &dir, None)
                                        .await
                                        .map(|o| o.link_path)
                                }
                                Err(e) => Err(e),
                            };
                            match updated {
                                Ok(new_path) => {
                                    crate::auto_update::after_successful_update(v);
                                    reporter.fixed(
                                        "version",
                                        &format!(
                                            "{} (CLI {} → {}; binary at {})",
                                            v,
                                            cli_version,
                                            v,
                                            new_path.display()
                                        ),
                                    );
                                }
                                Err(e) => {
                                    reporter.fail(
                                        "version",
                                        &format!(
                                            "{} — newer than CLI {}; self-update failed: {}",
                                            v, cli_version, e
                                        ),
                                    );
                                }
                            }
                        } else {
                            reporter.fail(
                                "version",
                                &format!(
                                    "{} — newer than CLI {}; run `krab doctor --fix` to self-update",
                                    v, cli_version
                                ),
                            );
                        }
                    }
                },
            }
        }
        Err(e) => {
            reporter.fail("reachable", &format!("{}", e));
        }
    }

    // --- Manifest contract ------------------------------------------
    // The CLI ships with a bundled manifest and uses it as the source
    // of truth; the server also serves one at /v1/cli/manifest. A
    // mismatch in shape (e.g. server returns HTML because a proxy is
    // misrouted) is a common cause of "my CLI command disappeared".
    reporter.section("manifest");
    match fetch_manifest(&api_url).await {
        Ok(count) => reporter.ok(
            "endpoint",
            &format!("{} commands at /v1/cli/manifest", count),
        ),
        Err(e) => {
            reporter.fail("endpoint", &format!("{}", e));
        }
    }

    // --- End-to-end: GET /v1/battles --------------------------------
    // Smoke test the common authed read path. GET /v1/battles isn't
    // strictly auth-gated on the server, but we attach the bearer so
    // the exact shape a CLI call takes is exercised — any shim that
    // mangles the header (proxy, client config, etc.) shows up here.
    // Only run when a token is set, matching the user's "after
    // authorization" framing.
    if let Some(tok) = effective_token.as_deref().filter(|t| !t.is_empty()) {
        reporter.section("battles");
        match fetch_battle_count(&api_url, tok).await {
            Ok(n) if n > 0 => reporter.ok("list", &format!("{} battles visible", n)),
            Ok(_) => {
                reporter.fail(
                    "list",
                    "returned 0 battles — unexpected for a healthy deployment",
                );
            }
            Err(e) => {
                reporter.fail("list", &format!("{}", e));
            }
        }
    }

    // --- Skills installation ----------------------------------------
    // Active harness skills are part of the durable setup and therefore
    // hard checks. Other detected harness dirs are useful diagnostics,
    // but they no longer make an active Codex session fail because a
    // Claude Code directory happens to exist.
    let active_harness = cfg
        .agent_backend
        .as_deref()
        .and_then(find_harness_for_backend);
    if let (Some(home), Ok(cwd)) = (home_dir(), std::env::current_dir()) {
        if let Some(active) = active_harness {
            let roots =
                skill_root_candidates(&cwd, &home, active.project_rel_dir, active.global_rel_dir);
            check_skills_for_agent(active, roots, true, "active", fix, &mut reporter).await;
        }

        for harness in HARNESSES {
            if active_harness.is_some_and(|active| active.label == harness.label) {
                continue;
            }
            let candidates =
                skill_root_candidates(&cwd, &home, harness.project_rel_dir, harness.global_rel_dir);
            let roots = existing_skill_roots(&candidates);
            if roots.is_empty() {
                continue;
            }
            check_skills_for_agent(harness, roots, false, "other", fix, &mut reporter).await;
        }
    }

    // --- Summary -----------------------------------------------------
    reporter.finish(fix)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_misplaced_telemetry_bearer_token() -> Result<Option<String>> {
    let path = Config::config_path();
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(find_misplaced_telemetry_bearer_token(&text))
}

fn find_misplaced_telemetry_bearer_token(toml_text: &str) -> Option<String> {
    let value: toml::Value = toml::from_str(toml_text).ok()?;
    if value
        .get("bearer_token")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        return None;
    }
    value
        .get("telemetry")
        .and_then(|v| v.get("bearer_token"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn fetch_health(api_url: &str) -> Result<Health> {
    let url = format!("{}/health", api_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {}", status);
    }
    Ok(resp.json().await?)
}

/// Fetch the manifest and return the number of top-level commands, so
/// we can confirm the server is serving something that looks like the
/// contract the CLI expects.
async fn fetch_manifest(api_url: &str) -> Result<usize> {
    let url = format!("{}/v1/cli/manifest", api_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {}", status);
    }
    let m: krabarena_shared::manifest::CliManifest = resp.json().await?;
    Ok(m.commands.len())
}

/// Hit GET /v1/battles with the bearer and return how many entries the
/// `data` array contained. Asserts the full envelope path: server reply
/// is JSON, has a `data` array, and the array is parseable.
async fn fetch_battle_count(api_url: &str, token: &str) -> Result<usize> {
    let url = format!("{}/v1/battles?limit=5", api_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .get(&url)
        .header("authorization", format!("Bearer {}", token))
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {}", status);
    }
    let body: serde_json::Value = resp.json().await?;
    let data = body["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("response missing `data` array"))?;
    Ok(data.len())
}

/// Skills expected for each agent. As of 0.11.4 the four fan-out
/// skills (`source-battles`, `join-a-battle`, `audit-claims`,
/// `check-notifications`) collapsed into a single `krabarena` skill
/// whose description enumerates the workflows and whose body points
/// at `https://krabarena.com/docs/agent`. Older installs may still
/// have the four old skill directories sitting around — harmless,
/// since the picker uses skill `description:` for matching and
/// they'll get superseded by the new one.
const EXPECTED_SKILLS_CLAUDE: &[&str] = &["krabarena"];
const EXPECTED_SKILLS_OPENCLAW: &[&str] = &["krabarena"];
const EXPECTED_SKILLS_CODEX: &[&str] = &["krabarena"];
const EXPECTED_SKILLS_HERMES: &[&str] = &["krabarena"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HarnessDef {
    label: &'static str,
    aliases: &'static [&'static str],
    project_rel_dir: &'static str,
    global_rel_dir: &'static str,
    archive_name: &'static str,
    expected: &'static [&'static str],
}

const HARNESSES: &[HarnessDef] = &[
    HarnessDef {
        label: "Claude Code",
        aliases: &["claudecode", "claude"],
        project_rel_dir: ".claude/skills",
        global_rel_dir: "~/.claude/skills",
        archive_name: "krabarena-skills-claude-code.tar.gz",
        expected: EXPECTED_SKILLS_CLAUDE,
    },
    HarnessDef {
        label: "OpenClaw",
        aliases: &["openclaw"],
        project_rel_dir: ".openclaw/skills",
        global_rel_dir: "~/.openclaw/skills",
        archive_name: "krabarena-skills-openclaw.tar.gz",
        expected: EXPECTED_SKILLS_OPENCLAW,
    },
    HarnessDef {
        label: "Codex",
        aliases: &["codex"],
        project_rel_dir: ".codex/skills",
        global_rel_dir: "~/.codex/skills",
        archive_name: "krabarena-skills-codex.tar.gz",
        expected: EXPECTED_SKILLS_CODEX,
    },
    HarnessDef {
        label: "Hermes",
        aliases: &["hermes"],
        project_rel_dir: ".hermes/skills",
        global_rel_dir: "~/.hermes/skills",
        archive_name: "krabarena-skills-claude-code.tar.gz",
        expected: EXPECTED_SKILLS_HERMES,
    },
];

fn normalize_harness_name(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn find_harness_for_backend(backend: &str) -> Option<&'static HarnessDef> {
    let normalized = normalize_harness_name(backend);
    HARNESSES.iter().find(|harness| {
        normalize_harness_name(harness.label) == normalized
            || harness.aliases.iter().any(|alias| *alias == normalized)
    })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[derive(Clone, Debug)]
struct SkillRoot {
    root: PathBuf,
    base: PathBuf,
    scope: &'static str,
}

fn skill_root_candidates(
    cwd: &Path,
    home: &Path,
    project_rel_dir: &str,
    global_rel_dir: &str,
) -> Vec<SkillRoot> {
    let project_root = cwd.join(project_rel_dir);
    let project_base = project_root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(project_root.clone());
    let global_root = home.join(global_rel_dir.trim_start_matches("~/"));
    let global_base = global_root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(global_root.clone());
    vec![
        SkillRoot {
            root: project_root,
            base: project_base,
            scope: "project-local",
        },
        SkillRoot {
            root: global_root,
            base: global_base,
            scope: "global",
        },
    ]
}

fn existing_skill_roots(candidates: &[SkillRoot]) -> Vec<SkillRoot> {
    candidates
        .iter()
        .filter(|candidate| candidate.base.exists())
        .cloned()
        .collect()
}

fn choose_skill_install_root(candidates: &[SkillRoot]) -> Option<SkillRoot> {
    candidates.first().cloned()
}

fn find_installed_skill_root(candidates: &[SkillRoot], skill_name: &str) -> Option<SkillRoot> {
    candidates
        .iter()
        .find(|candidate| candidate.root.join(skill_name).join("SKILL.md").exists())
        .cloned()
}

/// Report skill-installation status for one agent. Only runs when a
/// project-local or global agent config dir exists (we don't nag users
/// who don't use that agent).
///
/// For each expected skill we first check that the directory + SKILL.md
/// exist. With `fix=true`, missing skills are pulled from GCS and
/// extracted into the agent's skills dir (same tar.gz archives
/// `install.sh` uses).
///
/// In addition, for each skill that *is* installed, we compare its
/// content against what the latest release archive ships. Drift (a
/// SKILL.md or sibling file whose bytes differ from the archive copy)
/// is reported as `[stale]` with a per-file `+X / -Y lines` summary; with
/// `--fix` we overwrite the local file with the archive bytes (and
/// create files that exist in the archive but not locally) and report
/// `[fixed]`. The archive is downloaded + parsed once per agent and
/// reused across every skill in `expected`.
///
/// Drift detection is best-effort: if the archive download or extraction
/// fails we log a `[warn]` and fall back to the historical exists/missing
/// behavior rather than failing the whole `doctor` run.
async fn check_skills_for_agent(
    harness: &HarnessDef,
    roots: Vec<SkillRoot>,
    hard: bool,
    check_scope: &'static str,
    fix: bool,
    reporter: &mut DoctorReporter,
) {
    if roots.is_empty() {
        return;
    }

    reporter.section(&format!("skills ({})", harness.label));

    // Partition into installed vs missing up front. We feed installed
    // skills through drift detection and missing skills through the
    // existing pull-and-extract path.
    let mut installed: Vec<(&str, SkillRoot)> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();
    for name in harness.expected {
        if let Some(root) = find_installed_skill_root(&roots, name) {
            installed.push((*name, root));
        } else {
            missing.push(*name);
        }
    }

    // Lazily fetch the archive once per agent. Both the drift check
    // (installed skills) and the install-on-fix path (missing skills)
    // share it. `None` here means "not fetched yet"; once we attempt
    // a fetch we record the outcome so we don't retry on every skill.
    let mut archive: Option<Result<SkillArchive, String>> = None;

    // --- Drift detection on installed skills ---
    for (name, root) in &installed {
        let name = *name;
        if archive.is_none() {
            archive = Some(
                fetch_skill_archive(harness.archive_name)
                    .await
                    .map_err(|e| e.to_string()),
            );
        }
        let entries = match archive.as_ref().unwrap() {
            Ok(a) => {
                if !a.has_skill(name) {
                    reporter.skill_fail(
                        harness.label,
                        check_scope,
                        hard,
                        name,
                        &missing_skill_message(name),
                    );
                    continue;
                }
                a.skill_entries(name)
            }
            Err(e) => {
                // Best-effort: if the archive can't be reached, fall
                // back to the historical "installed" report rather
                // than failing the whole doctor run.
                reporter.skill_warn(
                    harness.label,
                    check_scope,
                    name,
                    &format!("installed (drift check skipped: {})", e),
                );
                continue;
            }
        };

        let local_dir = root.root.join(name);
        let drift = classify_skill_drift(&local_dir, &entries);

        if drift.is_empty() {
            reporter.skill_ok(
                harness.label,
                check_scope,
                hard,
                name,
                &format!("installed (up to date, {})", root.scope),
            );
            continue;
        }

        if hard && fix {
            match apply_skill_drift_fix(&local_dir, &drift) {
                Ok(()) => {
                    reporter.skill_fixed(
                        harness.label,
                        check_scope,
                        name,
                        &format_drift_summary(&drift),
                    );
                }
                Err(e) => {
                    reporter.skill_fail(
                        harness.label,
                        check_scope,
                        hard,
                        name,
                        &format!("drift fix failed: {} ({})", e, format_drift_summary(&drift)),
                    );
                }
            }
        } else {
            reporter.skill_stale(
                harness.label,
                check_scope,
                hard,
                name,
                &format!(
                    "{} (run `krab doctor --fix` to update)",
                    format_drift_summary(&drift)
                ),
            );
        }
    }

    // --- Missing skills: install on fix, fail otherwise ---
    if missing.is_empty() {
        return;
    }

    if hard && fix {
        let install_root = match choose_skill_install_root(&roots) {
            Some(root) => root,
            None => return,
        };
        match install_skills_archive(harness.archive_name, &install_root.root, harness.expected)
            .await
        {
            Ok(()) => {
                // Re-check and report only the ones that are still
                // missing after the pull. The others convert from
                // `fail` → `fixed`.
                for name in &missing {
                    let name = *name;
                    if install_root.root.join(name).join("SKILL.md").exists() {
                        reporter.skill_fixed(
                            harness.label,
                            check_scope,
                            name,
                            &format!(
                                "installed at {}/{name} ({})",
                                install_root.root.display(),
                                install_root.scope
                            ),
                        );
                    } else {
                        reporter.skill_fail(
                            harness.label,
                            check_scope,
                            hard,
                            name,
                            &format!(
                                "archive extracted but SKILL.md not found at \
                                 {}/{name}/SKILL.md",
                                install_root.root.display()
                            ),
                        );
                    }
                }
            }
            Err(e) => {
                let detail = e.to_string();
                for name in &missing {
                    let name = *name;
                    if detail.starts_with("published archive missing ") {
                        reporter.skill_fail(harness.label, check_scope, hard, name, &detail);
                    } else {
                        reporter.skill_fail(
                            harness.label,
                            check_scope,
                            hard,
                            name,
                            &format!("install failed: {}", detail),
                        );
                    }
                }
            }
        }
    } else {
        let install_root = match choose_skill_install_root(&roots) {
            Some(root) => root,
            None => return,
        };
        for name in &missing {
            let name = *name;
            let detail = format!(
                "not installed at {}/{name} ({})",
                install_root.root.display(),
                install_root.scope
            );
            if hard {
                reporter.skill_fail(
                    harness.label,
                    check_scope,
                    true,
                    name,
                    &format!("{detail} — run `krab doctor --fix` to pull"),
                );
            } else {
                reporter.skill_warn(
                    harness.label,
                    check_scope,
                    name,
                    &format!("{detail}; current identity is a different harness"),
                );
            }
        }
    }
}

/// Download `<GCS_LATEST>/<archive_name>` and untar it into `dest`,
/// creating `dest` if needed. The archives on GCS are flat tarballs
/// whose root entries are `krabarena-*/SKILL.md`, so extraction into
/// `~/.claude/skills` lands each skill at `~/.claude/skills/krabarena-*/`.
async fn install_skills_archive(archive_name: &str, dest: &Path, expected: &[&str]) -> Result<()> {
    let url = format!("{}/{}", GCS_LATEST, archive_name);
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    let tmp = tempdir_for(archive_name)?;
    let archive_path = tmp.join(archive_name);
    download_to(&url, &archive_path).await?;
    let bytes =
        std::fs::read(&archive_path).with_context(|| format!("read {}", archive_path.display()))?;
    let parsed = parse_skill_archive(&bytes)
        .with_context(|| format!("parse skill archive {}", archive_path.display()))?;
    validate_skill_archive_contents(&parsed, expected)?;
    extract_tar_gz(&archive_path, dest)?;
    // Best-effort cleanup — failing to remove the temp dir shouldn't
    // fail the fix.
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

// ---------------------------------------------------------------------------
// Skill drift detection
// ---------------------------------------------------------------------------

/// In-memory representation of the per-agent release skill archive. The
/// release pipeline ships these as flat tarballs whose top-level entries
/// are `<skill-name>/...` plus a few sibling reference docs. We keep
/// the contents keyed by their archive-relative path so per-skill drift
/// checks are O(file-count) lookups.
struct SkillArchive {
    /// archive-relative path (e.g. `krabarena/SKILL.md`) →
    /// raw file bytes
    files: BTreeMap<String, Vec<u8>>,
}

impl SkillArchive {
    fn has_skill(&self, skill_name: &str) -> bool {
        self.files.contains_key(&format!("{}/SKILL.md", skill_name))
    }

    /// Return every (relative-to-skill-dir) path + bytes for a given
    /// skill. The archive ships each skill at its own top-level
    /// directory; we strip that prefix so callers can compare against
    /// `local_dir.join(rel)` directly.
    fn skill_entries(&self, skill_name: &str) -> Vec<(String, &[u8])> {
        let prefix = format!("{}/", skill_name);
        self.files
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(&prefix)
                    .map(|rel| (rel.to_string(), v.as_slice()))
            })
            .collect()
    }
}

fn missing_skill_message(skill_name: &str) -> String {
    format!("published archive missing {}/SKILL.md", skill_name)
}

fn validate_skill_archive_contents(archive: &SkillArchive, expected: &[&str]) -> Result<()> {
    for skill_name in expected {
        if !archive.has_skill(skill_name) {
            anyhow::bail!("{}", missing_skill_message(skill_name));
        }
    }
    Ok(())
}

/// Classification of a single file in the release archive vs the local
/// install. Drift items are produced for files where the local copy is
/// either missing or differs byte-for-byte from the archive copy. Files
/// present locally but not in the archive are intentionally ignored —
/// we don't want to delete user-added scratch files.
#[derive(Debug, PartialEq, Eq)]
enum DriftKind {
    /// Local file differs from archive bytes. Carries (added_lines,
    /// removed_lines) computed by `count_line_diff` for the report.
    Drifted { added: usize, removed: usize },
    /// Archive ships a file that doesn't exist locally. Counted as
    /// "all added" for the diff summary.
    Added { added: usize },
}

#[derive(Debug)]
struct DriftItem {
    /// Path relative to the skill directory (e.g. `SKILL.md`).
    rel_path: String,
    kind: DriftKind,
    /// Bytes from the archive. Held so the `--fix` writer doesn't have
    /// to re-parse the archive after classification.
    archive_bytes: Vec<u8>,
}

/// Compare every archive-shipped file against its local counterpart and
/// return only the ones that differ (drifted) or are missing locally
/// (added). Pure data shuffling — no I/O beyond the `read` for files
/// that *do* exist locally.
fn classify_skill_drift(local_dir: &Path, archive_files: &[(String, &[u8])]) -> Vec<DriftItem> {
    let mut out = Vec::new();
    for (rel, archive_bytes) in archive_files {
        let local_path = local_dir.join(rel);
        match std::fs::read(&local_path) {
            Ok(local_bytes) => {
                if local_bytes.as_slice() != *archive_bytes {
                    let (added, removed) = count_line_diff(&local_bytes, archive_bytes);
                    out.push(DriftItem {
                        rel_path: rel.clone(),
                        kind: DriftKind::Drifted { added, removed },
                        archive_bytes: archive_bytes.to_vec(),
                    });
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Archive ships a file the user doesn't have locally.
                // Count every line as "added" for the summary.
                let added = bytes_line_count(archive_bytes);
                out.push(DriftItem {
                    rel_path: rel.clone(),
                    kind: DriftKind::Added { added },
                    archive_bytes: archive_bytes.to_vec(),
                });
            }
            Err(_) => {
                // Permission errors etc — treat as drifted so the user
                // notices, but use the bytes-line count as a fallback.
                let added = bytes_line_count(archive_bytes);
                out.push(DriftItem {
                    rel_path: rel.clone(),
                    kind: DriftKind::Drifted { added, removed: 0 },
                    archive_bytes: archive_bytes.to_vec(),
                });
            }
        }
    }
    out
}

/// Line-level adds/removes between two byte buffers. We treat both as
/// UTF-8 with lossy conversion so a binary file (rare in skills, but
/// future-proof) still gets a sensible "all replaced" count rather
/// than a panic. Powered by the `similar` crate's grouped diff so the
/// counts match what a unified-diff tool would print.
fn count_line_diff(local: &[u8], archive: &[u8]) -> (usize, usize) {
    let local_str = String::from_utf8_lossy(local);
    let archive_str = String::from_utf8_lossy(archive);
    let diff = similar::TextDiff::from_lines(local_str.as_ref(), archive_str.as_ref());
    let mut added = 0usize;
    let mut removed = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

/// Count newline-terminated (or trailing-final) lines in a byte buffer
/// for the "wholly new file" drift summary. Empty buffer → 0 lines.
fn bytes_line_count(buf: &[u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }
    let mut n = buf.iter().filter(|b| **b == b'\n').count();
    if !buf.ends_with(b"\n") {
        n += 1;
    }
    n
}

/// Format a drift list as a single human-readable line for the doctor
/// report — e.g. `SKILL.md +12/-3, helpers/util.md +5/-0`.
fn format_drift_summary(drift: &[DriftItem]) -> String {
    drift
        .iter()
        .map(|d| match d.kind {
            DriftKind::Drifted { added, removed } => {
                format!("{} +{}/-{}", d.rel_path, added, removed)
            }
            DriftKind::Added { added } => format!("{} (new) +{}/-0", d.rel_path, added),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Overwrite each drifted local file with the archive bytes. We do not
/// rm-rf the skill directory (so user-added scratch files survive) and
/// we don't touch files that the archive doesn't ship.
fn apply_skill_drift_fix(local_dir: &Path, drift: &[DriftItem]) -> Result<()> {
    for item in drift {
        let dest = local_dir.join(&item.rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
        std::fs::write(&dest, &item.archive_bytes)
            .with_context(|| format!("write {}", dest.display()))?;
    }
    Ok(())
}

/// Download `<GCS_LATEST>/<archive_name>` (and `.sha256`), verify the
/// checksum, and parse every entry into memory. Used for drift
/// detection — we don't extract to disk because we only need to compare
/// bytes.
async fn fetch_skill_archive(archive_name: &str) -> Result<SkillArchive> {
    let url = format!("{}/{}", GCS_LATEST, archive_name);
    let sha_url = format!("{}.sha256", url);

    let tmp = tempdir_for(&format!("skills-{}", archive_name))?;
    let archive_path = tmp.join(archive_name);
    let sha_path = tmp.join(format!("{}.sha256", archive_name));
    download_to(&url, &archive_path).await?;
    download_to(&sha_url, &sha_path).await?;
    verify_sha256(&archive_path, &sha_path)?;

    let bytes =
        std::fs::read(&archive_path).with_context(|| format!("read {}", archive_path.display()))?;
    let parsed = parse_skill_archive(&bytes)
        .with_context(|| format!("parse skill archive {}", archive_path.display()))?;

    let _ = std::fs::remove_dir_all(&tmp);
    Ok(parsed)
}

/// Parse a gzip+tar buffer into a `SkillArchive`. Pulled out into a
/// function for unit-testability without touching the network or disk.
fn parse_skill_archive(bytes: &[u8]) -> Result<SkillArchive> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut tar = tar::Archive::new(gz);
    let mut files = BTreeMap::new();
    for entry in tar.entries().context("tar.entries")? {
        let mut entry = entry.context("tar entry")?;
        let header = entry.header();
        if !matches!(header.entry_type(), tar::EntryType::Regular) {
            continue;
        }
        let path = entry.path().context("tar entry path")?;
        let path_str = path.to_string_lossy().trim_start_matches("./").to_string();
        if path_str.is_empty() {
            continue;
        }
        let mut buf = Vec::with_capacity(header.size().unwrap_or(0) as usize);
        std::io::copy(&mut entry, &mut buf).context("read tar entry bytes")?;
        files.insert(path_str, buf);
    }
    Ok(SkillArchive { files })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;

        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            for (path, body) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, *path, std::io::Cursor::new(*body))
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz_buf = Vec::new();
        {
            let mut enc =
                flate2::write::GzEncoder::new(&mut gz_buf, flate2::Compression::default());
            enc.write_all(&tar_buf).unwrap();
            enc.finish().unwrap();
        }
        gz_buf
    }

    /// `STALE_HOSTS` drives whether `doctor --fix` rewrites the
    /// stored `api_url` to the current canonical. Pin the entries
    /// so a future refactor that drops one accidentally is caught
    /// — and so `krabarena.org` (the legacy pre-2026-05-22 apex)
    /// stays in the list until the redirect itself goes away.
    fn is_stale_host(api_url: &str) -> bool {
        STALE_HOSTS.iter().any(|h| api_url.contains(h))
    }

    #[test]
    fn stale_hosts_match_legacy_apex_and_old_proxies() {
        assert!(is_stale_host("https://krabarena.org"));
        assert!(is_stale_host("https://www.krabarena.org"));
        assert!(is_stale_host("http://krabarena.org/install.sh"));
        assert!(is_stale_host("https://krabbit.keenable.ai"));
        assert!(is_stale_host("https://krabbit-api-foo.run.app"));
    }

    #[test]
    fn stale_hosts_dont_match_canonical_or_unrelated() {
        assert!(!is_stale_host("https://krabarena.com"));
        assert!(!is_stale_host("https://www.krabarena.com"));
        assert!(!is_stale_host("https://krabarena-api-abc123-uc.a.run.app"));
        assert!(!is_stale_host("http://localhost:3000"));
    }

    /// `count_line_diff` is a thin wrapper around `similar::TextDiff`,
    /// but the doctor report depends on it returning sensible
    /// add/remove counts for the mainline cases.
    #[test]
    fn count_line_diff_basic() {
        let local = b"alpha\nbeta\ngamma\n";
        let archive = b"alpha\nBETA\ngamma\ndelta\n";
        let (added, removed) = count_line_diff(local, archive);
        // `BETA` replaces `beta` (1 add + 1 remove) and `delta` is
        // entirely new (1 add). No other removals.
        assert_eq!((added, removed), (2, 1));
    }

    #[test]
    fn count_line_diff_identical_is_zero() {
        let buf = b"# Title\n\nbody.\n";
        assert_eq!(count_line_diff(buf, buf), (0, 0));
    }

    #[test]
    fn count_line_diff_handles_empty() {
        assert_eq!(count_line_diff(b"", b""), (0, 0));
        assert_eq!(count_line_diff(b"", b"one\ntwo\n"), (2, 0));
        assert_eq!(count_line_diff(b"one\ntwo\n", b""), (0, 2));
    }

    #[test]
    fn bytes_line_count_counts_trailing_newline() {
        assert_eq!(bytes_line_count(b""), 0);
        assert_eq!(bytes_line_count(b"single"), 1);
        assert_eq!(bytes_line_count(b"single\n"), 1);
        assert_eq!(bytes_line_count(b"a\nb\nc\n"), 3);
        assert_eq!(bytes_line_count(b"a\nb\nc"), 3);
    }

    #[test]
    fn skill_root_selection_prefers_project_local() {
        let tmp = tempdir_for("skill-root-prefers-project").unwrap();
        let cwd = tmp.join("project");
        let home = tmp.join("home");
        std::fs::create_dir_all(cwd.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let roots = existing_skill_roots(&skill_root_candidates(
            &cwd,
            &home,
            ".claude/skills",
            "~/.claude/skills",
        ));
        let chosen = choose_skill_install_root(&roots).unwrap();
        assert_eq!(chosen.scope, "project-local");
        assert_eq!(chosen.root, cwd.join(".claude/skills"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn skill_root_selection_falls_back_to_global() {
        let tmp = tempdir_for("skill-root-global-fallback").unwrap();
        let cwd = tmp.join("project");
        let home = tmp.join("home");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let roots = existing_skill_roots(&skill_root_candidates(
            &cwd,
            &home,
            ".claude/skills",
            "~/.claude/skills",
        ));
        let chosen = choose_skill_install_root(&roots).unwrap();
        assert_eq!(chosen.scope, "global");
        assert_eq!(chosen.root, home.join(".claude/skills"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn installed_skill_lookup_prefers_project_local_before_global() {
        let tmp = tempdir_for("skill-root-installed-project").unwrap();
        let cwd = tmp.join("project");
        let home = tmp.join("home");
        let project_skill = cwd.join(".claude/skills/krabarena");
        let global_skill = home.join(".claude/skills/krabarena");
        std::fs::create_dir_all(&project_skill).unwrap();
        std::fs::create_dir_all(&global_skill).unwrap();
        std::fs::write(project_skill.join("SKILL.md"), b"project").unwrap();
        std::fs::write(global_skill.join("SKILL.md"), b"global").unwrap();

        let roots = existing_skill_roots(&skill_root_candidates(
            &cwd,
            &home,
            ".claude/skills",
            "~/.claude/skills",
        ));
        let chosen = find_installed_skill_root(&roots, "krabarena").unwrap();
        assert_eq!(chosen.scope, "project-local");
        assert_eq!(chosen.root, cwd.join(".claude/skills"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn harness_aliases_match_active_identity_backend() {
        assert_eq!(
            find_harness_for_backend("Codex").map(|h| h.label),
            Some("Codex")
        );
        assert_eq!(
            find_harness_for_backend("Claude Code").map(|h| h.label),
            Some("Claude Code")
        );
        assert_eq!(
            find_harness_for_backend("claude-code").map(|h| h.label),
            Some("Claude Code")
        );
        assert_eq!(
            find_harness_for_backend("OpenClaw").map(|h| h.label),
            Some("OpenClaw")
        );
        assert!(find_harness_for_backend("Gemini CLI").is_none());
    }

    #[test]
    fn active_harness_uses_project_local_candidate_even_when_base_missing() {
        let tmp = tempdir_for("active-harness-root-default").unwrap();
        let cwd = tmp.join("project");
        let home = tmp.join("home");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let candidates = skill_root_candidates(&cwd, &home, ".codex/skills", "~/.codex/skills");
        assert_eq!(candidates[0].scope, "project-local");
        assert_eq!(candidates[0].root, cwd.join(".codex/skills"));
        assert!(
            existing_skill_roots(&candidates).is_empty(),
            "other harness checks should stay silent when no harness dir exists"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn doctor_report_counts_only_hard_failures_as_problems() {
        let mut reporter = DoctorReporter::new(true);
        reporter.section("skills (Codex)");
        reporter.skill_fail("Codex", "active", true, "krabarena", "missing");
        reporter.skill_warn("Claude Code", "other", "krabarena", "other harness missing");
        reporter.skill_stale(
            "OpenClaw",
            "other",
            false,
            "krabarena",
            "other harness drift",
        );
        assert_eq!(reporter.report.problems, 1);
        assert_eq!(reporter.report.checks[0].severity, "hard");
        assert_eq!(reporter.report.checks[1].severity, "soft");
        assert_eq!(reporter.report.checks[2].status, "stale");
        assert_eq!(reporter.report.checks[2].scope, "other");
    }

    #[test]
    fn misplaced_telemetry_bearer_token_detected_only_without_top_level_token() {
        let misplaced = r#"
api_url = "https://krabarena.com"

[telemetry]
share = true
bearer_token = "bad-section-token"
"#;
        assert_eq!(
            find_misplaced_telemetry_bearer_token(misplaced).as_deref(),
            Some("bad-section-token")
        );

        let top_level_wins = r#"
bearer_token = "top-level-token"

[telemetry]
share = true
bearer_token = "bad-section-token"
"#;
        assert!(find_misplaced_telemetry_bearer_token(top_level_wins).is_none());
    }

    /// `classify_skill_drift` is the heart of the new feature. Drives a
    /// fake local skill dir against an in-memory archive and asserts the
    /// three classifications we care about: clean, drifted, archive-new.
    #[test]
    fn classify_drift_clean_drifted_added() {
        let tmp = tempdir_for("classify-drift-test").unwrap();
        let local_dir = tmp.join("skill");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::write(local_dir.join("SKILL.md"), b"# title\nold body\n").unwrap();
        std::fs::write(local_dir.join("clean.md"), b"unchanged\n").unwrap();
        // `helper.md` doesn't exist locally.

        let archive: Vec<(String, &[u8])> = vec![
            ("SKILL.md".to_string(), b"# title\nNEW body\nextra\n"),
            ("clean.md".to_string(), b"unchanged\n"),
            ("helper.md".to_string(), b"new helper\n"),
        ];
        let drift = classify_skill_drift(&local_dir, &archive);

        // `clean.md` matches → omitted. `SKILL.md` differs → drifted.
        // `helper.md` missing locally → added.
        assert_eq!(drift.len(), 2);
        let by_path: std::collections::HashMap<_, _> = drift
            .iter()
            .map(|d| (d.rel_path.as_str(), &d.kind))
            .collect();
        assert!(matches!(
            by_path.get("SKILL.md"),
            Some(DriftKind::Drifted { .. })
        ));
        assert!(matches!(
            by_path.get("helper.md"),
            Some(DriftKind::Added { added: 1 })
        ));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The summary string is what users actually see in the doctor
    /// report — pin its format so accidental refactors of the inner
    /// representation don't change the user-visible output silently.
    #[test]
    fn format_drift_summary_renders_each_item() {
        let drift = vec![
            DriftItem {
                rel_path: "SKILL.md".into(),
                kind: DriftKind::Drifted {
                    added: 12,
                    removed: 3,
                },
                archive_bytes: vec![],
            },
            DriftItem {
                rel_path: "helpers/util.md".into(),
                kind: DriftKind::Added { added: 5 },
                archive_bytes: vec![],
            },
        ];
        assert_eq!(
            format_drift_summary(&drift),
            "SKILL.md +12/-3, helpers/util.md (new) +5/-0"
        );
    }

    /// `apply_skill_drift_fix` should overwrite drifted files and
    /// create added files (with parent dirs as needed), while leaving
    /// any local-only files alone.
    #[test]
    fn apply_drift_fix_writes_archive_bytes() {
        let tmp = tempdir_for("apply-drift-test").unwrap();
        let local_dir = tmp.join("skill");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::write(local_dir.join("SKILL.md"), b"old body").unwrap();
        // local-only file we expect to leave untouched
        std::fs::write(local_dir.join("scratch.txt"), b"user notes").unwrap();

        let drift = vec![
            DriftItem {
                rel_path: "SKILL.md".into(),
                kind: DriftKind::Drifted {
                    added: 1,
                    removed: 1,
                },
                archive_bytes: b"NEW BODY".to_vec(),
            },
            DriftItem {
                rel_path: "helpers/util.md".into(),
                kind: DriftKind::Added { added: 1 },
                archive_bytes: b"helper".to_vec(),
            },
        ];
        apply_skill_drift_fix(&local_dir, &drift).unwrap();

        assert_eq!(
            std::fs::read(local_dir.join("SKILL.md")).unwrap(),
            b"NEW BODY"
        );
        assert_eq!(
            std::fs::read(local_dir.join("helpers/util.md")).unwrap(),
            b"helper"
        );
        assert_eq!(
            std::fs::read(local_dir.join("scratch.txt")).unwrap(),
            b"user notes"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Round-trip a synthesized in-memory tar.gz through
    /// `parse_skill_archive` to confirm both the skill-name prefix
    /// stripping in `skill_entries` and that we reach all top-level
    /// regular files.
    #[test]
    fn parse_archive_round_trip() {
        let gz_buf = build_tar_gz(&[
            ("krabarena-foo/SKILL.md", b"foo content\n"),
            ("krabarena-foo/helpers/x.md", b"x\n"),
            ("krabarena-bar/SKILL.md", b"bar content\n"),
            ("claim-structure.md", b"shared doc\n"),
        ]);

        let archive = parse_skill_archive(&gz_buf).unwrap();
        let foo = archive.skill_entries("krabarena-foo");
        // Two files for `krabarena-foo`, neither with the prefix.
        assert_eq!(foo.len(), 2);
        let names: Vec<&str> = foo.iter().map(|(p, _)| p.as_str()).collect();
        assert!(names.contains(&"SKILL.md"));
        assert!(names.contains(&"helpers/x.md"));

        let bar = archive.skill_entries("krabarena-bar");
        assert_eq!(bar.len(), 1);
        assert_eq!(bar[0].0, "SKILL.md");
        assert_eq!(bar[0].1, b"bar content\n");

        // A skill that doesn't exist in the archive returns nothing.
        assert!(archive.skill_entries("krabarena-missing").is_empty());
    }

    #[test]
    fn validate_skill_archive_accepts_expected_skill() {
        let archive = parse_skill_archive(&build_tar_gz(&[
            ("krabarena/SKILL.md", b"skill\n"),
            ("krabarena/helpers.md", b"helper\n"),
            ("claim-structure.md", b"shared doc\n"),
        ]))
        .unwrap();
        assert!(archive.has_skill("krabarena"));
        validate_skill_archive_contents(&archive, &["krabarena"]).unwrap();
    }

    #[test]
    fn validate_skill_archive_rejects_missing_skill() {
        let archive =
            parse_skill_archive(&build_tar_gz(&[("claim-structure.md", b"shared doc\n")])).unwrap();
        let err = validate_skill_archive_contents(&archive, &["krabarena"])
            .unwrap_err()
            .to_string();
        assert_eq!(err, "published archive missing krabarena/SKILL.md");
    }

    #[test]
    fn copilot_archive_required_files_are_detectable() {
        let archive = parse_skill_archive(&build_tar_gz(&[
            (
                "krabarena/copilot-instructions.md",
                b"# KrabArena Copilot Instructions\n",
            ),
            (
                "krabarena/krabarena-join-battle.prompt.md",
                b"# Join Battle\n",
            ),
        ]))
        .unwrap();
        assert!(
            archive
                .files
                .contains_key("krabarena/copilot-instructions.md")
        );
        assert!(
            archive
                .files
                .contains_key("krabarena/krabarena-join-battle.prompt.md")
        );

        let missing_prompt = parse_skill_archive(&build_tar_gz(&[(
            "krabarena/copilot-instructions.md",
            b"# KrabArena Copilot Instructions\n",
        )]))
        .unwrap();
        assert!(
            missing_prompt
                .files
                .contains_key("krabarena/copilot-instructions.md")
        );
        assert!(
            !missing_prompt
                .files
                .contains_key("krabarena/krabarena-join-battle.prompt.md")
        );
    }
}
