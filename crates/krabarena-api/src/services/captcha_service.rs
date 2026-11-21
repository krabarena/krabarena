//! Google reCAPTCHA Enterprise (score-based) server-side verification.
//!
//! Guards the voter email-code endpoint (`POST /auth/voter/email`) against
//! bots spamming verification emails to arbitrary addresses. A score-based
//! Enterprise key runs silently for every user (no challenge is ever shown);
//! Google's risk analysis returns a score in `[0.0, 1.0]` and we reject
//! sign-ins that fall below `RECAPTCHA_SCORE_THRESHOLD`.
//!
//! **Fully optional.** When the reCAPTCHA config isn't fully populated
//! (`RecaptchaConfig::is_enabled()` is false) verification is skipped — local
//! dev and key-less environments behave exactly as before.
//!
//! **Fail-open on outage.** If the Assessment API is unreachable or returns
//! something we can't parse we let the request through rather than locking
//! every user out during a third-party incident. The CSPRNG 8-digit code on
//! the verify step is the second layer if a bot slips past here.

use std::time::Duration;

use serde_json::json;

use crate::RecaptchaConfig;
use crate::error::ApiError;

/// reCAPTCHA Enterprise Assessment API base. The project id and `?key=` API
/// key are appended per call.
const ASSESSMENT_URL_BASE: &str = "https://recaptchaenterprise.googleapis.com/v1/projects";

/// Cap the assessment round-trip. The shared `proxy_http` client carries a
/// 120s timeout tuned for the battle proxy — far too long to hold a login
/// request hostage on a slow Google response. A captcha check should fail
/// fast (and fail open) instead.
const ASSESSMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Verify a reCAPTCHA Enterprise token by creating an assessment.
///
/// Returns `Ok(())` when:
/// - `cfg` isn't fully configured (captcha disabled — no network call), or
/// - the token is valid, its action matches `expected_action`, and the risk
///   score is at or above `cfg.score_threshold`, or
/// - the Assessment call fails at the transport layer, returns a non-2xx
///   status (quota/outage/misconfig), or returns an unparseable body
///   (fail-open).
///
/// Returns `Err(ApiError::CaptchaFailed)` when captcha is enabled and the
/// token is missing, invalid, carries the wrong action, or scores below the
/// threshold.
pub async fn verify_enterprise(
    http: &reqwest::Client,
    cfg: &RecaptchaConfig,
    token: &str,
    expected_action: &str,
    remote_ip: Option<&str>,
) -> Result<(), ApiError> {
    verify_enterprise_at(
        ASSESSMENT_URL_BASE,
        http,
        cfg,
        token,
        expected_action,
        remote_ip,
    )
    .await
}

