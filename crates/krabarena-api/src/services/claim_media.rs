//! Inline-media + attachment pipeline for claim bodies.
//!
//! On every `POST /v1/battles/{id}/claims` we walk the body for two
//! kinds of references:
//!
//! 1. **Image refs** — `![alt](images/foo.png)`, where the path is
//!    relative to the artifact bundle. The image is extracted,
//!    MIME-sniffed, hashed, run through GCP Vision SafeSearch, and
//!    uploaded to the public CDN at
//!    `bundles/<artifact_hash>/<bundle_path>` (folder structure
//!    preserved). The body is rewritten so `![alt](path)` becomes
//!    `![alt](public_url)`.
//!
//! 2. **Attachment refs** — `![alt](report.html)` and
//!    `![alt](notes.md)`. These refs are STRIPPED from the body. The
//!    referenced file is uploaded as-is to the same CDN prefix, with
//!    every transitive image dep (`<img src>` for HTML, `![]()` for
//!    markdown) extracted, moderated, and uploaded alongside —
//!    keeping the bundle's folder structure on the CDN. The result is
//!    a self-contained CDN-hosted page: when a reader opens the
//!    attachment, all relative `<img>` refs resolve via the preserved
//!    folder layout. The UI surfaces these as link cards under the
//!    "Show full report" cut, opening in a new tab.
//!
//! HTML attachments are sanitized before upload: `<script>...</script>`
//! and `on*=` event-handler attributes are stripped. `<style>` and
//! everything else (CSS classes, `<table>`, `<figure>`, etc.) are
//! preserved so the original layout survives.
//!
//! # Caps (enforced before any network call)
//!
//! - `MAX_IMAGES_PER_CLAIM = 100` — sized for 25-site
//!   "original × regenerated" visual-comparison reports (50 images)
//!   with headroom.
//! - `MAX_BYTES_PER_IMAGE = 1_000_000` (1 MB; this matches the FileMap's
//!   per-entry cap so an oversized image is already impossible — this
//!   constant is the friendly version we surface as
//!   `CLAIM_MEDIA_TOO_LARGE`).
//!
//! FileMap's own 10 MB per-tarball cap binds the aggregate, so a
//! separate total-bytes cap inside this module is redundant.
//!
//! # Fail-closed
//!
//! Vision SafeSearch is required for every IMAGE. If the API call
//! fails for any reason (network, auth, 5xx) the whole submission
//! fails with `ClaimMediaModerationUnavailable` (HTTP 503). We never
//! accept an image that hasn't been moderated. HTML/MD attachments
//! themselves are not Vision-moderated (they're text); their
//! transitive image deps are.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;

use crate::error::ApiError;
use crate::services::review_tools::FileMap;

// -- Caps -------------------------------------------------------------

const MAX_IMAGES_PER_CLAIM: usize = 100;
const MAX_BYTES_PER_IMAGE: usize = 1_000_000;

// -- Public types -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    HtmlAttachment,
    MarkdownAttachment,
}

