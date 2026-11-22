use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::models::proxy::{ProxyInfo, ProxySpec, ServicePublicInfo};
use crate::services::proxy_billing::{self, ChargeDecision, ChargeSource};

/// Load and parse the proxy spec for a battle.
pub async fn get_proxy_spec(pool: &PgPool, battle_id: Uuid) -> Result<ProxySpec, ApiError> {
    let row = sqlx::query_as::<_, (Option<serde_json::Value>,)>(
        "SELECT proxy_spec FROM battles WHERE id = $1",
    )
    .bind(battle_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .ok_or(ApiError::BattleNotFound)?;

    let spec_json = row
        .0
        .ok_or_else(|| ApiError::ValidationError("this battle has no proxy configured".into()))?;

    serde_json::from_value(spec_json)
        .map_err(|e| ApiError::ValidationError(format!("invalid proxy spec: {e}")))
}

/// Save proxy spec on a battle.
pub async fn set_proxy_spec(
    pool: &PgPool,
    battle_id: Uuid,
    spec: &ProxySpec,
) -> Result<(), ApiError> {
    let json = serde_json::to_value(spec).map_err(|e| ApiError::Internal(e.into()))?;
    sqlx::query("UPDATE battles SET proxy_spec = $1 WHERE id = $2")
        .bind(&json)
        .bind(battle_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    // Ensure budget row exists
    sqlx::query("INSERT INTO proxy_budgets (battle_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(battle_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(())
}

/// Get public proxy info (no secrets).
///
/// Spend is aggregated on demand per service: the sum of its
/// `proxy_charges` (since migration 079 every proxied call gets one —
/// `flat`, `actual`, `capped`, `fallback`, `duplicate`, `abandoned`, or
/// an in-flight `reserved` hold) plus `cost_per_1k_cents / 1000` for
/// each older `proxy_logs` row that has no charge (calls made before
/// charges were recorded for that service).
///
/// The legacy `proxy_budgets.total_spent_cents` cache is no longer
/// authoritative — it stays updated in `log_request` for back-compat
/// with anyone reading it directly, but every UI surface and budget
/// gate goes through this aggregation.
pub async fn get_proxy_info(pool: &PgPool, battle_id: Uuid) -> Result<ProxyInfo, ApiError> {
    let spec = get_proxy_spec(pool, battle_id).await?;
    get_proxy_info_with_spec(pool, battle_id, &spec).await
}

/// Same as [`get_proxy_info`] but using a caller-provided spec —
/// saves one round-trip when the caller already has the battle row
/// in hand (e.g. `web::pages::battle_detail` reads `proxy_spec`
/// straight off the `battles` row that was loaded for the rest of
/// the render). Still hits the DB for per-service usage, which can't
/// be derived from the spec alone.
pub async fn get_proxy_info_with_spec(
    pool: &PgPool,
    battle_id: Uuid,
    spec: &ProxySpec,
) -> Result<ProxyInfo, ApiError> {
    let mut conn = pool.acquire().await.map_err(internal)?;
    let usage = load_usage(&mut conn, battle_id, None).await?;
    Ok(assemble_proxy_info(spec, &usage))
}

/// Wire text for `ProxyInfo::rate_limit_scope` — keep in sync with
/// `docs/proxy-spec.md` and `web::rate_limit`.
pub const PROXY_RATE_LIMIT_SCOPE: &str = "per user, per battle, all services combined, \
     fixed 60-second window; proxy calls are exempt from the platform-wide request limit, \
     so this is the effective limit (HTTP 429 + Retry-After when exceeded)";

pub const PROXY_LOCK_TIMEOUT: &str = "3s";

fn internal(e: sqlx::Error) -> ApiError {
    if is_proxy_contention(&e) {
        return ApiError::ProxyBusy;
    }
    ApiError::Internal(e.into())
}

fn is_proxy_contention(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::Database(db) => db.code().as_deref() == Some("55P03"),
        _ => false,
    }
}

/// Per-service usage for one battle (optionally scoped to one user).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ServiceUsage {
    /// `proxy_logs` rows.
    pub request_count: i64,
    /// `proxy_logs` rows without a `proxy_charges` row — billed at the
    /// flat `cost_per_1k_cents` rate.
    pub uncharged_count: i64,
    /// Sum of `proxy_charges.amount_micro_usd` (reservations included,
    /// stale ones too — at the reserved cap).
    pub charged_micro: i64,
    /// Charges settled at an upstream-reported cost (actual or capped).
    pub actual_cost_requests: i64,
}

pub type UsageMap = HashMap<String, ServiceUsage>;

/// A reservation that has not settled after this long (upstream
/// timeout is 120s) is swept to `abandoned` at its reserved amount —
/// the per-request cap, else the flat rate — by the next reservation on
/// the same battle. The process died mid-call (a dropped handler
/// settles itself through `routes::proxy`'s drop guard); the sponsor
/// was probably billed upstream, so it is never counted at less.
const STALE_RESERVATION: &str = "5 minutes";

async fn load_usage(
    conn: &mut sqlx::PgConnection,
    battle_id: Uuid,
    user_id: Option<Uuid>,
) -> Result<UsageMap, ApiError> {
    let logs: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT l.service_name, COUNT(*)::BIGINT, \
           COUNT(*) FILTER (WHERE NOT EXISTS ( \
             SELECT 1 FROM proxy_charges c WHERE c.proxy_log_id = l.id))::BIGINT \
         FROM proxy_logs l \
         WHERE l.battle_id = $1 AND ($2::uuid IS NULL OR l.user_id = $2) \
         GROUP BY l.service_name",
    )
    .bind(battle_id)
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(internal)?;

    // Reservations (stale or not) count at their reserved amount — the
    // per-request cap, else the flat rate — never at less.
    let charges: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT service_name, \
           COALESCE(SUM(amount_micro_usd), 0)::BIGINT, \
           COUNT(*) FILTER (WHERE source IN ('actual', 'capped'))::BIGINT \
         FROM proxy_charges \
         WHERE battle_id = $1 AND ($2::uuid IS NULL OR user_id = $2) \
         GROUP BY service_name",
    )
    .bind(battle_id)
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(internal)?;

    let mut usage = UsageMap::new();
    for (name, count, uncharged) in logs {
        let u = usage.entry(name).or_default();
        u.request_count = count;
        u.uncharged_count = uncharged;
    }
    for (name, micro, actual) in charges {
        let u = usage.entry(name).or_default();
        u.charged_micro = micro;
        u.actual_cost_requests = actual;
    }
    Ok(usage)
}

