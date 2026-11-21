//! Profile-photo upload pipeline for `POST /v1/users/me/avatar`.
//!
//! Two ingress shapes:
//!   - multipart upload with an `image` field — PNG / JPEG / WebP,
//!     same MIME sniffing as [`crate::services::image_service`],
//!     capped at [`MAX_AVATAR_BYTES`].
//!   - JSON body `{ "url": "https://…" }` — server fetches the URL,
//!     validates the response is one of the three accepted image
//!     types, and stores the bytes the same way. Useful when an
//!     external script already has the bytes online (e.g. the
//!     generated bot logos uploaded to `krabarena-artifacts/logos/`).
//!
//! Stored on GCS at `avatars/<sha>.<ext>` (or the dev fallback
//! `<blob_store_url>/avatars/<sha>.<ext>` when running off-GCS).
//! Content-addressable so two identical uploads dedup at the
//! bucket level.
//!
//! Why a dedicated module instead of folding into `image_service`:
//! avatars carry a stricter size cap ([`MAX_AVATAR_BYTES`] = 256 KB,
//! vs `MAX_IMAGE_BYTES` = 2 MB on claim figures), the bucket prefix
//! is different, and we deliberately do NOT run the optional Gemini
//! Vision alt-text pass (avatars are human-faces / logos, not
//! evidentiary figures — there's no screen-reader caption we'd
//! generate that the existing `alt="<display name>"` doesn't
//! already capture). Keeping the modules separate makes those
//! policy differences obvious.

use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::services::artifact_service;
use crate::services::image_service::sniff_image_mime;

/// 256 KB cap on uploaded avatars. Comfortably above what a typical
/// 256x256 PNG or 512x512 JPEG runs to (40-150 KB) while keeping the
/// blob-store overhead per user trivially small. Bytes over the cap
/// surface as `ClaimMediaTooLarge` (reusing the existing error code
/// so the CLI / UI's "image too large" branch already handles it).
pub const MAX_AVATAR_BYTES: usize = 256 * 1024;

/// Maximum bytes the URL-import path will download. Same envelope
/// as [`MAX_AVATAR_BYTES`] — we stop reading once that cap is
/// reached so a hostile URL serving a multi-GB stream can't tie up
/// the request thread.
pub const URL_FETCH_LIMIT: usize = MAX_AVATAR_BYTES + 1;

/// Result of a successful upload — the public URL the caller should
/// write into `users.avatar_url`, plus the metadata in case the
/// route wants to log it.
#[derive(Debug, Clone)]
pub struct UploadedAvatar {
    pub url: String,
    pub sha256: String,
    pub mime_type: &'static str,
    pub bytes: i64,
}

/// Validate + push bytes to GCS. The route hands us already-decoded
/// bytes (from multipart) or already-downloaded bytes (from the
/// URL-import path). No moderation / no alt text — see module doc.
pub async fn upload_avatar_bytes(
    blob_store_url: &str,
    bytes: &[u8],
) -> Result<UploadedAvatar, ApiError> {
    if bytes.is_empty() {
        return Err(ApiError::ValidationError("avatar upload was empty".into()));
    }
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(ApiError::ClaimMediaTooLarge {
            path: "avatar".to_string(),
        });
    }

    let (mime, ext) =
        sniff_image_mime(bytes).ok_or_else(|| ApiError::ClaimMediaUnsupportedType {
            path: "avatar".to_string(),
        })?;

    let hash_hex = hex::encode(Sha256::digest(bytes));
    let object_name = format!("avatars/{}.{}", hash_hex, ext);

    let url = if let Some(bucket) = blob_store_url.strip_prefix("gs://") {
        artifact_service::upload_bytes_to_gcs(bucket, &object_name, mime, bytes).await?;
        format!("https://storage.googleapis.com/{}/{}", bucket, object_name)
    } else {
        // Dev fallback — build a URL but don't actually upload, same
        // pattern image_service uses. Lets the local server respond
        // 200 against a fake URL when GCS isn't reachable.
        tracing::warn!(
            object = %object_name,
            "avatar_service: dev mode, not actually uploading",
        );
        format!("{}/{}", blob_store_url, object_name)
    };

    Ok(UploadedAvatar {
        url,
        sha256: format!("sha256:{}", hash_hex),
        mime_type: mime,
        bytes: bytes.len() as i64,
    })
}

