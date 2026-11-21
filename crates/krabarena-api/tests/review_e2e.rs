//! End-to-end tests for the Claim KrabReviewer dry-run path.
//!
//! These tests mint a bearer for the dedicated `test-agent` user (role =
//! `user`) via the gated `/v1/test/mint-token` endpoint, then exercise
//! `POST /v1/claims/review` with good and bad inputs. Dry-run only — no
//! battles, claims, or verifications are ever persisted, so test-agent
//! activity can't pollute the real leaderboard.
//!
//! They are marked `#[ignore]` so normal `cargo test` skips them. Run:
//!
//!   TEST_AUTH_SECRET=<secret> cargo test -p krabarena-api \
//!       --test review_e2e -- --ignored --nocapture
//!
//! Environment:
//!   TEST_AUTH_SECRET   Shared secret that must match the server's env.
//!                      Tests skip cleanly if unset so CI doesn't explode.
//!   KRAB_TEST_BASE     Optional base URL (default: https://krabarena.com).
//!
//! Assertion style: the KrabReviewer is Gemini Flash, so exact prose is
//! not stable. Assertions are regex-based against *signal families* the
//! reviewer is expected to flag (off-topic / low-effort / opinion /
//! structure / falsifiability). This survives model wording changes.

use regex::RegexBuilder;
use reqwest::multipart;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::io::Write;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://krabarena.com";
// Existing prod battle used as the context for claim reviews.
// "What is the best Web Access API for AI Agents?"
const TEST_BATTLE_ID: &str = "09ab746f-52fd-44e2-9db7-71fa00e63bd7";

fn base_url() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn test_auth_secret() -> Option<String> {
    std::env::var("TEST_AUTH_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
}

fn http() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .expect("build http client")
}

/// Build a minimal valid artifact tar.gz. Every artifact the KrabReviewer
/// sees must carry RUN.md + README.md at the root (enforced in
/// `review_service::check_required_artifact_files`) — without them the
/// pre-filter blocks the claim before Gemini is ever called, which makes
/// the E2E assertions about Gemini's reasoning unreachable.
///
/// Writing the helper was surprisingly delicate: the reviewer reads the
/// RUN.md / README.md contents as part of the evidence, so any text that
/// advertises "this is synthetic / test fixture / not a real benchmark"
/// trips Gemini's off-topic detector and taints tests that want to see
/// Gemini pass the bundle. Contents here are the neutral skeleton of a
/// real reproduction recipe + report — enough to satisfy the structure
/// requirement without biasing the review.
/// A valid `structure.json` for the latency benchmark the test bundles
/// describe. Required at the bundle root since 0.18.7 — without it the
/// deterministic pre-filter declines the claim before Gemini runs (see
/// `review_service::check_claim_structure_and_sections`).
const VALID_STRUCTURE_JSON: &[u8] = br#"{
    "schema_version": 1,
    "benchmark": {
        "name": "Proxy latency",
        "task": "End-to-end latency of factual queries through the KrabArena proxy",
        "dataset": "50 factual queries (bundled in the script)",
        "metric": "latency_p95_ms",
        "metric_unit": "ms",
        "higher_is_better": false,
        "setup": "Python 3.11, 50 queries, p50/p95/p99 over wall-clock per request, us-central1"
    },
    "leaderboard": [
        {"rank": 1, "solution": "KrabArena proxy", "score": 540.0}
    ],
    "winners": ["KrabArena proxy"]
}"#;

/// Low-level: gzip a tar of the given top-level files. Tests that need a
/// deliberately-incomplete bundle (missing or malformed `structure.json`)
/// call this directly; the happy-path [`bundle`] wraps it.
fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
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
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar_bytes).expect("gz write");
    gz.finish().expect("gz finish")
}

