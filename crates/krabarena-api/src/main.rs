use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{any, get, post};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeFile;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use krabarena_api::db::DB_QUERY_COUNT;

// Module tree + AppState live in `lib.rs` so other entry points
// (tests, future binaries) can share the same code.
use krabarena_api::{
    AppState, LeaderboardCaches, RecaptchaConfig, db, openapi, routes, services, web,
};
use utoipa::OpenApi;

/// Does this base URL look like a production deployment? Used by the
/// JWT-secret guard to decide whether the `dev-secret` default is fatal.
/// A localhost / 127.0.0.1 / `.run.app` (Cloud Run preview) / explicit
/// test host is treated as non-prod; anything else (a real domain) is
/// production. Conservative on purpose — better to fail a borderline
/// host than to silently sign forgeable tokens on a public domain.
fn is_production_host(public_base_url: &str) -> bool {
    let u = public_base_url.to_ascii_lowercase();
    let local = u.contains("localhost")
        || u.contains("127.0.0.1")
        || u.contains("0.0.0.0")
        || u.contains("://test")
        || u.contains("krabarena-test");
    !local
}

/// Insecure, publicly-known signing key used only when the operator
/// explicitly opts in with `KRAB_DEV_INSECURE_JWT=1` (local dev).
const INSECURE_DEV_JWT_SECRET: &str = "dev-secret";

/// Resolve the JWT signing secret. Fails CLOSED: an unset/blank
/// `JWT_SECRET` used to silently fall back to `dev-secret` (anyone could
/// forge admin tokens offline) unless `PUBLIC_BASE_URL` happened to be a
/// prod host. Now the server refuses to start without a secret unless
/// the operator explicitly sets `KRAB_DEV_INSECURE_JWT=1`.
fn resolve_jwt_secret(
    jwt_secret: Option<String>,
    insecure_opt_in: Option<String>,
) -> Result<String, String> {
    match jwt_secret.filter(|s| !s.trim().is_empty()) {
        Some(s) => Ok(s),
        None if insecure_opt_in.as_deref().map(str::trim) == Some("1") => {
            eprintln!(
                "WARNING: JWT_SECRET unset — KRAB_DEV_INSECURE_JWT=1 set, signing tokens with \
                 the public `{INSECURE_DEV_JWT_SECRET}` key. Never do this outside local dev."
            );
            Ok(INSECURE_DEV_JWT_SECRET.to_string())
        }
        None => Err(
            "JWT_SECRET is not set. Set a strong JWT_SECRET (e.g. `openssl rand -hex 32`), \
             or for throwaway local dev only, set KRAB_DEV_INSECURE_JWT=1 to use the insecure \
             `dev-secret` key."
                .to_string(),
        ),
    }
}

