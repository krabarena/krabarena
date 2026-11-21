use sha2::{Digest, Sha256};

use crate::error::ApiError;

const MAX_ARTIFACT_SIZE: usize = 100 * 1024 * 1024; // 100 MB

#[derive(Debug)]
pub struct ArtifactUpload {
    pub hash: String,
    pub url: String,
    pub size: i64,
}

/// Upload artifact bytes to GCS (or local fallback).
///
/// `blob_store_url` is expected to be either:
/// - A GCS bucket URL like `gs://krabarena-artifacts` (production)
/// - An HTTP URL like `http://localhost:3001/blob` (dev fallback)
pub async fn upload_artifact(
    blob_store_url: &str,
    data: &[u8],
) -> Result<ArtifactUpload, ApiError> {
    if data.is_empty() {
        return Err(ApiError::ClaimMissingArtifact);
    }

    if data.len() > MAX_ARTIFACT_SIZE {
        return Err(ApiError::ArtifactTooLarge);
    }

    let hash = format!("sha256:{}", hex::encode(Sha256::digest(data)));
    let object_name = &hash;

    if let Some(bucket) = blob_store_url.strip_prefix("gs://") {
        upload_bytes_to_gcs(bucket, object_name, "application/octet-stream", data).await?;
        let url = format!("https://storage.googleapis.com/{}/{}", bucket, object_name);
        Ok(ArtifactUpload {
            hash,
            url,
            size: data.len() as i64,
        })
    } else {
        // Dev fallback: generate a URL but don't actually store
        let url = format!("{}/{}", blob_store_url, object_name);
        tracing::warn!(hash = %hash, "artifact upload: dev mode, not stored");
        Ok(ArtifactUpload {
            hash,
            url,
            size: data.len() as i64,
        })
    }
}

/// Production-like environment detection for the private-store
/// policy: running on Cloud Run (`K_SERVICE` is set by the platform),
/// a real `gs://` public blob store, or an explicit non-localhost
/// `PUBLIC_BASE_URL`.
pub fn is_prod_like(
    on_cloud_run: bool,
    public_base_url_explicit: Option<&str>,
    blob_store_url: &str,
) -> bool {
    let non_local_base = public_base_url_explicit.is_some_and(|u| {
        let u = u.to_ascii_lowercase();
        !(u.contains("://localhost") || u.contains("://127.0.0.1") || u.contains("://0.0.0.0"))
    });
    on_cloud_run || non_local_base || blob_store_url.starts_with("gs://")
}

/// Decide where private artifacts go, at startup.
///
/// * `PRIVATE_BLOB_STORE_URL` set (and different from the public
///   `BLOB_STORE_URL`) → use it.
/// * Otherwise, in a production-like environment (`prod_like`: running
///   on Cloud Run, an explicit `PUBLIC_BASE_URL`, or a real `gs://`
///   public store) → `None`: private uploads are refused rather than
///   written into the world-readable public bucket.
/// * Otherwise (local dev, http blob store that stores nothing) → fall
///   back to the public store URL with a warning.
pub fn resolve_private_blob_store(
    private_env: Option<&str>,
    public_blob_store_url: &str,
    prod_like: bool,
) -> Option<String> {
    let private = private_env.map(str::trim).filter(|s| !s.is_empty());
    let norm = |s: &str| s.trim_end_matches('/').to_string();
    match private {
        Some(p) if norm(p) == norm(public_blob_store_url) => {
            if prod_like {
                tracing::error!(
                    "PRIVATE_BLOB_STORE_URL equals the public BLOB_STORE_URL; private \
                     artifact uploads are DISABLED"
                );
                None
            } else {
                tracing::warn!("PRIVATE_BLOB_STORE_URL equals BLOB_STORE_URL (dev only)");
                Some(p.to_string())
            }
        }
        Some(p) => Some(p.to_string()),
        None if prod_like => {
            tracing::error!(
                "PRIVATE_BLOB_STORE_URL is unset; private artifact uploads are DISABLED \
                 (refusing to store them in the public bucket)"
            );
            None
        }
        None => {
            tracing::warn!(
                "PRIVATE_BLOB_STORE_URL unset — dev mode, private artifacts use BLOB_STORE_URL"
            );
            Some(public_blob_store_url.to_string())
        }
    }
}