fn bundle(script_name: &str, script: &[u8]) -> Vec<u8> {
    let run_md = format!(
        "# Reproduction\n\n\
         1. Install Python 3.11+.\n\
         2. Set `PROXY_URL` and `BEARER_TOKEN` in the environment.\n\
         3. Run: `python3 {script_name}`.\n\n\
         Expected runtime: ~45 seconds.\n\
         Expected output: a JSON document on stdout with p50/p95/p99 keys.\n",
    );
    let readme_md = "# Report\n\n\
         The attached script measures end-to-end latency for a batch of \
         factual queries routed through the KrabArena proxy. It records \
         wall time per request and prints p50/p95/p99 at the end.\n\n\
         ## Inputs\n\n\
         - 50 factual queries (see `QUERIES` in the script).\n\
         - Bearer token supplied via environment.\n\n\
         ## Output\n\n\
         A single JSON line on stdout, e.g.:\n\n\
         ```\n\
         {\"p50\": 310, \"p95\": 540, \"p99\": 780}\n\
         ```\n";
    tar_gz(&[
        ("RUN.md", run_md.as_bytes()),
        ("README.md", readme_md.as_bytes()),
        ("structure.json", VALID_STRUCTURE_JSON),
        (script_name, script),
    ])
}

/// Mint a bearer for the named test user via the gated endpoint. Returns
/// `None` and prints a SKIP reason if TEST_AUTH_SECRET isn't set locally
/// or the server doesn't have the feature enabled.
async fn mint_token(user: &str) -> Option<String> {
    let Some(secret) = test_auth_secret() else {
        eprintln!("SKIP: TEST_AUTH_SECRET not set");
        return None;
    };
    let url = format!("{}/v1/test/mint-token", base_url());
    let resp = http()
        .post(&url)
        .header("x-test-auth", secret)
        .json(&json!({ "user": user }))
        .send()
        .await
        .expect("mint-token request failed");
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        eprintln!("SKIP: /v1/test/mint-token returned 404 — feature disabled on server?");
        return None;
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        panic!("mint-token unexpected status {status}: {body}");
    }
    let body: Value = resp.json().await.expect("mint-token returned non-JSON");
    body["data"]["bearer_token"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| {
            panic!("mint-token response missing data.bearer_token: {body}");
        })
}

/// Concatenate every suggestion.message / moderation_flag.reason / summary
/// string in a review into one haystack so regex assertions don't have to
/// walk the structure.
fn review_haystack(review: &Value) -> String {
    let mut out = String::new();
    if let Some(s) = review["summary"].as_str() {
        out.push_str(s);
        out.push('\n');
    }
    if let Some(arr) = review["suggestions"].as_array() {
        for s in arr {
            for key in ["message", "category", "id"] {
                if let Some(v) = s[key].as_str() {
                    out.push_str(v);
                    out.push('\n');
                }
            }
        }
    }
    if let Some(arr) = review["moderation_flags"].as_array() {
        for f in arr {
            for key in ["reason", "code"] {
                if let Some(v) = f[key].as_str() {
                    out.push_str(v);
                    out.push('\n');
                }
            }
        }
    }
    out
}

fn matches(haystack: &str, pattern: &str) -> bool {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .expect("valid regex")
        .is_match(haystack)
}

async fn claim_review(
    battle_id: &str,
    claim: &str,
    artifact: &[u8],
    bearer: &str,
) -> (StatusCode, Value) {
    let form = multipart::Form::new()
        .text("battle_id", battle_id.to_string())
        .text("claim", claim.to_string())
        .part(
            "artifact",
            multipart::Part::bytes(artifact.to_vec()).file_name("artifact.tar.gz"),
        );

    let url = format!("{}/v1/claims/review", base_url());
    let resp = http()
        .post(&url)
        .bearer_auth(bearer)
        .multipart(form)
        .send()
        .await
        .expect("claim review request");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

// ---------------------------------------------------------------------------
// Bad claim — self-admitted non-answer. Must be blocked.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn claim_review_bad_claim_blocked_with_on_topic_reasoning() {
    let Some(bearer) = mint_token("test-agent").await else {
        return;
    };

    let bad_claim = "# Synthetic test claim\n\n\
        ## Summary\n\
        This claim explicitly does not answer the Battle's question. It is a\n\
        meta-test of the KrabArena CLI review flow and should be rejected.\n\n\
        ## Methodology\n\
        None — the artifact just echoes a string.\n\n\
        ## Conclusion\n\
        No substantive conclusion.\n";
    let bad_artifact = bundle("artifact.sh", b"#!/bin/sh\necho test\n");

    let (status, body) = claim_review(TEST_BATTLE_ID, bad_claim, &bad_artifact, &bearer).await;
    assert!(
        status.is_success(),
        "claim review unexpected status {status}: {body}"
    );

    let review = &body["data"];
    let verdict = review["verdict"].as_str().unwrap_or("");
    assert_eq!(
        verdict, "block",
        "expected bad claim to be blocked, got verdict='{verdict}' in {body}"
    );

    let suggestions = review["suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let flags = review["moderation_flags"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !suggestions.is_empty() || !flags.is_empty(),
        "block verdict must come with suggestions or moderation flags: {body}"
    );

    let hay = review_haystack(review);
    let pattern = r"off.?topic|low.?effort|shallow|opinion|trivial|synthetic|meta.?test|does\s+not\s+answer|platform|substantive";
    assert!(
        matches(&hay, pattern),
        "block reasoning did not reference any expected signal. haystack=\n{hay}"
    );
}

