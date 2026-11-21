//! Focused local test of the 0.8.13 dedup pipeline.
//!
//! Exercises the new pieces directly without going through the
//! claim post HTTP handler, so unrelated pre-filters (flooding,
//! review timeouts, GCS upload) don't mask whether the dedup logic
//! itself works:
//!
//!   1. Gemini embed_text returns a 768-dim vector
//!   2. pgvector ALTER TABLE landed (claim_embedding column)
//!   3. nearest_neighbors_topk retrieves seeded rows in cosine order
//!   4. dedup_judge::judge classifies (duplicate / improvement / new)
//!
//! Required env: TEST_DATABASE_URL + GEMINI_API_KEY.
//! Marked `#[ignore]` so `cargo test --workspace` skips it; run
//! explicitly with:
//!
//!   cargo test -p krabarena-api --test dedup_local -- --ignored --nocapture

use flate2::Compression;
use flate2::write::GzEncoder;
use krabarena_api::services::claim_service;
use krabarena_api::services::dedup_judge::{self, DedupVerdictKind};
use krabarena_api::services::gemini_service::{self, GeminiClient};
use reqwest::multipart;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use std::io::Write;
use uuid::Uuid;

const BATTLE_ID: &str = "09ab746f-52fd-44e2-9db7-71fa00e63bd7";
const SERVER_BASE: &str = "http://127.0.0.1:3105";

async fn pool_and_gemini() -> (sqlx::PgPool, GeminiClient) {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .expect("connect");
    let key = std::env::var("GEMINI_API_KEY").ok();
    (pool, GeminiClient::new(key))
}

fn battle_uuid() -> Uuid {
    Uuid::parse_str(BATTLE_ID).unwrap()
}

#[tokio::test]
#[ignore]
async fn embed_returns_768_dims() {
    let (_pool, gemini) = pool_and_gemini().await;
    let v = gemini
        .embed_text("Tested 4 search APIs on SimpleQA-50; Exa wins at 88%.", 768)
        .await
        .expect("embed");
    assert_eq!(v.len(), 768, "expected 768-dim embedding, got {}", v.len());
    // Sanity: not all zeros.
    assert!(v.iter().any(|x| x.abs() > 0.0));
}

#[tokio::test]
#[ignore]
async fn cosine_self_sim_equals_one() {
    // Pure math; doesn't need the network or DB.
    use gemini_service::cosine_similarity;
    let v: Vec<f32> = (0..768).map(|i| (i as f32) / 768.0).collect();
    let sim = cosine_similarity(&v, &v);
    assert!(
        (sim - 1.0).abs() < 1e-5,
        "self sim should be 1.0, got {sim}"
    );
}

#[tokio::test]
#[ignore]
async fn cosine_for_paraphrase_is_high_for_orthogonal_is_low() {
    let (_pool, gemini) = pool_and_gemini().await;
    let a = gemini
        .embed_text(
            "# Exa beats Tavily on SimpleQA-200\n\nExa scored 91.5%, Tavily 86%.",
            768,
        )
        .await
        .expect("embed a");
    let b_paraphrase = gemini
        .embed_text(
            "# On SimpleQA-200, Exa (91.5%) edges Tavily (86%)\n\nNew-config rerun.",
            768,
        )
        .await
        .expect("embed b");
    let c_unrelated = gemini
        .embed_text(
            "# Latency comparison: Brightdata 2.6s vs Keenable 720ms\n\nKeenable is 3.6x faster.",
            768,
        )
        .await
        .expect("embed c");

    use gemini_service::cosine_similarity;
    let sim_para = cosine_similarity(&a, &b_paraphrase);
    let sim_diff = cosine_similarity(&a, &c_unrelated);
    eprintln!("sim(a, paraphrase) = {sim_para:.4}");
    eprintln!("sim(a, unrelated)  = {sim_diff:.4}");
    assert!(
        sim_para > sim_diff,
        "paraphrase should be more similar than unrelated"
    );
    assert!(
        sim_para >= dedup_judge::SIMILARITY_LOWER_BOUND,
        "paraphrase sim {sim_para} should clear judge threshold {}",
        dedup_judge::SIMILARITY_LOWER_BOUND
    );
    assert!(
        sim_diff < dedup_judge::SIMILARITY_LOWER_BOUND,
        "unrelated sim {sim_diff} should be below judge threshold"
    );
}

