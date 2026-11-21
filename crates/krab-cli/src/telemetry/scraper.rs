//! The [`SessionScraper`] trait. One implementation per agent
//! harness. Adding a new harness is one file + one entry in
//! [`super::all_scrapers`].
//!
//! The trait is deliberately minimal:
//!
//!   - `agent()` — the static name we tag records with.
//!   - `default_paths()` — where on disk this harness writes session
//!     logs by default. Implementations should return only paths
//!     that exist (filter at probe time, not at parse time).
//!   - `parse(path)` — read one session log into one or more
//!     [`super::TelemetryRecord`]s. Returns Ok(empty) for malformed
//!     files; only returns Err on truly catastrophic I/O failures
//!     (e.g. permission denied on a directory we listed ourselves).
//!
//! The dispatch in [`super::ingest_all`] silently swallows per-file
//! Err so one bad file can't kill the whole ingest.

use std::path::{Path, PathBuf};

use super::TelemetryRecord;

/// Stable lowercase bucket keys for `TelemetryRecord::errors`. Each
/// scraper picks the most specific bucket it can; `OTHER` is the
/// catch-all so unfamiliar shapes still get counted.
pub mod buckets {
    /// Edit/Write before Read — typical of Anthropic Claude Code's
    /// "you must Read first" guard.
    pub const EDIT_BEFORE_READ: &str = "edit_before_read";
    /// Shell command returned a non-zero exit code (the most common
    /// failure in any CLI-driven session).
    pub const SHELL_EXIT_NONZERO: &str = "shell_exit_nonzero";
    /// Permission-gate denial — sandbox or policy refusal. Mostly
    /// surfaces in agents that enforce a sandbox (Claude Code) but
    /// any harness can adopt it.
    pub const PERMISSION_GATE: &str = "permission_gate";
    /// Python parser error in an inline `python -c …` — a shape
    /// we've seen the model emit a lot when piping JSON.
    pub const PYTHON_PARSE: &str = "python_parse";
    /// Anything else — a record we recognized as an error but couldn't
    /// classify into a more specific bucket.
    pub const OTHER: &str = "other";
}

/// One implementation per agent harness. See module docs.
pub trait SessionScraper: Send + Sync {
    /// The static name we tag records with (e.g. `"claude-code"`).
    fn agent(&self) -> &'static str;

    /// Paths to look for session logs in. Only return paths that
    /// exist — the dispatcher does not pre-check. Typical
    /// implementation: glob a well-known directory and filter to
    /// regular files.
    fn default_paths(&self) -> Vec<PathBuf>;

    /// Parse one session file into one or more telemetry records.
    /// Most harnesses produce one record per file; the trait is
    /// `Vec<…>` so a future log format that packs N sessions into
    /// one file (or one rotated batch) doesn't need a trait change.
    fn parse(&self, path: &Path) -> anyhow::Result<Vec<TelemetryRecord>>;
}