/// Upload a *private* artifact to the private store. Unlike
/// [`upload_artifact`], the returned `url` is a `gs://bucket/object`
/// reference, never a public `https://storage.googleapis.com/...`
/// link — private artifacts are only ever read back through the
/// authorized `GET /v1/claims/{id}/private-artifact` route.
///
/// `private_store` is `None` when the server is configured to refuse
/// private uploads (see [`resolve_private_blob_store`]).
pub async fn upload_private_artifact(
    private_store: Option<&str>,
    data: &[u8],
) -> Result<ArtifactUpload, ApiError> {
    let Some(store) = private_store else {
        return Err(ApiError::ValidationError(
            "private artifacts are not accepted on this server (no private store configured)"
                .into(),
        ));
    };
    let mut upload = upload_artifact(store, data).await?;
    if let Some(bucket) = store.strip_prefix("gs://") {
        upload.url = format!("gs://{}/{}", bucket.trim_end_matches('/'), upload.hash);
    }
    Ok(upload)
}

/// Split a stored artifact URL into `(bucket, object)` when it points
/// at GCS — either `gs://bucket/object` (private store) or a legacy
/// `https://storage.googleapis.com/bucket/object` link.
pub fn parse_gcs_url(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("gs://")
        .or_else(|| url.strip_prefix("https://storage.googleapis.com/"))?;
    let (bucket, object) = rest.split_once('/')?;
    if bucket.is_empty() || object.is_empty() {
        return None;
    }
    Some((bucket.to_string(), object.to_string()))
}

/// Stream a private artifact from GCS using the service account's
/// credentials (JSON API `alt=media`). Callers MUST have authorized
/// the viewer first — this function performs no access control.
pub async fn stream_private_artifact(url: &str) -> Result<axum::body::Body, ApiError> {
    let Some((bucket, object)) = parse_gcs_url(url) else {
        tracing::error!(url = %url, "private artifact: not a GCS url (dev store?)");
        return Err(ApiError::ArtifactUnavailable);
    };
    let token = get_gcp_access_token()
        .await
        .map_err(|_| ApiError::ArtifactUnavailable)?;
    let api = format!(
        "https://storage.googleapis.com/storage/v1/b/{}/o/{}?alt=media",
        urlencoding::encode(&bucket),
        urlencoding::encode(&object),
    );
    let resp = reqwest::Client::new()
        .get(&api)
        .header("authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|_| ApiError::ArtifactUnavailable)?;
    if !resp.status().is_success() {
        tracing::error!(status = %resp.status(), bucket = %bucket, "private artifact download failed");
        return Err(ApiError::ArtifactUnavailable);
    }
    Ok(axum::body::Body::from_stream(resp.bytes_stream()))
}

/// Download artifact bytes by URL (GCS public or signed URL).
pub async fn download_artifact(artifact_url: &str) -> Result<Vec<u8>, ApiError> {
    let client = reqwest::Client::new();
    let mut req = client.get(artifact_url);

    // If it's a GCS URL and we're on Cloud Run, use metadata token
    if artifact_url.contains("storage.googleapis.com") {
        if let Ok(token) = get_gcp_access_token().await {
            req = req.header("authorization", format!("Bearer {}", token));
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|_| ApiError::ArtifactUnavailable)?;

    if !resp.status().is_success() {
        tracing::error!(
            status = %resp.status(),
            url = %artifact_url,
            "artifact download failed"
        );
        return Err(ApiError::ArtifactUnavailable);
    }

    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|_| ApiError::ArtifactUnavailable)
}

/// Upload bytes to a GCS bucket using the JSON API.
///
/// `content_type` is set on the stored object so browsers serve it
/// with the right MIME — matters for image uploads (the default
/// `application/octet-stream` triggers a download instead of an
/// inline render).
pub async fn upload_bytes_to_gcs(
    bucket: &str,
    object_name: &str,
    content_type: &str,
    data: &[u8],
) -> Result<(), ApiError> {
    let token = get_gcp_access_token()
        .await
        .map_err(|_| ApiError::ArtifactUnavailable)?;

    let url = format!(
        "https://storage.googleapis.com/upload/storage/v1/b/{}/o?uploadType=media&name={}",
        bucket, object_name,
    );

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("authorization", format!("Bearer {}", token))
        .header("content-type", content_type)
        .body(data.to_vec())
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "GCS upload request failed");
            ApiError::ArtifactUnavailable
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::error!(status = %status, body = %body, "GCS upload failed");
        return Err(ApiError::ArtifactUnavailable);
    }

    tracing::info!(bucket = %bucket, object = %object_name, "object uploaded to GCS");
    Ok(())
}

