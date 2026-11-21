use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use krabarena_shared::domain::BattleStatus;
use krabarena_shared::field_limits::Field;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::models::battle::{CreateBattleRequest, UpdateBattleRequest};
use crate::routes::auth::require_admin;
use crate::services::config_service;

/// All roles accepted by the database `users_role_check` constraint
/// (migration 001 + later). Admin endpoints validate against this
/// list before issuing the UPDATE so a typo doesn't 22P02 from
/// Postgres.
const VALID_ROLES: &[&str] = &["admin", "editor", "moderator", "user"];
const VALID_ORG_ROLES: &[&str] = &["admin", "member"];
const VALID_USER_KINDS: &[&str] = &["human", "bot"];
const ADMIN_LIST_DEFAULT_LIMIT: i64 = 50;
const ADMIN_LIST_MAX_LIMIT: i64 = 200;

#[derive(Debug, Clone, Copy)]
struct AdminListPagination {
    limit: i64,
    offset: i64,
    count_only: bool,
}

#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
pub struct AdminListPaginationResponse {
    pub limit: i64,
    pub offset: i64,
    /// Total rows matching the filter, or `-1` when not recomputed. The
    /// `COUNT(*)` over the filtered set is only run for the first page
    /// (`offset == 0`) or a `count_only` request — paging deeper skips
    /// it (`-1`) so each page doesn't pay a full-set count. Re-request
    /// page 1 or `count_only=true` for the live total.
    pub count: i64,
    pub count_only: bool,
}

/// Sentinel for `AdminListPaginationResponse::count` on pages where the
/// `COUNT(*)` was intentionally skipped. See the field docs.
pub const ADMIN_COUNT_SKIPPED: i64 = -1;

impl AdminListPagination {
    fn response(self, count: i64) -> AdminListPaginationResponse {
        AdminListPaginationResponse {
            limit: self.limit,
            offset: self.offset,
            count,
            count_only: self.count_only,
        }
    }

    /// Whether to run the expensive `COUNT(*)`. Only the first page or a
    /// `count_only` request needs the total; deeper pages reuse the
    /// figure the caller already got from page 1.
    fn should_count(self) -> bool {
        self.count_only || self.offset == 0
    }
}

