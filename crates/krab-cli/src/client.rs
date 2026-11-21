use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use reqwest::multipart;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Rate-limit (429) retry budget. The server's limiter (since 0.26.0)
/// returns `429` + `Retry-After` when a client exceeds its per-IP /
/// per-user budget; rather than failing the command, we honor the hint
/// and retry a bounded number of times so a brief over-limit burst
/// self-heals. Mirrors the browser `fetch-retry.js` wrapper.
const MAX_RL_RETRIES: u32 = 3;
/// Hard ceiling on a single back-off sleep — a bogus/huge `Retry-After`
/// can't wedge the CLI. The limiter's window is 60s, so two ≤30s waits
/// always span a full reset.
const MAX_RL_WAIT: Duration = Duration::from_secs(30);

/// Local crate version (the one this binary was built with). Compared
/// against `X-Krab-Server-Version` on every response to detect a stale
/// CLI. See `check_server_version` below.
const LOCAL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Process-wide "have we already printed the version-mismatch warning"
/// guard. The first response that carries a mismatching
/// `X-Krab-Server-Version` flips this to true and emits one stderr
/// line; every subsequent response is a no-op. Lives at the crate
/// level so all `KrabClient` instances within a single CLI invocation
/// share the same fired-once state — e.g. the telemetry-flush client
/// spawned at startup doesn't double up with the user's command client.
static VERSION_MISMATCH_WARNED: AtomicBool = AtomicBool::new(false);

/// One-shot guard for persisting a newer server version into the
/// auto-update state file (see `auto_update::record_seen_server_version`).
static SEEN_NEWER_RECORDED: AtomicBool = AtomicBool::new(false);

/// Suppress the per-response version-mismatch warning for the rest of
/// this process — used when auto-update already printed its own
/// warning, so the user doesn't see two lines about the same thing.
pub fn mark_version_warning_shown() {
    VERSION_MISMATCH_WARNED.store(true, Ordering::Relaxed);
}

pub struct KrabClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
    /// When true, the version-mismatch warning is suppressed — JSON
    /// output mode shouldn't get prose stderr lines interleaved for
    /// shells that combine streams. Set via `with_json_output(true)`.
    suppress_version_warning: bool,
}

