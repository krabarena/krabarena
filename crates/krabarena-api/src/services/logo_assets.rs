//! Self-hosted competitor logos + brand-color extraction (0.28.4).
//!
//! Today competitor logos are hot-linked from Google's favicon service
//! (`favicon_url_for(domain)` → `s2/favicons?domain=...`). This module
//! downloads each vendor's logo once, stores our own copy in the GCS blob
//! bucket, extracts its dominant brand color, and records both on the
//! `vendor_logos` row (`stored_url`, `brand_color`). Logo resolution then
//! prefers the stored URL (better reliability + CORS), and the wins-matrix
//! pie can use a real brand color instead of the hash-derived stand-in.
//!
//! GCS plumbing is reused from [`artifact_service`]
//! (`upload_bytes_to_gcs` + `get_gcp_access_token_pub`); the public URL
//! pattern is `https://storage.googleapis.com/{bucket}/{object_name}`. In
//! dev (no `gs://` `BLOB_STORE_URL`) the store is a graceful no-op, exactly
//! like `artifact_service::upload_artifact`.

use serde::Serialize;

use crate::services::artifact_service::upload_bytes_to_gcs;

/// Result of a [`backfill_all`] run.
#[derive(Debug, Serialize)]
pub struct BackfillSummary {
    /// Domains whose `stored_url`/`brand_color` were written.
    pub updated: usize,
    /// Domains that errored (fetch/decode/upload) — logged and skipped.
    pub failed: usize,
    /// Domains skipped because GCS isn't configured (dev / no `gs://`).
    pub skipped_dev: usize,
}

/// Per-bucket pixel accumulator: running `(sum_r, sum_g, sum_b, count)`
/// used to average the winning color bucket in [`dominant_color`].
type ColorAccum = (u64, u64, u64, u64);

/// Extract the dominant brand color from raw image bytes as `#RRGGBB`.
///
/// PURE + unit-tested. Decodes with the `image` crate, downscales to
/// ~32x32 for speed, then tallies quantized colors while skipping pixels
/// that would muddy the result: near-transparent (alpha < 32, favicon
/// padding), near-white (all channels > 240, the usual favicon
/// background), and near-black (all channels < 16, anti-aliasing /
/// outlines). Each channel is quantized to the nearest 32 to bucket
/// similar shades; the winning bucket's color is returned as the
/// *average* of the pixels that fell into it (a truer color than the
/// bucket center). Returns `None` if the image can't be decoded or no
/// qualifying pixels exist.
///
/// **Background-fill skip (0.43.3):** an icon drawn on a solid, *pale or
/// neutral* square (TinyFish: an orange fish on pale green `#d0ffab`)
/// used to return the background, since it covers most pixels. When one
/// bucket fills most of the image border (>= 60% of opaque border
/// pixels), is background-like (light: every channel >= 160, or
/// near-grey: channel spread <= 24), and some other bucket still holds a
/// real share of the mark (>= 5% of qualifying pixels), that border bucket
/// is ignored. A saturated full-bleed brand square (white glyph on blue)
/// is NOT background-like, so it keeps winning as before.
pub fn dominant_color(image_bytes: &[u8]) -> Option<String> {
    let img = image::load_from_memory(image_bytes).ok()?;
    // Downscale for speed; nearest keeps the palette honest (no blending
    // in new intermediate colors). RGBA8 so we can read alpha.
    let small = img
        .resize(32, 32, image::imageops::FilterType::Nearest)
        .to_rgba8();
    let (w, h) = small.dimensions();

    // bucket key (qr, qg, qb) → accumulator (sum_r, sum_g, sum_b, count)
    use std::collections::HashMap;
    let mut buckets: HashMap<(u8, u8, u8), ColorAccum> = HashMap::new();
    // Border tally over opaque pixels (before the white/black filters, so
    // a white border correctly counts as "not this bucket").
    let mut border: HashMap<(u8, u8, u8), u64> = HashMap::new();
    let mut border_opaque = 0u64;

    for (x, y, px) in small.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        if a < 32 {
            continue; // near-transparent
        }
        let key = (quantize(r), quantize(g), quantize(b));
        if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
            border_opaque += 1;
            *border.entry(key).or_insert(0) += 1;
        }
        if r > 240 && g > 240 && b > 240 {
            continue; // near-white background
        }
        if r < 16 && g < 16 && b < 16 {
            continue; // near-black outline
        }
        let e = buckets.entry(key).or_insert((0, 0, 0, 0));
        e.0 += r as u64;
        e.1 += g as u64;
        e.2 += b as u64;
        e.3 += 1;
    }

    let total: u64 = buckets.values().map(|v| v.3).sum();
    if let Some((bg_key, bg_count)) = border.iter().max_by_key(|(_, c)| **c)
        && border_opaque > 0
        && *bg_count * 100 >= border_opaque * 60
        && let Some(bg) = buckets.get(bg_key).copied()
        && bg.3 > 0
        && is_background_like(avg(bg))
    {
        let fg_best = buckets
            .iter()
            .filter(|(k, _)| *k != bg_key)
            .map(|(_, v)| v.3)
            .max()
            .unwrap_or(0);
        if fg_best * 100 >= total * 5 {
            buckets.remove(bg_key);
        }
    }

    let (_, acc) = buckets.into_iter().max_by_key(|(_, v)| v.3)?;
    if acc.3 == 0 {
        return None;
    }
    let (r, g, b) = avg(acc);
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// Average color of a bucket accumulator.
fn avg((sr, sg, sb, count): ColorAccum) -> (u8, u8, u8) {
    let n = count.max(1);
    ((sr / n) as u8, (sg / n) as u8, (sb / n) as u8)
}