// ---------------------------------------------------------------------------
// Unstructured claim — missing the mandated sections. At least warn.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn claim_review_missing_sections_is_warned_or_blocked() {
    let Some(bearer) = mint_token("test-agent").await else {
        return;
    };

    let unstructured = "Tavily returns results quickly, maybe under 500ms, \
        based on some local tests I ran. Seems fine for most agent use cases.";
    let artifact = bundle(
        "artifact.sh",
        b"#!/bin/sh\ncurl -s https://api.tavily.com/\n",
    );

    let (status, body) = claim_review(TEST_BATTLE_ID, unstructured, &artifact, &bearer).await;
    assert!(
        status.is_success(),
        "claim review unexpected status {status}: {body}"
    );

    let review = &body["data"];
    let verdict = review["verdict"].as_str().unwrap_or("");
    assert!(
        matches!(verdict, "warn" | "block"),
        "unstructured claim should be warn or block, got '{verdict}' in {body}"
    );

    // The reviewer may focus on structure OR falsifiability/effort — both
    // are valid catches of what's wrong with an unstructured one-liner.
    let hay = review_haystack(review);
    let pattern = r"summary|methodology|conclusion|structure|section|heading|format|title|falsifi|opinion|vague|specific|low.?effort|detail";
    assert!(
        matches(&hay, pattern),
        "unstructured-claim feedback didn't reference any expected signal. haystack=\n{hay}"
    );
}

// ---------------------------------------------------------------------------
// Good claim — well-structured, on-topic, falsifiable. Must not be blocked.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn claim_review_good_claim_not_blocked() {
    let Some(bearer) = mint_token("test-agent").await else {
        return;
    };

    let good_claim = "# Keenable web search returns p95 under 600ms for factual queries\n\n\
        ## Summary\n\
        On a 50-query factual benchmark routed through the KrabArena proxy, \
        Keenable's web search API returns results with p95 end-to-end latency \
        under 600ms. This puts it inside the budget for agent retrieval loops.\n\n\
        ## Methodology\n\
        The attached Python script reads 50 factual queries drawn from the \
        TriviaQA held-out split, issues each to \
        `/v1/battles/{battle_id}/proxy/keenable/v1/search` via the KrabArena \
        proxy using a provided bearer token, and records `time_total` from \
        each request. It prints p50, p95, p99 at the end. The claim is \
        considered verified if p95 is under 600ms across the 50 requests \
        from us-central1.\n\n\
        ## Conclusion\n\
        Across 50 runs p95 was 540ms, p50 was 310ms, p99 was 780ms. This \
        claim is falsified if p95 exceeds 600ms on the same query set from \
        the same region, or if a named competitor (Tavily, Exa, SerpAPI, \
        Firecrawl) demonstrates a lower p95 on the same queries.\n";

    let good_artifact = br#"#!/usr/bin/env python3
"""Measure p95 latency of Keenable web search via the KrabArena proxy.

Reads PROXY_URL and BEARER_TOKEN from the environment. Runs each query in
QUERIES, records the end-to-end wall time, and prints p50/p95/p99.
"""
import os, time, statistics, urllib.parse, urllib.request, json

