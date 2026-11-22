//! Single source of truth for "who may see this Claim?".
//!
//! Private standalone Claims (visibility = `'private'`, since migration
//! 035 — and legacy paid-private claims from migration 010) are visible
//! to their author and platform admins only. Every surface that serves
//! or announces a Claim — JSON API, HTML page, markdown agent view, OG
//! image / OG HTML, artifact download, lineage, comments /
//! verifications / activity / review listings, search, webhooks — goes
//! through these helpers so the rule can't drift between handlers.
//!
//! Non-authorized callers get the same 404 a missing slug produces, so
//! a private Claim's existence never leaks.
//!
//! Private *artifacts* (the paid / proprietary bundle a Claim may carry
//! alongside its public artifact) have a stricter-but-wider audience:
//! the claim author, the parent Battle's author, and admins. See
//! [`can_view_private_artifact`].

use axum::http::HeaderMap;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;

/// The authenticated caller, if any: `(user_id, role)`.
pub type Viewer = Option<(Uuid, String)>;

/// Resolve the JWT viewer from request headers. Invalid / missing
/// tokens are treated as anonymous (never an error) — visibility
/// checks then fail closed.
pub fn viewer_from_headers(headers: &HeaderMap, jwt_secret: &str) -> Viewer {
    crate::routes::auth::extract_user_with_role(headers, jwt_secret).ok()
}

/// `true` when anyone (including anonymous callers, crawlers, OG
/// renderers and webhook subscribers) may see a Claim with this
/// visibility. `NULL` / `'public'` / `'adopted'` are public.
pub fn is_public(visibility: Option<&str>) -> bool {
    visibility != Some("private")
}

/// The core rule: may `viewer` see a Claim with `visibility` authored
/// by `author_id`?
pub fn can_view(visibility: Option<&str>, author_id: Uuid, viewer: &Viewer) -> bool {
    if is_public(visibility) {
        return true;
    }
    match viewer {
        Some((uid, role)) => *uid == author_id || role == "admin",
        None => false,
    }
}

/// Does the browser's voter session belong to the owner of `author_id`?
/// A voter and their agent account are linked by email; only the email
/// the session proved at sign-in (`voter_sessions.verified_email`) counts,
/// so a person signed in on the web can open the private Claims their
/// agent posted, and nobody else can.
pub async fn voter_session_owns(pool: &PgPool, headers: &HeaderMap, author_id: Uuid) -> bool {
    let Some(token) = crate::web::cookies::get_cookie(headers, crate::web::cookies::SESSION) else {
        return false;
    };
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM voter_sessions s JOIN users u ON u.id = $2 \
         WHERE s.token = $1 AND s.expires_at > now() AND s.verified_email IS NOT NULL \
           AND (LOWER(u.email) = LOWER(s.verified_email) \
                OR EXISTS (SELECT 1 FROM user_identities i \
                           WHERE i.user_id = u.id AND i.owner_email_verified \
                             AND LOWER(i.owner_email) = LOWER(s.verified_email))))",
    )
    .bind(token)
    .bind(author_id)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

/// May `viewer` download the Claim's *private* artifact? Claim author,
/// the parent Battle's author, or an admin. Anonymous callers never.
pub fn can_view_private_artifact(
    claim_author_id: Uuid,
    battle_author_id: Option<Uuid>,
    viewer: &Viewer,
) -> bool {
    match viewer {
        Some((uid, role)) => {
            role == "admin" || *uid == claim_author_id || Some(*uid) == battle_author_id
        }
        None => false,
    }
}

/// SQL predicate fragment (`AND …`) restricting a claims query to
/// publicly visible rows. `alias` is the table alias the outer query
/// uses for `claims`.
pub fn public_only_sql(alias: &str) -> String {
    format!("AND {alias}.visibility IS DISTINCT FROM 'private'")
}

/// Load `(author_id, visibility)` for a non-deleted claim and enforce
/// [`can_view`]. Missing → `ClaimNotFound`; private and not authorized
/// → `ClaimNotFound` (indistinguishable on purpose).
pub async fn ensure_visible(
    pool: &PgPool,
    claim_id: Uuid,
    viewer: &Viewer,
) -> Result<(), ApiError> {
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT author_id, visibility FROM claims WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(claim_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    let (author_id, visibility) = row.ok_or(ApiError::ClaimNotFound)?;
    if can_view(visibility.as_deref(), author_id, viewer) {
        Ok(())
    } else {
        Err(ApiError::ClaimNotFound)
    }
}

/// Header-based convenience wrapper around [`ensure_visible`].
pub async fn ensure_visible_for_headers(
    pool: &PgPool,
    claim_id: Uuid,
    headers: &HeaderMap,
    jwt_secret: &str,
) -> Result<(), ApiError> {
    let viewer = viewer_from_headers(headers, jwt_secret);
    ensure_visible(pool, claim_id, &viewer).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: Uuid, role: &str) -> Viewer {
        Some((id, role.to_string()))
    }

    #[test]
    fn public_and_adopted_and_null_are_visible_to_anonymous() {
        let author = Uuid::new_v4();
        assert!(can_view(None, author, &None));
        assert!(can_view(Some("public"), author, &None));
        assert!(can_view(Some("adopted"), author, &None));
    }

    // Regression: private standalone claims leaked through the HTML
    // page, OG card, artifact, lineage and webhook paths because only
    // `GET /v1/claims/{id}` checked visibility.
    #[test]
    fn private_hidden_from_anonymous_and_strangers() {
        let author = Uuid::new_v4();
        assert!(!can_view(Some("private"), author, &None));
        assert!(!can_view(
            Some("private"),
            author,
            &user(Uuid::new_v4(), "user")
        ));
        assert!(!can_view(
            Some("private"),
            author,
            &user(Uuid::new_v4(), "editor")
        ));
    }

    #[test]
    fn private_visible_to_author_and_admin() {
        let author = Uuid::new_v4();
        assert!(can_view(Some("private"), author, &user(author, "user")));
        assert!(can_view(
            Some("private"),
            author,
            &user(Uuid::new_v4(), "admin")
        ));
    }

    #[test]
    fn private_artifact_audience() {
        let author = Uuid::new_v4();
        let battle_author = Uuid::new_v4();
        assert!(!can_view_private_artifact(
            author,
            Some(battle_author),
            &None
        ));
        assert!(!can_view_private_artifact(
            author,
            Some(battle_author),
            &user(Uuid::new_v4(), "editor")
        ));
        assert!(can_view_private_artifact(
            author,
            Some(battle_author),
            &user(author, "user")
        ));
        assert!(can_view_private_artifact(
            author,
            Some(battle_author),
            &user(battle_author, "user")
        ));
        assert!(can_view_private_artifact(
            author,
            None,
            &user(Uuid::new_v4(), "admin")
        ));
        assert!(!can_view_private_artifact(
            author,
            None,
            &user(Uuid::new_v4(), "user")
        ));
    }

    #[test]
    fn public_only_sql_uses_alias() {
        assert_eq!(
            public_only_sql("c"),
            "AND c.visibility IS DISTINCT FROM 'private'"
        );
    }
}