impl KrabClient {
    pub fn new(base_url: &str, token: Option<String>) -> Self {
        // Trust BOTH the bundled Mozilla/webpki roots AND the OS trust store.
        // webpki-only (the old `rustls-tls` default) ignores the system store,
        // so any environment that injects its own CA — corporate MITM proxies
        // (Zscaler/Netskope), a sandbox egress TLS-inspection CA, etc. — made
        // every request die at the handshake with an opaque "error sending
        // request". Native roots pull in /etc/ssl + SSL_CERT_FILE/SSL_CERT_DIR
        // (and the macOS Keychain / Windows store) so we trust those injected
        // CAs; keeping the bundled webpki roots means we still work in a
        // scratch/distroless container that has no system CA store at all.
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .tls_built_in_webpki_certs(true)
            .tls_built_in_native_certs(true)
            .build()
            .expect("failed to build HTTP client");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            suppress_version_warning: false,
        }
    }

    /// Builder-style setter for the JSON-output flag. When true, the
    /// CLI was invoked with `--format json` and we MUST NOT print the
    /// version-mismatch warning to stderr (so pipeable output stays
    /// clean when shells combine streams). Default is `false` =
    /// human mode = warning allowed.
    pub fn with_json_output(mut self, json: bool) -> Self {
        self.suppress_version_warning = json;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_header(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("Bearer {}", t))
    }

    /// Inspect a response for `X-Krab-Server-Version` and, on the first
    /// mismatch in this process's lifetime, emit a single-line
    /// non-blocking warning to stderr. Subsequent mismatches in the
    /// same invocation are no-ops (see `VERSION_MISMATCH_WARNED`).
    /// Suppressed entirely when this client was constructed with
    /// `with_json_output(true)` so JSON consumers don't see prose
    /// interleaved with their pipeable output.
    fn check_server_version(&self, resp: &reqwest::Response) {
        let Some(server) = resp.headers().get("x-krab-server-version") else {
            return;
        };
        let Ok(server) = server.to_str() else {
            return;
        };
        // Remember a newer server version (once per process) so the next
        // invocation's auto-update can act on it without waiting for its
        // 24h `/health` check window. Cheap: only on mismatch.
        if crate::self_update::semver_cmp(server, LOCAL_VERSION) == std::cmp::Ordering::Greater
            && SEEN_NEWER_RECORDED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            crate::auto_update::record_seen_server_version(server);
        }
        if !should_warn_about_version_mismatch(
            LOCAL_VERSION,
            server,
            VERSION_MISMATCH_WARNED.load(Ordering::Relaxed),
            self.suppress_version_warning,
        ) {
            return;
        }
        // Race-safe one-shot: CAS so only the first thread to observe
        // a mismatch prints. Other concurrent responses see `Err` and
        // silently skip.
        if VERSION_MISMATCH_WARNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            eprintln!(
                "\u{26a0} krab {} (local) \u{2014} server is {}. Some commands may not exist locally.",
                LOCAL_VERSION, server
            );
            eprintln!("   Update: krab doctor --fix");
        }
    }

    /// Send a request, transparently retrying on `429 Too Many
    /// Requests`. On a 429 with retries left we sleep for the server's
    /// `Retry-After` (clamped to `MAX_RL_WAIT`) and try again. The
    /// request is re-issued from a clone, so this only applies to
    /// cloneable bodies — a streaming multipart upload (the artifact
    /// POST) can't be cloned, so it's sent once with no retry (an upload
    /// over the limit just surfaces the 429 as before). Non-429
    /// responses and transport errors are returned immediately, so every
    /// existing caller path (success unwrap, `format_api_error`) is
    /// unchanged.
    async fn send_retrying(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let mut attempt = 0u32;
        loop {
            let Some(clone) = req.try_clone() else {
                // Non-cloneable body (streaming multipart) → can't retry.
                return Ok(req.send().await?);
            };
            let resp = clone.send().await?;
            if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS || attempt >= MAX_RL_RETRIES
            {
                return Ok(resp);
            }
            attempt += 1;
            let wait = retry_after(resp.headers()).min(MAX_RL_WAIT);
            if !self.suppress_version_warning {
                eprintln!(
                    "\u{231b} Rate limited — retrying in {}s (attempt {}/{})",
                    wait.as_secs().max(1),
                    attempt,
                    MAX_RL_RETRIES
                );
            }
            tokio::time::sleep(wait).await;
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut req = self.http.get(self.url(path));
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        let body = parse_response_body(resp).await?;

        if !status.is_success() {
            anyhow::bail!(format_api_error(&body, status));
        }

        Ok(serde_json::from_value(body["data"].clone())?)
    }

    pub async fn delete_envelope(
        &self,
        path: &str,
        json: &Value,
    ) -> Result<(reqwest::StatusCode, Value)> {
        let mut req = self.http.delete(self.url(path)).json(json);
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        let body = parse_response_body(resp).await?;
        Ok((status, body))
    }

    pub async fn get_paginated(&self, path: &str) -> Result<Value> {
        let mut req = self.http.get(self.url(path));
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        let body = parse_response_body(resp).await?;

        if !status.is_success() {
            anyhow::bail!(format_api_error(&body, status));
        }

        Ok(body)
    }

    pub async fn post<T: DeserializeOwned>(&self, path: &str, json: &Value) -> Result<T> {
        let mut req = self.http.post(self.url(path)).json(json);
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        let body = parse_response_body(resp).await?;

        if !status.is_success() {
            anyhow::bail!(format_api_error(&body, status));
        }

        Ok(serde_json::from_value(body["data"].clone())?)
    }

    /// Fire-and-forget POST that returns the HTTP status code only.
    /// Used by the telemetry flush path where the body is fixed
    /// shape and we only care whether it landed (2xx) or didn't
    /// (anything else). The response body is not read; it's just
    /// dropped at the end of the function. Auth is attached when
    /// available; without a token we still send the request — the
    /// server will return 401, which is `false` from the caller's
    /// POV and keeps the records queued for after `krab auth`.
    pub async fn post_raw(&self, path: &str, json: &Value) -> Result<u16> {
        let mut req = self.http.post(self.url(path)).json(json);
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        Ok(resp.status().as_u16())
    }

    pub async fn patch<T: DeserializeOwned>(&self, path: &str, json: &Value) -> Result<T> {
        let mut req = self.http.patch(self.url(path)).json(json);
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        let body = parse_response_body(resp).await?;

        if !status.is_success() {
            anyhow::bail!(format_api_error(&body, status));
        }

        Ok(serde_json::from_value(body["data"].clone())?)
    }

    /// PUT — used by admin config endpoints (`admin config-set`). Same
    /// envelope contract as POST/PATCH: error response is parsed into
    /// the standard `ApiErrorBody`; success unwraps `data`.
    pub async fn put<T: DeserializeOwned>(&self, path: &str, json: &Value) -> Result<T> {
        let mut req = self.http.put(self.url(path)).json(json);
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        let body = parse_response_body(resp).await?;

        if !status.is_success() {
            anyhow::bail!(format_api_error(&body, status));
        }

        Ok(serde_json::from_value(body["data"].clone())?)
    }

    /// DELETE returning no body (204 No Content) — used by `krab claim
    /// delete`. We deliberately don't decode `data` like get/post/patch
    /// because successful DELETE responses carry an empty body.
    pub async fn delete(&self, path: &str) -> Result<()> {
        let mut req = self.http.delete(self.url(path));
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(());
        }
        // Errors carry a JSON body; bubble the message up.
        let body = parse_response_body(resp).await?;
        if !status.is_success() {
            anyhow::bail!(format_api_error(&body, status));
        }
        Ok(())
    }

    pub async fn post_multipart<T: DeserializeOwned>(
        &self,
        path: &str,
        form: multipart::Form,
    ) -> Result<T> {
        let mut req = self.http.post(self.url(path)).multipart(form);
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        let body = parse_response_body(resp).await?;

        if !status.is_success() {
            anyhow::bail!(format_api_error(&body, status));
        }

        Ok(serde_json::from_value(body["data"].clone())?)
    }

    /// Like `post_multipart`, but returns `(status_code, raw_body)` so the
    /// caller can branch on structured error codes (e.g. the KrabReviewer
    /// 409 `CLAIM_NEEDS_REVISION` that carries the review body).
    pub async fn post_multipart_raw(
        &self,
        path: &str,
        form: multipart::Form,
    ) -> Result<(reqwest::StatusCode, Value)> {
        let mut req = self.http.post(self.url(path)).multipart(form);
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    pub async fn download(&self, path: &str) -> Result<Vec<u8>> {
        let mut req = self.http.get(self.url(path));
        if let Some(auth) = self.auth_header() {
            req = req.header("authorization", auth);
        }
        let resp = self.send_retrying(req).await?;
        self.check_server_version(&resp);
        if !resp.status().is_success() {
            anyhow::bail!("Download failed: {}", resp.status());
        }
        Ok(resp.bytes().await?.to_vec())
    }
}