/// Public wrapper around the GCP metadata token fetch — exposed so
/// sibling services (`favicons`) can upload to GCS without
/// duplicating the metadata-server request boilerplate.
pub async fn get_gcp_access_token_pub() -> Result<String, anyhow::Error> {
    get_gcp_access_token().await
}

/// Get an access token from the GCP metadata server (works on Cloud Run).
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

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: private artifacts were written into the PUBLIC
    // `krabarena-artifacts` bucket (allUsers objectViewer, listable).
    #[test]
    fn prod_without_private_store_refuses() {
        assert_eq!(
            resolve_private_blob_store(None, "gs://krabarena-artifacts", true),
            None
        );
        assert_eq!(
            resolve_private_blob_store(Some("  "), "gs://krabarena-artifacts", true),
            None
        );
    }

    #[test]
    fn prod_refuses_private_store_equal_to_public() {
        assert_eq!(
            resolve_private_blob_store(
                Some("gs://krabarena-artifacts/"),
                "gs://krabarena-artifacts",
                true
            ),
            None
        );
    }

    #[test]
    fn configured_private_store_is_used() {
        assert_eq!(
            resolve_private_blob_store(
                Some("gs://krabarena-artifacts-private"),
                "gs://krabarena-artifacts",
                true
            )
            .as_deref(),
            Some("gs://krabarena-artifacts-private")
        );
    }

    #[test]
    fn prod_like_detection() {
        assert!(is_prod_like(true, None, "http://localhost:3001/blob"));
        assert!(is_prod_like(false, None, "gs://krabarena-artifacts"));
        assert!(is_prod_like(
            false,
            Some("https://krabarena.com"),
            "http://localhost:3001/blob"
        ));
        assert!(!is_prod_like(
            false,
            Some("http://localhost:3000"),
            "https://blob.vercel-storage.com"
        ));
        assert!(!is_prod_like(false, None, "http://localhost:3001/blob"));
    }

    #[test]
    fn dev_falls_back_to_public_store() {
        assert_eq!(
            resolve_private_blob_store(None, "http://localhost:3001/blob", false).as_deref(),
            Some("http://localhost:3001/blob")
        );
    }

    #[tokio::test]
    async fn upload_private_refused_without_store() {
        let err = upload_private_artifact(None, b"secret").await.unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)));
    }

    #[tokio::test]
    async fn dev_private_upload_never_returns_public_gcs_url() {
        let up = upload_private_artifact(Some("http://localhost:3001/blob"), b"secret")
            .await
            .unwrap();
        assert!(!up.url.contains("storage.googleapis.com"));
    }

    #[test]
    fn parse_gcs_urls() {
        assert_eq!(
            parse_gcs_url("gs://krabarena-artifacts-private/sha256:ab"),
            Some(("krabarena-artifacts-private".into(), "sha256:ab".into()))
        );
        assert_eq!(
            parse_gcs_url("https://storage.googleapis.com/krabarena-artifacts/sha256:ab"),
            Some(("krabarena-artifacts".into(), "sha256:ab".into()))
        );
        assert_eq!(parse_gcs_url("http://localhost:3001/blob/sha256:ab"), None);
        assert_eq!(parse_gcs_url("gs://bucket-only"), None);
    }
}