impl MediaKind {
    pub fn as_db_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::HtmlAttachment => "html_attachment",
            MediaKind::MarkdownAttachment => "markdown_attachment",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessedMedia {
    pub rewritten_body: String,
    pub records: Vec<MediaRecord>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MediaRecord {
    pub sha256: String,
    pub bundle_path: String,
    pub public_url: String,
    pub mime_type: String,
    pub bytes: i64,
    pub kind: MediaKind,
    /// Author-provided alt text from the original `![alt](path)` ref
    /// in the body. Carried for tests + future UI use; not currently
    /// persisted (the UI falls back to `bundle_path`).
    pub label: String,
    pub moderation: serde_json::Value,
}

// -- Internal sink trait so unit tests don't hit the network ----------

#[derive(Debug, Clone)]
pub(crate) struct ModerationResult {
    pub passes: bool,
    pub reason: Option<String>,
    pub raw: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MediaError {
    #[error("moderation unavailable: {0}")]
    ModerationUnavailable(String),
    #[error("upload failed: {0}")]
    UploadFailed(String),
}

pub(crate) trait MediaSink {
    fn moderate(
        &self,
        mime: &str,
        bytes: &[u8],
    ) -> impl std::future::Future<Output = Result<ModerationResult, MediaError>> + Send;

    fn upload(
        &self,
        object_name: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> impl std::future::Future<Output = Result<String, MediaError>> + Send;
}

// -- Public entry point ----------------------------------------------

/// Process inline media + attachment references in `claim_body`.
///
/// `artifact_hash` is the SHA-256 of the uploaded artifact tarball
/// (already computed in `routes::claims::create_claim`); it forms the
/// CDN path prefix `bundles/<artifact_hash>/`. Two claims with
/// byte-identical bundles share the same prefix, giving us
/// content-addressable dedup at the bundle level.
pub async fn process(
    blob_store_url: &str,
    artifact_hash: &str,
    artifact_bytes: &[u8],
    artifact_filename: Option<&str>,
    claim_body: &str,
) -> Result<ProcessedMedia, ApiError> {
    let sink = GcsVisionSink {
        blob_store_url: blob_store_url.to_string(),
    };
    process_with_sink(
        &sink,
        artifact_hash,
        artifact_bytes,
        artifact_filename,
        claim_body,
    )
    .await
}

/// Persist accepted media records under `claim_id`. Idempotent — the
/// `(claim_id, sha256)` unique index makes a re-run a no-op.
pub async fn persist(
    pool: &sqlx::PgPool,
    claim_id: Uuid,
    records: &[MediaRecord],
) -> Result<(), ApiError> {
    for r in records {
        sqlx::query(
            "INSERT INTO claim_media \
             (claim_id, sha256, bundle_path, public_url, mime_type, bytes, kind, \
              moderation_verdict, moderation_categories) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'pass', $8) \
             ON CONFLICT (claim_id, sha256) DO NOTHING",
        )
        .bind(claim_id)
        .bind(&r.sha256)
        .bind(&r.bundle_path)
        .bind(&r.public_url)
        .bind(&r.mime_type)
        .bind(r.bytes)
        .bind(r.kind.as_db_str())
        .bind(&r.moderation)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    }
    Ok(())
}

// -- Core pipeline (sink-injected for tests) -------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PendingUpload {
    /// Stored redundantly with the HashMap key for readability.
    bundle_path: String,
    bytes: Vec<u8>,
    mime: String,
    kind: MediaKind,
    /// Author-provided alt text (only meaningful for refs that came
    /// straight from the body — transitive deps inherit empty).
    label: String,
}

pub(crate) async fn process_with_sink(
    sink: &(impl MediaSink + Sync),
    artifact_hash: &str,
    artifact_bytes: &[u8],
    artifact_filename: Option<&str>,
    claim_body: &str,
) -> Result<ProcessedMedia, ApiError> {
    // 1. Build the FileMap once. Single-file fallback so a one-image
    //    artifact still gets a sensible "missing" error rather than a
    //    parse error.
    let file_map = match FileMap::from_tar_gz(artifact_bytes) {
        Ok(m) => m,
        Err(_) => FileMap::from_single_file(artifact_bytes, artifact_filename),
    };

    // 2. Find every `![alt](path)` ref in the body. If none, cheap
    //    exit.
    let body_refs = find_image_refs(claim_body);
    if body_refs.is_empty() {
        return Ok(ProcessedMedia {
            rewritten_body: claim_body.to_string(),
            records: Vec::new(),
        });
    }

    // 3. Walk every body ref:
    //    - external (https://, data:, gs://) → leave alone
    //    - .html / .md → attachment + recurse for image deps
    //    - other → image
    let mut to_upload: HashMap<String, PendingUpload> = HashMap::new();

    for r in &body_refs {
        if is_external(&r.url) {
            continue;
        }
        let normalized = normalize_path(&r.url);
        if to_upload.contains_key(&normalized) {
            continue;
        }
        match attachment_kind(&r.url) {
            Some(kind) => {
                ingest_attachment(&file_map, &normalized, &r.alt, kind, &mut to_upload)?;
            }
            None => {
                ingest_image(&file_map, &normalized, &r.alt, &mut to_upload)?;
            }
        }
    }

    // 4. Cap on number of images.
    let n_images = to_upload
        .values()
        .filter(|u| u.kind == MediaKind::Image)
        .count();
    if n_images > MAX_IMAGES_PER_CLAIM {
        return Err(ApiError::ClaimMediaTooMany);
    }

    // 5. Vision SafeSearch on every image (only).
    let mut moderation_by_path: HashMap<String, serde_json::Value> = HashMap::new();
    for (path, u) in &to_upload {
        if u.kind != MediaKind::Image {
            continue;
        }
        let result = sink.moderate(&u.mime, &u.bytes).await.map_err(|e| {
            tracing::error!(error = %e, path = %path, "Vision SafeSearch failed");
            ApiError::ClaimMediaModerationUnavailable
        })?;
        if !result.passes {
            return Err(ApiError::ClaimMediaRejected {
                path: path.clone(),
                reason: result.reason.unwrap_or_else(|| "blocked".to_string()),
            });
        }
        moderation_by_path.insert(path.clone(), result.raw);
    }

    // 6. Upload everything to GCS preserving paths under
    //    `bundles/<artifact_hash>/<bundle_path>`. HTML attachments are
    //    sanitized first (script + on-event attributes stripped).
    let mut records: Vec<MediaRecord> = Vec::with_capacity(to_upload.len());
    let mut public_url_by_path: HashMap<String, String> = HashMap::new();
    for (path, u) in &to_upload {
        let payload: Vec<u8> = if u.kind == MediaKind::HtmlAttachment {
            // Sanitize HTML before publication. Keeps <style>, classes,
            // and layout; strips <script> and on*= attrs so a malicious
            // bundle can't ship JS that runs when a reader opens the
            // CDN-hosted page.
            let html = std::str::from_utf8(&u.bytes).unwrap_or("");
            sanitize_html_for_publish(html).into_bytes()
        } else {
            u.bytes.clone()
        };
        let object_name = format!("bundles/{}/{}", artifact_hash, path);
        let public_url = sink
            .upload(&object_name, &u.mime, &payload)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, path = %path, "media upload failed");
                ApiError::ArtifactUnavailable
            })?;
        let sha = format!("sha256:{}", hex::encode(Sha256::digest(&payload)));
        records.push(MediaRecord {
            sha256: sha,
            bundle_path: path.clone(),
            public_url: public_url.clone(),
            mime_type: u.mime.clone(),
            bytes: payload.len() as i64,
            kind: u.kind,
            label: u.label.clone(),
            moderation: moderation_by_path
                .get(path)
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        });
        public_url_by_path.insert(path.clone(), public_url);
    }