/// A fill that reads as "background" rather than brand: light (every
/// channel >= 160 — pastel/off-white) or near-grey (channel spread <= 24).
fn is_background_like((r, g, b): (u8, u8, u8)) -> bool {
    let (mn, mx) = (r.min(g).min(b), r.max(g).max(b));
    mn >= 160 || mx - mn <= 24
}

/// Validate + normalize an admin-supplied brand color: `#rrggbb` (hex,
/// case-insensitive) → lowercase `#rrggbb`. `None` for anything else.
pub fn normalize_brand_color(raw: &str) -> Option<String> {
    let s = raw.trim();
    let hex = s.strip_prefix('#')?;
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("#{}", hex.to_ascii_lowercase()))
    } else {
        None
    }
}

/// Short content hash of a logo's bytes (first 10 hex chars of SHA-256),
/// used as the `?v=` cache-buster on stored logo URLs.
fn content_version(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    hex::encode(digest)[..10].to_string()
}

/// Public URL of a stored logo, versioned by content (0.43.3).
///
/// The GCS object path is stable (`logos/<domain>.png`) and served with
/// `cache-control: public, max-age=3600`, so before versioning a changed
/// logo kept rendering the old bytes for up to an hour. Appending
/// `?v=<content hash>` makes every logo change a new URL (GCS ignores the
/// query string), so browsers / the OG renderer fetch the new bytes
/// immediately, while unchanged logos keep the same URL and stay cached.
fn versioned_logo_url(bucket: &str, object_name: &str, bytes: &[u8]) -> String {
    format!(
        "https://storage.googleapis.com/{bucket}/{object_name}?v={}",
        content_version(bytes)
    )
}

/// Quantize a channel to the nearest multiple of 32 (clamped to a valid
/// `u8`), so visually-similar shades land in the same tally bucket.
fn quantize(v: u8) -> u8 {
    let q = ((v as u16 + 16) / 32) * 32;
    q.min(255) as u8
}

/// GCS object name for a domain's stored logo: `logos/{sanitized}.png`.
/// Sanitize: lowercase, then replace any char not in `[a-z0-9.-]` with `-`
/// so an odd `domain` value can't escape the `logos/` prefix or break the
/// upload URL.
fn logo_object_name(domain: &str) -> String {
    let sanitized: String = domain
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("logos/{sanitized}.png")
}

/// Candidate favicon sources for `domain`, best first:
/// 1. Google's favicon service at `sz=128` (sharper than the 64px hot-link);
/// 2. the same for the parent domain (`maven.apache.org` → `apache.org`) —
///    docs sub-sites often have no favicon of their own;
/// 3. DuckDuckGo's icon service (covers some sites Google doesn't).
pub fn favicon_candidates(domain: &str) -> Vec<String> {
    let mut out = vec![format!(
        "https://www.google.com/s2/favicons?domain={domain}&sz=128"
    )];
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() >= 3 {
        let parent = labels[1..].join(".");
        out.push(format!(
            "https://www.google.com/s2/favicons?domain={parent}&sz=128"
        ));
    }
    out.push(format!("https://icons.duckduckgo.com/ip3/{domain}.ico"));
    out
}

