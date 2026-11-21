//! E2E test for battle-candidates voter-session auth.
//!
//! Regression guard for the 0.9.3 → 0.9.4 bug where the route used
//! `x-krabarena-token` (CLI JWT) against the `users` FK while the
//! public-web auth is actually `x-voter-token` (voter_sessions) against
//! the `voters` FK. Every POST to /v1/battle-candidates returned 401
//! in prod until 029 + the route rewrite landed.
//!
//! Run against a live local or test-env server:
//!   TEST_DATABASE_URL=postgres://... \
//!   KRAB_TEST_BASE=http://127.0.0.1:3001 \
//!     cargo test -p krabarena-api --test battle_candidates_e2e -- --ignored
//!
//! Skips if either env var is unset.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

fn base_url() -> Option<String> {
    std::env::var("KRAB_TEST_BASE")
        .ok()
        .filter(|s| !s.is_empty())
}

fn db_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|s| !s.is_empty())
}

/// Insert a throwaway voter + voter_session and return
/// (voter_id, session_token). Caller is responsible for cleanup so a
/// re-run lands on a fresh row (we tag the provider_id with a UUID so
/// the `(provider, provider_id)` unique constraint can't trip).
async fn mint_voter(pool: &PgPool) -> (Uuid, String) {
    let provider_id = format!("e2e-{}", Uuid::new_v4());
    // `voters.provider` has a CHECK constraint (`email | google | github |
    // linkedin`), so use `email` for the synthetic test row.
    let voter_id: Uuid = sqlx::query_scalar(
        "INSERT INTO voters (provider, provider_id, display_name, email)
         VALUES ('email', $1, 'E2E Tester', NULL) RETURNING id",
    )
    .bind(&provider_id)
    .fetch_one(pool)
    .await
    .expect("insert voter");
    let token = Uuid::new_v4().to_string();
    let expires = chrono::Utc::now() + chrono::Duration::days(1);
    sqlx::query("INSERT INTO voter_sessions (voter_id, token, expires_at) VALUES ($1, $2, $3)")
        .bind(voter_id)
        .bind(&token)
        .bind(expires)
        .execute(pool)
        .await
        .expect("insert voter_session");
    (voter_id, token)
}