/// Pure predicate for the version-mismatch warning. Returns true only
/// when **all** of:
/// - `local != server` — the strings disagree. Plain `!=` instead of
///   semver-aware comparison: the user's only action is "run the
///   installer", which always fetches whatever the server reports —
///   so "behind" and "ahead" both warrant the same banner. If the
///   server is BEHIND we still hit this branch, but in practice the
///   server is the rolling deploy and the CLI is the lagging binary,
///   so the noise is negligible.
/// - `!already_warned` — process-wide one-shot. Subsequent responses
///   in the same invocation skip silently.
/// - `!format_is_json` — JSON output mode suppresses prose stderr to
///   keep streams clean for shells that combine them.
pub fn should_warn_about_version_mismatch(
    local: &str,
    server: &str,
    already_warned: bool,
    format_is_json: bool,
) -> bool {
    local != server && !already_warned && !format_is_json
}

/// Read a response body and parse as JSON, falling back to a synthetic
/// error envelope if the server returned non-JSON (e.g. axum's default
/// `Json<T>` rejection which emits text/plain for 422s). Without this,
/// any non-JSON error body produced the opaque `Error: error decoding
/// response body` — users had to reach for curl to see what actually
/// went wrong.
async fn parse_response_body(resp: reqwest::Response) -> Result<Value> {
    let bytes = resp.bytes().await?;
    if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
        return Ok(v);
    }
    // Non-JSON body — wrap the raw text in the envelope shape so the
    // generic error renderer can still pull a code/message out of it.
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    Ok(serde_json::json!({
        "error": {
            "code": "NON_JSON_RESPONSE",
            "message": if text.is_empty() { "(empty response body)".to_string() } else { text },
        }
    }))
}

/// Format an API error envelope for display. Prior version dropped the
/// `meta.request_id` on the floor, forcing users who hit a 500 to re-run
/// with `--format json` to grab the id before filing a report. Surfacing
/// the id on the human path is a small cost for a big diagnostic win.
fn format_api_error(body: &Value, status: reqwest::StatusCode) -> String {
    let code = body["error"]["code"].as_str().unwrap_or_else(|| match () {
        _ if status.is_server_error() => "SERVER_ERROR",
        _ if status.is_client_error() => "CLIENT_ERROR",
        _ => "UNKNOWN",
    });
    let msg = body["error"]["message"].as_str().unwrap_or("Unknown error");
    let request_id = body["meta"]["request_id"].as_str().unwrap_or("");
    if request_id.is_empty() {
        format!("[{}] {}", code, msg)
    } else {
        format!("[{}] {} (request_id: {})", code, msg, request_id)
    }
}

