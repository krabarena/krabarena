//! Local pre-flight validation for claim artifact bundles.
//!
//! The server's deterministic pre-filter (in `review_service.rs`) blocks
//! any artifact that's missing RUN.md or README.md at the root. Without
//! this CLI check the agent burns a round-trip + KrabReviewer call to
//! learn that, and the resulting error ("Blocked by deterministic
//! pre-filter") is opaque enough that agents like Hermes have been
//! known to guess "CLI/server version mismatch" instead of inspecting
//! the bundle.
//!
//! Validation rules pinned here MUST stay aligned with
//! `services/review_service.rs::check_required_artifact_files` on the
//! server side. When the server adds a new required file, mirror it
//! here so the CLI fails first.
//!
//! See `docs/claim-structure.md` for the canonical contract.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use krabarena_shared::claim_structure::{ClaimStructure, ParseError};

/// Outcome of inspecting a claim artifact bundle. `errors` blocks the
/// upload; `warnings` is printed to stderr but allowed through (the
/// server may still reject — e.g. for a missing structure.json the
/// publish goes through but the leaderboard table doesn't render).
#[derive(Debug, Default)]
pub struct ArtifactReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ArtifactReport {
    pub fn is_blocking(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Validate a `structure.json` payload against the shared schema, the
/// same `ClaimStructure` gate the server runs. Returns blocking error
/// strings (empty when valid). Shared by the in-bundle check and the
/// inline `--structure` pre-flight so both surfaces mirror the server
/// identically.
pub fn validate_structure_json(bytes: &[u8]) -> Vec<String> {
    let raw = String::from_utf8_lossy(bytes);
    match ClaimStructure::parse_and_validate(&raw) {
        Ok(_) => Vec::new(),
        Err(ParseError::Json(e)) => vec![format!(
            "structure.json is not valid JSON: {e}. Fix the syntax; see \
             https://krabarena.com/docs/claim-structure"
        )],
        Err(ParseError::Validation(errs)) => errs
            .iter()
            .map(|err| {
                format!(
                    "structure.json failed validation: {err}. See \
                     https://krabarena.com/docs/claim-structure"
                )
            })
            .collect(),
    }
}

/// Validate the claim artifact at `path` against the server's
/// deterministic pre-filter. Returns an `ArtifactReport` listing
/// blocking errors + non-blocking warnings. The caller surfaces the
/// errors before sending the multipart request.
///
/// Checks (all blocking unless noted):
/// 1. Extension is `.tar.gz` or `.tgz` — the server's tarball
///    extractor (`FileMap::from_tar_gz`) only fires on those two
///    suffixes; a plain `.tar` is treated as a single binary blob
///    and the per-file-presence check fails.
/// 2. Bundle decodes as a valid gzipped tar (best-effort; truncated /
///    corrupt archives blow up here instead of server-side).
/// 3. `RUN.md` and `README.md` exist at the bundle ROOT (paths with
///    or without a leading `./`). Case-insensitive comparison —
///    server normalizes to lowercase too.
/// 4. `structure.json` at the root — REQUIRED (0.18.7). Missing it
///    blocks; present-but-invalid (unparseable JSON, or a field-rule
///    violation per `ClaimStructure::validate` — empty leaderboard,
///    duplicate rank, winner not in the table, empty setup/metric)
///    blocks too — UNLESS `--ignore-review` is passed (see the
///    `ignore_review` exception below), in which case both downgrade to
///    warnings. The server's deterministic gate behaves the same way, so
///    the CLI fails first. (The four required markdown sections are
///    body-level, not part of the bundle — the server enforces those.)
///
///    EXCEPTION — `structure_provided_inline`: when the publish also
///    passes `--structure <file>`, the server reads that inline field
///    and ignores any bundle copy. So with inline present we don't
///    require (or validate) the bundle's `structure.json` — the inline
///    file is validated at the call site instead, mirroring the server.
///
///    EXCEPTION — `ignore_review`: the server treats a structure/section
///    problem as an OVERRIDABLE `warn` (a publisher passing
///    `--ignore-review` can still ship). So when `ignore_review` is set we
///    downgrade structure.json problems from blocking errors to warnings,
///    mirroring the server. RUN.md / README.md stay hard errors — those
///    are a non-overridable server block.
pub fn validate_claim_artifact(
    path: &Path,
    structure_provided_inline: bool,
    ignore_review: bool,
) -> Result<ArtifactReport> {
    let mut report = ArtifactReport::default();

    let path_str = path.to_string_lossy();
    let lower = path_str.to_lowercase();
    let is_gzipped_tar = lower.ends_with(".tar.gz") || lower.ends_with(".tgz");
    if !is_gzipped_tar {
        report.errors.push(format!(
            "artifact '{path_str}' must end in .tar.gz or .tgz; the server's bundle \
             extractor only treats those extensions as tarballs (a plain .tar is \
             read as a single binary blob and the RUN.md/README.md presence check \
             rejects it). See https://krabarena.com/docs/claim-structure"
        ));
        // Don't try to inspect the archive when the extension is wrong —
        // it's likely not even a tarball, and we'd just emit a confusing
        // decompression error on top of the clear extension error.
        return Ok(report);
    }

    let file = File::open(path).with_context(|| format!("cannot open artifact '{}'", path_str))?;
    let gz = GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    let mut has_run_md = false;
    let mut has_readme_md = false;
    let mut structure_json_bytes: Option<Vec<u8>> = None;
    let mut total_entries: usize = 0;

    let entries = archive
        .entries()
        .context("failed to read entries from artifact tar.gz")?;
    for entry in entries {
        let mut entry = entry.context("malformed entry in artifact tar.gz")?;
        total_entries += 1;

        // Mirror the server's normalization: strip a leading "./" so
        // `tar -czf bundle.tar.gz .` (which emits "./RUN.md") and
        // bare `tar -czf bundle.tar.gz RUN.md README.md` (which emits
        // "RUN.md") both pass.
        let normalized = {
            let path = entry.path().context("artifact entry has non-utf8 path")?;
            let path_str = path.to_string_lossy();
            path_str
                .strip_prefix("./")
                .or_else(|| path_str.strip_prefix(".\\"))
                .unwrap_or(&path_str)
                .to_string()
        };

        // Top-level only — exclude entries in subdirectories.
        if normalized.contains('/') || normalized.contains('\\') {
            continue;
        }

        match normalized.to_ascii_lowercase().as_str() {
            "run.md" => has_run_md = true,
            "readme.md" => has_readme_md = true,
            "structure.json" => {
                // Capture the bytes so we can validate the leaderboard
                // schema, not just its presence (mirrors the server's
                // ClaimStructure gate).
                let mut buf = Vec::new();
                entry
                    .read_to_end(&mut buf)
                    .context("failed to read structure.json from artifact tar.gz")?;
                structure_json_bytes = Some(buf);
            }
            _ => {}
        }
    }

    if total_entries == 0 {
        report
            .errors
            .push("artifact tar.gz contains no entries".into());
        return Ok(report);
    }

    if !has_run_md {
        report.errors.push(
            "artifact bundle is missing RUN.md at the root. The server's deterministic \
             pre-filter requires it (reproduction recipe). See \
             https://krabarena.com/docs/claim-structure#runmd--exact-reproduction-recipe"
                .into(),
        );
    }
    if !has_readme_md {
        report.errors.push(
            "artifact bundle is missing README.md at the root. The server's deterministic \
             pre-filter requires it (extended report). See \
             https://krabarena.com/docs/claim-structure#readmemd--the-extended-report"
                .into(),
        );
    }
    // structure.json is a review-overridable form check: with
    // `--ignore-review` the server downgrades it to a `warn` the publish
    // can bypass, so route its problems to warnings (not errors) then.
    let sink = |report: &mut ArtifactReport, msg: String| {
        if ignore_review {
            report.warnings.push(msg);
        } else {
            report.errors.push(msg);
        }
    };
    match structure_json_bytes {
        // Inline `--structure` wins server-side; the bundle copy (absent
        // or present) is ignored there, so don't require/validate it
        // here. The inline file is validated at the call site.
        _ if structure_provided_inline => {}
        None => sink(
            &mut report,
            "artifact bundle is missing structure.json at the root (REQUIRED since \
             0.18.7). Without the machine-readable leaderboard there is no Claim, only \
             a write-up — the server declines it. Add the JSON sidecar per \
             https://krabarena.com/docs/claim-structure#structurejson--the-leaderboard-schema"
                .into(),
        ),
        Some(bytes) => {
            for msg in validate_structure_json(&bytes) {
                sink(&mut report, msg);
            }
        }
    }

    Ok(report)
}

/// Read the artifact into memory + run validation. Same outcome as
/// `validate_claim_artifact` but for callers who already have the
/// bytes in hand (e.g. after `std::fs::read`). Used by tests; the
/// CLI uses the path-based wrapper.
///
/// Each call writes to a fresh per-thread, per-counter subdirectory
/// so parallel test threads can't race on the same path.
#[cfg(test)]
pub fn validate_claim_artifact_bytes(
    filename: &str,
    bytes: &[u8],
    structure_provided_inline: bool,
    ignore_review: bool,
) -> Result<ArtifactReport> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "krab-artifact-check-{}-{:?}-{}",
        std::process::id(),
        std::thread::current().id(),
        n,
    ));
    std::fs::create_dir_all(&tmp)?;
    let path = tmp.join(filename);
    {
        use std::io::Write;
        let mut f = File::create(&path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    let report = validate_claim_artifact(&path, structure_provided_inline, ignore_review)?;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&tmp);
    Ok(report)
}