/// Spend for one service in µ$. A service no longer in the spec keeps
/// its recorded actual charges (real money) but its flat-billed rows
/// count as 0, as before.
pub fn service_spend_micro(spec: &ProxySpec, name: &str, u: &ServiceUsage) -> i64 {
    let flat = spec
        .services
        .get(name)
        .map(|s| proxy_billing::flat_micro_per_request(s.cost_per_1k_cents))
        .unwrap_or(0);
    u.charged_micro
        .saturating_add(u.uncharged_count.saturating_mul(flat))
}

/// Overflow-safe (saturating) — a garbage row must not wrap the total
/// negative and reopen an exhausted budget.
pub fn total_spend_micro(spec: &ProxySpec, usage: &UsageMap) -> i64 {
    usage
        .iter()
        .map(|(name, u)| service_spend_micro(spec, name, u))
        .fold(0i64, i64::saturating_add)
}

fn assemble_proxy_info(spec: &ProxySpec, usage: &UsageMap) -> ProxyInfo {
    let mut names: Vec<String> = spec.services.keys().cloned().collect();
    names.sort();

    let empty = ServiceUsage::default();
    let mut total_requests: i64 = 0;
    let service_details: Vec<ServicePublicInfo> = names
        .iter()
        .map(|name| {
            let svc = &spec.services[name];
            let u = usage.get(name.as_str()).unwrap_or(&empty);
            let micro = service_spend_micro(spec, name, u);
            total_requests = total_requests.saturating_add(u.request_count);
            ServicePublicInfo {
                name: name.clone(),
                docs_url: svc.docs_url.clone(),
                pricing_note: svc.pricing_note.clone(),
                cost_per_1k_cents: svc.cost_per_1k_cents,
                request_count: u.request_count,
                spent_cents: proxy_billing::micro_to_cents(micro),
                spent_usd: proxy_billing::micro_to_usd(micro),
                billing: svc.billing.as_ref().map(|b| b.source.as_str()),
                actual_cost_requests: u.actual_cost_requests,
            }
        })
        .collect();

    let total_micro = total_spend_micro(spec, usage);
    // Headline `spent_cents` / `remaining_cents` stay integer for wire
    // compatibility (floored); `spent_usd` carries the precise value.
    let spent_cents_int =
        (total_micro / proxy_billing::MICRO_PER_CENT).clamp(0, i64::from(i32::MAX)) as i32;
    let remaining_cents = spec.budget_cents.saturating_sub(spent_cents_int).max(0);

    ProxyInfo {
        services: names,
        service_details,
        budget_cents: spec.budget_cents,
        per_claim_cents: spec.per_claim_cents,
        rate_limit_rpm: crate::web::rate_limit::effective_proxy_rpm(
            spec.rate_limit_rpm,
            crate::web::rate_limit::TierLimits::default().global_per_min,
        ) as i32,
        rate_limit_scope: PROXY_RATE_LIMIT_SCOPE,
        spent_cents: spent_cents_int,
        remaining_cents,
        total_requests: total_requests.clamp(0, i64::from(i32::MAX)) as i32,
        spent_usd: proxy_billing::micro_to_usd(total_micro),
    }
}

