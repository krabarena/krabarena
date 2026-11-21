use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Path as AxumPath;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use thiserror::Error;
use tokio::fs;
use tower_http::services::ServeDir;

const DIST_PREFIX: &str = "/static/dist/";
const IMMUTABLE_CACHE: &str = "public, max-age=604800, immutable";

/// Resolve the absolute path to the `static/` directory regardless of
/// process cwd. Cloud Run sets `STATIC_DIR=/app/static`; local cargo
/// runs usually start from either the crate or workspace root.
pub fn static_dir() -> PathBuf {
    if let Ok(p) = std::env::var("STATIC_DIR") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for candidate in [cwd.join("static"), cwd.join("crates/krabarena-api/static")] {
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("static")
}

/// Build the `/static` router. `/static/dist/*` is served by the
/// fingerprint-aware asset handler; every other legacy static path
/// falls back to the historical `ServeDir` behavior.
pub fn static_router<S>(static_root: PathBuf) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let dist_root = Arc::new(static_root.join("dist"));
    let route_root = Arc::clone(&dist_root);
    Router::<S>::new()
        .route(
            "/dist/{*path}",
            get(
                move |AxumPath(path): AxumPath<String>, headers: HeaderMap| {
                    let root = Arc::clone(&route_root);
                    async move { serve_dist_asset(root, path, headers).await }
                },
            ),
        )
        .fallback_service(ServeDir::new(static_root))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViteEntryAssets {
    pub script: String,
    pub stylesheets: Vec<String>,
    pub icon_sprite: Option<String>,
}

impl ViteEntryAssets {
    fn from_manifest_entry(entry: &ViteManifestEntry, manifest: &ViteManifest) -> Self {
        Self {
            script: public_dist_path(&entry.file),
            stylesheets: entry
                .css
                .iter()
                .map(|file| public_dist_path(file))
                .collect(),
            icon_sprite: icon_sprite_path(entry, manifest),
        }
    }

    fn dev_fallback(entrypoint: &str, dev_server_url: Option<&str>) -> Self {
        let dev_server_url = dev_server_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(|url| url.trim_end_matches('/'));
        let script = dev_server_url
            .map(|url| format!("{url}/{entrypoint}"))
            .unwrap_or_else(|| format!("{DIST_PREFIX}{entrypoint}"));
        let icon_sprite = dev_server_url
            .map(|url| format!("{url}/src/icons/krab-icons.svg"))
            .unwrap_or_else(|| format!("{DIST_PREFIX}icons/krab-icons.svg"));
        Self {
            script,
            stylesheets: Vec::new(),
            icon_sprite: Some(icon_sprite),
        }
    }
}

fn icon_sprite_path(entry: &ViteManifestEntry, manifest: &ViteManifest) -> Option<String> {
    entry
        .assets
        .iter()
        .find(|file| file.ends_with(".svg") && file.contains("krab-icons"))
        .or_else(|| {
            manifest
                .get("src/icons/krab-icons.svg")
                .map(|entry| &entry.file)
        })
        .map(|file| public_dist_path(file))
}

impl Default for ViteEntryAssets {
    fn default() -> Self {
        Self::dev_fallback("src/main.js", None)
    }
}

#[derive(Debug, Error)]
pub enum ViteManifestError {
    #[error("Vite manifest not found at {path}")]
    MissingManifest { path: String },
    #[error("failed to read Vite manifest at {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse Vite manifest at {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("Vite manifest entrypoint not found: {entrypoint}")]
    MissingEntrypoint { entrypoint: String },
}

#[derive(Debug, Deserialize)]
struct ViteManifestEntry {
    file: String,
    #[serde(default)]
    css: Vec<String>,
    #[serde(default)]
    assets: Vec<String>,
}

type ViteManifest = std::collections::BTreeMap<String, ViteManifestEntry>;

pub fn lookup_vite_entry(
    static_root: impl AsRef<Path>,
    entrypoint: &str,
) -> Result<ViteEntryAssets, ViteManifestError> {
    lookup_vite_entry_with_env(
        static_root.as_ref(),
        entrypoint,
        std::env::var("NODE_ENV").ok().as_deref() == Some("production"),
        std::env::var("VITE_DEV_SERVER_URL").ok().as_deref(),
    )
}

pub fn frontend_entry_assets() -> ViteEntryAssets {
    match lookup_vite_entry(static_dir(), "src/main.js") {
        Ok(assets) => assets,
        Err(error) => {
            tracing::warn!(error = %error, "frontend.assets.lookup_failed");
            ViteEntryAssets::default()
        }
    }
}

fn lookup_vite_entry_with_env(
    static_root: &Path,
    entrypoint: &str,
    production: bool,
    dev_server_url: Option<&str>,
) -> Result<ViteEntryAssets, ViteManifestError> {
    let path = manifest_path(static_root);
    if !path.exists() {
        if production {
            return Err(ViteManifestError::MissingManifest {
                path: path.display().to_string(),
            });
        }
        return Ok(ViteEntryAssets::dev_fallback(entrypoint, dev_server_url));
    }

    let manifest = read_manifest(&path)?;
    let entry = manifest
        .get(entrypoint)
        .ok_or_else(|| ViteManifestError::MissingEntrypoint {
            entrypoint: entrypoint.to_string(),
        })?;
    Ok(ViteEntryAssets::from_manifest_entry(entry, &manifest))
}

fn manifest_path(static_root: &Path) -> PathBuf {
    static_root.join("dist").join(".vite").join("manifest.json")
}

fn read_manifest(path: &Path) -> Result<ViteManifest, ViteManifestError> {
    let body = std::fs::read_to_string(path).map_err(|source| ViteManifestError::Read {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&body).map_err(|source| ViteManifestError::Parse {
        path: path.display().to_string(),
        source,
    })
}

fn public_dist_path(file: &str) -> String {
    format!("{DIST_PREFIX}{}", file.trim_start_matches('/'))
}

async fn serve_dist_asset(dist_root: Arc<PathBuf>, path: String, headers: HeaderMap) -> Response {
    let Some(relative_path) = sanitize_asset_path(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if relative_path
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == ".vite")
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    let asset_path = dist_root.join(&relative_path);
    if !asset_path.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let accepts_gzip = accepts_gzip(&headers);
    let gzip_path = asset_path.with_file_name(format!(
        "{}.gz",
        asset_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ));
    let (body_path, gzip) = if accepts_gzip && gzip_path.is_file() {
        (gzip_path, true)
    } else {
        (asset_path, false)
    };

    let bytes = match fs::read(&body_path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(path = %body_path.display(), error = %error, "static.dist.read_failed");
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for(&relative_path)),
    );
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if is_hashed_asset(&relative_path) {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(IMMUTABLE_CACHE),
        );
    }
    if gzip {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    }
    response
}

