//! Binary self-update machinery shared by `krab doctor --fix` and the
//! default-on auto-update (`crate::auto_update`).
//!
//! Downloads the matching `krabarena-<platform>-<arch>.tar.gz` from the
//! release bucket (the same archives `install.sh` uses), verifies its
//! `.sha256` sidecar, probes the extracted binary with `--version`, and
//! atomically swaps a versioned-binary + symlink layout into place:
//!
//! ```text
//! ~/.krab/bin/krab-X.Y.Z   — real binaries, one per installed version
//! ~/.krab/bin/krab         — symlink → krab-<active>
//! ```
//!
//! Upgrade procedure:
//!   1. Download tarball + sha256, verify the checksum.
//!   2. Extract into a scratch dir; the archive must contain `krab`.
//!   3. chmod 755 and run `<new> --version`; abort if it doesn't run,
//!      prints a non-semver string, or (auto-update) isn't newer.
//!   4. Copy into the install dir as `krab-<new>.tmp`, chmod, then
//!      `rename` to `krab-<new>` (atomic).
//!   5. Atomically swap the `krab` symlink (write a sibling symlink,
//!      then `rename` it over the existing entry). If this fails the
//!      freshly installed `krab-<new>` is removed again (rollback) and
//!      the old layout is untouched.
//!   6. GC: keep the 3 most recent versions.
//!
//! The running process never has its binary file replaced — it stays at
//! `krab-<old>` — so this is safe on macOS sandboxes that block writing
//! to the executing binary path. A pre-0.8.19 regular-file `krab` is
//! migrated to the symlink layout by the same rename.
//!
//! Network access goes through the [`Fetcher`] trait so tests drive the
//! whole flow with a fake downloader and a temp install dir.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// GCS bucket that holds the `latest/` release artifacts. Must match
/// `CLI_BUCKET` in .github/workflows/release.yml and `GCS_BASE` in
/// install.sh — if these drift, doctor --fix pulls one version and the
/// installer pulls another.
pub const GCS_LATEST: &str = "https://storage.googleapis.com/krabarena-cli-releases/latest";

/// Downloads a URL to a local file. Production uses [`HttpFetcher`];
/// tests use a fake that writes fixtures, so no test hits the network.
pub trait Fetcher {
    fn fetch(&self, url: &str, dest: &Path) -> impl std::future::Future<Output = Result<()>>;
}

/// Real HTTP downloader (60s per-file timeout).
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    async fn fetch(&self, url: &str, dest: &Path) -> Result<()> {
        download_to(url, dest).await
    }
}

/// The release bucket's binary isn't newer than the running CLI — the
/// server deployed before `Release` finished uploading. Typed so the
/// auto-updater can retry sooner than for hard failures.
#[derive(Debug)]
pub struct ReleaseNotNewer {
    pub published: String,
    pub local: String,
}

impl std::fmt::Display for ReleaseNotNewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "latest published release is {}, not newer than {} yet",
            self.published, self.local
        )
    }
}

impl std::error::Error for ReleaseNotNewer {}

/// Result of a successful update.
#[derive(Debug)]
pub struct UpdateOutcome {
    /// The `krab` symlink that now points at the new binary.
    pub link_path: PathBuf,
    /// Version reported by the new binary's `--version`.
    pub new_version: String,
}

/// Directory the running binary is installed in, with symlinks
/// resolved: `~/.krab/bin/krab → krab-0.43.0` resolves to
/// `~/.krab/bin`, and an extra `/usr/local/bin/krab → ~/.krab/bin/krab`
/// link also lands in `~/.krab/bin` instead of writing into
/// `/usr/local/bin`.
pub fn current_install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolve current_exe")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("current_exe has no parent directory: {}", exe.display()))
}

