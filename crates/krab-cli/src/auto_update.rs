//! Default-on self-update: when the server runs a newer KrabArena
//! version than this binary, download the latest release, swap it in
//! (see `crate::self_update`) and re-exec the new binary with the same
//! argv/env — all before the user's command runs, so new commands and
//! flags work immediately.
//!
//! Runs on raw argv *before* clap parses: a command that only exists in
//! the newer manifest would otherwise fail to parse and we'd never get
//! the chance to update.
//!
//! Throttling: the server version is checked over the network (2.5s
//! timeout on `/health`) at most once per 24h; the result is cached in
//! `~/.krab/update-check.json`. A newer `X-Krab-Server-Version` response
//! header seen by any command is also recorded there, so the next
//! invocation updates without waiting for the window. Failed updates
//! back off for 24h (1h when the release bucket simply hasn't caught up
//! with the server deploy yet) — but only for the version that failed:
//! once the server moves past it, the next command tries again.
//!
//! Concurrency: agents often run several `krab` commands at once. The
//! download+swap is serialized with an exclusive lock on
//! `~/.krab/update.lock`; a process that finds the `krab` symlink
//! already pointing at the target (another run won) just re-execs it,
//! and a failure never arms the backoff when the install is in fact
//! up to date. (Before 0.43.8 a losing concurrent run armed a 24h
//! backoff that then also blocked every later version — the CLI sat on
//! an old build for hours while each response said the server was newer.)
//!
//! Safety:
//!   - never writes to stdout (keeps `--format json` clean)
//!   - any failure → one stderr warning pointing at `krab doctor --fix`,
//!     then the original command continues on the old binary
//!   - the re-exec'd process carries `KRAB_UPDATED_FROM=<old>` and never
//!     tries again (no loops)
//!
//! Opt-outs: `KRAB_NO_AUTO_UPDATE=1`, `auto_update = false` in
//! `~/.krab/config.toml`, or `CI=true` (unless `KRAB_AUTO_UPDATE=1`
//! forces it). Skipped for `--version`, `--help`/`help`, `doctor` (has
//! its own `--fix`), and `auth *`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::self_update::{self, HttpFetcher, ReleaseNotNewer, semver_cmp};

const LOCAL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set on the re-exec'd process; its presence disables auto-update.
pub const ENV_UPDATED_FROM: &str = "KRAB_UPDATED_FROM";
/// `KRAB_NO_AUTO_UPDATE=1` disables auto-update.
pub const ENV_NO_AUTO_UPDATE: &str = "KRAB_NO_AUTO_UPDATE";
/// `KRAB_AUTO_UPDATE=1` forces auto-update on (overrides `CI` and the
/// config key, but not `KRAB_NO_AUTO_UPDATE`).
pub const ENV_FORCE_AUTO_UPDATE: &str = "KRAB_AUTO_UPDATE";

const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const FAILURE_BACKOFF_SECS: u64 = 24 * 60 * 60;
const LAGGING_RELEASE_BACKOFF_SECS: u64 = 60 * 60;
const HEALTH_TIMEOUT: Duration = Duration::from_millis(2500);

/// Subcommands that never trigger an auto-update.
const SKIPPED_COMMANDS: &[&str] = &["help", "doctor", "auth"];
/// How long to wait for a concurrent auto-update to release the lock
/// before giving up on the lock and proceeding anyway.
const LOCK_WAIT: Duration = Duration::from_secs(150);

/// Global flags that consume the following argv entry.
const GLOBAL_FLAGS_WITH_VALUE: &[&str] = &["--api-url", "--token", "--format"];

/// Persisted throttle state (`~/.krab/update-check.json`).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Unix seconds of the last `/health` version check (success or not).
    #[serde(default)]
    pub last_check: Option<u64>,
    /// Latest server version observed (from `/health` or a response header).
    #[serde(default)]
    pub server_version: Option<String>,
    /// Unix seconds before which no check/update is attempted (failure backoff).
    #[serde(default)]
    pub retry_after: Option<u64>,
    /// The version whose update failed and armed `retry_after`. The
    /// backoff only applies while the server is still on this version
    /// (absent = legacy state: the backoff applies to every version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_version: Option<String>,
}