fn sanitize_asset_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return None;
    }
    let path = Path::new(path);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            _ => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|encoding| encoding.trim().eq_ignore_ascii_case("gzip"))
        })
}

fn is_hashed_asset(path: &Path) -> bool {
    path.components()
        .next()
        .is_some_and(|component| component.as_os_str() == "assets")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.rsplit_once('-').is_some())
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" | "webmanifest" => "application/json; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "html" => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs;
    use std::io::Write;
    use tower::ServiceExt;
    use uuid::Uuid;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("krabarena-assets-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).expect("create test dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, path: &str, body: impl AsRef<[u8]>) {
            let path = self.0.join(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(path, body).expect("write file");
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    async fn body_bytes(body: Body) -> Vec<u8> {
        to_bytes(body, usize::MAX)
            .await
            .expect("read body")
            .to_vec()
    }

    fn gzip_bytes(body: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).expect("gzip write");
        encoder.finish().expect("gzip finish")
    }

    #[test]
    fn manifest_lookup_returns_script_and_css_paths() {
        let root = TestDir::new();
        root.write(
            "dist/.vite/manifest.json",
            r#"{
              "src/main.js": {
                "file": "assets/main-DL2p8Qz.js",
                "css": ["assets/main-BhNrs1.css"],
                "assets": ["assets/krab-icons-B7Tc2.svg"],
                "isEntry": true
              }
            }"#,
        );

        let entry =
            lookup_vite_entry_with_env(root.path(), "src/main.js", true, None).expect("lookup");
        assert_eq!(entry.script, "/static/dist/assets/main-DL2p8Qz.js");
        assert_eq!(
            entry.stylesheets,
            vec!["/static/dist/assets/main-BhNrs1.css"]
        );
        assert_eq!(
            entry.icon_sprite,
            Some("/static/dist/assets/krab-icons-B7Tc2.svg".to_string())
        );
    }

    #[test]
    fn manifest_lookup_uses_separate_icon_sprite_entry() {
        let root = TestDir::new();
        root.write(
            "dist/.vite/manifest.json",
            r#"{
              "src/icons/krab-icons.svg": {
                "file": "assets/krab-icons-CQIGh_sJ.svg",
                "src": "src/icons/krab-icons.svg"
              },
              "src/main.js": {
                "file": "assets/main-DL2p8Qz.js",
                "css": ["assets/main-BhNrs1.css"],
                "isEntry": true
              }
            }"#,
        );

        let entry =
            lookup_vite_entry_with_env(root.path(), "src/main.js", true, None).expect("lookup");
        assert_eq!(
            entry.icon_sprite,
            Some("/static/dist/assets/krab-icons-CQIGh_sJ.svg".to_string())
        );
    }

    #[test]
    fn manifest_lookup_reports_missing_entrypoint() {
        let root = TestDir::new();
        root.write(
            "dist/.vite/manifest.json",
            r#"{"src/main.js":{"file":"x.js"}}"#,
        );

        let err = lookup_vite_entry_with_env(root.path(), "src/missing.js", true, None)
            .expect_err("missing entry");
        assert!(err.to_string().contains("src/missing.js"));
    }

    #[test]
    fn manifest_lookup_uses_dev_fallback_when_manifest_is_absent() {
        let root = TestDir::new();
        let entry = lookup_vite_entry_with_env(
            root.path(),
            "src/main.js",
            false,
            Some("http://127.0.0.1:5173/"),
        )
        .expect("dev fallback");

        assert_eq!(entry.script, "http://127.0.0.1:5173/src/main.js");
        assert!(entry.stylesheets.is_empty());
        assert_eq!(
            entry.icon_sprite,
            Some("http://127.0.0.1:5173/src/icons/krab-icons.svg".to_string())
        );
    }

    #[tokio::test]
    async fn dist_asset_serves_plain_file_without_gzip() {
        let root = TestDir::new();
        root.write("dist/assets/main-DL2p8Qz.js", b"console.log('ok');");
        root.write(
            "dist/assets/main-DL2p8Qz.js.gz",
            gzip_bytes(b"console.log('ok');"),
        );

        let response = static_router(root.path().to_path_buf())
            .oneshot(
                Request::builder()
                    .uri("/dist/assets/main-DL2p8Qz.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            IMMUTABLE_CACHE
        );
        assert_eq!(
            body_bytes(response.into_body()).await,
            b"console.log('ok');"
        );
    }

    #[tokio::test]
    async fn dist_asset_serves_gzip_when_accepted() {
        let root = TestDir::new();
        let gz = gzip_bytes(b"body{}");
        root.write("dist/assets/main-BhNrs1.css", b"body{}");
        root.write("dist/assets/main-BhNrs1.css.gz", &gz);

        let response = static_router(root.path().to_path_buf())
            .oneshot(
                Request::builder()
                    .uri("/dist/assets/main-BhNrs1.css")
                    .header(header::ACCEPT_ENCODING, "br, gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_ENCODING).unwrap(),
            "gzip"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            response.headers().get(header::VARY).unwrap(),
            "Accept-Encoding"
        );
        assert_eq!(body_bytes(response.into_body()).await, gz);
    }

    #[tokio::test]
    async fn dist_asset_does_not_serve_manifest() {
        let root = TestDir::new();
        root.write("dist/.vite/manifest.json", "{}");

        let response = static_router(root.path().to_path_buf())
            .oneshot(
                Request::builder()
                    .uri("/dist/.vite/manifest.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn dist_asset_rejects_path_traversal() {
        let root = TestDir::new();
        let response = serve_dist_asset(
            Arc::new(root.path().join("dist")),
            "../theme-forum.css".into(),
            HeaderMap::new(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn static_router_keeps_legacy_static_paths() {
        let root = TestDir::new();
        root.write("theme-forum.css", "body{}");
        root.write("demo/index.html", "<h1>Demo</h1>");

        let css_response = static_router(root.path().to_path_buf())
            .oneshot(
                Request::builder()
                    .uri("/theme-forum.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(css_response.status(), StatusCode::OK);

        let demo_response = static_router(root.path().to_path_buf())
            .oneshot(
                Request::builder()
                    .uri("/demo/index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(demo_response.status(), StatusCode::OK);
    }
}