/// Build the per-IP rate limiter from `REDIS_URL`. Returns `None`
/// (limiter disabled, middleware is a pass-through) when the var is
/// unset/empty or the pool can't be constructed. Pool construction does
/// not connect — deadpool connects lazily on first use — so a Redis
/// that's down at boot doesn't block startup; the middleware fails open
/// at request time.
fn build_rate_limiter() -> Option<web::rate_limit::RateLimiter> {
    let url = std::env::var("REDIS_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())?;
    match deadpool_redis::Config::from_url(url).create_pool(Some(deadpool_redis::Runtime::Tokio1)) {
        Ok(pool) => {
            let limits = web::rate_limit::TierLimits {
                global_per_min: env_u32("RATE_LIMIT_GLOBAL_PER_MIN", 120),
                strict_per_min: env_u32("RATE_LIMIT_STRICT_PER_MIN", 10),
                window_secs: 60,
            };
            tracing::info!(
                global = limits.global_per_min,
                strict = limits.strict_per_min,
                "Rate limiter enabled (Redis)"
            );
            Some(web::rate_limit::RateLimiter::new(pool, limits))
        }
        Err(e) => {
            tracing::warn!(error = %e, "REDIS_URL set but pool build failed — rate limiting disabled");
            None
        }
    }
}

/// Parse a `u32` env var, falling back to `default` when unset or
/// unparseable.
fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

fn build_router(state: AppState) -> Router {
    Router::new()
        // Admin (config, CMS cache reload, user management — requires admin role)
        .route(
            "/internal/discord-monitor/run",
            post(routes::discord_monitor::run),
        )
        .route(
            "/v1/admin/config/{key}",
            get(routes::admin::get_config).put(routes::admin::set_config),
        )
        .route("/v1/admin/cms/reload", post(routes::admin::reload_cms))
        // Vendor→logo map (data-driven, migration 055): list / upsert /
        // delete the rows the battle-solutions backfill derives
        // competitor logos from. Replaces the old hardcoded const.
        .route(
            "/v1/admin/vendor-logos",
            get(routes::admin::list_vendor_logos)
                .post(routes::admin::upsert_vendor_logo)
                .delete(routes::admin::delete_vendor_logo),
        )
        // Self-hosted logos + brand colors (migration 057): download each
        // vendor's favicon into our GCS bucket, extract its dominant color,
        // and store `stored_url`/`brand_color`. Run once post-deploy.
        .route(
            "/v1/admin/vendor-logos/refresh-assets",
            post(routes::admin::refresh_vendor_logo_assets),
        )
        // CLI telemetry inbox — authenticated, dedup-on-write.
        // See migration 027 + routes/telemetry.rs.
        .route("/v1/telemetry/cli", post(routes::telemetry::ingest_cli))
        .route("/v1/admin/users", get(routes::admin::list_users))
        // Voter digest audience (migration 068) — mirrors
        // /v1/admin/users?digest=eligible but for the separate
        // `voters` table.
        .route("/v1/admin/voters", get(routes::admin::list_voters))
        // Usage stats (crawlers / humans / CLI / content) — data source
        // for the daily digest.
        .route("/v1/admin/usage", get(routes::admin::usage_stats))
        // Same path slot accepts UUID *or* email (`get_user` resolves);
        // PATCH only accepts UUID. Both are gated by `require_admin`.
        .route(
            "/v1/admin/users/{id}",
            get(routes::admin::get_user).patch(routes::admin::update_user),
        )
        // 0.19.0 — Mint a fresh JWT for an existing user. Used to
        // bootstrap the merged `krabagent` bot after migration 049
        // (which can't mint tokens from psql), or any future case
        // where we need to issue a token outside the device-flow /
        // bot-creation paths. Admin-only.
        .route(
            "/v1/admin/users/{user_id}/mint-jwt",
            post(routes::admin::mint_user_jwt),
        )
        // Solutions registry endpoints removed in 0.12.0; competitor
        // logos now live inline in each claim's
        // `structure.json.leaderboard[i].logo_url`. Migration 036 drops
        // the underlying table. See CHANGELOG.md.
        //
        // 0.13.0 reintroduces a Battle-owned solutions list at
        // `battles.solutions` (migration 038); these admin endpoints
        // backfill it by walking every claim's leaderboard and
        // deriving vendor logos from a hand-curated regex map. The
        // per-row `logo_url` inside structure.json is the
        // standalone-claim fallback and stays.
        .route(
            "/v1/admin/battles/{battle_id}/backfill-solutions",
            post(routes::admin::backfill_battle_solutions),
        )
        .route(
            "/v1/admin/battles/backfill-all-solutions",
            post(routes::admin::backfill_all_battle_solutions),
        )
        // 0.21.0 — Admin reversible soft-delete + restore for Battles
        // and Claims, plus a corpus-wide summary backfill. All gated
        // by `require_admin` (platform moderation). Soft-delete sets
        // `deleted_at = now()`; every public read path filters
        // `deleted_at IS NULL` so the row vanishes (detail → 404).
        // Restore clears the tombstone. backfill-summaries re-runs the
        // FULL regen for every live battle with the new
        // refuted/deleted-aware logic.
        .route(
            "/v1/admin/battles/backfill-summaries",
            post(routes::admin::backfill_all_summaries),
        )
        .route(
            "/v1/admin/battles",
            get(routes::admin::list_battles_admin).post(routes::admin::create_battle_admin),
        )
        .route(
            "/v1/admin/battles/{battle_id}/restore",
            post(routes::admin::restore_battle),
        )
        .route(
            "/v1/admin/battles/{battle_id}",
            get(routes::admin::get_battle_admin)
                .patch(routes::admin::update_battle_admin)
                .delete(routes::admin::soft_delete_battle),
        )
        .route(
            "/v1/admin/claims/{claim_id}/restore",
            post(routes::admin::restore_claim),
        )
        .route(
            "/v1/admin/claims/{claim_id}",
            get(routes::admin::get_claim_admin)
                .patch(routes::admin::update_claim_admin)
                .delete(routes::admin::soft_delete_claim),
        )
        .route("/v1/admin/claims", get(routes::admin::list_claims_admin))
        // Auth (krab CLI users)
        .route("/v1/auth/email", post(routes::auth::initiate_email))
        .route("/v1/auth/email/verify", post(routes::auth::verify_email))
        .route("/auth/verify", get(routes::auth::verify_email_link))
        .route("/auth/migrate", post(routes::auth::migrate))
        // Battles (curated contested questions; create gated to editor/admin)
        .route(
            "/v1/battles",
            get(routes::battles::list_battles).post(routes::battles::create_battle),
        )
        .route(
            "/v1/battles/{battle_id}",
            get(routes::battles::get_battle).patch(routes::battles::update_battle),
        )
        .route(
            "/v1/battles/{battle_id}/sub-battles",
            get(routes::battles::list_sub_battles),
        )
        // Proxy (sponsored API access for claims)
        .route(
            "/v1/battles/{battle_id}/proxy-spec",
            get(routes::battles::get_proxy_spec).put(routes::battles::set_proxy_spec),
        )
        .route(
            "/v1/battles/{battle_id}/proxy-info",
            get(routes::battles::get_proxy_info),
        )
        .route(
            "/v1/battles/{battle_id}/review",
            post(routes::battles::review_battle),
        )
        .route(
            "/v1/battles/{battle_id}/regenerate-summary",
            post(routes::battles::regenerate_summary),
        )
        // Per-battle presentation guidance + knobs (author-or-admin).
        .route(
            "/v1/battles/{battle_id}/presentation",
            get(routes::battles::get_presentation).put(routes::battles::set_presentation),
        )
        // (Re)generate crisp benchmark column labels via the LLM labeler.
        .route(
            "/v1/battles/{battle_id}/relabel",
            post(routes::battles::relabel_battle),
        )
        // Eager OG cache-warm on demand (admin-gated). Re-renders +
        // re-caches the battle's OG card and its live claims' cards.
        .route(
            "/v1/battles/{battle_id}/regenerate-og",
            post(routes::battles::regenerate_og),
        )
        .route(
            routes::proxy::PROXY_PATH_ROUTE,
            any(routes::proxy::handle_proxy),
        )
        .route(
            routes::proxy::PROXY_ROOT_ROUTE,
            any(routes::proxy::handle_proxy_root),
        )
        .route(
            routes::proxy::PROXY_ROOT_SLASH_ROUTE,
            any(routes::proxy::handle_proxy_root),
        )
        // Battle cover image (editor-only upload) — migration 032.
        .route(
            "/v1/battles/{battle_id}/cover-image",
            post(routes::battles::upload_cover_image).delete(routes::battles::delete_cover_image),
        )
        // Battle-scoped comments (since migration 044, 2026-06-01).
        // Same shape as claim comments; KrabScout uses this to file
        // one-paragraph link+summary drops on matching open Battles.
        .route(
            "/v1/battles/{battle_id}/comments",
            get(routes::comments::list_battle_comments)
                .post(routes::comments::create_battle_comment),
        )
        // Claims
        .route(
            "/v1/battles/{battle_id}/claims",
            get(routes::claims::list_claims).post(routes::claims::create_claim),
        )
        // Standalone claims (no battle yet, since migration 035).
        // Author posts to `POST /v1/claims` when there's no curated
        // Battle to attach to; can later be moved via
        // `POST /v1/claims/{id}/adopt`. Visibility flips through the
        // admin-only `POST /v1/admin/claims/{id}/promote`.
        .route(
            "/v1/claims",
            get(routes::claims::list_latest_claims).post(routes::claims::create_standalone_claim),
        )
        .route(
            "/v1/claims/standalone",
            get(routes::claims::list_standalone_claims),
        )
        .route(
            "/v1/claims/{claim_id}/adopt",
            post(routes::claims::adopt_claim),
        )
        .route(
            "/v1/admin/claims/{claim_id}/promote",
            post(routes::claims::promote_claim),
        )
        // Admin recheck — re-fire `claim.posted` so KrabChecker / any
        // active webhook subscriber re-runs its workflow against a
        // specific Claim. Platform admin only (NOT org-admin);
        // moderation, not org management. See 0.15.2 CHANGELOG.
        .route(
            "/v1/admin/claims/{claim_id}/recheck",
            post(routes::admin::recheck_claim),
        )
        // Backfill: (re)generate the reviewer's viral-share layer
        // (og_summary + presentation) for a Claim that predates the
        // feature, or whose presentation needs a refresh, via a single
        // Gemini call (not a full re-review). Platform admin only.
        .route(
            "/v1/admin/claims/{claim_id}/regenerate-presentation",
            post(routes::admin::regenerate_presentation),
        )
        .route(
            "/v1/claims/{claim_id}",
            get(routes::claims::get_claim).delete(routes::claims::delete_claim),
        )
        .route(
            "/v1/claims/{claim_id}/artifact",
            get(routes::claims::get_artifact),
        )
        // Private artifact — author / battle author / admin only,
        // streamed from the non-public bucket.
        .route(
            "/v1/claims/{claim_id}/private-artifact",
            get(routes::claims::get_private_artifact),
        )
        // Revise a claim: posts a NEW version superseding {claim_id}.
        // Author-or-admin; battle inferred from the target server-side.
        .route(
            "/v1/claims/{claim_id}/edit",
            post(routes::claims::edit_claim),
        )
        .route(
            "/v1/claims/{claim_id}/lineage",
            get(routes::claims::get_lineage),
        )
        // Generic image upload — any authenticated user POSTs a
        // multipart `image` field and gets back `{ url, suggested_alt,
        // markdown }`. Paste the `markdown` snippet into a claim body
        // (or anywhere else markdown is rendered). No DB tracking;
        // image metadata lives in the markdown itself. See
        // `services::image_service` and `routes::images`. The bundle-
        // extraction pipeline (`services::claim_media`) stays the
        // primary path for images packaged with the artifact tarball;
        // this is the alternative when an author wants to attach a
        // standalone figure without a bundle round-trip.
        .route("/v1/images", post(routes::images::upload_image))
        // KrabReviewer
        .route("/v1/claims/review", post(routes::reviews::create_review))
        .route("/v1/reviews/{review_id}", get(routes::reviews::get_review))
        .route(
            "/v1/claims/{claim_id}/review",
            get(routes::reviews::get_shell_review),
        )
        .route(
            "/v1/users/me/moderation",
            get(routes::reviews::my_moderation_status),
        )
        // Profile-photo upload (0.18.1). Multipart `image` field OR
        // JSON `{ url }`. Updates `users.avatar_url` (migration 046)
        // and echoes the refreshed `UserResponse`.
        .route("/v1/users/me/avatar", post(routes::users::upload_avatar))
        .route(
            "/v1/users/me/profile",
            get(routes::users::get_my_profile).patch(routes::users::patch_my_profile),
        )
        // Verifications (Verify / Refute — replaces Response)
        .route(
            "/v1/claims/{claim_id}/verifications",
            get(routes::verifications::list_verifications)
                .post(routes::verifications::create_verification),
        )
        // Comments
        .route(
            "/v1/claims/{claim_id}/comments",
            get(routes::comments::list_comments).post(routes::comments::create_comment),
        )
        // Activity (verifications + comments merged)
        .route(
            "/v1/claims/{claim_id}/activity",
            get(routes::activity::claim_activity),
        )
        // Notifications (activity on user's Claims)
        .route(
            "/v1/notifications",
            get(routes::activity::user_notifications),
        )
        .route(
            "/v1/notifications/unread-count",
            get(routes::activity::notifications_unread_count),
        )
        .route(
            "/v1/notifications/read-all",
            post(routes::activity::notifications_mark_all_read),
        )
        .route(
            "/v1/notifications/{id}/read",
            post(routes::activity::notification_mark_read),
        )
        // Votes (human voters — upvote/downvote on Claims)
        .route(
            "/v1/claims/{claim_id}/votes",
            get(routes::votes::get_vote_counts).post(routes::votes::cast_vote),
        )
        // Battle candidates (community-proposed Battles)
        .route(
            "/v1/battle-candidates",
            get(routes::battle_candidates::list).post(routes::battle_candidates::create),
        )
        .route(
            "/v1/battle-candidates/{id}/vote",
            post(routes::battle_candidates::vote),
        )
        .route(
            "/v1/battle-candidates/{id}/unvote",
            post(routes::battle_candidates::unvote),
        )
        .route(
            "/v1/battle-candidates/{id}",
            axum::routing::delete(routes::battle_candidates::delete),
        )
        // Voter OAuth
        .route("/auth/voter/email", post(routes::oauth::email_login))
        .route(
            "/auth/voter/email/verify",
            post(routes::oauth::email_verify),
        )
        .route(
            "/auth/voter/github/callback",
            get(routes::oauth::github_callback),
        )
        .route(
            "/auth/voter/google/callback",
            get(routes::oauth::google_callback),
        )
        .route(
            "/auth/voter/linkedin/callback",
            get(routes::oauth::linkedin_callback),
        )
        // Provider sign-in entry: sets the HttpOnly OAuth-state nonce
        // cookie and redirects to the provider with a matching `state`.
        .route(
            "/auth/voter/{provider}/start",
            get(routes::oauth::oauth_start),
        )
        .route("/auth/voter/me", get(routes::oauth::voter_me))
        .route("/auth/device", get(routes::oauth::device_login))
        .route("/auth/device/confirm", post(routes::oauth::device_confirm))
        .route("/v1/auth/device/poll", get(routes::oauth::device_poll))
        .route(
            "/auth/voter/github/client_id",
            get(routes::oauth::github_client_id),
        )
        .route("/login", get(web::login::login_page))
        .route("/login/callback", get(routes::oauth::login_callback))
        // Health
        .route("/health", get(routes::health::health))
        // Agents (leaderboard + profiles; was /v1/krabbers)
        .route("/v1/agents", get(routes::agents::leaderboard))
        .route("/v1/agents/{user_id}", get(routes::agents::contributor))
        .route(
            "/v1/agents/{user_id}/profile",
            axum::routing::patch(routes::users::patch_agent_profile),
        )
        // Arena (agent-backend/model leaderboard)
        .route("/v1/arena", get(routes::arena::arena_leaderboard))
        // Hybrid search — semantic (Gemini gemini-embedding-001 cosine)
        // + lexical (pg_trgm) + freshness + hotness across battles /
        // claims / users. See `services::search_service` for the ranking
        // formula and `services::embedding_service` for the pluggable
        // provider (Gemini with a deterministic SHA-256 hash fallback
        // when GEMINI_API_KEY is unset, so the column shape stays stable
        // across dev / prod / tests).
        .route("/v1/search", get(routes::search::search))
        // Slim typeahead for the header + /search boxes — same retrieval
        // as /v1/search, capped small and projected to {kind,title,url}.
        .route("/v1/search/suggest", get(routes::search::suggest))
        // Unified tag dictionary (domain_tags ∪ topic_tags) — powers the
        // home-page tag autocomplete and the `/t/<tag>` landing pages.
        .route("/v1/tags", get(routes::tags::list_tags))
        // Webhook subscriptions (since 0.14.0). Any authenticated
        // user can subscribe to platform events; secret is returned
        // exactly once on subscribe. See `docs/webhooks.md` + the
        // `webhook_dispatch` service.
        .route(
            "/v1/webhooks",
            get(routes::webhooks::list).post(routes::webhooks::subscribe),
        )
        .route(
            "/v1/webhooks/{id}",
            axum::routing::delete(routes::webhooks::delete),
        )
        .route(
            "/v1/webhooks/{id}/test",
            post(routes::webhooks::test_delivery),
        )
        // Orgs / entities (since 0.15.0). `entities` is the table;
        // "org" is the user-facing label. PR A adds the multi-admin
        // org model (`users.org_role`) + a generic change-permissions
        // endpoint that handles promotion and demotion (transfer is
        // now just "promote another member then demote yourself").
        // PR B will add admin-only bot creation here.
        .route("/v1/me/org", get(routes::orgs::get_my_org))
        .route("/v1/me/org/members", get(routes::orgs::list_my_org_members))
        .route("/v1/orgs/{id}", get(routes::orgs::get_org))
        .route("/v1/orgs/{id}/members", get(routes::orgs::list_members))
        .route(
            "/v1/orgs/{id}/members/{user_id}/permissions",
            post(routes::orgs::set_member_permissions),
        )
        .route(
            "/v1/me/org/members/{user_id}/permissions",
            post(routes::orgs::set_my_org_member_permissions),
        )
        // Bot creation + lifecycle (PR B — since 0.15.1). Org-admin
        // only. The create response is the one and only time the
        // bot's JWT is surfaced; subsequent reads don't echo it,
        // same convention as webhook subscription secrets in #157.
        .route("/v1/orgs/{id}/bots", post(routes::orgs::create_bot))
        .route("/v1/me/org/bots", post(routes::orgs::create_bot_my_org))
        .route(
            "/v1/orgs/{id}/bots/{user_id}/rotate-token",
            post(routes::orgs::rotate_bot_token),
        )
        .route(
            "/v1/me/org/bots/{user_id}/rotate-token",
            post(routes::orgs::rotate_bot_token_my_org),
        )
        .route(
            "/v1/orgs/{id}/bots/{user_id}",
            axum::routing::delete(routes::orgs::delete_bot),
        )
        .route(
            "/v1/me/org/bots/{user_id}",
            axum::routing::delete(routes::orgs::delete_bot_my_org),
        )
        // Web UI (HTML pages, human-browsable)
        .route("/", get(web::pages::index))
        .route(
            "/fragments/home/tags",
            get(web::pages::home_tag_suggestions),
        )
        .route(
            "/fragments/battles/{battle_id}/claims",
            get(web::pages::battle_claims_fragment),
        )
        .route("/fragments/agents", get(web::pages::agents_fragment))
        .route(
            "/battles",
            get(|| async { axum::response::Redirect::permanent("/") }),
        )
        .route(
            "/battles/",
            get(|| async { axum::response::Redirect::permanent("/") }),
        )
        .route("/battles/{battle_id}", get(web::pages::battle_detail))
        // Standalone claims index (migration 035). Order matters —
        // axum's path-matcher checks specific routes before
        // `{claim_id}` so the index page wins over the slug route.
        .route("/claims/standalone", get(web::pages::standalone_claims))
        .route("/claims/{claim_id}", get(web::pages::claim_detail))
        // Per-entity OG/Twitter card images (1200x630 PNG). Hash-busted
        // URLs cached in GCS so repeat scrapes are a single HEAD/GET
        // against `og/<kind>/<id>-<hash>.png`. See
        // `services/og_image.rs` for the renderer.
        .route("/og/battles/{id}", get(routes::og::battle_card))
        .route("/og/claims/{id}", get(routes::og::claim_card))
        // HTML OG-card templates — the source the `og-render` headless
        // Chromium microservice screenshots into the leaderboard PNG.
        // Internal-ish (the render service navigates here) but harmless
        // if public. See `routes/og_html.rs` + `services/og-render/`.
        .route("/og-html/battles/{id}", get(routes::og_html::battle_html))
        .route("/og-html/claims/{id}", get(routes::og_html::claim_html))
        .route("/about", get(web::pages::about))
        .route("/dev/uikit", get(web::pages::uikit_showcase))
        .route("/changelog", get(web::pages::changelog))
        .route("/arena", get(web::pages::arena))
        // /search HTML page (0.19.2) — companion to /v1/search.
        // Renders the same form server-side so the topnav search box
        // can submit a plain `GET /search?q=…` without JS.
        .route("/search", get(web::pages::search_page))
        .route(
            "/battle_candidates",
            get(web::pages::battle_candidates_page),
        )
        .route(
            "/battle-candidates",
            get(|| async { axum::response::Redirect::permanent("/battle_candidates") }),
        )
        .route("/agents", get(web::pages::leaderboard))
        .route("/agents/{user_id}", get(web::pages::contributor_profile))
        .route("/notifications", get(web::pages::notifications))
        // Email preferences + one-click unsubscribe
        .route(
            "/email/unsub",
            get(web::prefs::unsub).post(web::prefs::unsub_post),
        )
        // Voter digest opt-out (migration 068) — separate HMAC
        // namespace from the user unsubscribe link above.
        .route(
            "/email/unsub/voter",
            get(web::prefs::voter_unsub).post(web::prefs::voter_unsub_post),
        )
        // Instant notification email opt-out (0.43.0, migration 070) —
        // "instant" HMAC namespace.
        .route(
            "/email/unsub/instant",
            get(web::prefs::instant_unsub).post(web::prefs::instant_unsub_post),
        )
        .route(
            "/profile/email-preferences",
            get(web::prefs::prefs_get).post(web::prefs::prefs_post),
        )
        .route("/profile", get(web::profile::profile_page))
        .route(
            "/profile/transactions",
            get(web::profile::transactions_page),
        )
        .route(
            "/profile/agent-notifications",
            get(web::profile::profile_notifications_page),
        )
        .route("/v1/profile/me", get(routes::profile::get_me))
        .route(
            "/v1/profile/agent-notifications",
            get(routes::profile::get_agent_notifications),
        )
        .route(
            "/v1/profile/agent-notifications/unread-count",
            get(routes::profile::get_agent_notifications_unread_count),
        )
        .route(
            "/v1/profile/agent-notifications/read-all",
            post(routes::profile::post_agent_notifications_read_all),
        )
        .route(
            "/v1/profile/agent-notifications/{id}/read",
            post(routes::profile::post_agent_notification_read),
        )
        .route("/v1/profile/logout", post(routes::profile::post_logout))
        .route(
            "/v1/profile/wallet",
            get(routes::profile::get_wallet).put(routes::profile::put_wallet),
        )
        .route(
            "/v1/profile/transactions",
            get(routes::profile::get_transactions),
        )
        // The Crawler Zoo (0.44.0) — live menagerie of the bots that
        // visit us, built on `user_logs`. See `services/zoo.rs`.
        .route("/zoo", get(web::zoo::zoo_home))
        .route(
            "/zoo/",
            get(|| async { axum::response::Redirect::permanent("/zoo") }),
        )
        .route("/zoo/leaderboard", get(web::zoo::zoo_leaderboard))
        .route("/zoo/bots/{slug}", get(web::zoo::zoo_bot))
        // Food bowls (0.45.0): feed via the vending machine; the first
        // bot to fetch the .txt eats. See `services/zoo_feed.rs`.
        .route("/zoo/bots/{slug}/feed", post(web::zoo::zoo_feed))
        .route("/zoo/bowls/{file}", get(web::zoo::zoo_bowl_txt))
        // Zoo OG/Twitter cards (0.44.1). Under /og/ so they are not
        // logged as visits.
        .route("/og/zoo", get(web::zoo::og_home))
        .route("/og/zoo/leaderboard", get(web::zoo::og_leaderboard))
        .route("/og/zoo/bots/{slug}", get(web::zoo::og_bot))
        // The Trap Room: robots.txt-disallowed honeypot. Both the bare
        // prefix and anything below it land on the same page.
        .route("/zoo/trap", get(web::zoo::zoo_trap))
        .route("/zoo/trap/", get(web::zoo::zoo_trap))
        .route("/zoo/trap/{*rest}", get(web::zoo::zoo_trap))
        // SEO: robots.txt + sitemap.xml
        .route("/robots.txt", get(web::seo::robots_txt))
        .route("/sitemap.xml", get(web::seo::sitemap_xml))
        .route("/.well-known/security.txt", get(web::seo::security_txt))
        .route("/security.txt", get(web::seo::security_txt))
        // Agent onboarding: `/skill.md` (the join guide — "Read
        // https://krabarena.com/skill.md and follow the instructions to
        // join KrabArena") + `/llms.txt` (machine-readable site index).
        .route("/skill.md", get(routes::docs::skill_md))
        .route("/llms.txt", get(web::ai_index::llms_txt))
        // Live AI-search indexes (0.43.9): full Battle + Claim dump and
        // a JSON Feed of the newest Claims.
        .route("/llms-full.txt", get(web::ai_index::llms_full_txt))
        .route("/feed.json", get(web::ai_index::feed_json))
        // Browsers auto-request `/favicon.ico` regardless of <link> tags, so
        // serve it from the root path (not just /static/) to avoid noisy 404s.
        .route_service(
            "/favicon.ico",
            ServeFile::new(web::assets::static_dir().join("favicon.ico")),
        )
        // CLI installer + manifest
        .route("/install.sh", get(routes::cli::install_script))
        .route("/cli/{archive}", get(routes::cli::download_cli))
        .route("/v1/cli/manifest", get(routes::cli::serve_manifest))
        .route("/v1/about", get(routes::cli::about_json))
        .route("/v1/changelog", get(routes::cli::changelog_json))
        // Public canonical docs at `/docs/{name}` — reads from the
        // `docs/` folder at runtime, content-negotiates HTML vs raw
        // markdown. Drop a new `docs/foo.md` in the repo and it ships
        // at https://krabarena.com/docs/foo (HTML) +
        // https://krabarena.com/docs/foo.md (markdown) — no
        // per-document route.
        .route("/docs", get(routes::docs::doc_index))
        .route("/docs/{name}", get(routes::docs::doc_page))
        // Test-only token mint — inert unless TEST_AUTH_SECRET is set on
        // the server. Returns 404 in prod by default.
        .route("/v1/test/mint-token", post(routes::test_auth::mint_token))
        // OpenAPI spec + Swagger UI (since 0.8.24).
        // `SwaggerUi::url(path, doc)` registers BOTH the JSON
        // endpoint and the UI HTML — don't add a separate
        // `/v1/openapi.json` route or axum panics with overlapping
        // handlers. Spec is fed by `#[utoipa::path]` annotations on
        // route handlers; UI is hosted at `/docs/api` for browsers.
        .merge(
            utoipa_swagger_ui::SwaggerUi::new("/docs/api")
                .url("/v1/openapi.json", openapi::ApiDoc::openapi()),
        )
        // Static files
        .nest(
            "/static",
            web::assets::static_router(web::assets::static_dir()),
        )
        // `/demo` and `/demo/` both serve the self-contained demo
        // walkthrough (`static/demo/index.html`). Self-contained
        // means base64-inlined images + GA already injected — no
        // sidecar assets, no extra routes needed. Lives under
        // `/static/demo/` for free via the ServeDir above; this
        // pair of redirects gives the cleaner shareable URL.
        .route(
            "/demo",
            get(|| async { axum::response::Redirect::permanent("/static/demo/") }),
        )
        .route(
            "/demo/",
            get(|| async { axum::response::Redirect::permanent("/static/demo/") }),
        )
        .fallback(web::errors::plain_not_found)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            web::errors::html_error_pages,
        ))
        // Absolute request-body ceiling (DoS backstop). Multipart
        // upload routes (claim artifacts up to 100 MB) are exempt from
        // this — axum's `Multipart` streams field-by-field and isn't
        // bounded by `DefaultBodyLimit` — and the battle proxy reads
        // its body with an explicit `to_bytes(.., 10 MB)`. So this only
        // caps buffering extractors (Json/Form/Bytes). Set ABOVE the
        // 100 MB artifact ceiling so it can never clip a legitimate
        // upload even if a future extractor consults it.
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(axum::middleware::from_fn(count_db_queries))
        .layer(TraceLayer::new_for_http())
        // CORS locked to known origins (was `permissive()` ≤ 0.23).
        .layer(build_cors(&state.public_base_url))
        // Cookie auth + device id + CSRF + visit logging. Inner to the
        // rate limiter so over-limit requests skip the session lookup.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            web::auth_context::auth_middleware,
        ))
        // Per-IP Redis rate limiter — runs before route matching so an
        // over-limit request never touches the DB. No-op when the
        // limiter is `None` (REDIS_URL unset in dev).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            web::rate_limit::rate_limit,
        ))
        // `.org → .com` 308. Inside the security-header layer so the
        // redirect response is still hardened.
        .layer(axum::middleware::from_fn(
            web::redirect::redirect_org_to_com,
        ))
        // Outside the limiter/auth/redirect layers so every API
        // response — including a 429 or a 401 short-circuit — tells the
        // CLI which version the server runs (drives `krab` auto-update).
        .layer(axum::middleware::from_fn(stamp_server_version))
        // Outermost — stamps hardening headers on EVERY response,
        // including the limiter's 429 and the redirect's 308.
        .layer(axum::middleware::from_fn(web::security_headers::apply))
        .with_state(state)
}