/// `Some(error)` when `spent_micro` has reached a non-zero cap.
fn cap_error(spent_micro: i64, cap_cents: i32, what: &str) -> Option<ApiError> {
    (cap_cents > 0 && spent_micro >= i64::from(cap_cents) * proxy_billing::MICRO_PER_CENT).then(
        || {
            ApiError::ValidationError(format!(
                "{what} budget exhausted: spent {:.4}¢ of {}¢",
                proxy_billing::micro_to_cents(spent_micro),
                cap_cents
            ))
        },
    )
}

const BATTLE_TOTAL: Uuid = Uuid::nil();

/// Check the battle-wide and per-claimer caps without reserving (read
/// only). The proxy route uses [`reserve_charge`], which runs the same
/// check atomically with its reservation.
pub async fn check_budgets(
    pool: &PgPool,
    spec: &ProxySpec,
    battle_id: Uuid,
    user_id: Uuid,
) -> Result<(), ApiError> {
    if spec.budget_cents <= 0 && spec.per_claim_cents <= 0 {
        return Ok(());
    }
    let mut conn = pool.acquire().await.map_err(internal)?;
    let charged = read_spend(&mut conn, battle_id, user_id, false).await?;
    enforce_caps(&mut conn, spec, battle_id, user_id, charged).await
}