fn admin_list_pagination(
    limit: Option<i64>,
    offset: Option<i64>,
    count_only: Option<bool>,
) -> Result<AdminListPagination, ApiError> {
    let limit = limit.unwrap_or(ADMIN_LIST_DEFAULT_LIMIT);
    if !(1..=ADMIN_LIST_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::ValidationError(format!(
            "limit must be between 1 and {ADMIN_LIST_MAX_LIMIT}"
        )));
    }
    let offset = offset.unwrap_or(0);
    if offset < 0 {
        return Err(ApiError::ValidationError(
            "offset must be greater than or equal to 0".into(),
        ));
    }
    Ok(AdminListPagination {
        limit,
        offset,
        count_only: count_only.unwrap_or(false),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdminSortDirection {
    Asc,
    Desc,
}

impl AdminSortDirection {
    const fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct AdminSortField {
    public: &'static str,
    sql: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct AdminSortTerm {
    public: &'static str,
    sql: &'static str,
    direction: AdminSortDirection,
}

impl AdminSortTerm {
    const fn asc(public: &'static str, sql: &'static str) -> Self {
        Self {
            public,
            sql,
            direction: AdminSortDirection::Asc,
        }
    }

    const fn desc(public: &'static str, sql: &'static str) -> Self {
        Self {
            public,
            sql,
            direction: AdminSortDirection::Desc,
        }
    }
}

fn admin_order_by(
    order_by: Option<&str>,
    allowed: &[AdminSortField],
    default: &[AdminSortTerm],
    tie_breaker: AdminSortTerm,
) -> Result<String, ApiError> {
    let mut terms = match order_by.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) => parse_admin_order_by(raw, allowed)?,
        None => default.to_vec(),
    };
    if !terms.iter().any(|term| term.public == tie_breaker.public) {
        terms.push(tie_breaker);
    }
    Ok(terms
        .into_iter()
        .map(|term| format!("{} {}", term.sql, term.direction.sql()))
        .collect::<Vec<_>>()
        .join(", "))
}

fn parse_admin_order_by(
    raw: &str,
    allowed: &[AdminSortField],
) -> Result<Vec<AdminSortTerm>, ApiError> {
    let mut terms = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err(ApiError::ValidationError(
                "order_by must not contain empty sort fields".into(),
            ));
        }
        let (direction, field) = match token.as_bytes()[0] {
            b'+' => (AdminSortDirection::Asc, token[1..].trim()),
            b'-' => (AdminSortDirection::Desc, token[1..].trim()),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => (AdminSortDirection::Asc, token),
            other => {
                return Err(ApiError::ValidationError(format!(
                    "order_by token '{token}' has invalid prefix '{}'",
                    other as char
                )));
            }
        };
        if field.is_empty() {
            return Err(ApiError::ValidationError(
                "order_by sort field must not be empty".into(),
            ));
        }
        let Some(sort_field) = allowed.iter().find(|candidate| candidate.public == field) else {
            let valid = allowed
                .iter()
                .map(|candidate| candidate.public)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ApiError::ValidationError(format!(
                "order_by field '{field}' is not supported; valid fields: {valid}"
            )));
        };
        terms.push(AdminSortTerm {
            public: sort_field.public,
            sql: sort_field.sql,
            direction,
        });
    }
    Ok(terms)
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AdminUserRow {
    id: Uuid,
    entity_id: Uuid,
    display_name: String,
    email: Option<String>,
    entity_name: String,
    entity_domain: String,
    role: String,
    org_role: String,
    kind: String,
    suspended_until: Option<DateTime<Utc>>,
    suspension_reason: Option<String>,
    rate_limit_until: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    claim_count: i64,
    battle_count: i64,
    last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct AdminClaimAuthor {
    pub user_id: Uuid,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub entity_domain: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminClaimBattle {
    pub battle_id: Uuid,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct AdminClaimReviewSummary {
    pub review_id: Uuid,
    pub verdict: Option<String>,
    pub summary: Option<String>,
    pub moderation_flags: Value,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct AdminClaimCounts {
    pub verify_count: i32,
    pub refute_count: i32,
    pub upvote_count: i32,
    pub downvote_count: i32,
    pub comment_count: i32,
    pub verification_rows: i64,
}

#[derive(Debug, Serialize)]
pub struct AdminClaimListItem {
    pub id: Uuid,
    pub slug: Option<String>,
    pub title: String,
    pub preview: String,
    pub author: AdminClaimAuthor,
    pub battle: Option<AdminClaimBattle>,
    pub topic: Option<String>,
    pub topic_tags: Vec<String>,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub counts: AdminClaimCounts,
    pub latest_review: Option<AdminClaimReviewSummary>,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct AdminClaimListResponse {
    pub items: Vec<AdminClaimListItem>,
    pub pagination: AdminListPaginationResponse,
}

#[derive(Debug, Serialize)]
pub struct AdminClaimInspectResponse {
    #[serde(flatten)]
    pub item: AdminClaimListItem,
    pub claim: String,
    pub sections: Value,
    pub artifact: Value,
    pub claim_structure: Option<Value>,
    pub warnings: Vec<String>,
    pub risk_signals: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminBattleAuthor {
    pub user_id: Uuid,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub entity_domain: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminBattleCounts {
    pub claim_count: i64,
    pub live_claim_count: i64,
    pub deleted_claim_count: i64,
    pub total_confirmed: i64,
    pub total_refuted: i64,
    pub verification_rows: i64,
}

#[derive(Debug, Serialize)]
pub struct AdminBattleSummaryState {
    pub has_executive_summary: bool,
    pub has_short_summary: bool,
    pub executive_summary: Option<String>,
    pub short_summary: Option<String>,
    pub og_summary: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct AdminBattleListItem {
    pub id: Uuid,
    pub slug: Option<String>,
    pub url: String,
    pub battle_id: Uuid,
    pub battle_slug: Option<String>,
    pub battle_title: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub domain_tags: Vec<String>,
    pub author: AdminBattleAuthor,
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub counts: AdminBattleCounts,
    pub has_summary: bool,
    pub has_solutions: bool,
    pub missing_solutions: bool,
    pub stale_summary: bool,
}

#[derive(Debug, Serialize)]
pub struct AdminBattleListResponse {
    pub items: Vec<AdminBattleListItem>,
    pub pagination: AdminListPaginationResponse,
}

#[derive(Debug, Serialize)]
pub struct AdminBattleClaimItem {
    pub id: Uuid,
    pub slug: Option<String>,
    pub title: String,
    pub url: String,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub verify_count: i32,
    pub refute_count: i32,
    pub claim_structure: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct AdminBattleInspectResponse {
    #[serde(flatten)]
    pub item: AdminBattleListItem,
    pub proxy_spec: Option<Value>,
    pub proxy_spec_present: bool,
    pub cover_image_url: Option<String>,
    pub cover_image_alt: Option<String>,
    pub solutions: Value,
    pub summaries: AdminBattleSummaryState,
    pub claims: Vec<AdminBattleClaimItem>,
    pub leaderboard: Value,
    pub warnings: Vec<String>,
    pub risk_signals: Vec<String>,
    pub available_actions: Vec<&'static str>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AdminBattleRow {
    id: Uuid,
    author_id: Uuid,
    author_name: Option<String>,
    author_email: Option<String>,
    entity_domain: Option<String>,
    parent_id: Option<Uuid>,
    slug: Option<String>,
    title: String,
    description: String,
    domain_tags: Vec<String>,
    status: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    executive_summary: Option<String>,
    short_summary: Option<String>,
    og_summary: Option<String>,
    proxy_spec: Option<Value>,
    cover_image_url: Option<String>,
    cover_image_alt: Option<String>,
    solutions: Value,
    deleted_at: Option<DateTime<Utc>>,
    claim_count: i64,
    live_claim_count: i64,
    deleted_claim_count: i64,
    total_confirmed: i64,
    total_refuted: i64,
    verification_rows: i64,
}

#[derive(Debug, Deserialize)]
pub struct ListBattlesAdminQuery {
    pub status: Option<String>,
    pub domain_tag: Option<String>,
    pub has_claims: Option<bool>,
    pub missing_solutions: Option<bool>,
    pub stale_summary: Option<bool>,
    pub updated_after: Option<DateTime<Utc>>,
    pub updated_before: Option<DateTime<Utc>>,
    pub include_deleted: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub count_only: Option<bool>,
    pub order_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBattleAdminRequest {
    pub title: String,
    pub description: String,
    pub domain_tags: Vec<String>,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub proxy_spec: Option<Value>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub produced_by: Option<Vec<String>>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateBattleAdminRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub domain_tags: Option<Vec<String>>,
    pub status: Option<String>,
    pub proxy_spec: Option<Value>,
    #[serde(default)]
    pub produced_by: Option<Vec<String>>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AdminClaimRow {
    id: Uuid,
    slug: Option<String>,
    claim: String,
    author_id: Uuid,
    author_name: Option<String>,
    author_email: Option<String>,
    entity_domain: Option<String>,
    battle_id: Option<Uuid>,
    battle_title: Option<String>,
    battle_slug: Option<String>,
    topic: Option<String>,
    topic_tags: Vec<String>,
    visibility: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
    verify_count: i32,
    refute_count: i32,
    upvote_count: i32,
    downvote_count: i32,
    comment_count: i32,
    verification_rows: i64,
    review_id: Option<Uuid>,
    review_verdict: Option<String>,
    review_summary: Option<String>,
    review_flags: Option<Value>,
    review_created_at: Option<DateTime<Utc>>,
    artifact_hash: String,
    artifact_url: String,
    artifact_size: i64,
    artifact_filename: Option<String>,
    claim_structure: Option<Value>,
    warnings_ignored: bool,
}

#[derive(Debug, Deserialize)]
pub struct ListClaimsAdminQuery {
    pub battle: Option<String>,
    pub author: Option<String>,
    pub visibility: Option<String>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub review_verdict: Option<String>,
    pub has_warnings: Option<bool>,
    pub standalone: Option<bool>,
    pub without_verifications: Option<bool>,
    pub include_deleted: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub count_only: Option<bool>,
    pub order_by: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateClaimAdminRequest {
    pub title: Option<String>,
    pub claim: Option<String>,
    pub claim_structure: Option<Value>,
    pub topic: Option<String>,
    pub topic_tags: Option<Vec<String>>,
    /// Producer label slugs (admin-only). A non-empty array (slugs
    /// validated against [`crate::models::producer`]) tags the claim;
    /// an empty array clears the label. Absent = leave unchanged.
    #[serde(default)]
    pub produced_by: Option<Vec<String>>,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct AdminReasonRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

include!("config.rs");
include!("users.rs");
include!("voters.rs");
include!("battles.rs");
include!("claims.rs");
include!("vendor_logos.rs");
include!("usage.rs");

#[cfg(test)]
include!("tests.rs");
