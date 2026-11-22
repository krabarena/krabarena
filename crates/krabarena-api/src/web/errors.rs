use askama::Template;
use axum::extract::Request;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};

use crate::AppState;
use crate::web::meta::PageMeta;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorPageKind {
    Battle,
    Claim,
    Generic,
}

impl ErrorPageKind {
    fn extra_label(self) -> Option<&'static str> {
        match self {
            Self::Battle => Some("Browse Battles"),
            Self::Claim => Some("Browse Claims"),
            Self::Generic => None,
        }
    }

    fn extra_href(self) -> Option<&'static str> {
        match self {
            Self::Battle => Some("/?tab=battles"),
            Self::Claim => Some("/?tab=claims"),
            Self::Generic => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ErrorPageCopy {
    eyebrow: &'static str,
    title: &'static str,
    message: &'static str,
    not_found_kind: ErrorPageKind,
}

impl ErrorPageCopy {
    fn for_status(status: StatusCode, path: &str) -> Self {
        let not_found_kind = not_found_kind_from_path(path);
        match status {
            StatusCode::NOT_FOUND => match not_found_kind {
                ErrorPageKind::Battle => Self {
                    eyebrow: "KrabArena could not find this Battle",
                    title: "Battle not found",
                    message: "This Battle may have been removed, restored under another link, or never existed.",
                    not_found_kind,
                },
                ErrorPageKind::Claim => Self {
                    eyebrow: "KrabArena could not find this Claim",
                    title: "Claim not found",
                    message: "This Claim may have been removed, superseded by a newer version, or never existed.",
                    not_found_kind,
                },
                ErrorPageKind::Generic => Self {
                    eyebrow: "KrabArena could not find this page",
                    title: "Page not found",
                    message: "The page you are looking for is not available on KrabArena.",
                    not_found_kind,
                },
            },
            StatusCode::UNAUTHORIZED => Self {
                eyebrow: "Authentication required",
                title: "Sign in required",
                message: "Please sign in and try this action again.",
                not_found_kind,
            },
            StatusCode::FORBIDDEN => Self {
                eyebrow: "Access denied",
                title: "Access denied",
                message: "Your account does not have access to this page.",
                not_found_kind,
            },
            StatusCode::SERVICE_UNAVAILABLE => Self {
                eyebrow: "KrabArena is temporarily unavailable",
                title: "Service temporarily unavailable",
                message: "This part of KrabArena is temporarily unavailable. Please try again shortly.",
                not_found_kind,
            },
            _ => Self {
                eyebrow: "KrabArena hit an internal error",
                title: "Something went wrong",
                message: "The page could not be loaded because of an internal error. Please try again shortly.",
                not_found_kind,
            },
        }
    }
}

#[derive(Template)]
#[template(path = "pages/error.html")]
struct ErrorTemplate {
    meta: PageMeta,
    status_code: u16,
    eyebrow: &'static str,
    title: &'static str,
    message: &'static str,
    extra_label: &'static str,
    extra_href: &'static str,
    has_extra_action: bool,
}

pub async fn html_error_pages(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();
    let response = next.run(req).await;
    let status = response.status();

    if !is_web_error_status(status)
        || is_markdown_response(&response)
        || !should_render_html_error(&path, &headers)
    {
        return response;
    }

    render_status(&state.public_base_url, &path, status)
}

pub async fn plain_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

pub fn render_404(public_base_url: &str, path: &str) -> Response {
    render_status(public_base_url, path, StatusCode::NOT_FOUND)
}

pub fn render_500(public_base_url: &str, path: &str) -> Response {
    render_status(public_base_url, path, StatusCode::INTERNAL_SERVER_ERROR)
}

pub fn render_status(public_base_url: &str, path: &str, status: StatusCode) -> Response {
    let copy = ErrorPageCopy::for_status(status, path);
    let kind = copy.not_found_kind;
    let meta = PageMeta::new(public_base_url, path)
        .with_title(copy.title)
        .with_description(copy.message)
        .noindex_follow();
    let template = ErrorTemplate {
        meta,
        status_code: status.as_u16(),
        eyebrow: copy.eyebrow,
        title: copy.title,
        message: copy.message,
        extra_label: kind.extra_label().unwrap_or(""),
        extra_href: kind.extra_href().unwrap_or(""),
        has_extra_action: status == StatusCode::NOT_FOUND && kind.extra_label().is_some(),
    };
    let body = template
        .render()
        .unwrap_or_else(|_| fallback_html(status, copy.title));
    (status, Html(body)).into_response()
}

pub fn render_template_body<T: Template>(template: T, context: &'static str) -> Option<String> {
    template
        .render()
        .map_err(|e| {
            tracing::error!(
                error = %e,
                template = context,
                "web.template.render_failed"
            );
        })
        .ok()
}

pub fn render_template_or_500<T: Template>(
    template: T,
    public_base_url: &str,
    path: &str,
    context: &'static str,
) -> Response {
    match render_template_body(template, context) {
        Some(body) => Html(body).into_response(),
        None => render_500(public_base_url, path),
    }
}

pub fn render_template_with_status_or_500<T: Template>(
    template: T,
    status: StatusCode,
    public_base_url: &str,
    path: &str,
    context: &'static str,
) -> Response {
    match render_template_body(template, context) {
        Some(body) => (status, Html(body)).into_response(),
        None => render_500(public_base_url, path),
    }
}

pub fn render_plain_500() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
}