QUERIES = [
    "capital of France", "speed of light in km per second", "who wrote Hamlet",
    "population of Tokyo", "melting point of iron", "longest river in Asia",
    "inventor of the telephone", "atomic number of gold", "year Apollo 11 landed",
    "tallest mountain on Mars",
] * 5  # 50 queries

def run(q: str) -> float:
    url = os.environ["PROXY_URL"] + "?q=" + urllib.parse.quote(q)
    req = urllib.request.Request(url, headers={
        "Authorization": "Bearer " + os.environ["BEARER_TOKEN"],
    })
    start = time.perf_counter()
    urllib.request.urlopen(req, timeout=5).read()
    return (time.perf_counter() - start) * 1000

times = [run(q) for q in QUERIES]
qs = statistics.quantiles(times, n=100)
print(json.dumps({"p50": qs[49], "p95": qs[94], "p99": qs[98]}))
"#;

    let good_artifact = bundle("artifact.py", good_artifact);
    let (status, body) = claim_review(TEST_BATTLE_ID, good_claim, &good_artifact, &bearer).await;
    assert!(
        status.is_success(),
        "claim review unexpected status {status}: {body}"
    );

    let review = &body["data"];
    let verdict = review["verdict"].as_str().unwrap_or("");
    assert_ne!(
        verdict,
        "block",
        "well-structured on-topic claim was blocked. review:\n{}",
        serde_json::to_string_pretty(review).unwrap_or_default()
    );
    assert!(
        matches!(verdict, "pass" | "warn"),
        "verdict must be pass or warn for a good claim, got '{verdict}' in {body}"
    );
}

// ---------------------------------------------------------------------------
// Structure gate (0.18.7) — a well-sectioned claim whose bundle ships NO
// (or an invalid) structure.json trips the deterministic pre-filter, before
// Gemini. The verdict is an OVERRIDABLE `warn` (a publisher who passes
// --ignore-review can still ship; KrabScout's shim strips that flag, so the
// bot stays strict). Regression guard for the "junk Claims publish" gap:
// form is a deterministic warn, not a soft one the LLM could miss.
// ---------------------------------------------------------------------------

/// A claim body with all four required sections, so the section gate is
/// satisfied and only the structure.json gate can fire.
const WELL_SECTIONED_CLAIM: &str = "# Proxy p95 stays under 600ms\n\n\
    ## Summary\nThe proxy returns p95 under 600ms on 50 factual queries.\n\n\
    ## Methodology\nRan the bundled script against the proxy, 50 queries, \
    recorded p50/p95/p99.\n\n\
    ## Conclusion\np95 was 540ms; falsified if it exceeds 600ms on the same set.\n";

#[tokio::test]
#[ignore]
async fn claim_review_missing_structure_json_warns() {
    let Some(bearer) = mint_token("test-agent").await else {
        return;
    };

    // RUN.md + README.md present, body fully sectioned — but NO
    // structure.json in the bundle and none inline.
    let artifact = tar_gz(&[
        ("RUN.md", b"# Reproduction\n\n1. Run `python3 run.py`.\n"),
        (
            "README.md",
            b"# Report\n\nFull latency report with per-query rows.\n",
        ),
        ("run.py", b"print('{\"p95\": 540}')\n"),
    ]);

    let (status, body) =
        claim_review(TEST_BATTLE_ID, WELL_SECTIONED_CLAIM, &artifact, &bearer).await;
    assert!(
        status.is_success(),
        "claim review unexpected status {status}: {body}"
    );

    let review = &body["data"];
    assert_eq!(
        review["verdict"].as_str().unwrap_or(""),
        "warn",
        "missing structure.json is an overridable warn at the pre-filter. review:\n{}",
        serde_json::to_string_pretty(review).unwrap_or_default()
    );
    let hay = review_haystack(review);
    assert!(
        matches(&hay, "structure"),
        "missing-structure warn should reference structure.json. haystack=\n{hay}"
    );
}

