//! Append-only user activity log (`user_logs`). Every event is written
//! fire-and-forget so logging never blocks or fails a request. Keyed by
//! the `ka_device` cookie so anonymous + logged-in activity share an id
//! that future feed personalization can consume.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct LogEvent {
    pub device_id: Uuid,
    pub voter_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    /// "visit" | "login" | "logout" | "vote"
    pub event: &'static str,
    pub path: Option<String>,
    pub method: Option<String>,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
}

/// Fire-and-forget insert. Returns immediately; a DB error is logged at
/// `warn` and never surfaces to the caller.
pub fn spawn_log(pool: &PgPool, ev: LogEvent) {
    let pool = pool.clone();
    tokio::spawn(async move {
        let res = sqlx::query(
            "INSERT INTO user_logs \
             (device_id, voter_id, user_id, event, path, method, referer, user_agent) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(ev.device_id)
        .bind(ev.voter_id)
        .bind(ev.user_id)
        .bind(ev.event)
        .bind(ev.path)
        .bind(ev.method)
        .bind(ev.referer)
        .bind(ev.user_agent)
        .execute(&pool)
        .await;
        if let Err(e) = res {
            tracing::warn!("user_log insert failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_event_builds() {
        let ev = LogEvent {
            device_id: Uuid::nil(),
            voter_id: None,
            user_id: None,
            event: "visit",
            path: Some("/".into()),
            method: Some("GET".into()),
            referer: None,
            user_agent: None,
        };
        assert_eq!(ev.event, "visit");
        assert!(ev.voter_id.is_none());
    }
}
