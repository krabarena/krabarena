//! Manifest: bundled at compile time. The CLI always runs against its own
//! compiled-in manifest so `--help` is offline-safe and behavior matches the
//! binary. `manifest-refresh` is a diagnostic that writes the server's view
//! to disk for inspection; it is not loaded back at startup.

use anyhow::Result;
use krabarena_shared::manifest::CliManifest;
use std::path::PathBuf;
use std::time::Duration;

fn diagnostic_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".krab")
        .join("manifest.json")
}

/// The manifest used at startup — always the compiled-in version.
pub fn load_cached_or_builtin() -> CliManifest {
    krabarena_shared::manifest_builder::build_manifest()
}

/// Drop the on-disk server manifest snapshot after a binary update so
/// nothing (doctor, humans diffing it) compares against a stale copy.
/// The new binary's compiled-in manifest is authoritative. Best-effort.
pub fn invalidate_cache() {
    let _ = std::fs::remove_file(diagnostic_path());
}

/// Fetch the server's manifest and write it to `~/.krab/manifest.json` for
/// inspection / diffing against the bundled version. The CLI does not read it
/// back; the bundled manifest is always authoritative for this binary.
pub async fn refresh(api_url: &str, token: Option<&str>) -> Result<()> {
    let m = fetch_from_server(api_url, token).await?;
    let path = diagnostic_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&m)?)?;
    eprintln!(
        "Wrote server manifest ({} commands) to {}",
        m.commands.len(),
        path.display()
    );
    Ok(())
}

async fn fetch_from_server(api_url: &str, token: Option<&str>) -> Result<CliManifest> {
    let url = format!("{}/v1/cli/manifest", api_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.get(&url).timeout(Duration::from_secs(5));
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {}", t));
    }
    let resp = req.send().await?;
    let manifest: CliManifest = resp.json().await?;
    Ok(manifest)
}