/// End-to-end: seed two claims with embeddings on the throwaway battle,
/// then check that nearest_neighbors_topk + dedup_judge correctly
/// classify a new claim.
///
/// Cleans up after itself by deleting the seeded rows.
#[tokio::test]
#[ignore]
async fn dedup_pipeline_end_to_end() {
    let (pool, gemini) = pool_and_gemini().await;
    let battle = battle_uuid();

    let seed_text = "# Local dedup seed — SimpleQA-50: Exa 88%, Tavily 78%\n\
                     ## Summary\n\nN=50 random; Exa wins on factual recall.";
    let seed_embed = gemini.embed_text(seed_text, 768).await.expect("embed seed");
    let seed_vec = pgvector::Vector::from(seed_embed.clone());

    // Insert directly via SQL — we want to bypass the claim post
    // handler's full validation pipeline; here we just need a row
    // with an embedding to compare against.
    let test_admin_user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role) \
         VALUES ((SELECT id FROM entities LIMIT 1), 'Dedup Local Seeder', \
                 'dedup-local-seed@dedup.test', 'email', 'admin') \
         ON CONFLICT (email) DO UPDATE SET role = EXCLUDED.role \
         RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("seed user");

    let seed_id: Uuid = sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, \
                             artifact_size, slug, claim_embedding) \
         VALUES ($1, $2, $3, $4, 'http://test/dedup-seed', 1, 'local-dedup-seed-' || gen_random_uuid()::text, $5) \
         RETURNING id",
    )
    .bind(battle)
    .bind(test_admin_user_id)
    .bind(seed_text)
    .bind(format!("dedup-seed-{}", uuid::Uuid::new_v4()))
    .bind(&seed_vec)
    .fetch_one(&pool)
    .await
    .expect("insert seed claim");
    eprintln!("seeded claim {seed_id}");

    // -------- Case 1: nearly-identical text → should top the list with very high sim --------
    let new_text_dup = "# Local dedup seed — SimpleQA-50: Exa 88%, Tavily 78%\n\
                        ## Summary\n\nN=50 random; Exa wins on factual recall.";
    let dup_embed = gemini
        .embed_text(new_text_dup, 768)
        .await
        .expect("embed dup");
    let candidates_dup =
        claim_service::nearest_neighbors_topk(&pool, Some(battle), &dup_embed, 3, None)
            .await
            .expect("query dup");
    let top_dup = candidates_dup.first().expect("candidate exists");
    eprintln!(
        "[dup] top sim = {:.4}, id = {}",
        top_dup.similarity, top_dup.id
    );
    assert_eq!(top_dup.id, seed_id, "exact-match should retrieve the seed");
    assert!(
        top_dup.similarity >= dedup_judge::HARD_DUPLICATE_THRESHOLD,
        "exact-match sim {} should clear hard threshold {}",
        top_dup.similarity,
        dedup_judge::HARD_DUPLICATE_THRESHOLD
    );

    // -------- Case 2: paraphrase → should still match but in the borderline window --------
    let new_text_para = "# Dedup local — On SimpleQA-50, Exa hits 88% recall vs Tavily 78%\n\
                         ## Summary\n\nRandom 50-question subsample; Exa leads.";
    let para_embed = gemini
        .embed_text(new_text_para, 768)
        .await
        .expect("embed para");
    let candidates_para =
        claim_service::nearest_neighbors_topk(&pool, Some(battle), &para_embed, 3, None)
            .await
            .expect("query para");
    let top_para = candidates_para.first().expect("candidate exists");
    eprintln!(
        "[para] top sim = {:.4}, id = {}",
        top_para.similarity, top_para.id
    );
    assert_eq!(top_para.id, seed_id, "paraphrase should retrieve the seed");
    assert!(
        top_para.similarity >= dedup_judge::SIMILARITY_LOWER_BOUND,
        "paraphrase sim {} should clear LLM threshold {}",
        top_para.similarity,
        dedup_judge::SIMILARITY_LOWER_BOUND
    );

    // -------- Case 3: ask the LLM judge to classify the paraphrase --------
    if gemini.is_configured() {
        let verdict = dedup_judge::judge(&gemini, new_text_para, &candidates_para)
            .await
            .expect("judge");
        eprintln!(
            "[judge para] verdict = {:?}, related = {:?}, added_value = {:?}, rationale = {:?}",
            verdict.verdict,
            verdict.related_claim_id,
            verdict.added_value_summary,
            verdict.rationale
        );
        assert!(
            matches!(
                verdict.verdict,
                DedupVerdictKind::Duplicate | DedupVerdictKind::Improvement
            ),
            "paraphrase of identical numbers/conclusion should be duplicate or improvement, not new"
        );
    }

    // -------- Case 4: clearly different claim → low sim, judge should say `new` --------
    let new_text_diff = "# Latency: Brightdata 2.6s vs Keenable 720ms — Keenable is 3.6x faster\n\
                         ## Summary\n\nWall-clock per request, 200 calls.";
    let diff_embed = gemini
        .embed_text(new_text_diff, 768)
        .await
        .expect("embed diff");
    let candidates_diff =
        claim_service::nearest_neighbors_topk(&pool, Some(battle), &diff_embed, 3, None)
            .await
            .expect("query diff");
    let top_diff = candidates_diff.first().expect("candidate exists");
    eprintln!(
        "[diff] top sim = {:.4}, id = {}",
        top_diff.similarity, top_diff.id
    );
    // Don't assert the precise sim — pgvector across the whole battle
    // may pull a closer match than `seed_id` (older real claims with
    // overlapping vocabulary). Just verify it stayed below the hard
    // threshold for clearly-different content.
    assert!(
        top_diff.similarity < dedup_judge::HARD_DUPLICATE_THRESHOLD,
        "different claim should not hit hard dedup threshold (got {})",
        top_diff.similarity
    );

    // -------- Cleanup --------
    sqlx::query("DELETE FROM claims WHERE id = $1")
        .bind(seed_id)
        .execute(&pool)
        .await
        .expect("delete seed");
    eprintln!("cleaned up seed {seed_id}");
}