    records.sort_by(|a, b| a.bundle_path.cmp(&b.bundle_path));

    // 7. Body rewrite:
    //    - For images: `![alt](path)` → `![alt](public_url)`
    //    - For attachments: strip the `![alt](path)` token entirely
    //      (it surfaces in the UI's Attachments section instead)
    let rewritten_body = rewrite_body(claim_body, &body_refs, &public_url_by_path);

    Ok(ProcessedMedia {
        rewritten_body,
        records,
    })
}

/// Resolve an attachment ref: read it from the bundle, walk for
/// transitive image deps, ingest each into `to_upload`. The
/// attachment itself ends up in `to_upload` with kind=Html/Markdown.
fn ingest_attachment(
    file_map: &FileMap,
    bundle_path: &str,
    label: &str,
    kind: AttachmentKind,
    to_upload: &mut HashMap<String, PendingUpload>,
) -> Result<(), ApiError> {
    let bytes =
        bundle_lookup(file_map, bundle_path).ok_or_else(|| ApiError::ClaimMediaMissing {
            path: bundle_path.to_string(),
        })?;
    let text = std::str::from_utf8(bytes).map_err(|_| ApiError::ClaimMediaUnsupportedType {
        path: bundle_path.to_string(),
    })?;
    let base_dir = parent_dir(bundle_path);
    let dep_paths = match kind {
        AttachmentKind::Html => find_html_image_deps(text, &base_dir),
        AttachmentKind::Markdown => find_markdown_image_deps(text, &base_dir),
    };
    let media_kind = match kind {
        AttachmentKind::Html => MediaKind::HtmlAttachment,
        AttachmentKind::Markdown => MediaKind::MarkdownAttachment,
    };
    let mime = match kind {
        AttachmentKind::Html => "text/html; charset=utf-8".to_string(),
        AttachmentKind::Markdown => "text/markdown; charset=utf-8".to_string(),
    };
    to_upload.insert(
        bundle_path.to_string(),
        PendingUpload {
            bundle_path: bundle_path.to_string(),
            bytes: bytes.to_vec(),
            mime,
            kind: media_kind,
            label: label.to_string(),
        },
    );
    for dep in dep_paths {
        if to_upload.contains_key(&dep) {
            continue;
        }
        // Defense in depth: an attachment referencing parent dirs is
        // likely a bug or attempt to escape the bundle root. Skip
        // such refs silently — the CDN-hosted attachment will show
        // a broken image, not load some sibling.
        if dep.contains("..") {
            continue;
        }
        ingest_image(file_map, &dep, "", to_upload)?;
    }
    Ok(())
}

fn ingest_image(
    file_map: &FileMap,
    bundle_path: &str,
    label: &str,
    to_upload: &mut HashMap<String, PendingUpload>,
) -> Result<(), ApiError> {
    let bytes =
        bundle_lookup(file_map, bundle_path).ok_or_else(|| ApiError::ClaimMediaMissing {
            path: bundle_path.to_string(),
        })?;
    if bytes.len() > MAX_BYTES_PER_IMAGE {
        return Err(ApiError::ClaimMediaTooLarge {
            path: bundle_path.to_string(),
        });
    }
    let mime = sniff_image_mime(bytes).ok_or_else(|| ApiError::ClaimMediaUnsupportedType {
        path: bundle_path.to_string(),
    })?;
    to_upload.insert(
        bundle_path.to_string(),
        PendingUpload {
            bundle_path: bundle_path.to_string(),
            bytes: bytes.to_vec(),
            mime,
            kind: MediaKind::Image,
            label: label.to_string(),
        },
    );
    Ok(())
}

// -- Helpers ---------------------------------------------------------

#[derive(Debug, Clone)]
struct ImageRef {
    full: String,
    start: usize,
    len: usize,
    alt: String,
    url: String,
}

