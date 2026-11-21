//! 301-redirect legacy `krabarena.org` traffic to `krabarena.com`.
//!
//! The apex canonical flipped on 2026-05-22 (see CHANGELOG 0.10.0).
//! `.org` lives on permanently as a 301 source so the installer
//! one-liner, existing OAuth redirect URIs, prior blog posts, and
//! Mailgun unsubscribe links keep working without breaking SEO.
//!
//! The middleware is registered near the outermost edge of
//! `build_router` (just inside `web::security_headers`, which wraps it
//! so even the 308 carries hardening headers) so it short-circuits
//! before any handler matches a path. Subdomains other than `www`
//! (e.g. `mg.krabarena.org`, Mailgun's CNAME) don't terminate on this
//! LB and never reach this code.

use axum::extract::Request;
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

pub async fn redirect_org_to_com(req: Request, next: Next) -> Response {
    if let Some(target) = legacy_target(req.headers().get(header::HOST)) {
        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let location = format!("https://{}{}", target, path_and_query);
        return Redirect::permanent(&location).into_response();
    }
    next.run(req).await
}

fn legacy_target(host_header: Option<&axum::http::HeaderValue>) -> Option<&'static str> {
    let raw = host_header?.to_str().ok()?;
    let host = raw.split(':').next().unwrap_or(raw).to_ascii_lowercase();
    match host.as_str() {
        "krabarena.org" => Some("krabarena.com"),
        "www.krabarena.org" => Some("www.krabarena.com"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn h(s: &str) -> HeaderValue {
        HeaderValue::from_str(s).unwrap()
    }

    #[test]
    fn apex_org_redirects_to_com() {
        assert_eq!(
            legacy_target(Some(&h("krabarena.org"))),
            Some("krabarena.com")
        );
    }

    #[test]
    fn www_org_redirects_to_www_com() {
        assert_eq!(
            legacy_target(Some(&h("www.krabarena.org"))),
            Some("www.krabarena.com")
        );
    }

    #[test]
    fn host_with_port_still_matches() {
        assert_eq!(
            legacy_target(Some(&h("krabarena.org:443"))),
            Some("krabarena.com")
        );
    }

    #[test]
    fn mixed_case_host_matches() {
        assert_eq!(
            legacy_target(Some(&h("Krabarena.ORG"))),
            Some("krabarena.com")
        );
    }

    #[test]
    fn com_host_passes_through() {
        assert_eq!(legacy_target(Some(&h("krabarena.com"))), None);
        assert_eq!(legacy_target(Some(&h("www.krabarena.com"))), None);
    }

    #[test]
    fn unknown_subdomain_passes_through() {
        // mg.krabarena.org is Mailgun's MX/CNAME — doesn't terminate on this LB.
        assert_eq!(legacy_target(Some(&h("mg.krabarena.org"))), None);
        assert_eq!(legacy_target(Some(&h("api.krabarena.org"))), None);
    }

    #[test]
    fn missing_host_passes_through() {
        assert_eq!(legacy_target(None), None);
    }

    /// Full middleware exercise — drives a request through a
    /// minimal router that has `redirect_org_to_com` attached and
    /// asserts the response carries the right status + Location.
    ///
    /// Pins the response code to **308** (PERMANENT_REDIRECT). This
    /// is what `Redirect::permanent()` actually emits per RFC 7538
    /// (not 301). The `redirect_e2e` tests against live prod assert
    /// the same thing — together they catch both client-visible
    /// changes (axum upgrades that alter the default code) and
    /// server-side behaviour drift.
    #[tokio::test]
    async fn middleware_emits_308_with_location_for_legacy_org() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode, header};
        use axum::routing::get;
        use axum::{Router, middleware};
        use tower::util::ServiceExt;

        let app = Router::new()
            .route("/whatever", get(|| async { "should not reach" }))
            .layer(middleware::from_fn(redirect_org_to_com));

        let req = Request::builder()
            .uri("/whatever?cb=1")
            .header(header::HOST, "krabarena.org")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            resp.headers().get(header::LOCATION).unwrap(),
            "https://krabarena.com/whatever?cb=1"
        );
    }

    #[tokio::test]
    async fn middleware_passes_through_com_host() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode, header};
        use axum::routing::get;
        use axum::{Router, middleware};
        use tower::util::ServiceExt;

        let app = Router::new()
            .route("/whatever", get(|| async { "served" }))
            .layer(middleware::from_fn(redirect_org_to_com));

        let req = Request::builder()
            .uri("/whatever")
            .header(header::HOST, "krabarena.com")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }
}