/// Core of [`verify_enterprise`], parameterized on the Assessment API base URL
/// (the part before `/{project}/assessments`) so tests can drive it against a
/// local mock server instead of Google.
async fn verify_enterprise_at(
    base_url: &str,
    http: &reqwest::Client,
    cfg: &RecaptchaConfig,
    token: &str,
    expected_action: &str,
    remote_ip: Option<&str>,
) -> Result<(), ApiError> {
    // Captcha disabled — nothing to verify.
    if !cfg.is_enabled() {
        return Ok(());
    }

    // An absent / empty token with the key configured is a hard fail:
    // a real browser with the widget loaded always sends one.
    if token.is_empty() {
        tracing::warn!("recaptcha: missing token on a captcha-protected request");
        return Err(ApiError::CaptchaFailed);
    }

    let url = format!(
        "{base_url}/{}/assessments?key={}",
        cfg.project_id, cfg.api_key
    );

    let mut event = json!({
        "token": token,
        "siteKey": cfg.site_key,
        "expectedAction": expected_action,
    });
    if let Some(ip) = remote_ip.filter(|s| !s.is_empty()) {
        event["userIpAddress"] = json!(ip);
    }
    let body = json!({ "event": event });

    let resp = http
        .post(&url)
        .json(&body)
        .timeout(ASSESSMENT_TIMEOUT)
        .send()
        .await;

    let r = match resp {
        Ok(r) => r,
        Err(e) => {
            // Transport-level failure (DNS, TLS, timeout). Fail open so a
            // Google outage doesn't take sign-in down with it.
            tracing::warn!(error = %e, "recaptcha: assessment unreachable — failing open");
            return Ok(());
        }
    };

    // reqwest does NOT treat 4xx/5xx as errors, so the status must be checked
    // before parsing. A non-2xx here is *our* problem (quota/429, 503, or an
    // auth/config error) — never the user's token, which comes back 200 with
    // `tokenProperties.valid=false`. Fail OPEN so a Google incident or a
    // misconfig can't lock every sign-in out; log it loudly for ops. No retry:
    // a login request must fail fast, and fail-open is the safety net.
    let status = r.status();
    if !status.is_success() {
        let transient = status.as_u16() == 429 || status.is_server_error();
        let body = r.text().await.unwrap_or_default();
        tracing::warn!(
            status = %status,
            transient,
            body = %body,
            "recaptcha: assessment returned non-2xx — failing open"
        );
        return Ok(());
    }

    let assessment = match r.json::<serde_json::Value>().await {
        Ok(v) => v,
        Err(e) => {
            // Couldn't parse a 2xx body — treat as a transient upstream problem
            // and fail open, same rationale as above.
            tracing::warn!(error = %e, "recaptcha: assessment response unparseable — failing open");
            return Ok(());
        }
    };

    let token_props = &assessment["tokenProperties"];

    // `valid` gates everything: an expired / duplicate / forged token or a
    // site-key mismatch lands here.
    if !token_props["valid"].as_bool().unwrap_or(false) {
        let reason = token_props["invalidReason"].as_str().unwrap_or("unknown");
        tracing::warn!(invalid_reason = %reason, "recaptcha: token invalid");
        return Err(ApiError::CaptchaFailed);
    }

    // The action baked into the token must match what the server expects; a
    // mismatch means the token was minted for a different action (replay /
    // forgery).
    let action = token_props["action"].as_str().unwrap_or_default();
    if action != expected_action {
        tracing::warn!(action = %action, expected = %expected_action, "recaptcha: action mismatch");
        return Err(ApiError::CaptchaFailed);
    }

    // Risk score in [0.0, 1.0]; higher = more likely human. Reject below the
    // configured threshold. A missing score (e.g. a challenge-based key)
    // reads as 0.0 → rejected, which is why a SCORE key is required.
    let score = assessment["riskAnalysis"]["score"].as_f64().unwrap_or(0.0);
    if score < cfg.score_threshold {
        let reasons = assessment["riskAnalysis"]["reasons"].to_string();
        tracing::warn!(
            score,
            threshold = cfg.score_threshold,
            reasons = %reasons,
            "recaptcha: score below threshold"
        );
        return Err(ApiError::CaptchaFailed);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A not-fully-configured config means captcha is disabled — verification
    /// must pass without touching the network, even with no token. This is
    /// the local-dev / key-less path and must never error.
    #[tokio::test]
    async fn empty_config_skips_verification() {
        let http = reqwest::Client::new();
        let cfg = RecaptchaConfig::default();
        assert!(
            verify_enterprise(&http, &cfg, "", "voter_email", None)
                .await
                .is_ok()
        );
        assert!(
            verify_enterprise(&http, &cfg, "anything", "voter_email", None)
                .await
                .is_ok()
        );
    }

    /// With the config populated but no token, fail closed before any network
    /// call — a legitimate browser always supplies a token.
    #[tokio::test]
    async fn configured_missing_token_fails() {
        let http = reqwest::Client::new();
        let result = verify_enterprise(&http, &enabled_cfg(), "", "voter_email", None).await;
        assert!(matches!(result, Err(ApiError::CaptchaFailed)));
    }

    /// A fully-configured (enabled) test config, score threshold 0.5.
    fn enabled_cfg() -> RecaptchaConfig {
        RecaptchaConfig {
            project_id: "proj".into(),
            site_key: "site".into(),
            api_key: "key".into(),
            score_threshold: 0.5,
        }
    }

    /// P1 regression: reqwest doesn't treat 4xx/5xx as errors, so a 429 with a
    /// JSON error body used to parse as a failed assessment and block sign-in.
    /// It must now fail OPEN (status checked before parse).
    #[tokio::test]
    async fn fails_open_on_429_json() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_string(r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED"}}"#),
            )
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let res = verify_enterprise_at(
            &server.uri(),
            &http,
            &enabled_cfg(),
            "tok",
            "voter_email",
            None,
        )
        .await;
        assert!(res.is_ok(), "429 must fail open, got {res:?}");
    }

    /// A 503 (Google outage) must also fail open.
    #[tokio::test]
    async fn fails_open_on_503() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let res = verify_enterprise_at(
            &server.uri(),
            &http,
            &enabled_cfg(),
            "tok",
            "voter_email",
            None,
        )
        .await;
        assert!(res.is_ok(), "503 must fail open, got {res:?}");
    }

    /// A valid token scoring below the threshold is rejected.
    #[tokio::test]
    async fn rejects_low_score() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tokenProperties": { "valid": true, "action": "voter_email" },
                "riskAnalysis": { "score": 0.1, "reasons": ["AUTOMATION"] }
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let res = verify_enterprise_at(
            &server.uri(),
            &http,
            &enabled_cfg(),
            "tok",
            "voter_email",
            None,
        )
        .await;
        assert!(
            matches!(res, Err(ApiError::CaptchaFailed)),
            "low score must reject, got {res:?}"
        );
    }

    /// A valid token scoring at/above the threshold passes.
    #[tokio::test]
    async fn accepts_high_score() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tokenProperties": { "valid": true, "action": "voter_email" },
                "riskAnalysis": { "score": 0.9, "reasons": [] }
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let res = verify_enterprise_at(
            &server.uri(),
            &http,
            &enabled_cfg(),
            "tok",
            "voter_email",
            None,
        )
        .await;
        assert!(res.is_ok(), "high score must pass, got {res:?}");
    }
}