fn find_image_refs(body: &str) -> Vec<ImageRef> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    // First pass: walk the markdown event stream to map every byte
    // range that is INSIDE inline code (`...`) or a fenced/indented
    // code block. Image-like syntax inside those is literal text, not
    // a markdown image — the regex must skip those matches.
    let parser = Parser::new_ext(body, Options::empty()).into_offset_iter();
    let mut excluded: Vec<std::ops::Range<usize>> = Vec::new();
    let mut code_block_start: Option<usize> = None;
    for (event, range) in parser {
        match event {
            Event::Code(_) => excluded.push(range),
            Event::Start(Tag::CodeBlock(_)) => code_block_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_block_start.take() {
                    excluded.push(start..range.end);
                }
            }
            _ => {}
        }
    }

    let re = regex::Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").expect("static regex");
    re.captures_iter(body)
        .filter_map(|cap| {
            let m = cap.get(0).expect("group 0");
            if excluded.iter().any(|r| r.contains(&m.start())) {
                return None;
            }
            Some(ImageRef {
                full: m.as_str().to_string(),
                start: m.start(),
                len: m.end() - m.start(),
                alt: cap
                    .get(1)
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_default(),
                url: cap
                    .get(2)
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum AttachmentKind {
    Html,
    Markdown,
}

fn attachment_kind(url: &str) -> Option<AttachmentKind> {
    if is_external(url) {
        return None;
    }
    let lower = url.trim().to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        Some(AttachmentKind::Html)
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") {
        Some(AttachmentKind::Markdown)
    } else {
        None
    }
}

fn is_external(url: &str) -> bool {
    let l = url.trim().to_ascii_lowercase();
    l.starts_with("http://")
        || l.starts_with("https://")
        || l.starts_with("data:")
        || l.starts_with("gs://")
}

/// Strip leading `./` and `/`, leaving a relative path.
fn normalize_path(path: &str) -> String {
    let mut p = path.trim();
    while let Some(rest) = p.strip_prefix("./") {
        p = rest;
    }
    p.trim_start_matches('/').to_string()
}

/// Sniff bytes; return a canonical MIME for the four allowed image types.
fn sniff_image_mime(bytes: &[u8]) -> Option<String> {
    let kind = infer::get(bytes)?;
    match kind.mime_type() {
        "image/png" | "image/jpeg" | "image/webp" | "image/gif" => {
            Some(kind.mime_type().to_string())
        }
        _ => None,
    }
}

/// `tar -czf bundle.tar.gz .` produces entries prefixed with `./`,
/// while `tar -czf bundle.tar.gz file1 file2` produces bare paths.
fn bundle_lookup<'a>(file_map: &'a FileMap, normalized: &str) -> Option<&'a [u8]> {
    file_map
        .read_bytes(normalized)
        .or_else(|| file_map.read_bytes(&format!("./{normalized}")))
}

/// `"a/b/c.html"` → `"a/b"`; `"x.html"` → `""`.
fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
    }
}

/// Resolve a relative path against `base_dir`. External URLs and
/// `..`-bearing paths pass through; the latter will be skipped at
/// ingest time as defense in depth.
fn resolve_relative(src: &str, base_dir: &str) -> String {
    let trimmed = src.trim();
    if is_external(trimmed) {
        return src.to_string();
    }
    let s = trimmed.trim_start_matches("./");
    if s.contains("..") {
        return src.to_string();
    }
    if base_dir.is_empty() || s.starts_with('/') {
        return s.trim_start_matches('/').to_string();
    }
    format!("{base_dir}/{s}")
}

