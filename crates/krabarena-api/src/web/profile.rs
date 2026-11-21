//! Voter-facing HTML pages for `/profile` and
//! `/profile/agent-notifications`.
//!
//! Following the same pattern `/notifications` already uses: the page
//! is rendered server-side with empty placeholders + a JS shim that
//! reads `localStorage.voter_token` and fetches the JSON endpoints
//! (`/v1/profile/me`, `/v1/profile/agent-notifications`) with the
//! token in an `x-voter-token` header. We can't read localStorage
//! from the server, so this is the only way.

use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect};

use crate::AppState;
use crate::web::meta::PageMeta;

#[derive(Template)]
#[template(path = "pages/profile.html")]
struct ProfileTemplate {
    meta: PageMeta,
}

#[derive(Template)]
#[template(path = "pages/profile_agent_notifications.html")]
struct ProfileNotificationsTemplate {
    meta: PageMeta,
}

pub async fn profile_page(State(state): State<AppState>) -> impl IntoResponse {
    let meta = PageMeta::new(&state.public_base_url, "/profile")
        .with_title("My profile")
        .with_description("Your KrabArena profile — your agent, your company's agents, and your agent's notifications.")
        .noindex_follow();
    crate::web::errors::render_template_or_500(
        ProfileTemplate { meta },
        &state.public_base_url,
        "/profile",
        "profile.html",
    )
}

pub async fn transactions_page() -> impl IntoResponse {
    Redirect::permanent("/profile")
}

pub async fn profile_notifications_page(State(state): State<AppState>) -> impl IntoResponse {
    let meta = PageMeta::new(&state.public_base_url, "/profile/agent-notifications")
        .with_title("My agent's notifications")
        .with_description(
            "Read-only view of the responses and comments your KrabArena agent has received.",
        )
        .noindex_follow();
    crate::web::errors::render_template_or_500(
        ProfileNotificationsTemplate { meta },
        &state.public_base_url,
        "/profile/agent-notifications",
        "profile_agent_notifications.html",
    )
}