/// How long to wait before retrying a `429`. Prefers the server's
/// `Retry-After` (integer seconds), then `RateLimit-Reset` (also
/// seconds), then a 1s default. Clamped to a sane `[1s, 60s]` so a
/// malformed/huge header can't translate into an unbounded sleep (the
/// caller additionally caps it at `MAX_RL_WAIT`).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Duration {
    let header_secs = |name: &str| -> Option<u64> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
    };
    let secs = header_secs("retry-after")
        .or_else(|| header_secs("ratelimit-reset"))
        .unwrap_or(1)
        .clamp(1, 60);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use serde_json::json;

    use reqwest::header::HeaderMap;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn retry_after_prefers_retry_after_header() {
        assert_eq!(retry_after(&headers(&[("retry-after", "5")])).as_secs(), 5);
    }

    #[test]
    fn retry_after_falls_back_to_ratelimit_reset_then_default() {
        assert_eq!(
            retry_after(&headers(&[("ratelimit-reset", "12")])).as_secs(),
            12
        );
        // Neither header present → 1s default.
        assert_eq!(retry_after(&HeaderMap::new()).as_secs(), 1);
    }

    #[test]
    fn retry_after_clamps_and_ignores_garbage() {
        // Huge value clamped to 60.
        assert_eq!(
            retry_after(&headers(&[("retry-after", "99999")])).as_secs(),
            60
        );
        // Non-numeric (HTTP-date form) → ignored → 1s default.
        assert_eq!(
            retry_after(&headers(&[(
                "retry-after",
                "Wed, 21 Oct 2026 07:28:00 GMT"
            )]))
            .as_secs(),
            1
        );
    }

    #[test]
    fn format_api_error_includes_request_id_when_present() {
        let body = json!({
            "error": { "code": "CLAIM_NOT_FOUND", "message": "Claim not found" },
            "meta": { "request_id": "b4c687c8-b5da-49c8-b582-245e7a060c74" }
        });
        let s = format_api_error(&body, StatusCode::NOT_FOUND);
        assert!(s.contains("CLAIM_NOT_FOUND"), "{s}");
        assert!(s.contains("Claim not found"), "{s}");
        assert!(
            s.contains("(request_id: b4c687c8-b5da-49c8-b582-245e7a060c74)"),
            "{s}"
        );
    }

    #[test]
    fn format_api_error_omits_request_id_when_absent() {
        let body = json!({
            "error": { "code": "CLAIM_BLOCKED", "message": "blocked" }
        });
        let s = format_api_error(&body, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(s, "[CLAIM_BLOCKED] blocked");
    }

    #[test]
    fn format_api_error_defaults_code_by_status_class() {
        // Non-envelope server error body.
        let s = format_api_error(&json!({}), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(s.starts_with("[SERVER_ERROR]"), "{s}");

        let s = format_api_error(&json!({}), StatusCode::BAD_REQUEST);
        assert!(s.starts_with("[CLIENT_ERROR]"), "{s}");
    }

    // Version-mismatch predicate: the 4 combinations of (mismatch,
    // already_warned, format_is_json). Only the all-allow path
    // (mismatch && !already_warned && !format_is_json) returns true.
    #[test]
    fn warn_predicate_fires_only_on_fresh_human_mismatch() {
        // (1) Fresh human-format mismatch → warn.
        assert!(should_warn_about_version_mismatch(
            "0.15.0", "0.15.4", false, false
        ));
    }

    #[test]
    fn warn_predicate_silent_when_already_warned() {
        // (2) Same mismatch, but the one-shot has fired → no warn.
        assert!(!should_warn_about_version_mismatch(
            "0.15.0", "0.15.4", true, false
        ));
    }

    #[test]
    fn warn_predicate_silent_in_json_mode() {
        // (3) Mismatch, fresh, but JSON output → no warn (clean stream).
        assert!(!should_warn_about_version_mismatch(
            "0.15.0", "0.15.4", false, true
        ));
    }

    #[test]
    fn warn_predicate_silent_when_versions_match() {
        // (4) Local == server → no warn regardless of other flags.
        assert!(!should_warn_about_version_mismatch(
            "0.15.4", "0.15.4", false, false
        ));
    }
}
