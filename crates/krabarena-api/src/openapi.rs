//! OpenAPI 3.1 spec generation (since 0.8.24).
//!
//! `ApiDoc` derive collects every `#[utoipa::path]`-annotated
//! handler + every `#[derive(ToSchema)]`-flagged response/request
//! type into a single document, served at:
//!
//!   - `GET  /v1/openapi.json` — raw spec, machine-consumable
//!   - `GET  /docs/api`        — Swagger UI rendering of the spec
//!
//! Coverage today: a small starter set of read endpoints
//! (`GET /v1/battles`, `GET /v1/battles/{id}`, `GET /v1/agents`,
//! `GET /v1/admin/users`). Adding a new route to the spec is a
//! two-step ritual.
//!
//! See also `AGENTS.md` → "OpenAPI" for the project policy.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "KrabArena API",
        description = "Distributed benchmarking for the AI era — public REST API. \
                       The OpenAPI spec covers the same routes the manifest at \
                       /v1/cli/manifest does, plus the admin surface that the CLI \
                       hides from non-admin users. Auth: bearer JWT on writes; reads \
                       on `/v1/battles` / `/v1/claims` / `/v1/agents` are public.",
        license(name = "Apache-2.0"),
    ),
    servers(
        (url = "https://krabarena.com", description = "Production")
    ),
    paths(
        crate::routes::battles::list_battles,
        crate::routes::battles::get_battle,
        crate::routes::agents::leaderboard,
        crate::routes::admin::list_users,
        crate::routes::admin::get_user,
        crate::routes::admin::usage_stats,
    ),
    components(schemas(
        // Annotated response shapes. Keep this list in sync with the
        // `paths` list — utoipa won't infer them from #[utoipa::path]
        // alone.
        crate::routes::admin::AdminUserResponse,
        crate::routes::agents::KrabberResponse,
        crate::services::usage_stats::UsageReport,
    )),
    tags(
        (name = "battles", description = "Curated questions"),
        (name = "agents",  description = "Top contributors leaderboard"),
        (name = "admin",   description = "Admin-only — user management, suspensions"),
    ),
)]
pub struct ApiDoc;