#[tokio::test]
#[ignore]
async fn claim_review_invalid_structure_json_warns() {
    let Some(bearer) = mint_token("test-agent").await else {
        return;
    };

    // structure.json present but fails the schema (empty leaderboard,
    // no winners, missing benchmark) → field-rule decline.
    let artifact = tar_gz(&[
        ("RUN.md", b"# Reproduction\n\n1. Run `python3 run.py`.\n"),
        (
            "README.md",
            b"# Report\n\nFull latency report with per-query rows.\n",
        ),
        (
            "structure.json",
            br#"{"schema_version":1,"leaderboard":[]}"#,
        ),
        ("run.py", b"print('{\"p95\": 540}')\n"),
    ]);

    let (status, body) =
        claim_review(TEST_BATTLE_ID, WELL_SECTIONED_CLAIM, &artifact, &bearer).await;
    assert!(
        status.is_success(),
        "claim review unexpected status {status}: {body}"
    );

    let review = &body["data"];
    assert_eq!(
        review["verdict"].as_str().unwrap_or(""),
        "warn",
        "invalid structure.json is an overridable warn at the pre-filter. review:\n{}",
        serde_json::to_string_pretty(review).unwrap_or_default()
    );
    let hay = review_haystack(review);
    assert!(
        matches(&hay, "leaderboard|structure|winner"),
        "invalid-structure warn should name the failing field. haystack=\n{hay}"
    );
}

// ---------------------------------------------------------------------------
// Text length guard — the route must reject a claim over 5000 chars
// *before* running Gemini. Regression: previously the length check was
// inside claim_service::create_claim, several layers down, so a
// too-long claim on `claim post` still ran the GCS upload and
// persisted a claim_reviews row on the way there. Now both endpoints
// (/v1/claims/review and /v1/battles/{id}/claims) short-circuit at
// the route boundary.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn claim_review_rejects_over_5000_chars_fast() {
    let Some(bearer) = mint_token("test-agent").await else {
        return;
    };

    // 5001 chars. We don't care about shape — just that it exceeds.
    let over = "a".repeat(5001);
    let started = std::time::Instant::now();
    let (status, body) =
        claim_review(TEST_BATTLE_ID, &over, b"#!/bin/sh\necho hi\n", &bearer).await;
    let elapsed = started.elapsed();

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "over-length claim must be 400, got {status}: {body}"
    );
    // v0.11.0 (PR #133) replaced the generic VALIDATION_ERROR with the
    // specific FIELD_TOO_LONG / FIELD_TOO_SHORT codes so clients can
    // surface a helpful message without parsing free-form text. The
    // payload still carries the field name and limit in `details`.
    let code = body["error"]["code"].as_str().unwrap_or("");
    assert_eq!(code, "FIELD_TOO_LONG", "unexpected error code: {body}");
    assert_eq!(
        body["error"]["details"]["field"].as_str().unwrap_or(""),
        "claim",
        "FIELD_TOO_LONG must name the offending field: {body}"
    );
    assert_eq!(
        body["error"]["details"]["limit_chars"]
            .as_i64()
            .unwrap_or(0),
        5000,
        "FIELD_TOO_LONG must carry the limit: {body}"
    );

    // Must be fast — a Gemini round-trip is on the order of 5-30s, so
    // anything under 2s proves the reviewer did not run. Generous
    // bound to tolerate cold-start latency on Cloud Run.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "length rejection took {elapsed:?}; this path should short-circuit before Gemini"
    );
}

#[tokio::test]
#[ignore]
async fn claim_review_rejects_empty_claim() {
    let Some(bearer) = mint_token("test-agent").await else {
        return;
    };
    let (status, body) = claim_review(TEST_BATTLE_ID, "", b"#!/bin/sh\necho hi\n", &bearer).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "empty claim must be 400, got {status}: {body}"
    );
    // v0.11.0 (PR #133): empty body now returns FIELD_TOO_SHORT instead
    // of the generic VALIDATION_ERROR — same shape as the over-length
    // test above, just on the lower bound.
    let code = body["error"]["code"].as_str().unwrap_or("");
    assert_eq!(code, "FIELD_TOO_SHORT", "unexpected error code: {body}");
    assert_eq!(
        body["error"]["details"]["field"].as_str().unwrap_or(""),
        "claim",
        "FIELD_TOO_SHORT must name the offending field: {body}"
    );
}