#[allow(dead_code)]
fn _swallow(_: &mut dyn Read) {}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    // A structure.json that passes ClaimStructure::parse_and_validate —
    // the bundles meant to be accepted must ship one now (0.18.7).
    const VALID_STRUCTURE: &[u8] = br#"{
        "schema_version": 1,
        "benchmark": {"name": "n", "task": "t", "dataset": "d",
            "metric": "m", "higher_is_better": true, "setup": "s"},
        "leaderboard": [{"rank": 1, "solution": "A", "score": 1.0}],
        "winners": ["A"]
    }"#;

    fn make_targz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let enc = GzEncoder::new(&mut out, Compression::default());
            let mut tar_w = tar::Builder::new(enc);
            for (name, data) in files {
                let mut hdr = tar::Header::new_gnu();
                hdr.set_size(data.len() as u64);
                hdr.set_mode(0o644);
                hdr.set_cksum();
                tar_w.append_data(&mut hdr, name, *data).unwrap();
            }
            tar_w.finish().unwrap();
        }
        out
    }

    #[test]
    fn rejects_plain_tar_extension() {
        // Hermes' real failure shape: built a `.tar` (no gzip).
        // Server extractor only triggers on `.tar.gz` / `.tgz`, so the
        // RUN.md/README.md presence check fails. Catch it locally.
        let report = validate_claim_artifact_bytes("artifact.tar", b"junk", false, false).unwrap();
        assert!(report.is_blocking());
        assert!(
            report.errors.iter().any(|e| e.contains(".tar.gz")),
            "extension error must reference .tar.gz: {:?}",
            report.errors
        );
    }

    #[test]
    fn accepts_well_formed_bundle_with_structure_json() {
        let data = make_targz(&[
            ("RUN.md", b"# How to reproduce\n\npython run.py"),
            ("README.md", b"# Report"),
            ("structure.json", VALID_STRUCTURE),
        ]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, false).unwrap();
        assert!(!report.is_blocking(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "no warnings expected: {:?}",
            report.warnings
        );
    }

    #[test]
    fn blocks_when_structure_json_missing() {
        // 0.18.7: structure.json is REQUIRED — absence now blocks
        // (was a warning before).
        let data = make_targz(&[("RUN.md", b"# repro"), ("README.md", b"# report")]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, false).unwrap();
        assert!(report.is_blocking(), "missing structure.json must block");
        assert!(
            report.errors.iter().any(|e| e.contains("structure.json")),
            "error must reference structure.json: {:?}",
            report.errors
        );
    }

    #[test]
    fn blocks_when_structure_json_invalid() {
        // Present but fails the schema (empty leaderboard, no winners,
        // missing benchmark) → blocks with a validation error.
        let data = make_targz(&[
            ("RUN.md", b"# repro"),
            ("README.md", b"# report"),
            (
                "structure.json",
                br#"{"schema_version":1,"leaderboard":[]}"#,
            ),
        ]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, false).unwrap();
        assert!(report.is_blocking(), "invalid structure.json must block");
        assert!(
            report.errors.iter().any(|e| e.contains("structure.json")),
            "error must reference structure.json: {:?}",
            report.errors
        );
    }

    #[test]
    fn allows_missing_bundle_structure_when_inline_provided() {
        // `--structure` inline wins server-side; the bundle copy is
        // optional then, so its absence must NOT block locally.
        let data = make_targz(&[("RUN.md", b"# repro"), ("README.md", b"# report")]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, true, false).unwrap();
        assert!(
            !report.is_blocking(),
            "inline structure → bundle copy optional: {:?}",
            report.errors
        );
    }

    #[test]
    fn ignores_invalid_bundle_structure_when_inline_provided() {
        // Server ignores the bundle copy when `--structure` is given, so
        // an invalid bundle structure.json must not block — the inline
        // file is validated separately at the call site.
        let data = make_targz(&[
            ("RUN.md", b"# repro"),
            ("README.md", b"# report"),
            ("structure.json", b"not valid json {"),
        ]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, true, false).unwrap();
        assert!(
            !report.is_blocking(),
            "inline structure → bundle copy ignored: {:?}",
            report.errors
        );
    }

    #[test]
    fn validate_structure_json_flags_empty_leaderboard() {
        let errs = validate_structure_json(br#"{"schema_version":1,"leaderboard":[]}"#);
        assert!(!errs.is_empty(), "empty leaderboard must error");
        assert!(errs.iter().any(|e| e.contains("structure.json")));
    }

    #[test]
    fn validate_structure_json_accepts_valid() {
        assert!(validate_structure_json(VALID_STRUCTURE).is_empty());
    }

    #[test]
    fn missing_structure_is_warning_not_block_under_ignore_review() {
        // --ignore-review downgrades the structure gate to an overridable
        // warn (mirrors the server), so a missing structure.json must NOT
        // block locally — it surfaces as a warning instead.
        let data = make_targz(&[("RUN.md", b"# repro"), ("README.md", b"# report")]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, true).unwrap();
        assert!(!report.is_blocking(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.iter().any(|w| w.contains("structure.json")),
            "warning must reference structure.json: {:?}",
            report.warnings
        );
    }

    #[test]
    fn invalid_structure_is_warning_not_block_under_ignore_review() {
        let data = make_targz(&[
            ("RUN.md", b"# repro"),
            ("README.md", b"# report"),
            (
                "structure.json",
                br#"{"schema_version":1,"leaderboard":[]}"#,
            ),
        ]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, true).unwrap();
        assert!(!report.is_blocking(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.iter().any(|w| w.contains("structure.json")),
            "warning must reference structure.json: {:?}",
            report.warnings
        );
    }

    #[test]
    fn missing_run_md_still_blocks_under_ignore_review() {
        // RUN.md / README.md are a non-overridable server block — even
        // with --ignore-review the CLI must still hard-error on them.
        let data = make_targz(&[
            ("README.md", b"# report"),
            ("structure.json", VALID_STRUCTURE),
        ]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, true).unwrap();
        assert!(
            report.is_blocking(),
            "missing RUN.md must block even under ignore_review"
        );
        assert!(report.errors.iter().any(|e| e.contains("RUN.md")));
    }

    #[test]
    fn blocks_when_run_md_missing() {
        let data = make_targz(&[("README.md", b"# report")]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, false).unwrap();
        assert!(report.is_blocking());
        assert!(
            report.errors.iter().any(|e| e.contains("RUN.md")),
            "error must reference RUN.md: {:?}",
            report.errors
        );
    }

    #[test]
    fn blocks_when_readme_md_missing() {
        let data = make_targz(&[("RUN.md", b"# repro")]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, false).unwrap();
        assert!(report.is_blocking());
        assert!(
            report.errors.iter().any(|e| e.contains("README.md")),
            "error must reference README.md: {:?}",
            report.errors
        );
    }

    /// Server normalizes "./RUN.md" -> "RUN.md" before the
    /// top-level check; the CLI must do the same. `tar -czf bundle .`
    /// is the canonical CLI invocation and emits every entry with a
    /// leading `./`.
    #[test]
    fn handles_dot_prefixed_paths() {
        let data = make_targz(&[
            ("./RUN.md", b"# repro"),
            ("./README.md", b"# report"),
            ("./structure.json", VALID_STRUCTURE),
        ]);
        let report = validate_claim_artifact_bytes("artifact.tgz", &data, false, false).unwrap();
        assert!(!report.is_blocking(), "errors: {:?}", report.errors);
        assert!(report.warnings.is_empty());
    }

    /// Case-insensitive match — `run.md` is the same as `RUN.md`.
    /// Server uses `to_ascii_lowercase()` for the same reason.
    #[test]
    fn handles_lowercase_filenames() {
        let data = make_targz(&[
            ("run.md", b"# repro"),
            ("readme.md", b"# report"),
            ("structure.json", VALID_STRUCTURE),
        ]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, false).unwrap();
        assert!(!report.is_blocking(), "errors: {:?}", report.errors);
    }

    /// `RUN.md` in a subdirectory does NOT satisfy the top-level check
    /// — server skips subdirectory paths via the `contains('/')` guard
    /// and the CLI must too.
    #[test]
    fn rejects_run_md_in_subdirectory() {
        let data = make_targz(&[
            ("subdir/RUN.md", b"# wrong place"),
            ("README.md", b"# report"),
        ]);
        let report = validate_claim_artifact_bytes("artifact.tar.gz", &data, false, false).unwrap();
        assert!(report.is_blocking());
        assert!(report.errors.iter().any(|e| e.contains("RUN.md")));
    }
}
