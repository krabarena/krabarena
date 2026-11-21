//! Email preference management — unsubscribe link + in-app settings page.
//!
//! Auth sources, in priority order:
//!   1. `Authorization: Bearer …` or `x-krabarena-token` header with a
//!      valid JWT. CLI-driven flow: agent sets the token in the
//!      browser via a custom header (only really works from the CLI's
//!      own embedded browser tab; not realistic for humans).
//!   2. `?u=<uuid>&t=<hmac>` signed query params on the URL. This is
//!      the link every digest email sends, so human browser users
//!      arriving from their inbox can actually open the page and
//!      change their frequency without having to set a request
//!      header they can't set. HMAC is the same scheme as the
//!      one-click unsubscribe (`unsubscribe::sign/verify`).

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::web::meta::PageMeta;
use crate::web::unsubscribe;

#[derive(Deserialize)]
pub struct UnsubQuery {
    pub u: Uuid,
    pub t: String,
}

/// Optional signed-token auth for `/profile/email-preferences`. Both
/// fields must be present + the HMAC must verify; partial or missing
/// values are treated as "no token supplied, fall back to JWT".
#[derive(Deserialize, Default)]
pub struct PrefsAuthQuery {
    #[serde(default)]
    pub u: Option<Uuid>,
    #[serde(default)]
    pub t: Option<String>,
}

#[derive(Template)]
#[template(path = "pages/prefs_unsub_ok.html")]
struct UnsubOkTemplate {
    meta: PageMeta,
    display_name: String,
    /// What was turned off ("digest emails" / "instant notification emails").
    scope: &'static str,
    /// "Resubscribe from settings" link.
    prefs_url: String,
}

#[derive(Template)]
#[template(path = "pages/prefs_settings.html")]
struct PrefsTemplate {
    meta: PageMeta,
    display_name: String,
    frequency: String,
    /// Instant notification emails on (`user_notification_prefs`,
    /// migration 070; no row = on).
    instant_email: bool,
    saved: bool,
    /// URL the <form action=…> submits to. Contains `?u=&t=` when the
    /// user arrived via a signed email link so the POST authenticates
    /// the same way the GET did.
    action_url: String,
}

/// `GET /email/unsub?u=<uuid>&t=<hmac>` — honoured for both GET and POST
/// so we satisfy RFC 8058's one-click header as well as the user-facing
/// link in the digest email.
pub async fn unsub(State(state): State<AppState>, Query(q): Query<UnsubQuery>) -> Response {
    if !unsubscribe::verify(q.u, &q.t, &state.jwt_secret) {
        return crate::web::errors::render_status(
            &state.public_base_url,
            "/email/unsub",
            StatusCode::UNAUTHORIZED,
        );
    }

    let _ = sqlx::query(
        r#"
        INSERT INTO user_email_preferences (user_id, digest_frequency, updated_at)
        VALUES ($1, 'off', now())
        ON CONFLICT (user_id) DO UPDATE
        SET digest_frequency = 'off', updated_at = now()
        "#,
    )
    .bind(q.u)
    .execute(&state.pool)
    .await;

    let display_name =
        sqlx::query_scalar::<_, String>("SELECT display_name FROM users WHERE id = $1")
            .bind(q.u)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "there".into());

    let meta = PageMeta::new(&state.public_base_url, "/email/unsub")
        .with_title("Unsubscribed")
        .with_description("You've been unsubscribed from KrabArena email updates.")
        .noindex_follow();
    crate::web::errors::render_template_or_500(
        UnsubOkTemplate {
            meta,
            display_name,
            scope: "digest emails",
            prefs_url: format!("/profile/email-preferences?u={}&t={}", q.u, q.t),
        },
        &state.public_base_url,
        "/email/unsub",
        "prefs_unsub_ok.html",
    )
}

/// Same as `unsub` but matches RFC 8058 one-click POST body.
pub async fn unsub_post(state: State<AppState>, Query(q): Query<UnsubQuery>) -> Response {
    unsub(state, Query(q)).await
}

#[derive(Deserialize)]
pub struct VoterUnsubQuery {
    pub v: Uuid,
    pub t: String,
}