async fn cleanup_voter(pool: &PgPool, voter_id: Uuid) {
    // voter_sessions has no CASCADE on voter_id, so clear it first
    // before dropping the voter. battle_candidate_votes +
    // battle_candidates.proposer_voter_id are CASCADE, so deleting
    // the voter clears those automatically.
    let _ = sqlx::query("DELETE FROM voter_sessions WHERE voter_id = $1")
        .bind(voter_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM voters WHERE id = $1")
        .bind(voter_id)
        .execute(pool)
        .await;
}

/// Mint a JWT for the synthetic `test-agent` (`role=user`) or
/// `test-admin` (`role=admin`) via the dev-only mint endpoint. Skips
/// if `TEST_AUTH_SECRET` isn't set on the server. Mirrors the helper
/// in `tests/review_e2e.rs`.
async fn mint_token(role: &str) -> Option<String> {
    let secret = std::env::var("TEST_AUTH_SECRET")
        .ok()
        .filter(|s| !s.is_empty())?;
    let base = base_url()?;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/test/mint-token"))
        .header("x-test-auth", &secret)
        .json(&serde_json::json!({"user": role}))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["data"]["bearer_token"].as_str().map(String::from)
}

#[tokio::test]
#[ignore]
async fn anon_post_returns_401() {
    let Some(base) = base_url() else {
        eprintln!("SKIP: KRAB_TEST_BASE not set");
        return;
    };
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/battle-candidates"))
        .json(&serde_json::json!({"title": "Should be rejected", "rationale": ""}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status().as_u16(), 401, "anon POST must 401");
}

#[tokio::test]
#[ignore]
async fn voter_session_can_propose_vote_and_unvote() {
    let Some(base) = base_url() else {
        eprintln!("SKIP: KRAB_TEST_BASE not set");
        return;
    };
    let Some(db) = db_url() else {
        eprintln!("SKIP: TEST_DATABASE_URL/DATABASE_URL not set");
        return;
    };
    let pool = PgPool::connect(&db).await.expect("db connect");
    let (voter_id, token) = mint_voter(&pool).await;
    let client = reqwest::Client::new();
    let title = format!("E2E candidate {} — auth regression guard", Uuid::new_v4());

    // 1. Propose with the voter token → 201 + uuid in `data`.
    let resp = client
        .post(format!("{base}/v1/battle-candidates"))
        .header("x-voter-token", &token)
        .json(&serde_json::json!({"title": &title, "rationale": "regression guard"}))
        .send()
        .await
        .expect("propose");
    assert_eq!(
        resp.status().as_u16(),
        201,
        "propose with voter token must 201"
    );
    let body: Value = resp.json().await.expect("propose json");
    let candidate_id = body["data"].as_str().expect("data is candidate uuid");

    // 2. Vote → 204.
    let resp = client
        .post(format!("{base}/v1/battle-candidates/{candidate_id}/vote"))
        .header("x-voter-token", &token)
        .send()
        .await
        .expect("vote");
    assert_eq!(resp.status().as_u16(), 204, "vote must 204");

    // 3. List → finds the candidate with votes_7d >= 1 and viewer_voted true
    //    (the GET is auth-optional; sending the token populates viewer_voted).
    let resp = client
        .get(format!("{base}/v1/battle-candidates"))
        .header("x-voter-token", &token)
        .send()
        .await
        .expect("list");
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.expect("list json");
    let arr = body["data"].as_array().expect("list array");
    let mine = arr
        .iter()
        .find(|c| c["id"].as_str() == Some(candidate_id))
        .expect("our candidate present");
    assert!(mine["votes_7d"].as_i64().unwrap_or(0) >= 1, "votes_7d ≥ 1");
    assert_eq!(mine["viewer_voted"], serde_json::Value::Bool(true));

    // 4. Unvote → 204; viewer_voted flips back to false.
    let resp = client
        .post(format!("{base}/v1/battle-candidates/{candidate_id}/unvote"))
        .header("x-voter-token", &token)
        .send()
        .await
        .expect("unvote");
    assert_eq!(resp.status().as_u16(), 204);

    let resp = client
        .get(format!("{base}/v1/battle-candidates"))
        .header("x-voter-token", &token)
        .send()
        .await
        .expect("list after unvote");
    let body: Value = resp.json().await.expect("list json");
    let mine = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_str() == Some(candidate_id))
        .expect("our candidate still present");
    assert_eq!(mine["viewer_voted"], serde_json::Value::Bool(false));

    cleanup_voter(&pool, voter_id).await;
}

#[tokio::test]
#[ignore]
async fn dup_open_title_returns_validation_error() {
    let Some(base) = base_url() else {
        eprintln!("SKIP: KRAB_TEST_BASE not set");
        return;
    };
    let Some(db) = db_url() else {
        eprintln!("SKIP: TEST_DATABASE_URL/DATABASE_URL not set");
        return;
    };
    let pool = PgPool::connect(&db).await.expect("db connect");
    let (voter_id, token) = mint_voter(&pool).await;
    let client = reqwest::Client::new();
    let title = format!("E2E dup-title guard {}", Uuid::new_v4());

    let resp1 = client
        .post(format!("{base}/v1/battle-candidates"))
        .header("x-voter-token", &token)
        .json(&serde_json::json!({"title": &title, "rationale": ""}))
        .send()
        .await
        .expect("first propose");
    assert_eq!(resp1.status().as_u16(), 201);

    let resp2 = client
        .post(format!("{base}/v1/battle-candidates"))
        .header("x-voter-token", &token)
        .json(&serde_json::json!({"title": &title, "rationale": ""}))
        .send()
        .await
        .expect("dup propose");
    assert!(
        resp2.status().is_client_error(),
        "dup title must be 4xx, got {}",
        resp2.status()
    );

    cleanup_voter(&pool, voter_id).await;
}

#[tokio::test]
#[ignore]
async fn delete_requires_editor_or_admin_role() {
    let Some(base) = base_url() else {
        eprintln!("SKIP: KRAB_TEST_BASE not set");
        return;
    };
    let Some(db) = db_url() else {
        eprintln!("SKIP: TEST_DATABASE_URL/DATABASE_URL not set");
        return;
    };
    let Some(admin_token) = mint_token("test-admin").await else {
        eprintln!("SKIP: TEST_AUTH_SECRET not set (can't mint admin token)");
        return;
    };
    let Some(user_token) = mint_token("test-agent").await else {
        eprintln!("SKIP: can't mint user token");
        return;
    };
    let pool = PgPool::connect(&db).await.expect("db connect");
    let (voter_id, vsession) = mint_voter(&pool).await;
    let client = reqwest::Client::new();
    let title = format!("E2E delete-auth-gate {}", Uuid::new_v4());

    let resp = client
        .post(format!("{base}/v1/battle-candidates"))
        .header("x-voter-token", &vsession)
        .json(&serde_json::json!({"title": &title, "rationale": ""}))
        .send()
        .await
        .expect("propose");
    assert_eq!(resp.status().as_u16(), 201);
    let body: Value = resp.json().await.expect("propose body");
    let cid = body["data"].as_str().expect("uuid").to_string();

    let r = client
        .delete(format!("{base}/v1/battle-candidates/{cid}"))
        .send()
        .await
        .expect("anon delete");
    assert_eq!(
        r.status().as_u16(),
        401,
        "anon must 401, got {}",
        r.status()
    );

    let r = client
        .delete(format!("{base}/v1/battle-candidates/{cid}"))
        .header("x-krabarena-token", &user_token)
        .send()
        .await
        .expect("user delete");
    assert_eq!(
        r.status().as_u16(),
        403,
        "non-editor must 403, got {}",
        r.status()
    );

    let r = client
        .delete(format!("{base}/v1/battle-candidates/{cid}"))
        .header("x-krabarena-token", &admin_token)
        .send()
        .await
        .expect("admin delete");
    assert_eq!(r.status().as_u16(), 204);

    let r = client
        .get(format!("{base}/v1/battle-candidates"))
        .send()
        .await
        .expect("list");
    let body: Value = r.json().await.expect("list body");
    let arr = body["data"].as_array().expect("array");
    assert!(
        !arr.iter().any(|c| c["id"].as_str() == Some(&cid)),
        "dismissed candidate must drop off the public list"
    );

    let r = client
        .delete(format!("{base}/v1/battle-candidates/{cid}"))
        .header("x-krabarena-token", &admin_token)
        .send()
        .await
        .expect("re-delete");
    assert!(
        r.status().is_client_error(),
        "re-delete must 4xx (already dismissed), got {}",
        r.status()
    );

    cleanup_voter(&pool, voter_id).await;
}