// ---------------------------------------------------------------------------
// Battle edit — PATCH must not re-validate fields the caller didn't touch.
//
// Regression: initial `status` allowlist guard fired on `row.status` even
// when `req.status.is_none()`, so a battle the system had transitioned to
// `contested` (by a Claim landing on it) could never be edited via PATCH
// for anything else (e.g. rotating `proxy_spec`). Plus the initial
// allowlist itself was wrong — invented values that aren't in the DB
// CHECK constraint. The fix: only validate `status` when the PATCH
// actually provides it, and align the allowlist with `holes_status_check
// = open|contested|holding|settled`.
// ---------------------------------------------------------------------------

async fn create_test_battle(bearer: &str) -> Option<String> {
    let url = format!("{}/v1/battles", base_url());
    let body = json!({
        "title": "e2e test battle — edit regression guard",
        "description": "Synthetic battle used to exercise the battle PATCH path. \
            Safe to leave around; pollutes only test-agent's leaderboard, which \
            lives on the test-e2e Neon branch.",
        "domain_tags": ["e2e-test"],
    });
    let resp = http()
        .post(&url)
        .header("authorization", format!("Bearer {}", bearer))
        .json(&body)
        .send()
        .await
        .expect("create battle");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        eprintln!("SKIP: could not create test battle ({status}): {body}");
        return None;
    }
    body["data"]["id"].as_str().map(String::from)
}

#[tokio::test]
#[ignore]
async fn battle_patch_without_status_does_not_revalidate_existing_status() {
    let Some(bearer) = mint_token("test-admin").await else {
        return;
    };
    let Some(battle_id) = create_test_battle(&bearer).await else {
        return;
    };

    // Transition via explicit PATCH — admin can set it to `active`.
    let patch_url = format!("{}/v1/battles/{}", base_url(), battle_id);
    let resp = http()
        .patch(&patch_url)
        .header("authorization", format!("Bearer {}", bearer))
        .json(&json!({ "status": "active" }))
        .send()
        .await
        .expect("patch status");
    assert!(
        resp.status().is_success(),
        "set-status PATCH should succeed on a valid transition, got {}",
        resp.status()
    );

    // The key assertion: a PATCH that changes only `proxy_spec` must
    // succeed even though the stored `status` is `active`. The
    // pre-fix code would 400 here with "status must be one of".
    //
    // The proxy_spec value has to be a *valid* one — the write-time
    // validator (added in v0.4.2) rejects empty-services specs, so
    // we pick a trivial but shape-correct one here. This test is
    // about status revalidation, not proxy_spec shape; the proxy
    // shape tests live in `models::proxy::tests` as unit tests.
    let resp = http()
        .patch(&patch_url)
        .header("authorization", format!("Bearer {}", bearer))
        .json(&json!({
            "proxy_spec": {
                "services": {
                    "echo": {
                        "upstream": "https://example.com",
                        "docs_url": "https://example.com/docs"
                    }
                },
                "budget_cents": 5000
            }
        }))
        .send()
        .await
        .expect("patch proxy_spec");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    assert!(
        status.is_success(),
        "PATCH with only proxy_spec must not re-validate stored status; got {status}: {body}"
    );
}

#[tokio::test]
#[ignore]
async fn battle_patch_rejects_invalid_status_when_provided() {
    let Some(bearer) = mint_token("test-admin").await else {
        return;
    };
    let Some(battle_id) = create_test_battle(&bearer).await else {
        return;
    };
    let patch_url = format!("{}/v1/battles/{}", base_url(), battle_id);
    let resp = http()
        .patch(&patch_url)
        .header("authorization", format!("Bearer {}", bearer))
        .json(&json!({ "status": "invalid-state" }))
        .send()
        .await
        .expect("patch invalid status");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let msg = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("status must be one of"),
        "expected allowlist-hint error, got {msg}"
    );
    assert!(
        msg.contains("active"),
        "error message should document the allowlist, got {msg}"
    );
}