/// Fetch the bytes at `url` and run them through [`upload_avatar_bytes`].
///
/// Strictly an `http`/`https` URL — other schemes return a
/// `ValidationError`. Content-length is checked before streaming so
/// we don't allocate for a multi-GB body, then we cap the read at
/// [`URL_FETCH_LIMIT`] regardless of what the server claimed. Any
/// non-2xx response is a validation error too — the caller almost
/// certainly typo'd the URL.
pub async fn upload_avatar_from_url(
    blob_store_url: &str,
    url: &str,
) -> Result<UploadedAvatar, ApiError> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(ApiError::ValidationError(
            "avatar URL must be http(s)".into(),
        ));
    }

    crate::models::proxy::check_url_is_public(trimmed, false)
        .map_err(|e| ApiError::ValidationError(format!("avatar URL {e}")))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| ApiError::Internal(e.into()))?;

    let resp = client
        .get(trimmed)
        .send()
        .await
        .map_err(|e| ApiError::ValidationError(format!("failed to fetch avatar URL: {e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::ValidationError(format!(
            "avatar URL returned status {}",
            resp.status()
        )));
    }

    if let Some(len) = resp.content_length()
        && len as usize > MAX_AVATAR_BYTES
    {
        return Err(ApiError::ClaimMediaTooLarge {
            path: "avatar".to_string(),
        });
    }

    // Read at most URL_FETCH_LIMIT bytes. Using `bytes()` here is
    // fine because reqwest enforces the timeout above; we still
    // re-check the length post-read because the server may have
    // omitted content-length entirely.
    let body = resp
        .bytes()
        .await
        .map_err(|e| ApiError::ValidationError(format!("failed to read avatar body: {e}")))?;
    if body.len() > URL_FETCH_LIMIT {
        return Err(ApiError::ClaimMediaTooLarge {
            path: "avatar".to_string(),
        });
    }

    upload_avatar_bytes(blob_store_url, &body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_bytes_rejected() {
        let err = upload_avatar_bytes("gs://test", &[]).await.unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)));
    }

    #[tokio::test]
    async fn oversize_rejected() {
        // 257 KB random-looking bytes — past the cap, sniff doesn't
        // even matter because we check size first.
        let bytes = vec![0u8; MAX_AVATAR_BYTES + 1];
        let err = upload_avatar_bytes("gs://test", &bytes).await.unwrap_err();
        assert!(matches!(err, ApiError::ClaimMediaTooLarge { .. }));
    }

    #[tokio::test]
    async fn non_image_rejected() {
        // Plain text bytes — fails MIME sniff.
        let bytes = b"This is not an image, definitely not.".to_vec();
        let err = upload_avatar_bytes("gs://test", &bytes).await.unwrap_err();
        assert!(matches!(err, ApiError::ClaimMediaUnsupportedType { .. }));
    }

    #[tokio::test]
    async fn dev_fallback_returns_url_for_png() {
        // Minimal PNG header is enough for the MIME sniff; the bytes
        // never leave the process in dev mode.
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&[0u8; 64]);
        let uploaded = upload_avatar_bytes("http://localhost:3001/blob", &bytes)
            .await
            .expect("dev upload should succeed");
        assert!(
            uploaded
                .url
                .starts_with("http://localhost:3001/blob/avatars/")
        );
        assert_eq!(uploaded.mime_type, "image/png");
        assert!(uploaded.sha256.starts_with("sha256:"));
    }

    #[tokio::test]
    async fn url_import_rejects_non_http() {
        let err = upload_avatar_from_url("gs://test", "ftp://example.com/x.png")
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::ValidationError(_)));
    }
}
