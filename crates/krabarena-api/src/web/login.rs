//! Unified web sign-in page at `/login`.
//!
//! Shows the three voter auth paths (GitHub, Google, email magic-link)
//! in a single card. OAuth URLs are generated server-side from the
//! `*_CLIENT_ID` env vars — a provider with no client id configured is
//! simply omitted, which lets the same template work on prod, staging,
//! and local dev without crashing on missing creds.
//!
//! Auth state itself is still client-side (localStorage), matching the
//! rest of the site; this page just funnels users to the right flow
//! and reuses `/login/callback` for OAuth token delivery.
//!
//! The `?return=<path>` query param is carried into localStorage so
//! `/login/callback` sends the user back where they came from.
//!
//! Note: this is distinct from `/auth/device`, which is the CLI-only
//! device-flow page. `/login` is for human browser users with no CLI
//! session involved.
use askama::Template;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::AppState;
use crate::web::meta::PageMeta;

#[derive(Deserialize)]
pub struct LoginQuery {
    #[serde(default)]
    pub r#return: Option<String>,
}

#[derive(Template)]
#[template(path = "pages/login.html")]
struct LoginTemplate {
    meta: PageMeta,
    github_url: Option<String>,
    google_url: Option<String>,
    return_to: String,
    /// reCAPTCHA Enterprise site key. Empty string = captcha disabled, in
    /// which case the template renders no widget and loads no Google script
    /// (local dev / key-less environments).
    recaptcha_site_key: String,
}

fn sanitize_return(raw: Option<String>) -> String {
    // Only allow same-origin relative paths, and restrict to a safe
    // charset so the value can be embedded inline in a JS string
    // literal without risk of breaking out. Anything weirder falls
    // back to "/" — which also closes the open-redirect hole that
    // `?return=https://evil.example` would otherwise open.
    let allowed = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '/' | '-' | '_' | '.' | '?' | '=' | '&' | '%' | '#' | '+' | ',' | ':' | '@'
            )
    };
    match raw {
        Some(p) if p.starts_with('/') && !p.starts_with("//") && p.chars().all(allowed) => p,
        _ => "/".to_string(),
    }
}

pub async fn login_page(
    State(state): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> impl IntoResponse {
    // Buttons link to our own `/auth/voter/{provider}/start`, which sets
    // the HttpOnly OAuth-state cookie and redirects to the provider with
    // a matching `state` (login-CSRF protection). A provider with no
    // client id configured is omitted.
    let (github_url, google_url) = crate::routes::oauth::web_provider_links();

    let meta = PageMeta::new(&state.public_base_url, "/login")
        .with_title("Sign in")
        .with_description("Sign in to KrabArena with GitHub, Google, or email.")
        .noindex_follow();

    crate::web::errors::render_template_or_500(
        LoginTemplate {
            meta,
            github_url,
            google_url,
            return_to: sanitize_return(q.r#return),
            recaptcha_site_key: state.recaptcha.site_key.clone(),
        },
        &state.public_base_url,
        "/login",
        "login.html",
    )
}