/// Find every `<img src="...">` and `<source srcset="...">` in `html`
/// and resolve each relative path against `base_dir`. External URLs
/// are passed through (so they aren't ingested).
fn find_html_image_deps(html: &str, base_dir: &str) -> Vec<String> {
    let re =
        regex::Regex::new(r#"(?i)<(?:img|source)\b[^>]*?\b(?:src|srcset)\s*=\s*["']([^"']+)["']"#)
            .expect("static regex");
    re.captures_iter(html)
        .filter_map(|cap| {
            let raw = cap.get(1)?.as_str();
            if is_external(raw) {
                return None;
            }
            Some(resolve_relative(raw, base_dir))
        })
        .map(|s| normalize_path(&s))
        .collect()
}

/// Find every `![]()` ref in markdown and resolve each relative path
/// against `base_dir`.
fn find_markdown_image_deps(md: &str, base_dir: &str) -> Vec<String> {
    let re = regex::Regex::new(r"!\[[^\]]*\]\(([^)]+)\)").expect("static regex");
    re.captures_iter(md)
        .filter_map(|cap| {
            let raw = cap.get(1)?.as_str();
            if is_external(raw) {
                return None;
            }
            Some(resolve_relative(raw, base_dir))
        })
        .map(|s| normalize_path(&s))
        .collect()
}

/// Strip `<script>` blocks and `on*=` event-handler attributes.
/// Keeps `<style>`, classes, layout attrs — so the original layout
/// of a Playwright-generated `report.html` survives unchanged.
fn sanitize_html_for_publish(html: &str) -> String {
    let script = regex::Regex::new(r"(?is)<script\b[^>]*>.*?</script>").expect("static");
    let on_dq = regex::Regex::new(r#"(?is)\s+on[a-z]+\s*=\s*"[^"]*""#).expect("static");
    let on_sq = regex::Regex::new(r#"(?is)\s+on[a-z]+\s*=\s*'[^']*'"#).expect("static");
    let on_bare = regex::Regex::new(r"(?is)\s+on[a-z]+\s*=\s*[^\s>]+").expect("static");
    let js_url = regex::Regex::new(r"(?i)javascript:").expect("static");
    let s = script.replace_all(html, "").into_owned();
    let s = on_dq.replace_all(&s, "").into_owned();
    let s = on_sq.replace_all(&s, "").into_owned();
    let s = on_bare.replace_all(&s, "").into_owned();
    js_url.replace_all(&s, "").into_owned()
}

/// Rewrite the body. Each `![alt](path)` ref:
///   * If `path` resolved to a public URL (= it's an image we
///     uploaded), replace with `![alt](public_url)`.
///   * If `path` was an attachment (no public URL we'd substitute
///     into `![]()` form, since the attachment renders separately),
///     strip the entire ref token.
///   * Otherwise (external URL, etc.) leave verbatim.
fn rewrite_body(body: &str, refs: &[ImageRef], public_by_path: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut cursor = 0usize;
    for r in refs {
        if r.start < cursor {
            continue;
        }
        out.push_str(&body[cursor..r.start]);
        let normalized = normalize_path(&r.url);
        if !is_external(&r.url) && attachment_kind(&r.url).is_some() {
            // Strip — attachment renders in the Attachments section.
        } else if let Some(public_url) = public_by_path.get(&normalized) {
            out.push_str("![");
            out.push_str(&r.alt);
            out.push_str("](");
            out.push_str(public_url);
            out.push(')');
        } else {
            // External or unprocessed — pass through unchanged.
            out.push_str(&r.full);
        }
        cursor = r.start + r.len;
    }
    out.push_str(&body[cursor..]);
    out
}

// -- Real GCS + Vision implementation --------------------------------

struct GcsVisionSink {
    blob_store_url: String,
}

impl MediaSink for GcsVisionSink {
    async fn moderate(&self, _mime: &str, bytes: &[u8]) -> Result<ModerationResult, MediaError> {
        if !self.blob_store_url.starts_with("gs://") {
            tracing::warn!("media moderation: dev mode, skipping Vision SafeSearch");
            return Ok(ModerationResult {
                passes: true,
                reason: None,
                raw: serde_json::json!({"dev_mode": true}),
            });
        }

        let token = get_gcp_access_token()
            .await
            .map_err(|e| MediaError::ModerationUnavailable(e.to_string()))?;

        let body = serde_json::json!({
            "requests": [{
                "image": {"content": B64.encode(bytes)},
                "features": [{"type": "SAFE_SEARCH_DETECTION"}]
            }]
        });

        let client = reqwest::Client::new();
        let resp = client
            .post("https://vision.googleapis.com/v1/images:annotate")
            .header("authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .map_err(|e| MediaError::ModerationUnavailable(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(MediaError::ModerationUnavailable(format!(
                "Vision API returned {}: {}",
                status, body
            )));
        }

        let parsed: VisionResponse = resp
            .json()
            .await
            .map_err(|e| MediaError::ModerationUnavailable(e.to_string()))?;

        let safe = parsed
            .responses
            .into_iter()
            .next()
            .and_then(|r| r.safe_search_annotation)
            .ok_or_else(|| {
                MediaError::ModerationUnavailable(
                    "Vision response missing safeSearchAnnotation".into(),
                )
            })?;

        let raw = serde_json::to_value(&safe).unwrap_or(serde_json::Value::Null);
        match rejects(&safe) {
            Some(reason) => Ok(ModerationResult {
                passes: false,
                reason: Some(reason),
                raw,
            }),
            None => Ok(ModerationResult {
                passes: true,
                reason: None,
                raw,
            }),
        }
    }

    async fn upload(
        &self,
        object_name: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<String, MediaError> {
        if let Some(bucket) = self.blob_store_url.strip_prefix("gs://") {
            let token = get_gcp_access_token()
                .await
                .map_err(|e| MediaError::UploadFailed(e.to_string()))?;
            let url = format!(
                "https://storage.googleapis.com/upload/storage/v1/b/{}/o?uploadType=media&name={}",
                bucket,
                urlencoding::encode(object_name),
            );
            let client = reqwest::Client::new();
            let resp = client
                .post(&url)
                .header("authorization", format!("Bearer {}", token))
                .header("content-type", content_type)
                .body(bytes.to_vec())
                .send()
                .await
                .map_err(|e| MediaError::UploadFailed(e.to_string()))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(MediaError::UploadFailed(format!("{}: {}", status, body)));
            }
            tracing::info!(bucket = %bucket, object = %object_name, "media uploaded to GCS");
            Ok(format!(
                "https://storage.googleapis.com/{}/{}",
                bucket, object_name
            ))
        } else {
            // Dev fallback — don't actually upload, but return a stable URL.
            let url = format!("{}/{}", self.blob_store_url, object_name);
            tracing::warn!(object = %object_name, "media upload: dev mode, not stored");
            Ok(url)
        }
    }
}

#[derive(Deserialize)]
struct VisionResponse {
    #[serde(default)]
    responses: Vec<VisionAnnotateResponse>,
}

#[derive(Deserialize)]
struct VisionAnnotateResponse {
    #[serde(rename = "safeSearchAnnotation", default)]
    safe_search_annotation: Option<SafeSearch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SafeSearch {
    #[serde(default)]
    adult: String,
    #[serde(default)]
    spoof: String,
    #[serde(default)]
    medical: String,
    #[serde(default)]
    violence: String,
    #[serde(default)]
    racy: String,
}

fn rejects(safe: &SafeSearch) -> Option<String> {
    fn at_least_likely(s: &str) -> bool {
        matches!(s, "LIKELY" | "VERY_LIKELY")
    }
    if at_least_likely(&safe.adult) {
        return Some(format!("adult: {}", safe.adult));
    }
    if at_least_likely(&safe.violence) {
        return Some(format!("violence: {}", safe.violence));
    }
    if safe.racy == "VERY_LIKELY" {
        return Some(format!("racy: {}", safe.racy));
    }
    None
}

/// Get an access token from the GCP metadata server. Same helper as
/// `artifact_service`'s — duplicated here per spec because extracting
/// a shared module isn't worth the churn for 12 lines.
async fn get_gcp_access_token() -> Result<String, anyhow::Error> {
    let client = reqwest::Client::new();
    let resp = client
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
        .header("Metadata-Flavor", "Google")
        .send()
        .await?;

    let body: serde_json::Value = resp.json().await?;
    body["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no access_token in metadata response"))
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Write;