/// Hard ceiling on buffered request bodies. See the `DefaultBodyLimit`
/// note in `build_router` — multipart uploads + the proxy are exempt,
/// so this is purely a DoS backstop for JSON/form/byte extractors and
/// is deliberately set above the 100 MB artifact ceiling.
const MAX_REQUEST_BODY_BYTES: usize = 128 * 1024 * 1024;

/// Build the CORS layer from the configured public base URL. Replaces
/// the old `CorsLayer::permissive()` (any origin / method / header).
/// Allows the canonical apex + `www` + the legacy `.org` host + local
/// dev origins, only the HTTP methods the API actually uses, and only
/// the auth / content headers clients send. No credentials are allowed
/// (auth rides in the `authorization` / `x-krabarena-token` headers,
/// not cookies), so wildcard-credential pitfalls don't apply.
fn build_cors(public_base_url: &str) -> CorsLayer {
    let mut origins: Vec<HeaderValue> = Vec::new();
    let mut push = |s: &str| {
        if let Ok(v) = s.parse::<HeaderValue>() {
            origins.push(v);
        }
    };
    // The configured canonical origin (prod: https://krabarena.com).
    push(public_base_url);
    // Static well-known origins. Duplicates of `public_base_url` are
    // harmless — the browser only needs one match.
    for o in [
        "https://krabarena.com",
        "https://www.krabarena.com",
        "https://krabarena.org",
        "https://www.krabarena.org",
        "http://localhost:3001",
        "http://127.0.0.1:3001",
    ] {
        push(o);
    }

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("x-krabarena-token"),
            HeaderName::from_static("x-test-auth"),
        ])
}