/// Running charge totals `(battle, claimer)` from `proxy_spend`, kept
/// exact by the `proxy_charges` trigger (migration 083). With `lock`,
/// both rows are created if missing and locked `FOR UPDATE` in a fixed
/// order (battle row first), matching the trigger's update order.
async fn read_spend(
    conn: &mut sqlx::PgConnection,
    battle_id: Uuid,
    user_id: Uuid,
    lock: bool,
) -> Result<(i64, i64), ApiError> {
    let select = if lock {
        "SELECT user_id, charged_micro_usd FROM proxy_spend \
         WHERE battle_id = $1 AND user_id IN ($2, $3) ORDER BY user_id FOR UPDATE"
    } else {
        "SELECT user_id, charged_micro_usd FROM proxy_spend \
         WHERE battle_id = $1 AND user_id IN ($2, $3)"
    };
    let mut rows: Vec<(Uuid, i64)> = sqlx::query_as(select)
        .bind(battle_id)
        .bind(BATTLE_TOTAL)
        .bind(user_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(internal)?;
    if lock && rows.len() < 2 {
        sqlx::query(
            "INSERT INTO proxy_spend (battle_id, user_id) VALUES ($1, $2), ($1, $3) \
             ON CONFLICT (battle_id, user_id) DO NOTHING",
        )
        .bind(battle_id)
        .bind(BATTLE_TOTAL)
        .bind(user_id)
        .execute(&mut *conn)
        .await
        .map_err(internal)?;
        rows = sqlx::query_as(select)
            .bind(battle_id)
            .bind(BATTLE_TOTAL)
            .bind(user_id)
            .fetch_all(&mut *conn)
            .await
            .map_err(internal)?;
    }
    let get = |id: Uuid| rows.iter().find(|(u, _)| *u == id).map_or(0, |(_, m)| *m);
    Ok((get(BATTLE_TOTAL), get(user_id)))
}

/// Flat-rate spend of requests logged before every call reserved a
/// charge (frozen per-service counts in `proxy_spend_legacy`, priced at
/// the current spec's rates; services no longer in the spec count 0).
async fn legacy_flat_micro(
    conn: &mut sqlx::PgConnection,
    spec: &ProxySpec,
    battle_id: Uuid,
    user_id: Uuid,
) -> Result<i64, ApiError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT service_name, uncharged_requests FROM proxy_spend_legacy \
         WHERE battle_id = $1 AND user_id = $2",
    )
    .bind(battle_id)
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(internal)?;
    Ok(rows
        .iter()
        .map(|(name, count)| {
            let flat = spec
                .services
                .get(name)
                .map(|s| proxy_billing::flat_micro_per_request(s.cost_per_1k_cents))
                .unwrap_or(0);
            count.saturating_mul(flat)
        })
        .fold(0i64, i64::saturating_add))
}

async fn enforce_caps(
    conn: &mut sqlx::PgConnection,
    spec: &ProxySpec,
    battle_id: Uuid,
    user_id: Uuid,
    (battle_charged, user_charged): (i64, i64),
) -> Result<(), ApiError> {
    if spec.budget_cents > 0 {
        let spent = battle_charged
            .saturating_add(legacy_flat_micro(conn, spec, battle_id, BATTLE_TOTAL).await?);
        if let Some(e) = cap_error(spent, spec.budget_cents, "battle") {
            return Err(e);
        }
    }
    if spec.per_claim_cents > 0 {
        let spent =
            user_charged.saturating_add(legacy_flat_micro(conn, spec, battle_id, user_id).await?);
        if let Some(e) = cap_error(spent, spec.per_claim_cents, "per-claimer") {
            return Err(e);
        }
    }
    Ok(())
}

/// Settle reservations orphaned by a dead process at their reserved
/// amount (they already count at it, so totals don't change). Served by
/// the partial index on open reservations, so it stays cheap.
async fn sweep_stale_reservations(pool: &PgPool, battle_id: Uuid) {
    if let Err(e) = sqlx::query(&format!(
        "UPDATE proxy_charges SET source = 'abandoned', settled_at = now() \
         WHERE battle_id = $1 AND source = 'reserved' \
           AND created_at < now() - interval '{STALE_RESERVATION}'"
    ))
    .bind(battle_id)
    .execute(pool)
    .await
    {
        tracing::warn!(%battle_id, error = %e, "proxy billing: stale reservation sweep failed");
    }
}