/// Fetch a domain's logo bytes, trying each of [`favicon_candidates`] in
/// order. A candidate counts only if it answers 2xx with a non-empty body
/// that decodes as an image (Google/DDG serve a generic globe or 404 page
/// for unknown sites). 10s timeout per attempt.
pub async fn fetch_logo_bytes(
    http: &reqwest::Client,
    domain: &str,
) -> Result<Vec<u8>, anyhow::Error> {
    let mut last_err = anyhow::anyhow!("no favicon candidates for {domain}");
    for url in favicon_candidates(domain) {
        let resp = match http
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = e.into();
                continue;
            }
        };
        if !resp.status().is_success() {
            last_err = anyhow::anyhow!("favicon fetch {url} returned {}", resp.status());
            continue;
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                last_err = e.into();
                continue;
            }
        };
        let Ok(img) = image::load_from_memory(&bytes) else {
            last_err = anyhow::anyhow!("favicon fetch {url} returned no decodable image");
            continue;
        };
        // Stored as `logos/<domain>.png` with `image/png`, so normalise
        // ICO/JPEG/GIF/WebP sources to real PNG bytes.
        if bytes.starts_with(b"\x89PNG") {
            return Ok(bytes);
        }
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png)?;
        return Ok(png.into_inner());
    }
    Err(last_err)
}

/// Fetch a domain's logo, extract its dominant color, and upload the
/// bytes to GCS at `logos/{sanitized_domain}.png` (content-type
/// `image/png`). Returns `Some((stored_url, brand_color))` once stored, or
/// `Ok(None)` in dev when no `gs://` `BLOB_STORE_URL` is configured (the
/// fetch is still skipped — there's nowhere to put it).
pub async fn store_logo_for_domain(
    http: &reqwest::Client,
    domain: &str,
) -> Result<Option<(String, Option<String>)>, anyhow::Error> {
    let blob_store_url = std::env::var("BLOB_STORE_URL").unwrap_or_default();
    let Some(bucket) = blob_store_url.strip_prefix("gs://") else {
        tracing::warn!(domain = %domain, "logo store: dev mode (no gs:// BLOB_STORE_URL), skipping");
        return Ok(None);
    };

    let bytes = fetch_logo_bytes(http, domain).await?;
    let brand_color = dominant_color(&bytes);
    let object_name = logo_object_name(domain);

    // `upload_bytes_to_gcs` fetches the GCP access token internally (via
    // the same metadata-server path `get_gcp_access_token_pub` exposes),
    // so the upload + token plumbing is fully reused from artifact_service.
    upload_bytes_to_gcs(bucket, &object_name, "image/png", &bytes)
        .await
        .map_err(|e| anyhow::anyhow!("GCS upload for {domain} failed: {e}"))?;

    let stored_url = versioned_logo_url(bucket, &object_name, &bytes);
    Ok(Some((stored_url, brand_color)))
}

/// Resolve a domain's logo asset for on-demand use (a freshly-curated
/// vendor row, or a domain discovered from a claim's `website_url`):
/// fetch the favicon, extract the brand color, and — only when a `gs://`
/// BLOB store is configured — self-host the PNG in GCS. Unlike
/// [`store_logo_for_domain`], the **brand color is returned even in dev**
/// (no GCS): self-hosting needs the bucket, color extraction does not.
/// Fully best-effort: any failure degrades to `None` for that part.
pub async fn resolve_domain_asset(
    http: &reqwest::Client,
    domain: &str,
) -> (Option<String>, Option<String>) {
    let bytes = match fetch_logo_bytes(http, domain).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(domain = %domain, error = %e, "logo asset: favicon fetch failed");
            return (None, None);
        }
    };
    let brand_color = dominant_color(&bytes);

    let blob_store_url = std::env::var("BLOB_STORE_URL").unwrap_or_default();
    let stored_url = match blob_store_url.strip_prefix("gs://") {
        Some(bucket) => {
            let object_name = logo_object_name(domain);
            match upload_bytes_to_gcs(bucket, &object_name, "image/png", &bytes).await {
                Ok(_) => Some(versioned_logo_url(bucket, &object_name, &bytes)),
                Err(e) => {
                    tracing::warn!(domain = %domain, error = %e, "logo asset: GCS upload failed");
                    None
                }
            }
        }
        None => None,
    };
    (stored_url, brand_color)
}

