use axum::Json;
use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};

use crate::envelope::ApiResponse;

const INSTALL_SCRIPT: &str = include_str!("../../../../install.sh");
const GCS_BASE: &str = "https://storage.googleapis.com/krabarena-cli-releases/latest";

/// GET /install.sh — serve the installer script
pub async fn install_script() -> impl IntoResponse {
    (
        [
            ("content-type", "text/x-shellscript; charset=utf-8"),
            ("content-disposition", "attachment; filename=\"install.sh\""),
        ],
        INSTALL_SCRIPT,
    )
}

/// GET /cli/{archive} — redirect to GCS for the actual binary download
pub async fn download_cli(Path(archive): Path<String>) -> Response {
    let url = format!("{}/{}", GCS_BASE, archive);
    Redirect::temporary(&url).into_response()
}

/// GET /v1/cli/manifest — serve the command manifest for the dynamic CLI.
pub async fn serve_manifest() -> impl IntoResponse {
    let manifest = krabarena_shared::manifest_builder::build_manifest();
    ([("cache-control", "public, max-age=300")], Json(manifest))
}

/// GET /v1/about — serve about content as JSON for the CLI.
pub async fn about_json() -> impl IntoResponse {
    use krabarena_shared::about;
    let vocab: Vec<serde_json::Value> = about::VOCAB
        .iter()
        .map(|v| {
            serde_json::json!({
                "word": v.word,
                "definition": v.definition,
                "example": v.example,
            })
        })
        .collect();
    let faq: Vec<serde_json::Value> = about::FAQ
        .iter()
        .map(|(q, a)| serde_json::json!({ "question": q, "answer": a }))
        .collect();
    Json(ApiResponse::new(serde_json::json!({
        "tagline": about::TAGLINE,
        "motto": about::MOTTO,
        "mission": about::MISSION,
        "vocab": vocab,
        "faq": faq,
        "sign_off": about::SIGN_OFF,
    })))
}

/// GET /v1/changelog — serve changelog as JSON for the CLI.
///
/// Works without auth so `krab changelog` runs before sign-in.
pub async fn changelog_json(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let is_admin = crate::routes::auth::extract_user_with_role(&headers, &state.jwt_secret)
        .map(|(_, role)| role == "admin")
        .unwrap_or(false);

    Json(ApiResponse::new(serde_json::json!({
        "content": krabarena_shared::changelog::CONTENT,
        "is_admin": is_admin,
    })))
}
