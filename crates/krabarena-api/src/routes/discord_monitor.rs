use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::services::discord_monitor::{
    DiscordBotClient, DiscordMonitorRunCounters, DiscordMonitorRunResponse,
    DiscordMonitorRunStatus, determine_run_status, is_authorized_cron_request, record_monitor_run,
    run_enabled_monitoring,
};

pub async fn run(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let provided_secret = headers.get("x-cron-secret").and_then(|v| v.to_str().ok());
    if !is_authorized_cron_request(&state.discord_monitor, provided_secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let run_id = Uuid::new_v4();
    let started_at = Utc::now();
    let mut status = determine_run_status(&state.discord_monitor);
    let mut counters = DiscordMonitorRunCounters::default();
    let mut error_message = None;

    if status == DiscordMonitorRunStatus::Success {
        let client = DiscordBotClient::from_config(&state.discord_monitor);
        match run_enabled_monitoring(&state.pool, &state.discord_monitor, &client, started_at).await
        {
            Ok(run_counters) => {
                counters = run_counters;
            }
            Err(err) => {
                status = DiscordMonitorRunStatus::Failed;
                counters = err.counters;
                error_message = Some(err.sanitized_message().to_string());
            }
        }
    }

    let finished_at = Utc::now();

    match record_monitor_run(
        &state.pool,
        run_id,
        started_at,
        finished_at,
        status,
        counters,
        error_message.as_deref(),
    )
    .await
    {
        Ok(record) => Json(ApiResponse::new(DiscordMonitorRunResponse::from_record(
            &record,
        )))
        .into_response(),
        Err(err) => {
            tracing::error!(
                error = %err,
                run_id = %run_id,
                "discord.monitor.run.persist_failed"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::new(DiscordMonitorRunResponse::failed(run_id))),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services;
    use crate::services::discord_monitor::DiscordMonitorConfig;
    use axum::body::to_bytes;
    use sqlx::postgres::PgPoolOptions;

    fn db_url() -> Option<String> {
        if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
            return Some(url);
        }
        let url = std::env::var("DATABASE_URL").ok()?;
        if url.contains("localhost") || url.contains("127.0.0.1") {
            Some(url)
        } else {
            eprintln!("SKIP: DATABASE_URL is not local; set TEST_DATABASE_URL for DB integration");
            None
        }
    }

    async fn test_state(config: DiscordMonitorConfig) -> Option<AppState> {
        let url = db_url()?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .ok()?;
        crate::db::run_migrations(&url).await.ok()?;
        Some(AppState {
            proxy_pool: pool.clone(),
            pool,
            jwt_secret: "test-jwt-secret".into(),
            blob_store_url: "http://localhost:3001/blob".into(),
            private_blob_store_url: None,
            public_base_url: config.public_base_url.clone(),
            discord_monitor: config,
            email_service: None,
            gemini: services::gemini_service::GeminiClient::new(None),
            proxy_http: reqwest::Client::new(),
            rate_limiter: None,
            leaderboards: std::sync::Arc::new(crate::LeaderboardCaches::new()),
            recaptcha: crate::RecaptchaConfig::default(),
        })
    }

    fn lazy_state(config: DiscordMonitorConfig) -> AppState {
        AppState {
            pool: PgPoolOptions::new()
                .max_connections(1)
                .connect_lazy("postgresql://localhost:1/krabarena")
                .expect("lazy pool"),
            proxy_pool: PgPoolOptions::new()
                .max_connections(1)
                .connect_lazy("postgresql://localhost:1/krabarena")
                .expect("lazy pool"),
            jwt_secret: "test-jwt-secret".into(),
            blob_store_url: "http://localhost:3001/blob".into(),
            private_blob_store_url: None,
            public_base_url: config.public_base_url.clone(),
            discord_monitor: config,
            email_service: None,
            gemini: services::gemini_service::GeminiClient::new(None),
            proxy_http: reqwest::Client::new(),
            rate_limiter: None,
            leaderboards: std::sync::Arc::new(crate::LeaderboardCaches::new()),
            recaptcha: crate::RecaptchaConfig::default(),
        }
    }

    fn headers(secret: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(secret) = secret {
            headers.insert("x-cron-secret", secret.parse().expect("valid header"));
        }
        headers
    }

    fn config(values: &[(&str, &str)]) -> DiscordMonitorConfig {
        let map = values
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<std::collections::HashMap<_, _>>();
        DiscordMonitorConfig::from_lookup(|key| map.get(key).cloned())
    }

    async fn count_run(pool: &sqlx::PgPool, id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM discord_monitor_runs WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("count monitor run")
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        serde_json::from_slice(&bytes).expect("response json")
    }

    #[tokio::test]
    async fn authorized_disabled_run_is_persisted() {
        let Some(state) = test_state(config(&[
            ("CRON_SHARED_SECRET", "letmein"),
            ("DISCORD_MONITOR_ENABLED", "false"),
        ]))
        .await
        else {
            eprintln!("SKIP: TEST_DATABASE_URL/DATABASE_URL unavailable");
            return;
        };

        let response = run(State(state.clone()), headers(Some("letmein"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["data"]["status"], "disabled");
        assert_eq!(body["data"]["messages_sent"], 0);
        let run_id = body["data"]["run_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .expect("run_id");
        assert_eq!(count_run(&state.pool, run_id).await, 1);
    }

    #[tokio::test]
    async fn missing_secret_does_not_persist_run() {
        let state = lazy_state(config(&[
            ("CRON_SHARED_SECRET", "letmein"),
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "bot-token"),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
        ]));

        let response = run(State(state.clone()), headers(None)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn record_monitor_run_inserts_success_row() {
        let Some(state) = test_state(config(&[
            ("CRON_SHARED_SECRET", "letmein"),
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "bot-token"),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
        ]))
        .await
        else {
            eprintln!("SKIP: TEST_DATABASE_URL/DATABASE_URL unavailable");
            return;
        };

        let run_id = Uuid::new_v4();
        let now = Utc::now();
        let record = record_monitor_run(
            &state.pool,
            run_id,
            now,
            now,
            crate::services::discord_monitor::DiscordMonitorRunStatus::Success,
            DiscordMonitorRunCounters::default(),
            None,
        )
        .await
        .expect("insert monitor run");

        assert_eq!(record.status.as_str(), "success");
        assert_eq!(count_run(&state.pool, run_id).await, 1);

        let (status, messages_sent, finished_at): (String, i32, Option<chrono::DateTime<Utc>>) =
            sqlx::query_as(
                "SELECT status, messages_sent, finished_at FROM discord_monitor_runs WHERE id = $1",
            )
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .expect("fetch run row");
        assert_eq!(status, "success");
        assert_eq!(messages_sent, 0);
        assert!(finished_at.is_some());
    }
}