/// Environment inputs to the enable/disable decision.
#[derive(Debug, Default, Clone)]
pub struct EnvFlags {
    pub no_auto_update: Option<String>,
    pub force: Option<String>,
    pub ci: Option<String>,
    pub updated_from: Option<String>,
}

impl EnvFlags {
    pub fn from_process() -> Self {
        let get = |k: &str| std::env::var(k).ok();
        Self {
            no_auto_update: get(ENV_NO_AUTO_UPDATE),
            force: get(ENV_FORCE_AUTO_UPDATE),
            ci: get("CI"),
            updated_from: get(ENV_UPDATED_FROM),
        }
    }
}

fn truthy(v: &Option<String>) -> bool {
    v.as_deref().is_some_and(|s| {
        matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Why auto-update is off for this invocation, if it is.
#[derive(Debug, PartialEq, Eq)]
pub enum Disabled {
    /// We are the freshly re-exec'd binary.
    ReExecGuard,
    EnvOptOut,
    ConfigOptOut,
    Ci,
}

pub fn disabled_reason(env: &EnvFlags, config_auto_update: Option<bool>) -> Option<Disabled> {
    if env.updated_from.as_deref().is_some_and(|s| !s.is_empty()) {
        return Some(Disabled::ReExecGuard);
    }
    if truthy(&env.no_auto_update) {
        return Some(Disabled::EnvOptOut);
    }
    if truthy(&env.force) {
        return None;
    }
    if config_auto_update == Some(false) {
        return Some(Disabled::ConfigOptOut);
    }
    if truthy(&env.ci) {
        return Some(Disabled::Ci);
    }
    None
}

/// Whether this argv (without argv[0]) may trigger an auto-update.
pub fn command_eligible(args: &[String]) -> bool {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--help" | "-h" | "--version" | "-V"))
    {
        return false;
    }
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if GLOBAL_FLAGS_WITH_VALUE.contains(&a) {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        return !SKIPPED_COMMANDS.contains(&a);
    }
    // No subcommand → clap prints help.
    false
}

/// What to do before running the command.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Skip,
    CheckServer,
    Update { server_version: String },
}

fn newer_than_local(server: Option<&str>, local: &str) -> Option<String> {
    server
        .filter(|s| semver_cmp(s, local) == std::cmp::Ordering::Greater)
        .map(str::to_string)
}

pub fn plan(state: &State, now: u64, local: &str) -> Plan {
    let target = newer_than_local(state.server_version.as_deref(), local);
    if state.retry_after.is_some_and(|t| now < t) {
        // The backoff is scoped to the version that failed: a server
        // that has since moved past it deserves a fresh attempt.
        let moved_past_failure = matches!(
            (target.as_deref(), state.failed_version.as_deref()),
            (Some(t), Some(f)) if semver_cmp(t, f) == std::cmp::Ordering::Greater
        );
        if !moved_past_failure {
            return Plan::Skip;
        }
    }
    // A newer server seen in any earlier response (or `/health`) wins
    // over the 24h `/health` throttle: update right now.
    if let Some(v) = target {
        return Plan::Update { server_version: v };
    }
    // A last_check in the future (clock went backwards) counts as stale.
    let fresh = state
        .last_check
        .is_some_and(|t| t <= now && now - t < CHECK_INTERVAL_SECS);
    if fresh { Plan::Skip } else { Plan::CheckServer }
}

/// Record the outcome of a `/health` check. A failed check still stamps
/// `last_check` so an unreachable server costs at most one timeout/day.
pub fn record_check(state: &mut State, now: u64, fetched: Option<String>) {
    state.last_check = Some(now);
    if let Some(v) = fetched {
        state.server_version = Some(v);
    }
}

/// Fold a server version seen in an API response header into the
/// state. Only ever raises `server_version`, and only for a server
/// newer than this binary; returns whether anything changed.
pub fn observe_server_version(state: &mut State, server: &str, local: &str) -> bool {
    if semver_cmp(server, local) != std::cmp::Ordering::Greater {
        return false;
    }
    let raises = state
        .server_version
        .as_deref()
        .is_none_or(|known| semver_cmp(server, known) == std::cmp::Ordering::Greater);
    if raises {
        state.server_version = Some(server.to_string());
    }
    raises
}

/// Arm the failure backoff for `target`.
pub fn record_failure(state: &mut State, now: u64, target: &str, backoff: u64) {
    state.retry_after = Some(now + backoff);
    state.failed_version = Some(target.to_string());
}

/// Version the install's `krab` symlink currently points at
/// (`krab -> krab-0.43.8` → `0.43.8`). `None` for a legacy
/// regular-file install or anything unparseable.
pub fn installed_link_version(install_dir: &Path) -> Option<String> {
    let target = std::fs::read_link(install_dir.join("krab")).ok()?;
    let name = target.file_name()?.to_str()?;
    let v = name.strip_prefix("krab-")?;
    let valid = !v.is_empty()
        && v.split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    valid.then(|| v.to_string())
}

/// True when the installed `krab` is already at (or past) `target` —
/// i.e. a concurrent run finished the update for us.
fn already_installed(install_dir: &Path, target: &str) -> bool {
    installed_link_version(install_dir)
        .is_some_and(|v| semver_cmp(&v, target) != std::cmp::Ordering::Less)
}

pub fn backoff_secs(err: &anyhow::Error) -> u64 {
    if err.downcast_ref::<ReleaseNotNewer>().is_some() {
        LAGGING_RELEASE_BACKOFF_SECS
    } else {
        FAILURE_BACKOFF_SECS
    }
}

/// Install locations we must not rewrite. `Silent` = developer builds
/// (`cargo run`), `Warn` = package-manager-owned paths.
#[derive(Debug, PartialEq, Eq)]
pub enum Managed {
    Silent,
    Warn(&'static str),
}

pub fn managed_install(dir: &Path) -> Option<Managed> {
    let s = dir.to_string_lossy();
    if s.contains("/target/debug") || s.contains("/target/release") {
        return Some(Managed::Silent);
    }
    if s.contains("/Cellar/") || s.contains("/homebrew/") || s.contains("/linuxbrew/") {
        return Some(Managed::Warn("installed by Homebrew"));
    }
    if s.starts_with("/nix/store/") {
        return Some(Managed::Warn("installed by Nix"));
    }
    None
}

fn state_path() -> PathBuf {
    Config::config_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("update-check.json")
}

pub fn load_state(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_state(path: &Path, state: &State) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, s);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Called by the HTTP client when a response advertises a newer server.
pub fn record_seen_server_version(server: &str) {
    let path = state_path();
    let mut state = load_state(&path);
    if observe_server_version(&mut state, server, LOCAL_VERSION) {
        save_state(&path, &state);
    }
}

/// Bookkeeping after any successful binary update (auto or
/// `doctor --fix`): clear the failure backoff and drop the cached
/// server manifest so the new binary's command set is what gets shown.
pub fn after_successful_update(server_version: &str) {
    let path = state_path();
    let mut state = load_state(&path);
    state.retry_after = None;
    state.failed_version = None;
    observe_server_version(&mut state, server_version, "0");
    save_state(&path, &state);
    crate::manifest::invalidate_cache();
}

async fn fetch_server_version(api_url: &str) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Health {
        version: Option<String>,
    }
    let http = reqwest::Client::builder()
        .use_rustls_tls()
        .tls_built_in_webpki_certs(true)
        .tls_built_in_native_certs(true)
        .connect_timeout(HEALTH_TIMEOUT)
        .timeout(HEALTH_TIMEOUT)
        .build()?;
    let url = format!("{}/health", api_url.trim_end_matches('/'));
    let h: Health = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    h.version
        .ok_or_else(|| anyhow::anyhow!("/health has no version"))
}

fn pre_parse_api_url(args: &[String]) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if let Some(rest) = a.strip_prefix("--api-url=") {
            return Some(rest.to_string());
        }
        if a == "--api-url" {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// Exclusive, process-wide lock on `~/.krab/update.lock`, so concurrent
/// `krab` runs don't download and swap at the same time (they used to
/// collide on the staging names and the loser armed a 24h backoff).
/// Released when the returned file is dropped or the process execs
/// (std opens files `O_CLOEXEC`). `None` = couldn't lock in time;
/// the caller proceeds unlocked (the swap itself is still atomic).
async fn acquire_update_lock(state_path: &Path) -> Option<std::fs::File> {
    let lock_path = state_path.with_file_name("update.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .ok()?;
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match try_lock_exclusive(&file) {
            Some(true) => return Some(file),
            Some(false) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            _ => return None,
        }
    }
}

/// Non-blocking exclusive `flock`. `Some(true)` = locked, `Some(false)`
/// = held by another process, `None` = locking unsupported/failed.
/// (`File::try_lock` would do, but it needs Rust 1.89; MSRV is 1.85.)
#[cfg(unix)]
fn try_lock_exclusive(file: &std::fs::File) -> Option<bool> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: plain syscall on a valid, owned file descriptor.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Some(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EWOULDBLOCK) => Some(false),
        _ => None,
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(_file: &std::fs::File) -> Option<bool> {
    None
}

fn warn_failed(target: &str, why: &str) {
    eprintln!(
        "\u{26a0} krab auto-update to {target} skipped: {why}. Run `krab doctor --fix` to update; continuing with {LOCAL_VERSION}."
    );
    crate::client::mark_version_warning_shown();
}

/// Entry point, called first thing in `main`. Returns normally when no
/// update happened (or it failed); on success the process is replaced
/// by the new binary and this never returns.
pub async fn run_before_command(argv: &[OsString]) {
    let args: Vec<String> = argv
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if !command_eligible(&args) {
        return;
    }
    let env = EnvFlags::from_process();
    let cfg = Config::load().unwrap_or_default();
    if disabled_reason(&env, cfg.auto_update).is_some() {
        return;
    }
    let forced = truthy(&env.force);
    let install_dir = self_update::current_install_dir();
    // Developer builds (`cargo run`, integration tests) never probe the
    // network or touch anything.
    if !forced
        && let Ok(dir) = &install_dir
        && managed_install(dir) == Some(Managed::Silent)
    {
        return;
    }

    let path = state_path();
    let mut state = load_state(&path);
    let now = unix_now();
    let target = match plan(&state, now, LOCAL_VERSION) {
        Plan::Skip => return,
        Plan::Update { server_version } => server_version,
        Plan::CheckServer => {
            let api_url = pre_parse_api_url(&args).unwrap_or_else(|| cfg.api_url().to_string());
            let fetched = fetch_server_version(&api_url).await.ok();
            record_check(&mut state, now, fetched);
            save_state(&path, &state);
            match newer_than_local(state.server_version.as_deref(), LOCAL_VERSION) {
                Some(v) => v,
                None => return,
            }
        }
    };

    // Re-read the state before writing so a concurrent run's newer
    // `server_version` / cleared backoff isn't clobbered by our stale copy.
    let fail = |why: String, backoff: u64| {
        let mut fresh = load_state(&path);
        record_failure(&mut fresh, now, &target, backoff);
        save_state(&path, &fresh);
        warn_failed(&target, &why);
    };
    drop(state);

    let install_dir = match install_dir {
        Ok(d) => d,
        Err(e) => return fail(e.to_string(), FAILURE_BACKOFF_SECS),
    };
    match managed_install(&install_dir) {
        Some(Managed::Warn(why)) if !forced => {
            return fail(
                format!("{} ({})", why, install_dir.display()),
                FAILURE_BACKOFF_SECS,
            );
        }
        _ => {}
    }

    let _lock = acquire_update_lock(&path).await;
    // A concurrent run may have finished the update while we waited.
    if already_installed(&install_dir, &target) {
        after_successful_update(&target);
        reexec(&install_dir.join("krab"), argv);
        return;
    }

    match self_update::self_update(&HttpFetcher, &install_dir, Some(LOCAL_VERSION)).await {
        Ok(outcome) => {
            after_successful_update(&outcome.new_version);
            eprintln!(
                "krab updated {} \u{2192} {}",
                LOCAL_VERSION, outcome.new_version
            );
            reexec(&outcome.link_path, argv);
        }
        Err(e) => {
            // Lost a race we couldn't serialize (lock timed out)? If the
            // install is up to date anyway, use it instead of backing off.
            if already_installed(&install_dir, &target) {
                after_successful_update(&target);
                reexec(&install_dir.join("krab"), argv);
                return;
            }
            let backoff = backoff_secs(&e);
            fail(format!("{e:#}"), backoff);
        }
    }
}

/// Replace this process with the new binary, same argv + env plus the
/// loop guard. Only returns if `exec` failed, in which case the old
/// binary carries on with the command.
fn reexec(binary: &Path, argv: &[OsString]) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(binary)
            .args(argv.iter().skip(1))
            .env(ENV_UPDATED_FROM, LOCAL_VERSION)
            .exec();
        eprintln!(
            "\u{26a0} could not start the updated krab ({err}); running this command with {LOCAL_VERSION}."
        );
    }
    #[cfg(not(unix))]
    {
        let _ = (binary, argv);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn env(pairs: &[(&str, &str)]) -> EnvFlags {
        let mut e = EnvFlags::default();
        for (k, v) in pairs {
            let v = Some(v.to_string());
            match *k {
                "no" => e.no_auto_update = v,
                "force" => e.force = v,
                "ci" => e.ci = v,
                "from" => e.updated_from = v,
                _ => unreachable!(),
            }
        }
        e
    }

    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn plan_updates_only_when_server_is_newer() {
        let now = 10 * DAY;
        let fresh = |v: &str| State {
            last_check: Some(now - 60),
            server_version: Some(v.into()),
            retry_after: None,
            failed_version: None,
        };
        assert_eq!(
            plan(&fresh("0.43.2"), now, "0.43.0"),
            Plan::Update {
                server_version: "0.43.2".into()
            }
        );
        assert_eq!(plan(&fresh("0.43.0"), now, "0.43.0"), Plan::Skip);
        assert_eq!(plan(&fresh("0.42.9"), now, "0.43.0"), Plan::Skip);
        // Numeric, not lexicographic.
        assert_eq!(
            plan(&fresh("0.43.10"), now, "0.43.9"),
            Plan::Update {
                server_version: "0.43.10".into()
            }
        );
    }

    #[test]
    fn plan_throttles_network_checks_to_once_per_day() {
        let now = 10 * DAY;
        let st = |last: Option<u64>| State {
            last_check: last,
            server_version: Some("0.43.0".into()),
            retry_after: None,
            failed_version: None,
        };
        assert_eq!(plan(&st(None), now, "0.43.0"), Plan::CheckServer);
        assert_eq!(plan(&st(Some(now - DAY + 1)), now, "0.43.0"), Plan::Skip);
        assert_eq!(plan(&st(Some(now - DAY)), now, "0.43.0"), Plan::CheckServer);
        // Clock skew: a future timestamp is treated as stale.
        assert_eq!(plan(&st(Some(now + 100)), now, "0.43.0"), Plan::CheckServer);
    }

    #[test]
    fn plan_respects_failure_backoff() {
        let now = 10 * DAY;
        let mut st = State {
            last_check: Some(now - 2 * DAY),
            server_version: Some("0.44.0".into()),
            retry_after: Some(now + 10),
            failed_version: None,
        };
        assert_eq!(plan(&st, now, "0.43.0"), Plan::Skip);
        st.retry_after = Some(now);
        assert_eq!(
            plan(&st, now, "0.43.0"),
            Plan::Update {
                server_version: "0.44.0".into()
            }
        );
    }

    /// Regression (0.43.8): prod moved 0.43.3 → 0.43.7 while the CLI's
    /// last `/health` check was minutes old. A response header showing
    /// the newer server must make the very next run update, without
    /// waiting out the 24h `/health` window.
    #[test]
    fn newer_header_triggers_update_inside_check_window() {
        let now = 10 * DAY;
        let mut st = State {
            last_check: Some(now - 60),
            server_version: Some("0.43.3".into()),
            ..State::default()
        };
        assert_eq!(plan(&st, now, "0.43.3"), Plan::Skip);
        assert!(observe_server_version(&mut st, "0.43.7", "0.43.3"));
        assert_eq!(st.last_check, Some(now - 60), "no /health round-trip");
        assert_eq!(
            plan(&st, now, "0.43.3"),
            Plan::Update {
                server_version: "0.43.7".into()
            }
        );
    }

    /// Regression (0.43.8): the observed root cause. A concurrent run that
    /// lost the race to swap in 0.43.4 armed a 24h backoff; that backoff
    /// then also swallowed 0.43.5 … 0.43.7 even though every response
    /// advertised them. The backoff now only covers the failed version.
    #[test]
    fn backoff_for_one_version_does_not_block_a_newer_one() {
        let now = 10 * DAY;
        let mut st = State {
            last_check: Some(now - 60),
            server_version: Some("0.43.4".into()),
            ..State::default()
        };
        record_failure(&mut st, now - 120, "0.43.4", FAILURE_BACKOFF_SECS);
        // Same version still backs off...
        assert_eq!(plan(&st, now, "0.43.3"), Plan::Skip);
        // ...but a newer server seen in a header gets a fresh attempt.
        assert!(observe_server_version(&mut st, "0.43.7", "0.43.3"));
        assert_eq!(
            plan(&st, now, "0.43.3"),
            Plan::Update {
                server_version: "0.43.7".into()
            }
        );
        // Legacy state (backoff without `failed_version`) stays conservative.
        st.failed_version = None;
        assert_eq!(plan(&st, now, "0.43.3"), Plan::Skip);
    }

    #[test]
    fn same_or_older_header_changes_nothing() {
        let now = 10 * DAY;
        let orig = State {
            last_check: Some(now - 60),
            server_version: Some("0.43.7".into()),
            ..State::default()
        };
        for header in ["0.43.7", "0.43.6", "0.42.9"] {
            let mut st = orig.clone();
            assert!(!observe_server_version(&mut st, header, "0.43.7"));
            assert_eq!(st, orig);
            assert_eq!(plan(&st, now, "0.43.7"), Plan::Skip);
        }
        // A header newer than local but older than an already-known newer
        // server never downgrades the recorded version.
        let mut st = State {
            server_version: Some("0.43.9".into()),
            ..State::default()
        };
        assert!(!observe_server_version(&mut st, "0.43.8", "0.43.7"));
        assert_eq!(st.server_version.as_deref(), Some("0.43.9"));
    }

    #[test]
    fn installed_link_version_reads_symlink_target() {
        let dir = self_update::tempdir_for("au-link").unwrap();
        assert_eq!(installed_link_version(&dir), None);
        std::fs::write(dir.join("krab-0.43.8"), b"").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("krab-0.43.8"), dir.join("krab")).unwrap();
            assert_eq!(installed_link_version(&dir).as_deref(), Some("0.43.8"));
            assert!(already_installed(&dir, "0.43.8"));
            assert!(already_installed(&dir, "0.43.7"));
            assert!(!already_installed(&dir, "0.43.9"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_check_stamps_time_even_on_failure() {
        let mut st = State {
            server_version: Some("0.43.0".into()),
            ..State::default()
        };
        record_check(&mut st, 42, None);
        assert_eq!(st.last_check, Some(42));
        assert_eq!(st.server_version.as_deref(), Some("0.43.0"));
        record_check(&mut st, 43, Some("0.43.2".into()));
        assert_eq!(st.server_version.as_deref(), Some("0.43.2"));
    }

    #[test]
    fn backoff_is_shorter_for_lagging_release() {
        let lag: anyhow::Error = ReleaseNotNewer {
            published: "0.43.0".into(),
            local: "0.43.0".into(),
        }
        .into();
        assert_eq!(backoff_secs(&lag), LAGGING_RELEASE_BACKOFF_SECS);
        assert_eq!(
            backoff_secs(&anyhow::anyhow!("permission denied")),
            FAILURE_BACKOFF_SECS
        );
    }

    #[test]
    fn opt_outs() {
        assert_eq!(disabled_reason(&env(&[]), None), None);
        assert_eq!(disabled_reason(&env(&[]), Some(true)), None);
        assert_eq!(
            disabled_reason(&env(&[("no", "1")]), None),
            Some(Disabled::EnvOptOut)
        );
        assert_eq!(disabled_reason(&env(&[("no", "0")]), None), None);
        assert_eq!(
            disabled_reason(&env(&[]), Some(false)),
            Some(Disabled::ConfigOptOut)
        );
        assert_eq!(
            disabled_reason(&env(&[("ci", "true")]), None),
            Some(Disabled::Ci)
        );
        // Explicit force beats CI and config...
        assert_eq!(
            disabled_reason(&env(&[("ci", "true"), ("force", "1")]), None),
            None
        );
        assert_eq!(disabled_reason(&env(&[("force", "1")]), Some(false)), None);
        // ...but never the explicit env opt-out.
        assert_eq!(
            disabled_reason(&env(&[("no", "1"), ("force", "1")]), None),
            Some(Disabled::EnvOptOut)
        );
    }

    #[test]
    fn reexec_guard_always_wins() {
        assert_eq!(
            disabled_reason(&env(&[("from", "0.43.0"), ("force", "1")]), Some(true)),
            Some(Disabled::ReExecGuard)
        );
    }

    #[test]
    fn skipped_commands() {
        assert!(command_eligible(&args(&["battle", "list", "--limit", "1"])));
        assert!(command_eligible(&args(&[
            "--format", "json", "claim", "show", "x"
        ])));
        assert!(command_eligible(&args(&[
            "--api-url",
            "https://x",
            "battle",
            "list"
        ])));
        assert!(!command_eligible(&args(&[])));
        assert!(!command_eligible(&args(&["--version"])));
        assert!(!command_eligible(&args(&["-V"])));
        assert!(!command_eligible(&args(&["--help"])));
        assert!(!command_eligible(&args(&["battle", "list", "-h"])));
        assert!(!command_eligible(&args(&["help", "battle"])));
        assert!(!command_eligible(&args(&["doctor", "--fix"])));
        assert!(!command_eligible(&args(&["auth", "login"])));
        assert!(!command_eligible(&args(&[
            "--token", "t", "auth", "status"
        ])));
        // A global flag's value is not mistaken for the subcommand.
        assert!(command_eligible(&args(&[
            "--token", "doctor", "battle", "list"
        ])));
    }

    #[test]
    fn managed_installs_are_detected() {
        assert_eq!(
            managed_install(Path::new("/Users/me/code/krab/target/debug")),
            Some(Managed::Silent)
        );
        assert!(matches!(
            managed_install(Path::new("/opt/homebrew/Cellar/krab/0.43.0/bin")),
            Some(Managed::Warn(_))
        ));
        assert!(matches!(
            managed_install(Path::new("/nix/store/abc-krab/bin")),
            Some(Managed::Warn(_))
        ));
        assert_eq!(managed_install(Path::new("/Users/me/.krab/bin")), None);
    }

    #[test]
    fn state_round_trips_and_tolerates_garbage() {
        let dir = self_update::tempdir_for("au-state").unwrap();
        let p = dir.join("update-check.json");
        assert_eq!(load_state(&p), State::default());
        let st = State {
            last_check: Some(1),
            server_version: Some("0.43.2".into()),
            retry_after: Some(2),
            failed_version: None,
        };
        save_state(&p, &st);
        assert_eq!(load_state(&p), st);
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(load_state(&p), State::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pre_parse_api_url_forms() {
        assert_eq!(
            pre_parse_api_url(&args(&["--api-url", "http://a", "battle"])).as_deref(),
            Some("http://a")
        );
        assert_eq!(
            pre_parse_api_url(&args(&["--api-url=http://b"])).as_deref(),
            Some("http://b")
        );
        assert_eq!(pre_parse_api_url(&args(&["battle", "list"])), None);
    }
}