    /// A single-pixel valid PNG.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x62, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x01, 0xe5, 0x27, 0xde, 0xfc, 0x00, 0x00, 0x00, 0x00, b'I', b'E',
        b'N', b'D', 0xae, 0x42, 0x60, 0x82,
    ];

    #[allow(dead_code)]
    const PNG_OTHER: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x7f,
        0xa8, 0x7d, 0x63, 0x00, 0x00, 0x00, 0x0c, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x62, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x01, 0xe5, 0x27, 0xde, 0xfc, 0x00, 0x00, 0x00, 0x00, b'I', b'E',
        b'N', b'D', 0xae, 0x42, 0x60, 0x82,
    ];

    const SVG_BYTES: &[u8] =
        b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";

    const TEST_HASH: &str = "sha256:deadbeef";

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            for (path, bytes) in files {
                let mut h = tar::Header::new_gnu();
                h.set_size(bytes.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, path, *bytes).expect("append");
            }
            b.finish().expect("finish tar");
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).expect("gz write");
        gz.finish().expect("gz finish")
    }

    #[derive(Default)]
    struct MockSink {
        passes: bool,
        reason: Option<String>,
        raw: serde_json::Value,
        calls: RefCell<Calls>,
    }

    #[derive(Default, Debug)]
    struct Calls {
        moderate: usize,
        upload: usize,
        last_object_names: Vec<String>,
    }

    impl MockSink {
        fn pass() -> Self {
            Self {
                passes: true,
                reason: None,
                raw: serde_json::json!({"adult": "VERY_UNLIKELY"}),
                calls: RefCell::default(),
            }
        }
        fn block(reason: &str) -> Self {
            Self {
                passes: false,
                reason: Some(reason.to_string()),
                raw: serde_json::json!({"adult": "VERY_LIKELY"}),
                calls: RefCell::default(),
            }
        }
    }

    unsafe impl Sync for MockSink {}

    impl MediaSink for &MockSink {
        async fn moderate(
            &self,
            _mime: &str,
            _bytes: &[u8],
        ) -> Result<ModerationResult, MediaError> {
            self.calls.borrow_mut().moderate += 1;
            Ok(ModerationResult {
                passes: self.passes,
                reason: self.reason.clone(),
                raw: self.raw.clone(),
            })
        }
        async fn upload(
            &self,
            object_name: &str,
            _content_type: &str,
            _bytes: &[u8],
        ) -> Result<String, MediaError> {
            let mut c = self.calls.borrow_mut();
            c.upload += 1;
            c.last_object_names.push(object_name.to_string());
            Ok(format!("https://cdn.example/{}", object_name))
        }
    }

    #[tokio::test]
    async fn body_inline_image_uploads_with_preserved_path() {
        let sink = MockSink::pass();
        let bundle = tar_gz(&[("images/foo.png", PNG_1X1)]);
        let body = "Look:\n\n![demo](images/foo.png)\n";
        let out = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap();
        assert!(
            out.rewritten_body
                .contains("![demo](https://cdn.example/bundles/sha256:deadbeef/images/foo.png)"),
            "body: {}",
            out.rewritten_body
        );
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.records[0].kind, MediaKind::Image);
        assert_eq!(out.records[0].bundle_path, "images/foo.png");
    }

    #[tokio::test]
    async fn html_attachment_with_image_dep() {
        let sink = MockSink::pass();
        let html = r#"<h2>Comparison</h2><img src="comparison.png">"#;
        let bundle = tar_gz(&[
            ("screenshot-rt/report.html", html.as_bytes()),
            ("screenshot-rt/comparison.png", PNG_1X1),
        ]);
        let body = "Body summary.\n\n![visual report](screenshot-rt/report.html)\n";
        let out = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap();

        // Body: attachment ref STRIPPED, no markdown image syntax
        // pointing at the html.
        assert!(
            !out.rewritten_body.contains("report.html"),
            "body should strip attachment ref: {}",
            out.rewritten_body
        );

        // Records: html + comparison.png, both with preserved paths.
        assert_eq!(out.records.len(), 2);
        let by_path: HashMap<&str, &MediaRecord> = out
            .records
            .iter()
            .map(|r| (r.bundle_path.as_str(), r))
            .collect();
        let html_rec = by_path.get("screenshot-rt/report.html").unwrap();
        assert_eq!(html_rec.kind, MediaKind::HtmlAttachment);
        assert!(
            html_rec
                .public_url
                .contains("/bundles/sha256:deadbeef/screenshot-rt/report.html")
        );
        assert_eq!(html_rec.label, "visual report");
        let img_rec = by_path.get("screenshot-rt/comparison.png").unwrap();
        assert_eq!(img_rec.kind, MediaKind::Image);
        assert!(
            img_rec
                .public_url
                .contains("/bundles/sha256:deadbeef/screenshot-rt/comparison.png")
        );
    }

    #[tokio::test]
    async fn markdown_attachment_with_image_dep() {
        let sink = MockSink::pass();
        let md = "# Notes\n\n![pic](inner.png)\n";
        let bundle = tar_gz(&[
            ("docs/notes.md", md.as_bytes()),
            ("docs/inner.png", PNG_1X1),
        ]);
        let body = "![see](docs/notes.md)";
        let out = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap();
        assert!(!out.rewritten_body.contains("notes.md"));
        let kinds: Vec<MediaKind> = out.records.iter().map(|r| r.kind).collect();
        assert!(kinds.contains(&MediaKind::MarkdownAttachment));
        assert!(kinds.contains(&MediaKind::Image));
        assert_eq!(out.records.len(), 2);
    }

    #[tokio::test]
    async fn html_attachment_strips_script_and_on_handlers() {
        let sink = MockSink::pass();
        let html = r#"<p onclick="alert(1)">x</p><script>alert(2)</script><img src="i.png">"#;
        let bundle = tar_gz(&[("rep.html", html.as_bytes()), ("i.png", PNG_1X1)]);
        let body = "![](rep.html)";
        let out = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap();
        let html_rec = out
            .records
            .iter()
            .find(|r| r.kind == MediaKind::HtmlAttachment)
            .expect("html record");
        // The uploaded HTML wouldn't be in the record itself; we sink-
        // mock so we can't look at the bytes that were uploaded. But
        // we can directly call the sanitizer to be sure it does the
        // right thing on this fixture.
        let sanitized = sanitize_html_for_publish(html);
        assert!(!sanitized.contains("alert(1)"), "got: {sanitized}");
        assert!(!sanitized.contains("alert(2)"), "got: {sanitized}");
        assert!(!sanitized.contains("onclick"));
        assert!(sanitized.contains("<img src=\"i.png\">"));
        // and the html record was created
        assert_eq!(html_rec.bundle_path, "rep.html");
    }

    #[tokio::test]
    async fn external_image_in_html_passes_through() {
        let sink = MockSink::pass();
        let html = r#"<img src="https://example.com/x.png">"#;
        let bundle = tar_gz(&[("rep.html", html.as_bytes())]);
        let body = "![](rep.html)";
        let out = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap();
        // Only the html itself was uploaded; the external img is not
        // ingested.
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.records[0].kind, MediaKind::HtmlAttachment);
    }

    #[tokio::test]
    async fn body_with_no_images() {
        let sink = MockSink::pass();
        let bundle = tar_gz(&[("README.md", b"hello")]);
        let body = "no images here, just text";
        let out = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap();
        assert_eq!(out.rewritten_body, body);
        assert!(out.records.is_empty());
        assert_eq!(sink.calls.borrow().moderate, 0);
    }

    #[tokio::test]
    async fn vision_blocks_adult_in_image_dep() {
        // An image referenced from an HTML attachment that fails
        // SafeSearch must block the whole submission — the html
        // wouldn't ship without all its assets having passed.
        let sink = MockSink::block("adult: VERY_LIKELY");
        let html = r#"<img src="bad.png">"#;
        let bundle = tar_gz(&[("rep.html", html.as_bytes()), ("bad.png", PNG_1X1)]);
        let body = "![](rep.html)";
        let err = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::ClaimMediaRejected { .. }));
    }

    #[tokio::test]
    async fn skip_unsupported_image_type() {
        let sink = MockSink::pass();
        let bundle = tar_gz(&[("logo.svg", SVG_BYTES)]);
        let body = "![](logo.svg)"; // .svg has no attachment_kind, treated as image
        let err = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::ClaimMediaUnsupportedType { .. }));
    }

    #[tokio::test]
    async fn missing_attachment() {
        let sink = MockSink::pass();
        let bundle = tar_gz(&[("only.txt", b"x")]);
        let body = "![](missing.html)";
        let err = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::ClaimMediaMissing { .. }));
    }

    #[tokio::test]
    async fn image_dedup_across_body_and_html() {
        let sink = MockSink::pass();
        let html = r#"<img src="shared.png">"#;
        let bundle = tar_gz(&[("rep.html", html.as_bytes()), ("shared.png", PNG_1X1)]);
        let body = "![inline](shared.png)\n\n![rep](rep.html)";
        let out = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), body)
            .await
            .unwrap();
        // html attachment + 1 image (deduped between body ref and
        // html dep). Note: both routes resolve to bundle path
        // "shared.png", so to_upload de-dups by key.
        let n_images = out
            .records
            .iter()
            .filter(|r| r.kind == MediaKind::Image)
            .count();
        assert_eq!(n_images, 1);
        assert_eq!(sink.calls.borrow().moderate, 1);
    }

    #[tokio::test]
    async fn caps_too_many_images() {
        let sink = MockSink::pass();
        let n = MAX_IMAGES_PER_CLAIM + 1;
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut body = String::new();
        for i in 0..n {
            let mut img = PNG_1X1.to_vec();
            let last = img.len() - 5;
            img[last] = (i & 0xff) as u8;
            img[last - 1] = ((i >> 8) & 0xff) as u8;
            files.push((format!("img_{i}.png"), img));
            body.push_str(&format!("![x](img_{i}.png)\n"));
        }
        let refs: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let bundle = tar_gz(&refs);
        let err = process_with_sink(&&sink, TEST_HASH, &bundle, Some("a.tar.gz"), &body)
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::ClaimMediaTooMany), "got {:?}", err);
    }

    #[test]
    fn rejects_blocks_thresholds_correctly() {
        let mk = |adult, violence, racy| SafeSearch {
            adult: String::from(adult),
            violence: String::from(violence),
            racy: String::from(racy),
            spoof: String::from("VERY_UNLIKELY"),
            medical: String::from("VERY_UNLIKELY"),
        };
        assert!(rejects(&mk("VERY_UNLIKELY", "VERY_UNLIKELY", "VERY_UNLIKELY")).is_none());
        assert!(rejects(&mk("POSSIBLE", "POSSIBLE", "LIKELY")).is_none());
        assert!(rejects(&mk("LIKELY", "VERY_UNLIKELY", "VERY_UNLIKELY")).is_some());
        assert!(rejects(&mk("VERY_UNLIKELY", "VERY_UNLIKELY", "VERY_LIKELY")).is_some());
    }

    #[test]
    fn find_image_refs_skips_inline_code() {
        let body = "Real: ![real](real.png) but `![code](code.png)` is verbatim.";
        let refs = find_image_refs(body);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].url, "real.png");
    }

    #[test]
    fn attachment_kind_recognises_extensions() {
        assert!(matches!(
            attachment_kind("report.html"),
            Some(AttachmentKind::Html)
        ));
        assert!(matches!(
            attachment_kind("notes.md"),
            Some(AttachmentKind::Markdown)
        ));
        assert!(attachment_kind("foo.png").is_none());
        assert!(attachment_kind("https://x.com/a.html").is_none());
    }

    #[test]
    fn parent_dir_works() {
        assert_eq!(parent_dir("a/b/c.html"), "a/b");
        assert_eq!(parent_dir("x.html"), "");
    }

    #[test]
    fn resolve_relative_external_passes_through() {
        assert_eq!(
            resolve_relative("https://x/y.png", "dir"),
            "https://x/y.png"
        );
    }

    #[test]
    fn resolve_relative_joins_base_dir() {
        assert_eq!(resolve_relative("foo.png", "dir"), "dir/foo.png");
    }

    #[test]
    fn find_html_image_deps_finds_imgs() {
        let html = r#"<img src="a.png"><source srcset="b.png"><img src="https://x/c.png">"#;
        let mut deps = find_html_image_deps(html, "rt");
        deps.sort();
        assert_eq!(deps, vec!["rt/a.png", "rt/b.png"]);
    }

    #[test]
    fn find_markdown_image_deps_finds_refs() {
        let md = "![a](a.png) and ![b](b.png) and ![c](https://x/c.png)";
        let mut deps = find_markdown_image_deps(md, "doc");
        deps.sort();
        assert_eq!(deps, vec!["doc/a.png", "doc/b.png"]);
    }

    #[test]
    fn sanitize_strips_script_and_handlers() {
        let html = r#"<p onclick="alert(1)" ondblclick='x'>hi</p><script>alert(2)</script>"#;
        let out = sanitize_html_for_publish(html);
        assert!(!out.contains("alert(1)"));
        assert!(!out.contains("alert(2)"));
        assert!(!out.contains("onclick"));
        assert!(!out.contains("ondblclick"));
        assert!(out.contains("hi"));
    }

    #[test]
    fn realistic_body_with_em_dash_and_attachment_round_trip() {
        // Live-replay smoke test: a body that has an em-dash + parens
        // in prose + 1 inline image ref + 1 html attachment ref. Used
        // to verify rewrite_body doesn't truncate at the em-dash byte
        // boundary and correctly strips the attachment.
        let body = "# Title — with em-dash\n\nRe-uploading.\nBody text mentions (parens) and a — dash.\n\n![alt — with em-dash](images/foo.png)\n\n## Methodology\n\n![visual report](report.html)\n\n## Conclusion\n\nDone.";
        let refs = find_image_refs(body);
        assert_eq!(refs.len(), 2);
        let mut public_by_path = HashMap::new();
        public_by_path.insert(
            "images/foo.png".to_string(),
            "https://cdn.example/foo.png".to_string(),
        );
        let out = rewrite_body(body, &refs, &public_by_path);
        // Image rewrite happened
        assert!(out.contains("https://cdn.example/foo.png"));
        // Attachment was stripped
        assert!(!out.contains("report.html"));
        // Body wasn't truncated mid-flight
        assert!(out.contains("Done."));
        assert!(out.contains("# Title — with em-dash"));
    }

    #[test]
    fn sanitize_keeps_style() {
        let html = r#"<style>body{margin:0}</style><p>x</p>"#;
        let out = sanitize_html_for_publish(html);
        assert!(out.contains("<style>"));
        assert!(out.contains("margin:0"));
    }
}