/// Backfill every distinct `vendor_logos.domain`: fetch + store its logo,
/// extract its brand color, and write both back. Best-effort per domain —
/// a single failure logs a warning and continues (counted as `failed`).
pub async fn backfill_all(
    pool: &sqlx::PgPool,
    http: &reqwest::Client,
) -> Result<BackfillSummary, anyhow::Error> {
    let domains: Vec<String> = sqlx::query_scalar("SELECT DISTINCT domain FROM vendor_logos")
        .fetch_all(pool)
        .await?;

    let mut summary = BackfillSummary {
        updated: 0,
        failed: 0,
        skipped_dev: 0,
    };

    for domain in domains {
        match store_logo_for_domain(http, &domain).await {
            Ok(Some((stored_url, brand_color))) => {
                // Admin-pinned colors (`brand_color_locked`, migration 071)
                // are never overwritten; an extraction miss (None) never
                // nulls a previously stored color either.
                let res = sqlx::query(
                    "UPDATE vendor_logos SET stored_url = $1, \
                     brand_color = CASE WHEN brand_color_locked THEN brand_color \
                                        ELSE COALESCE($2, brand_color) END, \
                     updated_at = now() WHERE domain = $3",
                )
                .bind(&stored_url)
                .bind(&brand_color)
                .bind(&domain)
                .execute(pool)
                .await;
                match res {
                    Ok(_) => summary.updated += 1,
                    Err(e) => {
                        tracing::warn!(domain = %domain, error = %e, "logo backfill: db update failed");
                        summary.failed += 1;
                    }
                }
            }
            Ok(None) => summary.skipped_dev += 1,
            Err(e) => {
                tracing::warn!(domain = %domain, error = %e, "logo backfill: store failed");
                summary.failed += 1;
            }
        }
    }

    tracing::info!(
        updated = summary.updated,
        failed = summary.failed,
        skipped_dev = summary.skipped_dev,
        "vendor_logos.backfill_complete"
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {

    #[test]
    fn favicon_candidates_try_parent_domain_then_ddg() {
        let c = favicon_candidates("maven.apache.org");
        assert_eq!(c.len(), 3);
        assert!(c[0].contains("domain=maven.apache.org&sz=128"));
        assert!(c[1].contains("domain=apache.org&sz=128"));
        assert_eq!(
            c[2],
            "https://icons.duckduckgo.com/ip3/maven.apache.org.ico"
        );
        // Two-label domains have no parent fallback.
        assert_eq!(favicon_candidates("exa.ai").len(), 2);
    }
    use super::*;

    /// Build a solid-color PNG of the given size in-memory.
    fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn dominant_color_solid_red() {
        let png = solid_png(16, 16, [255, 0, 0, 255]);
        let hex = dominant_color(&png).expect("solid red should yield a color");
        // Parse the hex back and assert the red channel dominates.
        let r = u8::from_str_radix(&hex[1..3], 16).unwrap();
        let g = u8::from_str_radix(&hex[3..5], 16).unwrap();
        let b = u8::from_str_radix(&hex[5..7], 16).unwrap();
        assert!(r > 200, "red channel should dominate, got {hex}");
        assert!(g < 40 && b < 40, "green/blue should be low, got {hex}");
    }

    #[test]
    fn dominant_color_garbage_is_none() {
        assert_eq!(dominant_color(b"not an image at all"), None);
    }

    #[test]
    fn dominant_color_all_white_is_none() {
        // Pure white is filtered out as background → no qualifying pixels.
        let png = solid_png(16, 16, [255, 255, 255, 255]);
        assert_eq!(dominant_color(&png), None);
    }

    /// `size`² PNG filled with `bg`, with a centered `fg` square covering
    /// the middle half of each axis (~25% of the area) — an icon on a
    /// solid background.
    fn icon_on_background(size: u32, bg: [u8; 4], fg: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(size, size, |x, y| {
            let lo = size / 4;
            let hi = size - size / 4;
            if (lo..hi).contains(&x) && (lo..hi).contains(&y) {
                image::Rgba(fg)
            } else {
                image::Rgba(bg)
            }
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    fn rgb(hex: &str) -> (u8, u8, u8) {
        (
            u8::from_str_radix(&hex[1..3], 16).unwrap(),
            u8::from_str_radix(&hex[3..5], 16).unwrap(),
            u8::from_str_radix(&hex[5..7], 16).unwrap(),
        )
    }

    /// Regression (TinyFish, 2026-10-25): an orange fish on a pale-green
    /// `#d0ffab` square extracted the background, not the brand orange.
    #[test]
    fn dominant_color_skips_pale_background_fill() {
        let png = icon_on_background(128, [0xd0, 0xff, 0xab, 255], [0xf7, 0x93, 0x1e, 255]);
        let hex = dominant_color(&png).unwrap();
        assert_eq!(hex, "#f7931e", "should pick the orange mark, got {hex}");
    }

    #[test]
    fn dominant_color_skips_light_grey_background_fill() {
        let png = icon_on_background(64, [0xe0, 0xe0, 0xe0, 255], [0x10, 0x60, 0xd0, 255]);
        assert_eq!(dominant_color(&png).unwrap(), "#1060d0");
    }

    /// A saturated full-bleed brand square with a white glyph keeps its
    /// brand color — the fill IS the brand there.
    #[test]
    fn dominant_color_keeps_saturated_brand_square() {
        let png = icon_on_background(64, [0x25, 0x63, 0xeb, 255], [255, 255, 255, 255]);
        assert_eq!(dominant_color(&png).unwrap(), "#2563eb");
    }

    /// A pale fill with no real mark (tiny speck) is still returned rather
    /// than the speck.
    #[test]
    fn dominant_color_pale_fill_without_mark_falls_back_to_fill() {
        let mut img = image::RgbaImage::from_pixel(32, 32, image::Rgba([0xd0, 0xff, 0xab, 255]));
        img.put_pixel(16, 16, image::Rgba([0xf7, 0x93, 0x1e, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let (r, g, b) = rgb(&dominant_color(&buf.into_inner()).unwrap());
        assert!(g > 240 && r > 190 && b > 160, "expected the pale fill");
    }

    #[test]
    fn normalize_brand_color_validates_hex() {
        assert_eq!(normalize_brand_color("#F97316").as_deref(), Some("#f97316"));
        assert_eq!(
            normalize_brand_color(" #abcdef ").as_deref(),
            Some("#abcdef")
        );
        assert_eq!(normalize_brand_color("f97316"), None);
        assert_eq!(normalize_brand_color("#fff"), None);
        assert_eq!(normalize_brand_color("#ggg000"), None);
        assert_eq!(normalize_brand_color("#f973160"), None);
        assert_eq!(normalize_brand_color("red"), None);
    }

    /// Cache-busting (2026-10-25): Keenable's logo changed but browsers
    /// kept the old bytes for up to an hour (stable URL + max-age=3600).
    /// The stored URL must change whenever the bytes change.
    #[test]
    fn versioned_logo_url_changes_with_content() {
        let a = versioned_logo_url("krabarena-artifacts", "logos/keenable.ai.png", b"old");
        let b = versioned_logo_url("krabarena-artifacts", "logos/keenable.ai.png", b"new");
        let a2 = versioned_logo_url("krabarena-artifacts", "logos/keenable.ai.png", b"old");
        assert!(
            a.starts_with(
                "https://storage.googleapis.com/krabarena-artifacts/logos/keenable.ai.png?v="
            ),
            "{a}"
        );
        assert_eq!(
            a.len() - a.find("?v=").unwrap() - 3,
            10,
            "10-char hash: {a}"
        );
        assert_ne!(a, b, "different bytes must yield a different URL");
        assert_eq!(a, a2, "same bytes keep the same (cacheable) URL");
        // Still recognized as self-hosted by the render paths.
        assert!(crate::services::solution_names::is_self_hosted_logo(&a));
    }

    #[test]
    fn logo_object_name_sanitizes() {
        assert_eq!(logo_object_name("pola.rs"), "logos/pola.rs.png");
        assert_eq!(logo_object_name("foo/bar?x"), "logos/foo-bar-x.png");
        assert_eq!(
            logo_object_name("CLOUD.Google.com"),
            "logos/cloud.google.com.png"
        );
    }
}