/// `GET /email/unsub/voter?v=<voter_id>&t=<hmac>` — voter analogue of
/// `unsub` above (migration 068). Voters live in a separate table
/// with independently-generated UUIDs, so a token minted for this
/// route uses the `"voter"`-prefixed HMAC namespace
/// (`unsubscribe::sign_kind`/`verify_kind`) rather than the original
/// unprefixed `sign`/`verify` used for `users` — that keeps a leaked
/// or guessed user-unsubscribe link from also opting out a voter whose
/// id happens to collide.
pub async fn voter_unsub(
    State(state): State<AppState>,
    Query(q): Query<VoterUnsubQuery>,
) -> Response {
    if !unsubscribe::verify_kind("voter", q.v, &q.t, &state.jwt_secret) {
        return crate::web::errors::render_status(
            &state.public_base_url,
            "/email/unsub/voter",
            StatusCode::UNAUTHORIZED,
        );
    }

    let _ = sqlx::query("UPDATE voters SET digest_opt_out = TRUE WHERE id = $1")
        .bind(q.v)
        .execute(&state.pool)
        .await;

    let display_name =
        sqlx::query_scalar::<_, String>("SELECT display_name FROM voters WHERE id = $1")
            .bind(q.v)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "there".into());

    let meta = PageMeta::new(&state.public_base_url, "/email/unsub/voter")
        .with_title("Unsubscribed")
        .with_description("You've been unsubscribed from KrabArena email updates.")
        .noindex_follow();
    crate::web::errors::render_template_or_500(
        UnsubOkTemplate {
            meta,
            display_name,
            scope: "digest emails",
            prefs_url: "/profile".into(),
        },
        &state.public_base_url,
        "/email/unsub/voter",
        "prefs_unsub_ok.html",
    )
}

/// Same as `voter_unsub` but matches RFC 8058 one-click POST body.
pub async fn voter_unsub_post(
    state: State<AppState>,
    Query(q): Query<VoterUnsubQuery>,
) -> Response {
    voter_unsub(state, Query(q)).await
}

/// `GET /email/unsub/instant?u=<uuid>&t=<hmac>` — one-click "stop
/// instant notification emails" link carried by every instant
/// notification email (since 0.43.0). Token namespace `"instant"`
/// (`unsubscribe::sign_kind`), so a digest-unsubscribe token can't be
/// replayed here and vice versa. Only flips
/// `user_notification_prefs.instant_email`; the digest frequency is
/// untouched.
pub async fn instant_unsub(State(state): State<AppState>, Query(q): Query<UnsubQuery>) -> Response {
    use crate::services::notification_email::UNSUB_TOKEN_KIND;
    if !unsubscribe::verify_kind(UNSUB_TOKEN_KIND, q.u, &q.t, &state.jwt_secret) {
        return crate::web::errors::render_status(
            &state.public_base_url,
            "/email/unsub/instant",
            StatusCode::UNAUTHORIZED,
        );
    }

    let _ = set_instant_email(&state, q.u, false).await;

    let display_name =
        sqlx::query_scalar::<_, String>("SELECT display_name FROM users WHERE id = $1")
            .bind(q.u)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "there".into());

    let meta = PageMeta::new(&state.public_base_url, "/email/unsub/instant")
        .with_title("Unsubscribed")
        .with_description("You've turned off instant KrabArena notification emails.")
        .noindex_follow();
    // The preferences page authenticates with the digest-style token,
    // which we can mint here since the instant token just verified.
    let prefs_token = unsubscribe::sign(q.u, &state.jwt_secret);
    crate::web::errors::render_template_or_500(
        UnsubOkTemplate {
            meta,
            display_name,
            scope: "instant notification emails",
            prefs_url: format!("/profile/email-preferences?u={}&t={}", q.u, prefs_token),
        },
        &state.public_base_url,
        "/email/unsub/instant",
        "prefs_unsub_ok.html",
    )
}

/// RFC 8058 one-click POST variant of `instant_unsub`.
pub async fn instant_unsub_post(state: State<AppState>, Query(q): Query<UnsubQuery>) -> Response {
    instant_unsub(state, Query(q)).await
}

async fn set_instant_email(state: &AppState, user_id: Uuid, on: bool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO user_notification_prefs (user_id, instant_email, updated_at)
        VALUES ($1, $2, now())
        ON CONFLICT (user_id) DO UPDATE
        SET instant_email = excluded.instant_email, updated_at = now()
        "#,
    )
    .bind(user_id)
    .bind(on)
    .execute(&state.pool)
    .await
    .map(|_| ())
}

#[derive(Deserialize)]
pub struct PrefsForm {
    pub frequency: String,
    /// Checkbox — present (`"on"`) when checked, absent when not.
    #[serde(default)]
    pub instant_email: Option<String>,
    /// Hidden marker so a form rendered before 0.43.0 (no checkbox)
    /// can't silently turn instant emails off.
    #[serde(default)]
    pub instant_email_present: Option<String>,
}

/// Instant-email value a submitted form asks for: `None` = leave as is
/// (form predates the checkbox), else checked / unchecked.
pub fn form_instant_email(form: &PrefsForm) -> Option<bool> {
    form.instant_email_present
        .as_ref()
        .map(|_| form.instant_email.is_some())
}