// ---------------------------------------------------------------------
// HTTP-level integration tests
// ---------------------------------------------------------------------
//
// These exercise the full claim post / delete handlers against a
// locally-running krabarena-api. They each pre-seed via SQL (so the
// flood pre-filter doesn't fire on multiple posts) then make EXACTLY
// ONE HTTP call per test before cleaning up.
//
// Pre-reqs (set in shell or via .env.test.local):
//   TEST_DATABASE_URL — Neon test branch URL
//   GEMINI_API_KEY    — for embedding + judge
// And a local server already running on 127.0.0.1:3105 with:
//   DATABASE_URL=$TEST_DATABASE_URL TEST_AUTH_SECRET=testtoken-12345
//   JWT_SECRET=testsecret-1234567890123456789012345678901234
//   BLOB_STORE_URL=http://localhost/blob
//
// Tests skip cleanly if the server isn't reachable.

const SERVER_TEST_AUTH_SECRET: &str = "testtoken-12345";

async fn server_alive() -> bool {
    reqwest::Client::new()
        .get(format!("{SERVER_BASE}/health"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn mint_admin_token() -> String {
    let url = format!("{SERVER_BASE}/v1/test/mint-token");
    let resp = reqwest::Client::new()
        .post(&url)
        .header("x-test-auth", SERVER_TEST_AUTH_SECRET)
        .json(&json!({ "user": "test-admin" }))
        .send()
        .await
        .expect("mint-token request");
    let status = resp.status();
    let body: Value = resp.json().await.expect("mint-token JSON");
    assert!(status.is_success(), "mint-token failed: {status} {body}");
    body["data"]["bearer_token"]
        .as_str()
        .expect("bearer_token in response")
        .to_string()
}

/// Build a minimal valid bundle (RUN.md + README.md + structure.json)
/// so the deterministic pre-filter doesn't reject for missing files.
/// The `jitter` byte changes the artifact hash — useful when we want
/// to bypass hash-based dedup but not the embedding check.
fn http_bundle(jitter: u64) -> Vec<u8> {
    let run_md = format!(
        "# Reproduction\n\n1. Set `KRAB_TOKEN`.\n2. python3 run.py\n\nRuntime ~30s. (jitter={jitter})\n"
    );
    let readme_md = "# Report\n\n\
        Smoke benchmark of provider X vs provider Y on a 50-question set. \
        Records p50/p95 wall-clock per request and aggregates accuracy via \
        an LLM judge.\n";
    let structure = serde_json::json!({
        "schema_version": 1,
        "benchmark": {
            "name": "Local Dedup Smoke",
            "task": "irrelevant",
            "dataset": "synthetic",
            "metric": "accuracy",
            "metric_unit": "%",
            "higher_is_better": true,
            "setup": "local dedup HTTP smoke test"
        },
        "leaderboard": [{"rank": 1, "solution": "A", "score": 88}],
        "winners": ["A"],
    })
    .to_string();
    let files: [(&str, &[u8]); 3] = [
        ("RUN.md", run_md.as_bytes()),
        ("README.md", readme_md.as_bytes()),
        ("structure.json", structure.as_bytes()),
    ];
    let mut tar_bytes = Vec::new();
    {
        let mut b = tar::Builder::new(&mut tar_bytes);
        for (path, bytes) in files.iter() {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, path, *bytes).expect("append");
        }
        b.finish().expect("finish tar");
    }
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&tar_bytes).expect("gz write");
    gz.finish().expect("gz finish")
}

async fn post_claim_http(
    token: &str,
    claim: &str,
    bundle: Vec<u8>,
    predecessor: Option<Uuid>,
) -> (reqwest::StatusCode, Value) {
    let url = format!("{SERVER_BASE}/v1/battles/{BATTLE_ID}/claims");
    let mut form = multipart::Form::new()
        .text("claim", claim.to_string())
        .text("ignore_review", "true")
        .text("agent_backend", "Claude Code")
        .text("agent_model", "claude-opus-4-7")
        .part(
            "artifact",
            multipart::Part::bytes(bundle)
                .file_name("bundle.tar.gz")
                .mime_str("application/gzip")
                .expect("mime"),
        );
    if let Some(pid) = predecessor {
        form = form.text("predecessor_id", pid.to_string());
    }
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap()
        .post(&url)
        .header("x-krabarena-token", token)
        .multipart(form)
        .send()
        .await
        .expect("post claim");
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .unwrap_or_else(|_| serde_json::json!({"_parse_failed": true}));
    (status, body)
}

async fn delete_claim_http(token: &str, claim_id: Uuid) -> (reqwest::StatusCode, Value) {
    let url = format!("{SERVER_BASE}/v1/claims/{claim_id}");
    let resp = reqwest::Client::new()
        .delete(&url)
        .header("x-krabarena-token", token)
        .send()
        .await
        .expect("delete claim");
    let status = resp.status();
    // 204 has no body; json() will fail.
    let body: Value = if status == reqwest::StatusCode::NO_CONTENT {
        serde_json::json!({})
    } else {
        resp.json()
            .await
            .unwrap_or_else(|_| serde_json::json!({"_parse_failed": true}))
    };
    (status, body)
}

/// SQL-seed a claim row by `test-admin` with a precomputed embedding.
/// Returns (claim_id, author_id). The returned author_id is the same
/// user `test-admin` corresponds to in the DB — minted-token requests
/// use this same row.
async fn seed_claim(pool: &sqlx::PgPool, gemini: &GeminiClient, text: &str) -> (Uuid, Uuid) {
    let embed = gemini.embed_text(text, 768).await.expect("embed seed");
    let v = pgvector::Vector::from(embed);
    let battle = battle_uuid();
    // Resolve the test-admin user — already created by the mint-token
    // path on the first call. If it doesn't exist yet, mint a token
    // first (in the calling test) then retry.
    // mint-token writes the test-admin row to this email — seed
    // helpers must be called AFTER `mint_admin_token()` so the row
    // exists.
    let user_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM users WHERE email = 'test-admin@test-e2e.krabarena.org'",
    )
    .fetch_one(pool)
    .await
    .expect("test-admin user not found — call mint_admin_token() first");
    let claim_id: Uuid = sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, \
                             artifact_size, slug, claim_embedding) \
         VALUES ($1, $2, $3, $4, 'http://test/seed', 1, \
                 'http-dedup-seed-' || gen_random_uuid()::text, $5) \
         RETURNING id",
    )
    .bind(battle)
    .bind(user_id)
    .bind(text)
    .bind(format!("http-seed-{}", Uuid::new_v4()))
    .bind(&v)
    .fetch_one(pool)
    .await
    .expect("seed claim insert");
    (claim_id, user_id)
}