/// Tower middleware that scopes a fresh `DB_QUERY_COUNT` for the
/// duration of each HTTP request and attaches the final value as
/// `X-DB-Queries` on the response. Every sqlx query routed through
/// `state.pool` increments the counter via the `before_acquire`
/// hook installed in `db::create_pool`. Background tasks spawned
/// outside this scope don't see the task-local and don't pollute
/// the count.
///
/// Cost: one Arc clone + one atomic load per request. The counter
/// itself is `Relaxed` because total ordering doesn't matter — we
/// just want the count.
async fn count_db_queries(req: Request, next: Next) -> Response {
    let counter: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let read = counter.clone();
    let mut response = DB_QUERY_COUNT.scope(counter, next.run(req)).await;
    let n = read.load(Ordering::Relaxed);
    if let Ok(hv) = HeaderValue::from_str(&n.to_string()) {
        response.headers_mut().insert("x-db-queries", hv);
    }
    response
}

/// Stamp `X-Krab-Server-Version: <CARGO_PKG_VERSION>` on every API
/// response (`/v1/*` and `/health`, any status) so the `krab` CLI can
/// notice a newer server from whatever command it just ran and
/// auto-update on the next invocation. The value is baked at build time
/// (`env!`) so it matches `/health`'s `version` field exactly. HTML
/// pages and static files are skipped — only API consumers care.
async fn stamp_server_version(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let is_api = path.starts_with("/v1/") || path == "/health";
    let mut response = next.run(req).await;
    if is_api {
        response.headers_mut().insert(
            "x-krab-server-version",
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        );
    }
    response
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .json()
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://localhost:5432/krabarena".into());
    let jwt_secret = resolve_jwt_secret(
        std::env::var("JWT_SECRET").ok(),
        std::env::var("KRAB_DEV_INSECURE_JWT").ok(),
    )
    .unwrap_or_else(|msg| panic!("{msg}"));
    let blob_store_url =
        std::env::var("BLOB_STORE_URL").unwrap_or_else(|_| "http://localhost:3001/blob".into());
    let public_base_url_explicit = std::env::var("PUBLIC_BASE_URL").ok();
    let public_base_url = public_base_url_explicit
        .clone()
        .unwrap_or_else(|| "https://krabarena.com".into())
        .trim_end_matches('/')
        .to_string();

    // Second guard: even with the explicit KRAB_DEV_INSECURE_JWT=1 opt-in
    // (or JWT_SECRET literally set to `dev-secret`), refuse to boot when
    // PUBLIC_BASE_URL is *explicitly* a production host — that combination
    // would sign forgeable tokens anyone could mint.
    if jwt_secret == INSECURE_DEV_JWT_SECRET
        && let Some(explicit) = public_base_url_explicit.as_deref()
        && is_production_host(explicit)
    {
        panic!(
            "JWT_SECRET is the insecure default `dev-secret` but PUBLIC_BASE_URL ({explicit}) \
             is a production host. Set a strong JWT_SECRET before booting against prod."
        );
    }

    // Private artifacts live in their own non-public bucket. In a
    // production-like environment (Cloud Run sets K_SERVICE; an
    // explicit PUBLIC_BASE_URL or a real gs:// public store also
    // count) a missing PRIVATE_BLOB_STORE_URL disables private uploads
    // instead of silently writing them to the public bucket.
    let prod_like = krabarena_api::services::artifact_service::is_prod_like(
        std::env::var("K_SERVICE").is_ok(),
        public_base_url_explicit.as_deref(),
        &blob_store_url,
    );
    let private_blob_store_url =
        krabarena_api::services::artifact_service::resolve_private_blob_store(
            std::env::var("PRIVATE_BLOB_STORE_URL").ok().as_deref(),
            &blob_store_url,
            prod_like,
        );

    let discord_monitor =
        krabarena_api::services::discord_monitor::DiscordMonitorConfig::from_env();
    tracing::info!(
        enabled = discord_monitor.enabled,
        has_bot_token = discord_monitor.has_bot_token(),
        has_channel_id = discord_monitor.has_channel_id(),
        "discord.monitor.configured"
    );
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| format!("0.0.0.0:{}", port));

    let pool = match db::create_pool(&database_url).await {
        Ok(pool) => {
            tracing::info!("Connected to database");
            pool
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to connect to database, starting without DB");
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_secs(1))
                .connect_lazy(&database_url)
                .unwrap()
        }
    };

    let proxy_pool = match db::create_proxy_pool(&database_url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to open the proxy pool, sharing the main pool");
            pool.clone()
        }
    };

    // One-time backfill of `_sqlx_migrations` for legacy databases
    // that were provisioned manually. Idempotent; safe to leave the
    // env var set across deploys, but the recommended runbook is
    // bootstrap once, then unset.
    if std::env::var("BOOTSTRAP_SQLX_MIGRATIONS").ok().as_deref() == Some("true") {
        match db::bootstrap_sqlx_migrations(&pool).await {
            Ok(n) => tracing::info!(rows_inserted = n, "sqlx.bootstrap.complete"),
            Err(e) => {
                tracing::error!(error = %e, "sqlx.bootstrap.failed");
                std::process::exit(1);
            }
        }
    }

    // Optional auto-apply of pending migrations at startup. Off by
    // default. Requires `_sqlx_migrations` to be populated first
    // (see `BOOTSTRAP_SQLX_MIGRATIONS`).
    if std::env::var("SQLX_AUTO_MIGRATE").ok().as_deref() == Some("true") {
        // Pass the URL (not the pooled `pool`): migrations run over a
        // dedicated connection to Neon's direct endpoint so sqlx's
        // session-level advisory lock isn't orphaned by the pooler.
        match db::run_migrations(&database_url).await {
            Ok(()) => tracing::info!("sqlx.migrate.complete"),
            Err(e) => {
                tracing::error!(error = %e, "sqlx.migrate.failed");
                std::process::exit(1);
            }
        }
    }

    // Source-of-truth invariant: every Field in
    // `krabarena_shared::field_limits` must have a matching DB CHECK
    // constraint with identical min/max. Panics on drift so the
    // server refuses to serve traffic against a schema that doesn't
    // enforce the limits the API advertises. `SKIP_SCHEMA_CHECK=true`
    // for bootstrap scenarios where migration 030 hasn't been applied
    // yet; default is strict.
    db::schema_check::verify_or_die(&pool).await;

    // Warm the CMS in-process cache. Fires every `cms_*` key fetch
    // concurrently so the cost is one RTT; from here, every page
    // render's header pulls from memory and pays zero pool acquires
    // for the lifetime of this process (or until an admin
    // `config-set` or `cms-reload` invalidates a key).
    krabarena_api::services::cms_service::preload(&pool).await;
    // Hand the same pool to the gemini audit module so every
    // outbound Gemini call lands a per-attempt row in `gemini_calls`.
    // Fire-and-forget inserts — never block or fail a review.
    krabarena_api::services::gemini_audit::init_pool(pool.clone());

    let email_from = std::env::var("EMAIL_FROM")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let email_service = match (
        std::env::var("MAILGUN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        std::env::var("MAILGUN_DOMAIN")
            .ok()
            .filter(|s| !s.trim().is_empty()),
    ) {
        (Some(api_key), Some(domain)) => {
            tracing::info!(domain = %domain, "Mailgun email service configured");
            Some(services::email_service::EmailService::new(
                api_key,
                domain,
                public_base_url.clone(),
                email_from,
            ))
        }
        _ => {
            tracing::warn!(
                "MAILGUN_API_KEY/MAILGUN_DOMAIN not set — email verification codes will be logged only"
            );
            None
        }
    };

    let gemini_api_key = std::env::var("GEMINI_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    if gemini_api_key.is_none() {
        tracing::warn!(
            "GEMINI_API_KEY not set — KrabReviewer will issue synthetic pass reviews (dev mode only)"
        );
    }
    let gemini = services::gemini_service::GeminiClient::new(gemini_api_key);

    // GA4 — only emit the gtag.js snippet when the env is set. Dev
    // and test envs leave `GA_MEASUREMENT_ID` unset so localhost /
    // staging traffic doesn't show up in the prod GA dashboard. The
    // value is read on-demand by `web::meta::PageMeta::new` via a
    // OnceLock cache; this branch is just a startup log.
    if let Ok(id) = std::env::var("GA_MEASUREMENT_ID") {
        let id = id.trim();
        if !id.is_empty() {
            tracing::info!(ga_id = %id, "GA4 enabled");
        }
    }

    // Shared reqwest client for the battle proxy. Build ONCE so the
    // TCP + TLS pool is reused across all proxy requests — without
    // this every proxy call paid a fresh handshake (~500-700ms).
    // Settings tuned for upstream search APIs (Cloudflare-fronted,
    // many sub-second calls in bursts):
    //   - pool_idle_timeout(60s) keeps warm connections through
    //     normal traffic gaps without holding stale ones forever.
    //   - tcp_keepalive(30s) catches dead conns sooner than the
    //     OS default (~2h).
    //   - timeout(120s) matches the per-request timeout the old
    //     per-request client used in proxy.rs.
    let proxy_http = reqwest::Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("failed to build shared proxy reqwest client");

    // Per-IP / per-user rate limiter. Enabled only when `REDIS_URL` is
    // set; the pool connects lazily (so a down Redis at boot doesn't
    // block startup — the middleware fails open at request time). Limits
    // are env-tunable; defaults are 120/min global, 10/min on the strict
    // auth surface.
    let rate_limiter = build_rate_limiter();

    // reCAPTCHA Enterprise (score-based) sign-in bot gate. Disabled unless the
    // project id, site key, and API key are all set — see `RecaptchaConfig`.
    // The API key rides in as a Cloud Run env var from a GitHub Actions secret
    // (deploy.yml); the site key is public and rendered into the page.
    let recaptcha = RecaptchaConfig {
        project_id: std::env::var("RECAPTCHA_ENTERPRISE_PROJECT_ID").unwrap_or_default(),
        site_key: std::env::var("RECAPTCHA_SITE_KEY").unwrap_or_default(),
        api_key: std::env::var("RECAPTCHA_ENTERPRISE_API_KEY").unwrap_or_default(),
        score_threshold: std::env::var("RECAPTCHA_SCORE_THRESHOLD")
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(0.5),
    };
    if recaptcha.is_enabled() {
        tracing::info!(
            project_id = %recaptcha.project_id,
            score_threshold = recaptcha.score_threshold,
            "reCAPTCHA Enterprise enabled for voter email sign-in"
        );
    } else {
        tracing::warn!(
            "RECAPTCHA_ENTERPRISE_* not fully set — sign-in captcha disabled (dev / key-less mode)"
        );
    }

    // Instant notification emails (since 0.43.0). Needs BOTH Mailgun
    // configured AND `NOTIFICATION_EMAILS_ENABLED=true`; local dev and
    // the `krabarena-test` service have neither, so they never send.
    // Optional `NOTIFICATION_EMAIL_ALLOWLIST` restricts recipients.
    {
        let config = services::notification_email::NotificationEmailConfig::from_env();
        match (&email_service, config.enabled) {
            (Some(svc), true) => {
                let allowlisted = config.allowlist.len();
                let installed = services::notification_email::install(
                    services::notification_email::NotifierRuntime {
                        mailer: Arc::new(svc.clone()),
                        config,
                        public_base_url: svc.public_base_url().to_string(),
                        link_secret: jwt_secret.clone(),
                    },
                );
                tracing::info!(
                    installed,
                    allowlisted,
                    "instant notification emails enabled"
                );
            }
            (None, true) => tracing::warn!(
                "NOTIFICATION_EMAILS_ENABLED=true but Mailgun is not configured — instant notification emails disabled"
            ),
            (_, false) => tracing::info!(
                "instant notification emails disabled (NOTIFICATION_EMAILS_ENABLED != true)"
            ),
        }
    }

    let state = AppState {
        pool: pool.clone(),
        proxy_pool,
        jwt_secret,
        blob_store_url,
        private_blob_store_url,
        public_base_url,
        discord_monitor,
        email_service,
        gemini,
        proxy_http,
        rate_limiter,
        leaderboards: Arc::new(LeaderboardCaches::new()),
        recaptcha,
    };

    // Kick off the search-index backfill as a background task — cap
    // is 100 rows per kind per startup (see
    // `services::search_index::startup_backfill`). Lazy + bounded
    // so a cold start against a freshly migrated DB still returns
    // from `bind` quickly; subsequent boots chip away at the backlog
    // until everything is indexed.
    tokio::spawn(krabarena_api::services::search_index::startup_backfill(
        pool,
    ));

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!(addr = %listen_addr, "KrabArena API listening");
    // `into_make_service_with_connect_info` exposes the socket peer addr
    // to the rate limiter's `ConnectInfo<SocketAddr>` extractor — the
    // fallback IP source when no `X-Forwarded-For` is present (local /
    // docker dev). In prod the LB always sets XFF, so this only matters
    // off-cloud, but it's harmless everywhere.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use sqlx::postgres::PgPoolOptions;
    use tower::util::ServiceExt;

    fn test_state() -> AppState {
        AppState {
            pool: PgPoolOptions::new()
                .connect_lazy("postgresql://krab:krab@localhost:5432/krabarena")
                .expect("test pool should be created lazily"),
            proxy_pool: PgPoolOptions::new()
                .connect_lazy("postgresql://krab:krab@localhost:5432/krabarena")
                .expect("test pool should be created lazily"),
            jwt_secret: "test-secret".into(),
            blob_store_url: "http://127.0.0.1:3000/static/artifacts".into(),
            private_blob_store_url: None,
            public_base_url: "http://127.0.0.1:3000".into(),
            discord_monitor: services::discord_monitor::DiscordMonitorConfig::from_lookup(|_| None),
            email_service: None,
            gemini: services::gemini_service::GeminiClient::new(None),
            proxy_http: reqwest::Client::new(),
            rate_limiter: None,
            leaderboards: Arc::new(LeaderboardCaches::new()),
            recaptcha: RecaptchaConfig::default(),
        }
    }

    /// Regression guard: an unset JWT_SECRET used to fall back to the
    /// public `dev-secret` key, letting anyone forge admin JWTs offline.
    #[test]
    fn jwt_secret_fails_closed_when_unset() {
        assert!(resolve_jwt_secret(None, None).is_err());
        assert!(resolve_jwt_secret(Some("   ".into()), None).is_err());
        assert!(resolve_jwt_secret(None, Some("0".into())).is_err());
        assert!(resolve_jwt_secret(None, Some("true".into())).is_err());
        assert_eq!(
            resolve_jwt_secret(None, Some("1".into())).unwrap(),
            INSECURE_DEV_JWT_SECRET
        );
        assert_eq!(
            resolve_jwt_secret(Some("s3cret".into()), None).unwrap(),
            "s3cret"
        );
    }

    #[tokio::test]
    async fn battle_candidates_hyphen_path_redirects_to_canonical_path() {
        let response = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/battle-candidates")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/battle_candidates")
        );
    }

    /// The CLI's auto-update learns about a newer server from this
    /// header on ANY API response — including error responses — so it
    /// must be present on `/v1/*` regardless of status, and absent on
    /// HTML pages.
    #[tokio::test]
    async fn api_responses_carry_server_version_header() {
        let get = |uri: &'static str| {
            build_router(test_state()).oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request should build"),
            )
        };
        let api = get("/v1/no-such-endpoint").await.expect("router responds");
        assert_eq!(
            api.headers()
                .get("x-krab-server-version")
                .and_then(|v| v.to_str().ok()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let page = get("/no-such-page").await.expect("router responds");
        assert!(page.headers().get("x-krab-server-version").is_none());
    }
}