/// Resolve the user_id the request is acting on, trying:
///   1. JWT (CLI-style header auth).
///   2. Signed `?u=&t=` query params (email-link style).
///
/// Returns `None` if neither is valid; caller renders the sign-in copy.
fn resolve_user(
    state: &AppState,
    headers: &HeaderMap,
    query: &PrefsAuthQuery,
) -> Option<(Uuid, bool)> {
    // Returns (user_id, came_via_signed_url).
    if let Ok(uid) = crate::routes::auth::extract_user_id(headers, &state.jwt_secret) {
        return Some((uid, false));
    }
    match (&query.u, &query.t) {
        (Some(uid), Some(token)) if unsubscribe::verify(*uid, token, &state.jwt_secret) => {
            Some((*uid, true))
        }
        _ => None,
    }
}

fn build_action_url(query: &PrefsAuthQuery, came_via_signed_url: bool) -> String {
    if came_via_signed_url && let (Some(uid), Some(token)) = (&query.u, &query.t) {
        // The HMAC token is hex (0-9a-f only — URL-safe) and the uuid
        // is also URL-safe, so no percent-encoding needed. Avoids
        // pulling in a urlencoding crate just for this single site.
        format!("/profile/email-preferences?u={uid}&t={token}")
    } else {
        "/profile/email-preferences".into()
    }
}

pub async fn prefs_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PrefsAuthQuery>,
) -> Response {
    render_prefs(&state, &headers, &q, false).await
}

pub async fn prefs_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PrefsAuthQuery>,
    Form(form): Form<PrefsForm>,
) -> Response {
    let Some((user_id, _)) = resolve_user(&state, &headers, &q) else {
        return crate::web::errors::render_status(
            &state.public_base_url,
            "/profile/email-preferences",
            StatusCode::UNAUTHORIZED,
        );
    };

    let freq = match form.frequency.as_str() {
        "daily" | "weekly" | "off" => form.frequency.clone(),
        _ => "daily".to_string(),
    };

    let _ = sqlx::query(
        r#"
        INSERT INTO user_email_preferences (user_id, digest_frequency, updated_at)
        VALUES ($1, $2, now())
        ON CONFLICT (user_id) DO UPDATE
        SET digest_frequency = excluded.digest_frequency, updated_at = now()
        "#,
    )
    .bind(user_id)
    .bind(&freq)
    .execute(&state.pool)
    .await;

    if let Some(on) = form_instant_email(&form) {
        let _ = set_instant_email(&state, user_id, on).await;
    }

    render_prefs(&state, &headers, &q, true).await
}

async fn render_prefs(
    state: &AppState,
    headers: &HeaderMap,
    query: &PrefsAuthQuery,
    saved: bool,
) -> Response {
    let Some((user_id, came_via_signed_url)) = resolve_user(state, headers, query) else {
        return crate::web::errors::render_status(
            &state.public_base_url,
            "/profile/email-preferences",
            StatusCode::UNAUTHORIZED,
        );
    };

    let display_name =
        sqlx::query_scalar::<_, String>("SELECT display_name FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();

    let freq = sqlx::query_scalar::<_, String>(
        "SELECT digest_frequency FROM user_email_preferences WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "daily".into());

    // Missing table (migration 070 not yet applied) or no row = on.
    let instant_email = sqlx::query_scalar::<_, bool>(
        "SELECT instant_email FROM user_notification_prefs WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(true);

    let meta = PageMeta::new(&state.public_base_url, "/profile/email-preferences")
        .with_title("Email preferences")
        .noindex_follow();
    crate::web::errors::render_template_or_500(
        PrefsTemplate {
            meta,
            display_name,
            frequency: freq,
            instant_email,
            saved,
            action_url: build_action_url(query, came_via_signed_url),
        },
        &state.public_base_url,
        "/profile/email-preferences",
        "prefs_settings.html",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(instant: Option<&str>, present: bool) -> PrefsForm {
        PrefsForm {
            frequency: "daily".into(),
            instant_email: instant.map(str::to_string),
            instant_email_present: present.then(|| "1".to_string()),
        }
    }

    #[test]
    fn instant_email_checkbox_semantics() {
        assert_eq!(form_instant_email(&form(Some("on"), true)), Some(true));
        // Unchecked box is absent from the POST body -> off.
        assert_eq!(form_instant_email(&form(None, true)), Some(false));
        // A pre-0.43 form (no hidden marker) never touches the pref.
        assert_eq!(form_instant_email(&form(None, false)), None);
    }
}