/// Reserve a charge for a proxied request — every service, flat-rate
/// or usage-billed, before the upstream call.
///
/// For a capped Battle, locks the Battle's and the Claimer's running
/// totals (`proxy_spend`), checks both caps and inserts a `reserved`
/// `proxy_charges` row holding `reserve_micro` (the per-request cap,
/// else the flat rate) in one short transaction; the trigger adds it to
/// the totals before the locks are released, so concurrent requests see
/// each other's reservations and can't all slip under the cap. Cost is
/// constant in the Battle's history. Settled by [`settle_charge`] once
/// the response is parsed — or, if abandoned after being sent, at its
/// reserved amount.
#[allow(clippy::too_many_arguments)]
pub async fn reserve_charge(
    pool: &PgPool,
    spec: &ProxySpec,
    battle_id: Uuid,
    user_id: Uuid,
    service_name: &str,
    proxy_log_id: Uuid,
    reserve_micro: i64,
    fallback_micro: i64,
) -> Result<Uuid, ApiError> {
    let capped = spec.budget_cents > 0 || spec.per_claim_cents > 0;
    if capped {
        sweep_stale_reservations(pool, battle_id).await;
    }
    let mut tx = pool.begin().await.map_err(internal)?;
    if capped {
        sqlx::query(&format!("SET LOCAL lock_timeout = '{PROXY_LOCK_TIMEOUT}'"))
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        let charged = read_spend(&mut tx, battle_id, user_id, true).await?;
        enforce_caps(&mut tx, spec, battle_id, user_id, charged).await?;
    }
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO proxy_charges \
           (battle_id, user_id, service_name, proxy_log_id, amount_micro_usd, \
            fallback_micro_usd, source) \
         VALUES ($1, $2, $3, $4, $5, $6, 'reserved') RETURNING id",
    )
    .bind(battle_id)
    .bind(user_id)
    .bind(service_name)
    .bind(proxy_log_id)
    .bind(reserve_micro.max(0))
    .bind(fallback_micro.max(0))
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(id)
}

/// Drop a reservation whose upstream call never produced a response
/// (connect error / timeout) — such requests aren't logged or billed.
pub async fn release_charge(pool: &PgPool, charge_id: Uuid) {
    if let Err(e) = sqlx::query("DELETE FROM proxy_charges WHERE id = $1 AND source = 'reserved'")
        .bind(charge_id)
        .execute(pool)
        .await
    {
        tracing::warn!(%charge_id, error = %e, "proxy billing: failed to release reservation");
    }
}

/// Settle a reservation to the decided amount. Returns the amount
/// finally charged, in µ$.
///
/// When the decision carries a dedupe key that this battle+service
/// already billed (a Monid poll repeating its run's cost), the charge
/// is recorded as `duplicate` and bills only the increase over what
/// the key was already charged — so a run first seen at a partial cost
/// and later at a larger final cost ends up billed the max seen, not
/// the first value. A per-key advisory lock serializes concurrent
/// settlements of the same key.
pub async fn settle_charge(pool: &PgPool, charge_id: Uuid, d: &ChargeDecision) -> i64 {
    match try_settle_charge(pool, charge_id, d).await {
        Ok(micro) => micro,
        Err(e) => {
            // Leave the reservation in place: it counts at its reserved
            // amount (the cap, else flat) — never at 0.
            tracing::warn!(%charge_id, error = %e, "proxy billing: failed to settle charge");
            d.amount_micro.max(0)
        }
    }
}