async fn cleanup_claim(pool: &sqlx::PgPool, claim_id: Uuid) {
    let _ = sqlx::query("DELETE FROM claim_media WHERE claim_id = $1")
        .bind(claim_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM verifications WHERE claim_id = $1")
        .bind(claim_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM claims WHERE id = $1")
        .bind(claim_id)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore]
async fn http_post_dup_text_blocks_via_hard_threshold() {
    if !server_alive().await {
        eprintln!("SKIP: server not running on {SERVER_BASE}");
        return;
    }
    let (pool, gemini) = pool_and_gemini().await;
    let token = mint_admin_token().await;
    let seed_text =
        "# HTTP dedup test — provider X scores 88% on benchmark Y\n\n## Summary\n\nN=50.";
    let (seed_id, _author_id) = seed_claim(&pool, &gemini, seed_text).await;

    // POST byte-different bundle but identical claim text — sim ~1.00 →
    // hits HARD_DUPLICATE_THRESHOLD without LLM.
    let (status, body) = post_claim_http(&token, seed_text, http_bundle(7), None).await;
    eprintln!(
        "[dup-text] status = {status}, body code = {:?}",
        body["error"]["code"]
    );
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "expected 409, got {status}: {body}"
    );
    assert_eq!(
        body["error"]["code"], "SEMANTIC_DUPLICATE_CLAIM",
        "expected SEMANTIC_DUPLICATE_CLAIM"
    );
    assert_eq!(
        body["error"]["existing_claim_id"],
        seed_id.to_string(),
        "expected handler to point at seed claim"
    );

    cleanup_claim(&pool, seed_id).await;
    eprintln!("cleaned up seed {seed_id}");
}

#[tokio::test]
#[ignore]
async fn http_post_paraphrase_blocks_via_judge() {
    if !server_alive().await {
        eprintln!("SKIP: server not running on {SERVER_BASE}");
        return;
    }
    let (pool, gemini) = pool_and_gemini().await;
    let token = mint_admin_token().await;
    let seed_text = "# HTTP dedup paraphrase — provider X hits 91.5% recall on benchmark Y\n\n\
                     ## Summary\n\nN=200 random; X leads.";
    let (seed_id, _) = seed_claim(&pool, &gemini, seed_text).await;

    let paraphrase = "# HTTP test — On benchmark Y (n=200), provider X reached 91.5% recall\n\n\
                     ## Summary\n\nRandom 200-question subsample; X tops the table.";
    let (status, body) = post_claim_http(&token, paraphrase, http_bundle(8), None).await;
    eprintln!(
        "[paraphrase] status = {status}, code = {:?}, msg = {:?}",
        body["error"]["code"], body["error"]["message"]
    );
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "expected 409 (judge should block as duplicate or NEEDS_PREDECESSOR), got {status}: {body}"
    );
    let code = body["error"]["code"].as_str().unwrap_or("");
    assert!(
        code == "SEMANTIC_DUPLICATE_CLAIM" || code == "NEEDS_PREDECESSOR",
        "expected SEMANTIC_DUPLICATE_CLAIM or NEEDS_PREDECESSOR, got {code}"
    );

    cleanup_claim(&pool, seed_id).await;
    eprintln!("cleaned up seed {seed_id}");
}

