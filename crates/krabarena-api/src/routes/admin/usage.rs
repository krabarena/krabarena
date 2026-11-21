// --- Usage stats (crawlers / humans / CLI / content) ---
//
// `GET /v1/admin/usage?date=YYYY-MM-DD&days=N` — admin-only data source
// for the daily digest. Shape + field docs:
// logic: `services/usage_stats.rs`, UA classes: `services/ua_classifier.rs`.

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    /// Last day of the window (inclusive, UTC). Default: yesterday.
    pub date: Option<String>,
    /// Window length in days, 1..=90. Default 1.
    pub days: Option<i64>,
}

fn parse_usage_window(
    q: &UsageQuery,
    today: chrono::NaiveDate,
) -> Result<crate::services::usage_stats::UsageWindow, ApiError> {
    let date = match q.date.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
                ApiError::ValidationError(format!("date must be YYYY-MM-DD, got '{s}'"))
            })?,
        ),
        None => None,
    };
    crate::services::usage_stats::UsageWindow::resolve(date, q.days, today)
        .map_err(ApiError::ValidationError)
}

/// GET /v1/admin/usage
///
/// Usage report for one UTC window: crawler hits + coverage by
/// category/family, human web activity, CLI usage, content created,
/// and deltas vs the previous window of equal length. Admin-gated.
#[utoipa::path(
    get,
    path = "/v1/admin/usage",
    tag = "admin",
    params(
        ("date" = Option<String>, Query, description = "Last day of the window, YYYY-MM-DD (UTC, inclusive). Default: yesterday"),
        ("days" = Option<i64>, Query, description = "Window length in days (1..90, default 1)"),
    ),
    responses(
        (status = 200, description = "Usage report", body = crate::services::usage_stats::UsageReport),
        (status = 400, description = "Bad date/days"),
        (status = 403, description = "Not an admin"),
    ),
)]
pub async fn usage_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let now = Utc::now();
    let window = parse_usage_window(&q, now.date_naive())?;
    let started = std::time::Instant::now();
    let report = crate::services::usage_stats::build_report(
        &state.pool,
        window,
        crate::routes::health::VERSION,
        now,
    )
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    tracing::info!(
        event = "admin.usage_stats",
        days = window.days,
        last_day = %window.last_day,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "usage report built"
    );
    Ok(Json(ApiResponse::new(report)))
}

#[cfg(test)]
mod usage_route_tests {
    use super::*;

    fn d(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn parses_date_and_days() {
        let q = UsageQuery {
            date: Some("2026-10-20".into()),
            days: Some(7),
        };
        let w = parse_usage_window(&q, d("2026-10-25")).unwrap();
        assert_eq!((w.first_day, w.last_day, w.days), (d("2026-10-14"), d("2026-10-20"), 7));
        let q = UsageQuery {
            date: Some("  ".into()),
            days: None,
        };
        assert_eq!(parse_usage_window(&q, d("2026-10-25")).unwrap().last_day, d("2026-10-24"));
    }

    #[test]
    fn rejects_bad_input() {
        for (date, days) in [
            (Some("2026/10/20"), None),
            (Some("yesterday"), None),
            (None, Some(0)),
            (None, Some(91)),
            (Some("2027-01-01"), None),
        ] {
            let q = UsageQuery {
                date: date.map(String::from),
                days,
            };
            assert!(
                matches!(parse_usage_window(&q, d("2026-10-25")), Err(ApiError::ValidationError(_))),
                "{date:?} {days:?}"
            );
        }
    }
}
