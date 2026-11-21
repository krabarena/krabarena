//! Auth/device middleware. Runs on every non-static request to:
//!   1. issue a stable `ka_device` cookie when absent,
//!   2. resolve the `ka_session` cookie into a voter identity,
//!   3. enforce double-submit CSRF on cookie-authed unsafe methods,
//!   4. fire-and-forget log HTML page visits,
//!   5. resolve a bearer JWT's CURRENT role / suspension / existence
//!      from the DB for the duration of the request (token revocation).
//!
//! It exposes an [`AuthContext`] request extension that handlers read
//! via axum's `Extension<AuthContext>`.

use crate::AppState;
use crate::services::user_log::{LogEvent, spawn_log};
use crate::services::voter_service;
use crate::web::cookies;
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header::SET_COOKIE},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct AuthContext {
    pub device_id: Uuid,
    /// Voter id when a valid `ka_session` cookie was presented.
    pub voter_id: Option<Uuid>,
    pub from_cookie_session: bool,
    pub session_token: Option<String>,
    pub csrf_cookie: Option<String>,
}

/// Page routes we log a `visit` for (excludes assets, APIs, fragments,
/// OG cards, auth endpoints).
pub fn is_html_route(path: &str) -> bool {
    !(path.starts_with("/static")
        || path.starts_with("/v1/")
        || path.starts_with("/auth/")
        || path.starts_with("/fragments/")
        || path.starts_with("/og/")
        || path.starts_with("/.well-known"))
}

/// CSRF is required only for cookie-authed state-changing requests.
pub fn wants_csrf(method: &str, from_cookie_session: bool) -> bool {
    from_cookie_session && matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
}

/// Plain HTML form POSTs that carry their own, stronger CSRF binding and
/// so can't (and needn't) send the `x-csrf-token` header. The CLI
/// device-flow approval (`/auth/device/confirm`) is authorized only by the
/// HttpOnly SameSite=Lax `ka_oauth_state` cookie hashing to the staged
/// row's `browser_binding` — a cross-site POST never carries that cookie.
pub fn csrf_exempt(path: &str) -> bool {
    path == "/auth/device/confirm"
}

pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    // Static assets don't need auth work (and shouldn't cost a DB lookup
    // per file); the device cookie is issued on the HTML page load.
    if path.starts_with("/static/") {
        return next.run(req).await;
    }

    let headers = req.headers().clone();
    let method = req.method().as_str().to_string();

    // 1. Device id (issue if absent).
    let (device_id, set_device) = match cookies::get_cookie(&headers, cookies::DEVICE)
        .and_then(|v| Uuid::parse_str(&v).ok())
    {
        Some(id) => (id, false),
        None => (Uuid::new_v4(), true),
    };

    // 2. Session (cookie preferred).
    let session_token = cookies::get_cookie(&headers, cookies::SESSION);
    let mut voter_id = None;
    let mut from_cookie_session = false;
    if let Some(tok) = session_token.as_deref() {
        if let Ok(Some(v)) = voter_service::get_voter_by_session(&state.pool, tok).await {
            voter_id = Some(v.id);
            from_cookie_session = true;
        }
    }

    let csrf_cookie = cookies::get_cookie(&headers, cookies::CSRF);

    // 3. CSRF double-submit for cookie-authed mutations.
    if wants_csrf(&method, from_cookie_session) && !csrf_exempt(&path) {
        let header_tok = headers.get("x-csrf-token").and_then(|v| v.to_str().ok());
        let ok = matches!((header_tok, csrf_cookie.as_deref()),
            (Some(h), Some(c)) if cookies::ct_eq(h, c));
        if !ok {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("csrf check failed"))
                .unwrap();
        }
    }

    // 4. Visit log (fire-and-forget, HTML GET only).
    if method == "GET" && is_html_route(&path) {
        spawn_log(
            &state.pool,
            LogEvent {
                device_id,
                voter_id,
                user_id: None,
                event: "visit",
                path: Some(path.clone()),
                method: Some(method.clone()),
                referer: headers
                    .get("referer")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
                user_agent: headers
                    .get("user-agent")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            },
        );
    }

    req.extensions_mut().insert(AuthContext {
        device_id,
        voter_id,
        from_cookie_session,
        session_token,
        csrf_cookie,
    });

    // 5. Live JWT state: resolve the bearer token's user against the DB
    //    (role / suspension / existence) so role checks never trust a
    //    stale 30-day claim. See `routes::auth::LiveAuth`.
    let mut resp = crate::routes::auth::with_live_auth(
        &state.pool,
        &headers,
        &state.jwt_secret,
        next.run(req),
    )
    .await;

    if set_device {
        let c = cookies::build(
            cookies::DEVICE,
            &device_id.to_string(),
            cookies::DEVICE_MAX_AGE,
            true,
            cookies::secure_cookies(),
        );
        if let Ok(hv) = c.parse() {
            resp.headers_mut().append(SET_COOKIE, hv);
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_routes_only() {
        assert!(is_html_route("/"));
        assert!(is_html_route("/battles/foo"));
        assert!(is_html_route("/claims/bar"));
        assert!(!is_html_route("/static/dist/x.css"));
        assert!(!is_html_route("/v1/claims/x/votes"));
        assert!(!is_html_route("/fragments/battles/x/claims"));
        assert!(!is_html_route("/auth/migrate"));
    }

    #[test]
    fn csrf_only_for_unsafe_cookie_auth() {
        assert!(wants_csrf("POST", true));
        assert!(wants_csrf("DELETE", true));
        assert!(!wants_csrf("GET", true));
        assert!(!wants_csrf("POST", false));
    }

    #[test]
    fn only_device_confirm_is_csrf_exempt() {
        assert!(csrf_exempt("/auth/device/confirm"));
        assert!(!csrf_exempt("/auth/voter/email/verify"));
        assert!(!csrf_exempt("/auth/device/confirm/x"));
    }
}