#[tokio::test]
#[ignore]
async fn http_delete_succeeds_for_owner() {
    if !server_alive().await {
        eprintln!("SKIP: server not running on {SERVER_BASE}");
        return;
    }
    let (pool, gemini) = pool_and_gemini().await;
    let token = mint_admin_token().await;
    let (claim_id, _) = seed_claim(
        &pool,
        &gemini,
        "# HTTP delete test — temp claim by test-admin\n\n## Summary\n\nN/A.",
    )
    .await;

    let (status, body) = delete_claim_http(&token, claim_id).await;
    eprintln!("[delete owner] status = {status}, body = {body}");
    assert_eq!(
        status,
        reqwest::StatusCode::NO_CONTENT,
        "expected 204, got {status}: {body}"
    );

    // Verify the row is actually gone.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM claims WHERE id = $1)")
        .bind(claim_id)
        .fetch_one(&pool)
        .await
        .expect("exists check");
    assert!(!exists, "claim should be deleted from DB");
}

#[tokio::test]
#[ignore]
async fn http_delete_blocked_by_engagement() {
    if !server_alive().await {
        eprintln!("SKIP: server not running on {SERVER_BASE}");
        return;
    }
    let (pool, gemini) = pool_and_gemini().await;
    let token = mint_admin_token().await;
    let (claim_id, author_id) = seed_claim(
        &pool,
        &gemini,
        "# HTTP delete-blocked test — temp claim with engagement\n\n## Summary\n\nN/A.",
    )
    .await;

    // Add a fake verification row so the engagement guard fires. We
    // bind the same author as the verifier — the guard doesn't care
    // about who, just that *any* engagement exists.
    sqlx::query(
        "INSERT INTO verifications (claim_id, author_id, verdict, environment, output_hash) \
         VALUES ($1, $2, 'confirmed', 'dedup-test', 'sha256:dedup-test')",
    )
    .bind(claim_id)
    .bind(author_id)
    .execute(&pool)
    .await
    .expect("insert verification");

    let (status, body) = delete_claim_http(&token, claim_id).await;
    eprintln!(
        "[delete engagement] status = {status}, code = {:?}",
        body["error"]["code"]
    );
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "expected 409 (engagement guard), got {status}: {body}"
    );
    assert_eq!(
        body["error"]["code"], "CLAIM_HAS_ENGAGEMENT",
        "expected CLAIM_HAS_ENGAGEMENT code"
    );
    assert_eq!(body["error"]["verifications"], 1);

    cleanup_claim(&pool, claim_id).await;
    eprintln!("cleaned up seed {claim_id}");
}
