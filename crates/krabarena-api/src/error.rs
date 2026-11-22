use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use krabarena_shared::field_limits::FieldLengthError;

use crate::envelope::{ApiErrorBody, ApiErrorResponse, ResponseMeta};

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum ApiError {
    #[error("Unauthorized")]
    Unauthorized,

    #[error("Forbidden: you do not own this resource")]
    Forbidden,

    #[error("Battle not found")]
    BattleNotFound,

    #[error("Claim not found")]
    ClaimNotFound,

    #[error("Claim requires an artifact")]
    ClaimMissingArtifact,

    #[error("Claim is immutable after creation")]
    ClaimImmutable,

    #[error("Artifact exceeds maximum size of 100 MB")]
    ArtifactTooLarge,

    #[error("Artifact storage temporarily unavailable")]
    ArtifactUnavailable,

    #[error("KrabReviewer temporarily unavailable: {0}")]
    ReviewerUnavailable(String),

    #[error(
        "Confirmed response requires execution evidence (environment, output_hash, duration_ms)"
    )]
    MissingExecutionEvidence,

    #[error(
        "Disconfirmed response requires failure details (failure_reason, conditions, reproduction_steps)"
    )]
    MissingFailureReason,

    #[error("Validation error: {0}")]
    ValidationError(String),

    /// Generic 404 for resources without a dedicated variant (e.g. admin
    /// lookups by id). Message is safe to surface.
    #[error("Not found: {0}")]
    NotFound(String),

    /// Last-admin protection — refuses to demote an org's only
    /// admin (would leave the org locked out of admin-only flows).
    /// Distinct from a generic `ValidationError` so the CLI / UI can
    /// branch on the structured code (`LAST_ADMIN`) and surface a
    /// "promote someone else first" hint instead of the raw message.
    #[error("Cannot demote the last admin of an org — promote another member to admin first")]
    LastAdmin,

    /// Length-bounded field rejected. Wire shape lives in
    /// `krabarena_shared::field_limits` — see that module for the
    /// platform-wide policy. The HTTP body carries the structured
    /// `details: { table, field, limit_chars, submitted_chars }`
    /// so CLI / agent tooling can recover deterministically.
    #[error(transparent)]
    FieldLength(#[from] FieldLengthError),

    #[error("Invalid email address")]
    InvalidEmail,

    #[error("Invalid verification token")]
    InvalidVerificationToken,

    #[error("Security check failed — please refresh and try again")]
    CaptchaFailed,

    #[error("Failed to send verification email: {0}")]
    EmailSendFailed(String),

    #[error("Claim cannot embed more than 100 images")]
    ClaimMediaTooMany,

    #[error("Embedded image `{path}` exceeds size cap")]
    ClaimMediaTooLarge { path: String },

    #[error("Embedded image `{path}` rejected by moderation: {reason}")]
    ClaimMediaRejected { path: String, reason: String },

    #[error("Embedded image `{path}` not found in artifact bundle")]
    ClaimMediaMissing { path: String },

    #[error("Embedded media `{path}` is not a supported image type (PNG/JPEG/WEBP/GIF only)")]
    ClaimMediaUnsupportedType { path: String },

    #[error("Image moderation service temporarily unavailable")]
    ClaimMediaModerationUnavailable,

    /// Per-IP rate limit exceeded (HTTP 429). The `web::rate_limit`
    /// middleware builds its own 429 response inline so it can attach
    /// `Retry-After` / `RateLimit-*` headers; this variant exists so
    /// the code registry + `error_codes_are_uppercase_snake` test stay
    /// complete and any future handler-level use returns a 429.
    #[error("Too many requests")]
    RateLimited,

    #[error("This Battle's proxy is busy with other requests; retry shortly")]
    ProxyBusy,

    /// Battle proxy: the method/path is outside the service's
    /// `allowed_methods` / `allowed_paths` (HTTP 403). The message
    /// lists what is allowed.
    #[error("{0}")]
    ProxyEndpointNotAllowed(String),

    /// Battle proxy: the upstream failed after the request was sent
    /// (timeout, broken body, response over the size cap) — HTTP 502.
    #[error("Upstream error: {0}")]
    UpstreamError(String),

    #[error("Internal server error")]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::BattleNotFound => "BATTLE_NOT_FOUND",
            Self::ClaimNotFound => "CLAIM_NOT_FOUND",
            Self::ClaimMissingArtifact => "CLAIM_MISSING_ARTIFACT",
            Self::ClaimImmutable => "CLAIM_IMMUTABLE",
            Self::ArtifactTooLarge => "ARTIFACT_TOO_LARGE",
            Self::ArtifactUnavailable => "ARTIFACT_UNAVAILABLE",
            Self::ReviewerUnavailable(_) => "REVIEWER_UNAVAILABLE",
            Self::MissingExecutionEvidence => "MISSING_EXECUTION_EVIDENCE",
            Self::MissingFailureReason => "MISSING_FAILURE_REASON",
            Self::ValidationError(_) => "VALIDATION_ERROR",
            Self::NotFound(_) => "NOT_FOUND",
            Self::LastAdmin => "LAST_ADMIN",
            Self::FieldLength(e) => e.code(),
            Self::InvalidEmail => "INVALID_EMAIL",
            Self::InvalidVerificationToken => "INVALID_VERIFICATION_TOKEN",
            Self::CaptchaFailed => "CAPTCHA_FAILED",
            Self::EmailSendFailed(_) => "EMAIL_SEND_FAILED",
            Self::ClaimMediaTooMany => "CLAIM_MEDIA_TOO_MANY",
            Self::ClaimMediaTooLarge { .. } => "CLAIM_MEDIA_TOO_LARGE",
            Self::ClaimMediaRejected { .. } => "CLAIM_MEDIA_REJECTED",
            Self::ClaimMediaMissing { .. } => "CLAIM_MEDIA_MISSING",
            Self::ClaimMediaUnsupportedType { .. } => "CLAIM_MEDIA_UNSUPPORTED_TYPE",
            Self::ClaimMediaModerationUnavailable => "MEDIA_MODERATION_UNAVAILABLE",
            Self::RateLimited => "RATE_LIMITED",
            Self::ProxyBusy => "PROXY_BUSY",
            Self::ProxyEndpointNotAllowed(_) => "PROXY_ENDPOINT_NOT_ALLOWED",
            Self::UpstreamError(_) => "UPSTREAM_ERROR",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::BattleNotFound | Self::ClaimNotFound | Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::ArtifactUnavailable | Self::ReviewerUnavailable(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Self::ClaimMediaModerationUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::RateLimited | Self::ProxyBusy => StatusCode::TOO_MANY_REQUESTS,
            Self::ClaimMediaTooMany
            | Self::ClaimMediaTooLarge { .. }
            | Self::ClaimMediaRejected { .. }
            | Self::ClaimMediaMissing { .. }
            | Self::ClaimMediaUnsupportedType { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::EmailSendFailed(_) | Self::UpstreamError(_) => StatusCode::BAD_GATEWAY,
            Self::ProxyEndpointNotAllowed(_) => StatusCode::FORBIDDEN,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        // Belt-and-suspenders secret redaction. Errors carrying inner
        // strings (e.g. `ReviewerUnavailable("gemini request failed:
        // …https://generativelanguage.googleapis.com/...?key=AIzaSy…"`)
        // would otherwise leak the upstream API key in the JSON body
        // the client sees. The Gemini transport site already redacts,
        // but if any future call path forgets, this final pass strips
        // every `?…` query string from any URL in the message. No-op
        // for messages without URLs.
        let body = ApiErrorResponse {
            error: ApiErrorBody {
                code: self.code().to_string(),
                message: crate::services::gemini_service::redact_url_secrets(&self.to_string()),
                details: match &self {
                    Self::ValidationError(msg) => Some(json!({ "field_errors": msg })),
                    Self::FieldLength(e) => Some(e.details()),
                    _ => None,
                },
            },
            meta: ResponseMeta::generate(),
        };

        if let Self::Internal(ref err) = self {
            tracing::error!(error = %err, code = self.code(), "Internal server error");
        }

        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_uppercase_snake() {
        let errors: Vec<ApiError> = vec![
            ApiError::Unauthorized,
            ApiError::Forbidden,
            ApiError::BattleNotFound,
            ApiError::ClaimNotFound,
            ApiError::ClaimMissingArtifact,
            ApiError::ClaimImmutable,
            ApiError::ArtifactTooLarge,
            ApiError::ArtifactUnavailable,
            ApiError::MissingExecutionEvidence,
            ApiError::MissingFailureReason,
            ApiError::ValidationError("test".into()),
            ApiError::InvalidEmail,
            ApiError::InvalidVerificationToken,
            ApiError::EmailSendFailed("test".into()),
            ApiError::ClaimMediaTooMany,
            ApiError::ClaimMediaTooLarge {
                path: "img.png".into(),
            },
            ApiError::ClaimMediaRejected {
                path: "img.png".into(),
                reason: "adult".into(),
            },
            ApiError::ClaimMediaMissing {
                path: "img.png".into(),
            },
            ApiError::ClaimMediaUnsupportedType {
                path: "img.svg".into(),
            },
            ApiError::ClaimMediaModerationUnavailable,
            ApiError::RateLimited,
            ApiError::ProxyEndpointNotAllowed("test".into()),
            ApiError::UpstreamError("test".into()),
        ];

        for err in &errors {
            let code = err.code();
            assert!(
                code.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "Error code must be UPPER_SNAKE_CASE: {}",
                code
            );
        }
    }

    #[test]
    fn response_errors_return_4xx() {
        assert_eq!(
            ApiError::MissingExecutionEvidence.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::MissingFailureReason.status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::ValidationError("x".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn auth_errors_return_401_403() {
        assert_eq!(
            ApiError::Unauthorized.status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ApiError::Forbidden.status_code(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn not_found_errors_return_404() {
        assert_eq!(
            ApiError::BattleNotFound.status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(ApiError::ClaimNotFound.status_code(), StatusCode::NOT_FOUND);
    }
}