/// Fail fast (before downloading anything) when the install dir isn't
/// writable — e.g. a root-owned system path.
pub fn ensure_writable(dir: &Path) -> Result<()> {
    let probe = dir.join(format!(".krab-write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"").with_context(|| format!("{} is not writable", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Download the latest release for this platform and install it into
/// `install_dir`. When `require_newer_than` is set, the new binary must
/// report a strictly newer version or nothing is installed (guards
/// against the release bucket lagging behind a server deploy).
pub async fn self_update<F: Fetcher>(
    fetcher: &F,
    install_dir: &Path,
    require_newer_than: Option<&str>,
) -> Result<UpdateOutcome> {
    let (platform, arch) = platform_arch()?;
    let archive = format!("krabarena-{}-{}.tar.gz", platform, arch);
    let archive_url = format!("{}/{}", GCS_LATEST, archive);
    let sha_url = format!("{}.sha256", archive_url);

    ensure_writable(install_dir)?;

    let tmp = tempdir_for("krab-self-update")?;
    let result = async {
        let archive_path = tmp.join(&archive);
        let sha_path = tmp.join(format!("{}.sha256", archive));
        fetcher.fetch(&archive_url, &archive_path).await?;
        fetcher.fetch(&sha_url, &sha_path).await?;
        install_from_archive(
            &archive_path,
            &sha_path,
            &tmp,
            install_dir,
            require_newer_than,
        )
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Verify + extract + probe + swap. Split out from [`self_update`] so
/// the filesystem half is testable without any downloader at all.
fn install_from_archive(
    archive_path: &Path,
    sha_path: &Path,
    scratch: &Path,
    install_dir: &Path,
    require_newer_than: Option<&str>,
) -> Result<UpdateOutcome> {
    let size = std::fs::metadata(archive_path)
        .with_context(|| format!("stat {}", archive_path.display()))?
        .len();
    if size == 0 {
        anyhow::bail!("downloaded archive is empty");
    }
    verify_sha256(archive_path, sha_path)?;

    let stage = scratch.join("stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).with_context(|| format!("create {}", stage.display()))?;
    extract_tar_gz(archive_path, &stage)?;

    let new_bin = stage.join("krab");
    if !new_bin.is_file() {
        anyhow::bail!("extracted archive did not contain a `krab` binary");
    }
    set_executable(&new_bin)?;

    // Probe BEFORE touching the install dir: catches wrong-arch / broken
    // binaries and a release that isn't actually newer.
    let probed_version = probe_binary_version(&new_bin)?;
    if let Some(local) = require_newer_than
        && semver_cmp(&probed_version, local) != Ordering::Greater
    {
        return Err(ReleaseNotNewer {
            published: probed_version,
            local: local.to_string(),
        }
        .into());
    }

    let link_path = install_dir.join("krab");
    let final_versioned = install_dir.join(format!("krab-{probed_version}"));
    // Per-process staging names: concurrent `krab` runs updating at the
    // same time must not delete/overwrite each other's staged files.
    let pid = std::process::id();
    let staged_versioned = install_dir.join(format!("krab-{probed_version}.{pid}.tmp"));
    let preexisting = final_versioned.exists();

    let _ = std::fs::remove_file(&staged_versioned);
    std::fs::copy(&new_bin, &staged_versioned).with_context(|| {
        format!(
            "copy {} -> {}",
            new_bin.display(),
            staged_versioned.display()
        )
    })?;
    if let Err(e) = set_executable(&staged_versioned) {
        let _ = std::fs::remove_file(&staged_versioned);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&staged_versioned, &final_versioned) {
        let _ = std::fs::remove_file(&staged_versioned);
        anyhow::bail!(
            "rename {} -> {} failed: {}",
            staged_versioned.display(),
            final_versioned.display(),
            e
        );
    }

    if let Err(e) = swap_symlink_atomically(&link_path, &final_versioned) {
        // Rollback: drop the binary we just added so the install dir is
        // exactly as we found it. The old `krab` entry is untouched
        // because the rename never happened.
        if !preexisting {
            let _ = std::fs::remove_file(&final_versioned);
        }
        return Err(e);
    }

    if let Err(e) = gc_old_versions(install_dir, &probed_version, 3) {
        // Non-fatal: leftover old binaries just take a few MB.
        eprintln!("note: GC of old krab-* versions failed: {e}");
    }

    Ok(UpdateOutcome {
        link_path,
        new_version: probed_version,
    })
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("chmod {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Run `<path> --version` to confirm the binary at least executes.
/// Returns the parsed semver string from its stdout (`krab 0.8.19` →
/// `0.8.19`). The child gets `KRAB_NO_AUTO_UPDATE=1` so a probe can
/// never itself trigger an update.
pub fn probe_binary_version(path: &Path) -> Result<String> {
    let output = Command::new(path)
        .arg("--version")
        .env("KRAB_NO_AUTO_UPDATE", "1")
        .output()
        .with_context(|| format!("run {} --version", path.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} --version exited with {}: stderr={}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout
        .split_whitespace()
        .last()
        .ok_or_else(|| anyhow::anyhow!("--version output was empty"))?
        .to_string();
    // Sanity: version should be `\d+\.\d+\.\d+` — we don't want to
    // create files like `krab-Hello World`.
    if version.is_empty()
        || !version
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        anyhow::bail!("--version returned non-semver string: {stdout:?}");
    }
    Ok(version)
}

/// Atomically write a symlink at `link_path → target`, replacing
/// whatever's there (existing symlink, regular file, or nothing):
/// write `<link_path>.next`, then `rename` it over `link_path`.
fn swap_symlink_atomically(link_path: &Path, target: &Path) -> Result<()> {
    let next = link_path.with_file_name(format!(
        "{}.next.{}",
        link_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("krab"),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&next);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, &next)
            .with_context(|| format!("symlink {} -> {}", next.display(), target.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        anyhow::bail!("symlink swap is unix-only — windows install layout TBD");
    }

    if let Err(e) = std::fs::rename(&next, link_path) {
        let _ = std::fs::remove_file(&next);
        anyhow::bail!(
            "atomic symlink swap {} -> {} failed: {}",
            next.display(),
            link_path.display(),
            e
        );
    }
    Ok(())
}

/// Delete `krab-X.Y.Z` files in `dir` keeping the `keep` most-recent
/// (by mtime) plus `keep_active`. Best-effort.
fn gc_old_versions(dir: &Path, keep_active: &str, keep: usize) -> Result<()> {
    let mut versioned: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let Some(suffix) = name_str.strip_prefix("krab-") else {
            continue;
        };
        if suffix == keep_active {
            continue;
        }
        if !suffix
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        if entry.file_type()?.is_file() {
            let mtime = entry.metadata()?.modified()?;
            versioned.push((entry.path(), mtime));
        }
    }
    versioned.sort_by_key(|v| std::cmp::Reverse(v.1));
    for (path, _) in versioned.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Stream a URL to disk. Fails on any non-2xx response.
pub async fn download_to(url: &str, dest: &Path) -> Result<()> {
    let resp = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("GET {} → HTTP {}", url, status);
    }
    let bytes = resp.bytes().await?;
    std::fs::write(dest, &bytes).with_context(|| format!("write {}", dest.display()))?;
    Ok(())
}

/// Validate the archive against its `.sha256` sidecar
/// (`<hex>  <filename>`, as `sha256sum` writes it).
pub fn verify_sha256(archive: &Path, sidecar: &Path) -> Result<()> {
    let sidecar_text =
        std::fs::read_to_string(sidecar).with_context(|| format!("read {}", sidecar.display()))?;
    let expected = sidecar_text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty sha256 sidecar at {}", sidecar.display()))?
        .to_ascii_lowercase();

    let bytes = std::fs::read(archive).with_context(|| format!("read {}", archive.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    if actual != expected {
        anyhow::bail!(
            "sha256 mismatch for {}: expected {}, got {}",
            archive.display(),
            expected,
            actual
        );
    }
    Ok(())
}

/// Shell out to `tar xzf <archive> -C <dest>` — same as `install.sh`.
pub fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let status = Command::new("tar")
        .arg("xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .context("spawn `tar` — is it on PATH?")?;
    if !status.success() {
        anyhow::bail!(
            "tar xzf {} -C {} exited {:?}",
            archive.display(),
            dest.display(),
            status.code()
        );
    }
    Ok(())
}

/// Map `std::env::consts` to the `{platform}-{arch}` pair used in GCS
/// archive names. Matches the matrix in .github/workflows/release.yml.
pub fn platform_arch() -> Result<(&'static str, &'static str)> {
    let platform = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        other => anyhow::bail!("unsupported OS for self-update: {}", other),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => anyhow::bail!("unsupported arch for self-update: {}", other),
    };
    Ok((platform, arch))
}

/// Per-call scratch dir (`$TMPDIR/krab-doctor-<pid>-<n>-<tag>`). The
/// counter keeps concurrent callers in one process (parallel unit
/// tests) from sharing a directory.
pub fn tempdir_for(tag: &str) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let safe: String = tag
        .chars()
        .map(|c| if c == '/' { '_' } else { c })
        .collect();
    let path =
        std::env::temp_dir().join(format!("krab-doctor-{}-{}-{}", std::process::id(), n, safe));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
    Ok(path)
}

/// Numeric dotted-version compare (`0.43.10 > 0.43.9`). Non-digit
/// characters are separators, so `v0.43.1` parses like `0.43.1`.
pub fn semver_cmp(a: &str, b: &str) -> Ordering {
    let parse = |s: &str| -> Vec<u32> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    parse(a).cmp(&parse(b))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Build a `krabarena-*.tar.gz` whose `krab` is a shell script that
    /// prints `version_output` for `--version`.
    fn fake_release(version_output: &str, exit_code: i32) -> Vec<u8> {
        use std::io::Write;
        let script = format!("#!/bin/sh\necho \"{version_output}\"\nexit {exit_code}\n");
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_size(script.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "krab", std::io::Cursor::new(script.as_bytes()))
                .unwrap();
            builder.finish().unwrap();
        }
        let mut gz = Vec::new();
        let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
        enc.write_all(&tar_buf).unwrap();
        enc.finish().unwrap();
        gz
    }

    fn sha_of(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        format!("{}  archive.tar.gz\n", hex::encode(h.finalize()))
    }

    /// Fake downloader: serves `.sha256` URLs from `sha`, everything
    /// else from `archive`. Records requested URLs.
    struct FakeFetcher {
        archive: Vec<u8>,
        sha: String,
        fail: bool,
        requested: RefCell<Vec<String>>,
    }

    impl FakeFetcher {
        fn new(archive: Vec<u8>) -> Self {
            let sha = sha_of(&archive);
            Self {
                archive,
                sha,
                fail: false,
                requested: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fetcher for FakeFetcher {
        async fn fetch(&self, url: &str, dest: &Path) -> Result<()> {
            self.requested.borrow_mut().push(url.to_string());
            if self.fail {
                anyhow::bail!("network unreachable (fake)");
            }
            if url.ends_with(".sha256") {
                std::fs::write(dest, &self.sha)?;
            } else {
                std::fs::write(dest, &self.archive)?;
            }
            Ok(())
        }
    }

    /// Temp install dir laid out like a real install: `krab-0.1.0` plus
    /// `krab → krab-0.1.0`.
    fn fake_install(tag: &str) -> PathBuf {
        let dir = tempdir_for(tag).unwrap();
        let old = dir.join("krab-0.1.0");
        std::fs::write(&old, "#!/bin/sh\necho \"krab 0.1.0\"\n").unwrap();
        set_executable(&old).unwrap();
        std::os::unix::fs::symlink(&old, dir.join("krab")).unwrap();
        dir
    }

    fn snapshot(dir: &Path) -> HashMap<String, String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let name = e.file_name().to_string_lossy().to_string();
                let link = std::fs::read_link(e.path())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                (name, link)
            })
            .collect()
    }

    fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn semver_cmp_orders_numerically() {
        assert_eq!(semver_cmp("0.43.10", "0.43.9"), Ordering::Greater);
        assert_eq!(semver_cmp("0.43.0", "0.43.0"), Ordering::Equal);
        assert_eq!(semver_cmp("0.42.9", "0.43.0"), Ordering::Less);
        assert_eq!(semver_cmp("v0.43.1", "0.43.1"), Ordering::Equal);
    }

    #[test]
    fn successful_update_swaps_symlink_atomically() {
        let dir = fake_install("su-success");
        let fetcher = FakeFetcher::new(fake_release("krab 9.9.9", 0));
        let out = block_on(self_update(&fetcher, &dir, Some("0.1.0"))).unwrap();
        assert_eq!(out.new_version, "9.9.9");
        assert_eq!(out.link_path, dir.join("krab"));
        assert_eq!(
            std::fs::read_link(dir.join("krab")).unwrap(),
            dir.join("krab-9.9.9")
        );
        // Old binary kept (GC keeps 3), no staging leftovers.
        assert!(dir.join("krab-0.1.0").exists());
        assert!(!dir.join("krab-9.9.9.tmp").exists());
        assert!(!dir.join("krab.next").exists());
        assert_eq!(probe_binary_version(&dir.join("krab")).unwrap(), "9.9.9");
        let urls = fetcher.requested.borrow();
        assert!(urls[0].starts_with(GCS_LATEST));
        assert!(urls[1].ends_with(".tar.gz.sha256"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn regular_file_install_is_migrated_to_symlink_layout() {
        let dir = tempdir_for("su-migrate").unwrap();
        std::fs::write(dir.join("krab"), "#!/bin/sh\necho \"krab 0.1.0\"\n").unwrap();
        let fetcher = FakeFetcher::new(fake_release("krab 9.9.9", 0));
        block_on(self_update(&fetcher, &dir, Some("0.1.0"))).unwrap();
        assert_eq!(
            std::fs::read_link(dir.join("krab")).unwrap(),
            dir.join("krab-9.9.9")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checksum_mismatch_leaves_install_untouched() {
        let dir = fake_install("su-badsha");
        let before = snapshot(&dir);
        let mut fetcher = FakeFetcher::new(fake_release("krab 9.9.9", 0));
        fetcher.sha = format!("{}  archive.tar.gz\n", "0".repeat(64));
        let err = block_on(self_update(&fetcher, &dir, Some("0.1.0"))).unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
        assert_eq!(snapshot(&dir), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn network_failure_leaves_install_untouched() {
        let dir = fake_install("su-nonet");
        let before = snapshot(&dir);
        let mut fetcher = FakeFetcher::new(fake_release("krab 9.9.9", 0));
        fetcher.fail = true;
        assert!(block_on(self_update(&fetcher, &dir, Some("0.1.0"))).is_err());
        assert_eq!(snapshot(&dir), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_archive_is_rejected() {
        let dir = fake_install("su-corrupt");
        let before = snapshot(&dir);
        let fetcher = FakeFetcher::new(b"definitely not a tarball".to_vec());
        assert!(block_on(self_update(&fetcher, &dir, Some("0.1.0"))).is_err());
        assert_eq!(snapshot(&dir), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_binary_is_rejected_before_install() {
        let dir = fake_install("su-broken");
        let before = snapshot(&dir);
        let fetcher = FakeFetcher::new(fake_release("krab 9.9.9", 3));
        assert!(block_on(self_update(&fetcher, &dir, Some("0.1.0"))).is_err());
        assert_eq!(snapshot(&dir), before);

        let fetcher = FakeFetcher::new(fake_release("Segmentation fault", 0));
        assert!(block_on(self_update(&fetcher, &dir, Some("0.1.0"))).is_err());
        assert_eq!(snapshot(&dir), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_not_newer_is_rejected() {
        let dir = fake_install("su-lagging");
        let before = snapshot(&dir);
        let fetcher = FakeFetcher::new(fake_release("krab 0.1.0", 0));
        let err = block_on(self_update(&fetcher, &dir, Some("0.1.0"))).unwrap_err();
        assert!(err.downcast_ref::<ReleaseNotNewer>().is_some(), "{err}");
        assert_eq!(snapshot(&dir), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_install_dir_is_skipped_without_download() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fake_install("su-readonly");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root ignores directory permissions; nothing to assert then.
        if ensure_writable(&dir).is_ok() {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let before = snapshot(&dir);
        let fetcher = FakeFetcher::new(fake_release("krab 9.9.9", 0));
        let err = block_on(self_update(&fetcher, &dir, Some("0.1.0"))).unwrap_err();
        assert!(err.to_string().contains("not writable"), "{err}");
        assert!(fetcher.requested.borrow().is_empty(), "must not download");
        assert_eq!(snapshot(&dir), before);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_symlink_swap_rolls_back_new_binary() {
        let dir = fake_install("su-rollback");
        // A non-empty directory named `krab` can't be replaced by
        // rename(2), so the swap step fails after `krab-9.9.9` landed.
        std::fs::remove_file(dir.join("krab")).unwrap();
        std::fs::create_dir(dir.join("krab")).unwrap();
        std::fs::write(dir.join("krab").join("blocker"), "x").unwrap();
        let before = snapshot(&dir);
        let fetcher = FakeFetcher::new(fake_release("krab 9.9.9", 0));
        let err = block_on(self_update(&fetcher, &dir, Some("0.1.0"))).unwrap_err();
        assert!(err.to_string().contains("symlink swap"), "{err}");
        assert_eq!(snapshot(&dir), before, "new binary must be rolled back");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