fn not_found_kind_from_path(path: &str) -> ErrorPageKind {
    if path.starts_with("/battles/") {
        ErrorPageKind::Battle
    } else if path.starts_with("/claims/") {
        ErrorPageKind::Claim
    } else {
        ErrorPageKind::Generic
    }
}

fn is_web_error_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
    )
}

fn is_markdown_response(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/markdown"))
}

fn should_render_html_error(path: &str, headers: &HeaderMap) -> bool {
    if is_excluded_path(path) {
        return false;
    }

    let Some(accept) = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    accept
        .split(',')
        .map(str::trim)
        .any(|part| part.starts_with("text/html") || part.starts_with("*/*"))
}

fn is_excluded_path(path: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "/v1/",
        "/internal/",
        "/static/",
        "/cli/",
        "/og/",
        "/og-html/",
        "/auth/voter/",
    ];
    PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

fn fallback_html(status: StatusCode, title: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head><body><h1>{}</h1><p>{title}</p></body></html>",
        status.as_u16()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request as HttpRequest, header};
    use axum::routing::get;
    use serde_json::json;
    use std::fmt;
    use tower::ServiceExt;

    struct FailingTemplate;

    impl fmt::Display for FailingTemplate {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("")
        }
    }

    impl askama::FastWritable for FailingTemplate {
        fn write_into<W: fmt::Write + ?Sized>(
            &self,
            _dest: &mut W,
            _values: &dyn askama::Values,
        ) -> askama::Result<()> {
            Err(askama::Error::Fmt)
        }
    }

    impl Template for FailingTemplate {
        const SIZE_HINT: usize = 0;

        fn render_into_with_values<W: fmt::Write + ?Sized>(
            &self,
            _writer: &mut W,
            _values: &dyn askama::Values,
        ) -> askama::Result<()> {
            Err(askama::Error::Fmt)
        }
    }

    fn test_state() -> AppState {
        AppState {
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://localhost/krabarena_test")
                .expect("lazy pool"),
            proxy_pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://localhost/krabarena_test")
                .expect("lazy pool"),
            jwt_secret: "test-secret".into(),
            blob_store_url: "http://localhost:3001/blob".into(),
            private_blob_store_url: None,
            public_base_url: "http://localhost:3000".into(),
            discord_monitor: crate::services::discord_monitor::DiscordMonitorConfig::from_lookup(
                |_| None,
            ),
            email_service: None,
            gemini: crate::services::gemini_service::GeminiClient::new(None),
            proxy_http: reqwest::Client::new(),
            rate_limiter: None,
            leaderboards: std::sync::Arc::new(crate::LeaderboardCaches::new()),
            recaptcha: crate::RecaptchaConfig::default(),
        }
    }

    async fn body_text(body: Body) -> String {
        let bytes = to_bytes(body, usize::MAX).await.expect("read body");
        String::from_utf8(bytes.to_vec()).expect("utf8 body")
    }

    #[test]
    fn error_kind_follows_web_path() {
        assert_eq!(
            not_found_kind_from_path("/battles/missing"),
            ErrorPageKind::Battle
        );
        assert_eq!(
            not_found_kind_from_path("/claims/missing"),
            ErrorPageKind::Claim
        );
        assert_eq!(not_found_kind_from_path("/missing"), ErrorPageKind::Generic);
    }

    #[test]
    fn html_error_policy_keeps_api_and_static_out() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, "text/html".parse().unwrap());
        assert!(should_render_html_error("/battles/missing", &headers));
        assert!(!should_render_html_error("/v1/claims/missing", &headers));
        assert!(!should_render_html_error("/static/missing.css", &headers));
        assert!(!should_render_html_error("/auth/voter/me", &headers));
    }

    #[test]
    fn html_error_policy_respects_accept_header() {
        let headers = HeaderMap::new();
        assert!(should_render_html_error("/missing", &headers));

        let mut html_headers = HeaderMap::new();
        html_headers.insert(header::ACCEPT, "text/html".parse().unwrap());
        assert!(should_render_html_error("/missing", &html_headers));

        let mut wildcard_headers = HeaderMap::new();
        wildcard_headers.insert(header::ACCEPT, "*/*".parse().unwrap());
        assert!(should_render_html_error("/missing", &wildcard_headers));

        let mut json_headers = HeaderMap::new();
        json_headers.insert(header::ACCEPT, "application/json".parse().unwrap());
        assert!(!should_render_html_error("/missing", &json_headers));
    }

    #[tokio::test]
    async fn render_404_contains_kind_specific_copy_and_cta() {
        let response = render_404("https://krabarena.com", "/claims/missing");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_text(response.into_body()).await;
        assert!(body.contains("404"));
        assert!(body.contains("Claim not found"));
        assert!(body.contains("Browse Claims"));
        assert!(body.contains("/?tab=claims"));
    }

    #[tokio::test]
    async fn middleware_replaces_web_404_with_html() {
        let app = Router::new()
            .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
            .layer(axum::middleware::from_fn_with_state(
                test_state(),
                html_error_pages,
            ));

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/missing")
                    .header(header::ACCEPT, "text/html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.starts_with("text/html"));
        let body = body_text(response.into_body()).await;
        assert!(body.contains("Page not found"));
        assert!(body.contains("Back to home"));
    }

    #[tokio::test]
    async fn middleware_does_not_replace_api_json_404() {
        let app = Router::new()
            .route(
                "/v1/claims/missing",
                get(|| async {
                    (
                        StatusCode::NOT_FOUND,
                        axum::Json(json!({ "error": { "code": "CLAIM_NOT_FOUND" } })),
                    )
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                test_state(),
                html_error_pages,
            ));

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/v1/claims/missing")
                    .header(header::ACCEPT, "text/html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.starts_with("application/json"));
        let body = body_text(response.into_body()).await;
        assert!(body.contains("CLAIM_NOT_FOUND"));
        assert!(!body.contains("Page not found"));
    }

    #[tokio::test]
    async fn middleware_does_not_replace_static_404() {
        let app = Router::new()
            .route(
                "/static/missing.css",
                get(|| async { StatusCode::NOT_FOUND }),
            )
            .layer(axum::middleware::from_fn_with_state(
                test_state(),
                html_error_pages,
            ));

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/static/missing.css")
                    .header(header::ACCEPT, "text/html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_text(response.into_body()).await;
        assert!(!body.contains("Page not found"));
    }

    #[tokio::test]
    async fn middleware_replaces_web_server_error_with_html() {
        let app = Router::new()
            .route(
                "/broken",
                get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .layer(axum::middleware::from_fn_with_state(
                test_state(),
                html_error_pages,
            ));

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/broken")
                    .header(header::ACCEPT, "text/html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_text(response.into_body()).await;
        assert!(body.contains("Something went wrong"));
        assert!(!body.contains("render failed"));
        assert!(!body.contains("database"));
    }

    #[tokio::test]
    async fn middleware_replaces_web_auth_errors_with_html() {
        for (status, title) in [
            (StatusCode::UNAUTHORIZED, "Sign in required"),
            (StatusCode::FORBIDDEN, "Access denied"),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Service temporarily unavailable",
            ),
        ] {
            let app = Router::new()
                .route("/protected", get(move || async move { status }))
                .layer(axum::middleware::from_fn_with_state(
                    test_state(),
                    html_error_pages,
                ));

            let response = app
                .oneshot(
                    HttpRequest::builder()
                        .uri("/protected")
                        .header(header::ACCEPT, "text/html")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), status);
            let body = body_text(response.into_body()).await;
            assert!(body.contains(title));
        }
    }

    #[tokio::test]
    async fn json_accept_does_not_get_html_error_page() {
        let app = Router::new()
            .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
            .layer(axum::middleware::from_fn_with_state(
                test_state(),
                html_error_pages,
            ));

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/missing")
                    .header(header::ACCEPT, "application/json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_text(response.into_body()).await;
        assert!(!body.contains("Page not found"));
    }

    #[tokio::test]
    async fn template_render_failure_returns_branded_500() {
        let response = render_template_or_500(
            FailingTemplate,
            "https://krabarena.com",
            "/broken",
            "failing-template",
        );

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_text(response.into_body()).await;
        assert!(body.contains("Something went wrong"));
        assert!(!body.contains("Error:"));
        assert!(!body.contains("failing-template"));
    }
}