async fn try_settle_charge(
    pool: &PgPool,
    charge_id: Uuid,
    d: &ChargeDecision,
) -> Result<i64, sqlx::Error> {
    let amount = d.amount_micro.max(0);
    let mut tx = pool.begin().await?;
    if let Some(key) = d.dedupe_key.as_deref() {
        let (battle_id, service_name): (Uuid, String) =
            sqlx::query_as("SELECT battle_id, service_name FROM proxy_charges WHERE id = $1")
                .bind(charge_id)
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended( \
               'proxy_dedupe:' || $1::text || ':' || $2::text || ':' || $3::text, 0))",
        )
        .bind(battle_id)
        .bind(&service_name)
        .bind(key)
        .execute(&mut *tx)
        .await?;
        let (seen, billed): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*)::BIGINT, COALESCE(SUM(amount_micro_usd), 0)::BIGINT \
             FROM proxy_charges \
             WHERE battle_id = $1 AND service_name = $2 AND id <> $4 \
               AND (dedupe_key = $3 OR duplicate_of_key = $3)",
        )
        .bind(battle_id)
        .bind(&service_name)
        .bind(key)
        .bind(charge_id)
        .fetch_one(&mut *tx)
        .await?;
        if seen > 0 {
            let delta = proxy_billing::dedupe_delta(amount, billed);
            sqlx::query(
                "UPDATE proxy_charges SET amount_micro_usd = $2, source = $3, \
                   duplicate_of_key = $4, reported_micro_usd = $5, settled_at = now() \
                 WHERE id = $1",
            )
            .bind(charge_id)
            .bind(delta)
            .bind(ChargeSource::Duplicate.as_str())
            .bind(key)
            .bind(d.reported_micro.unwrap_or(d.amount_micro))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(delta);
        }
    }
    sqlx::query(
        "UPDATE proxy_charges SET amount_micro_usd = $2, source = $3, dedupe_key = $4, \
           reported_micro_usd = $5, settled_at = now() \
         WHERE id = $1",
    )
    .bind(charge_id)
    .bind(amount)
    .bind(d.source.as_str())
    .bind(d.dedupe_key.as_deref())
    .bind(d.reported_micro)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(amount)
}

/// Log a proxied request with full request/response capture.
/// `log_id` is generated by the caller so a `proxy_charges` row
/// reserved before the upstream call can point at this log row.
#[allow(clippy::too_many_arguments)]
pub async fn log_request(
    pool: &PgPool,
    log_id: Uuid,
    battle_id: Uuid,
    user_id: Uuid,
    service_name: &str,
    method: &str,
    path: &str,
    request_body_hash: Option<&str>,
    request_body: Option<&str>,
    request_headers: &serde_json::Value,
    response_status: i32,
    response_body_size: Option<i32>,
    latency_ms: i32,
    cost_cents: i32,
    response_body: Option<&str>,
    response_headers: &serde_json::Value,
) {
    let _ = sqlx::query(
        "INSERT INTO proxy_logs (id, battle_id, user_id, service_name, method, path, \
         request_body_hash, request_body, request_headers, \
         response_status, response_body_size, latency_ms, cost_cents, \
         response_body, response_headers) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(log_id)
    .bind(battle_id)
    .bind(user_id)
    .bind(service_name)
    .bind(method)
    .bind(path)
    .bind(request_body_hash)
    .bind(request_body)
    .bind(request_headers)
    .bind(response_status)
    .bind(response_body_size)
    .bind(latency_ms)
    .bind(cost_cents)
    .bind(response_body)
    .bind(response_headers)
    .execute(pool)
    .await;

    // Update budget counter
    let _ = sqlx::query(
        "INSERT INTO proxy_budgets (battle_id, total_spent_cents, total_requests) \
         VALUES ($1, $2, 1) \
         ON CONFLICT (battle_id) DO UPDATE SET \
           total_spent_cents = proxy_budgets.total_spent_cents + $2, \
           total_requests = proxy_budgets.total_requests + 1, \
           updated_at = NOW()",
    )
    .bind(battle_id)
    .bind(cost_cents)
    .execute(pool)
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> ProxySpec {
        ProxySpec::validate(&json!({
            "services": {
                "keenable": {
                    "upstream": "https://api.keenable.ai",
                    "docs_url": "https://docs.keenable.ai/search",
                    "cost_per_1k_cents": 100
                },
                "monid": {
                    "upstream": "https://api.monid.ai",
                    "docs_url": "https://monid.ai/SKILL.md",
                    "cost_per_1k_cents": 0,
                    "billing": {
                        "type": "response_json",
                        "cost_path": "cost.value",
                        "dedupe_key_path": "runId",
                        "max_per_request_cents": 10
                    }
                },
                "openrouter": {
                    "upstream": "https://openrouter.ai",
                    "docs_url": "https://openrouter.ai/docs",
                    "cost_per_1k_cents": 100,
                    "billing": { "type": "response_json", "cost_path": "usage.cost" }
                }
            },
            "budget_cents": 2,
            "per_claim_cents": 1
        }))
        .unwrap()
    }

    fn usage(entries: &[(&str, i64, i64, i64, i64)]) -> UsageMap {
        entries
            .iter()
            .map(|(n, req, unch, micro, act)| {
                (
                    n.to_string(),
                    ServiceUsage {
                        request_count: *req,
                        uncharged_count: *unch,
                        charged_micro: *micro,
                        actual_cost_requests: *act,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn flat_only_matches_legacy_math() {
        // 3 keenable calls at $1/1K = 0.3¢, exactly what the old
        // count × rate aggregation produced.
        let info = assemble_proxy_info(&spec(), &usage(&[("keenable", 3, 3, 0, 0)]));
        let k = info
            .service_details
            .iter()
            .find(|d| d.name == "keenable")
            .unwrap();
        assert!((k.spent_cents - 0.3).abs() < 1e-9);
        assert_eq!(k.billing, None);
        assert_eq!(info.spent_cents, 0);
        assert!((info.spent_usd - 0.003).abs() < 1e-12);
        assert_eq!(info.total_requests, 3);
    }

    #[test]
    fn mixed_flat_and_actual_services() {
        let u = usage(&[
            // flat: 10 calls × 0.1¢ = 1¢
            ("keenable", 10, 10, 0, 0),
            // monid: two runs at $0.004 + one duplicate poll at 0 = 0.8¢
            ("monid", 3, 0, 8_000, 2),
            // openrouter: 2,698 legacy flat rows (2.698$) + 5 actual calls
            // totalling 140µ$ — history stays at the flat rate.
            ("openrouter", 2_703, 2_698, 140, 5),
        ]);
        let s = spec();
        assert_eq!(service_spend_micro(&s, "keenable", &u["keenable"]), 10_000);
        assert_eq!(service_spend_micro(&s, "monid", &u["monid"]), 8_000);
        assert_eq!(
            service_spend_micro(&s, "openrouter", &u["openrouter"]),
            2_698_140
        );
        let total = total_spend_micro(&s, &u);
        assert_eq!(total, 2_716_140);

        let info = assemble_proxy_info(&s, &u);
        assert_eq!(info.spent_cents, 271); // floored
        assert!((info.spent_usd - 2.71614).abs() < 1e-9);
        let m = info
            .service_details
            .iter()
            .find(|d| d.name == "monid")
            .unwrap();
        assert_eq!(m.billing, Some("response_json"));
        assert_eq!(m.actual_cost_requests, 2);
        assert!((m.spent_cents - 0.8).abs() < 1e-9);
        assert!((m.spent_usd - 0.008).abs() < 1e-12);
    }

    #[test]
    fn actual_costs_trip_the_budget_caps() {
        let s = spec(); // budget 2¢, per-claimer 1¢
        // Monid at $0.004/run used to be booked as $0 forever. Five
        // runs = 2¢ now exhausts the battle budget.
        let u = usage(&[("monid", 5, 0, 20_000, 5)]);
        let spent = total_spend_micro(&s, &u);
        assert!(cap_error(spent, s.budget_cents, "battle").is_some());
        assert!(cap_error(spent - 1, s.budget_cents, "battle").is_none());
        // Per-claimer: 1¢ = 10_000µ$.
        assert!(cap_error(10_000, s.per_claim_cents, "per-claimer").is_some());
        assert!(cap_error(9_999, s.per_claim_cents, "per-claimer").is_none());
        // 0 = unlimited.
        assert!(cap_error(i64::MAX / 2, 0, "battle").is_none());
    }

    #[test]
    fn removed_service_keeps_actual_charges_but_not_flat_rows() {
        let u = usage(&[("gone", 4, 2, 5_000, 2)]);
        assert_eq!(service_spend_micro(&spec(), "gone", &u["gone"]), 5_000);
        let info = assemble_proxy_info(&spec(), &u);
        assert!(info.service_details.iter().all(|d| d.name != "gone"));
        assert!((info.spent_usd - 0.005).abs() < 1e-12);
    }
}
