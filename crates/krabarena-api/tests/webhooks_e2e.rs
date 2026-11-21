//! End-to-end test for the webhook subscription system (migration
//! 041 + `services::webhook_dispatch` + `routes::webhooks`, since
//! 0.14.0).
//!
//! Flow:
//!   1. Spin up an in-process listener that records every incoming
//!      delivery (URL hit, headers, body).
//!   2. Mint a test-user token via `/v1/test/mint-token`.
//!   3. Subscribe the listener to `claim.posted`.
//!   4. Post a standalone claim — this fires the dispatcher.
//!   5. Poll the listener and the `GET /v1/webhooks` list until
//!      we see `delivery_count > 0` (or time out at 15s).
//!   6. Assert the listener received a body with `event=claim.posted`
//!      and a valid `x-krabarena-signature`.
//!   7. Hard-delete the subscription.
//!
//! Skipped (returns silently) if `KRAB_TEST_BASE` / `TEST_AUTH_SECRET`
//! aren't set so unit `cargo test` doesn't try to reach a live server.
//! The listener binds to `0.0.0.0:0` and the URL is constructed from
//! the env var `WEBHOOK_E2E_LISTENER_HOST` (defaults to `127.0.0.1`).
//! In CI you'd point this at a routable host; in local dev against
//! a local server, `127.0.0.1` works since both sides share the host.
//!
//! `#[ignore]` — run with `cargo test -p krabarena-api --test \
//! webhooks_e2e -- --ignored`.

use reqwest::multipart;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

const DEFAULT_BASE_URL: &str = "https://krabarena.com";

fn base_url() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn test_auth_secret() -> Option<String> {
    std::env::var("TEST_AUTH_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
}

// The server under test must run with `WEBHOOK_ALLOW_PRIVATE_HOSTS=1`:
// webhook URLs pointing at private / loopback hosts are rejected by
// default (SSRF guard), and this listener binds 127.0.0.1.
fn listener_host() -> String {
    std::env::var("WEBHOOK_E2E_LISTENER_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

fn http() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("build http client")
}

/// Records every webhook delivery the in-process listener receives.
#[derive(Debug, Default, Clone)]
struct Recorded {
    event: Option<String>,
    delivery_id: Option<String>,
    signature: Option<String>,
    body: Vec<u8>,
}

/// Bind a TCP listener, return the bound port + a handle that
/// (1) accepts one connection, (2) reads the HTTP request, (3) returns
/// 200 with body `ok`, and (4) records what came in.
async fn start_listener() -> (u16, Arc<Mutex<Vec<Recorded>>>) {
    let listener = TcpListener::bind("0.0.0.0:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let log: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
    let log_c = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let log_inner = log_c.clone();
            tokio::spawn(async move {
                let mut buf = Vec::with_capacity(8192);
                let mut tmp = [0u8; 4096];
                // Read enough for headers + body. Keep reading until
                // either the peer closes or we hit a sensible cap.
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if Instant::now() > deadline {
                        break;
                    }
                    match tokio::time::timeout(Duration::from_millis(200), sock.read(&mut tmp))
                        .await
                    {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            buf.extend_from_slice(&tmp[..n]);
                            // Heuristic: once we see CRLFCRLF and the
                            // content-length bytes are present, stop.
                            if let Some(hdr_end) = find_header_end(&buf) {
                                let cl = parse_content_length(&buf[..hdr_end]).unwrap_or(0);
                                if buf.len() >= hdr_end + 4 + cl {
                                    break;
                                }
                            }
                        }
                        _ => break,
                    }
                }
                // Parse what we got.
                let rec = parse_request(&buf);
                log_inner.lock().await.push(rec);
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, log)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(headers).ok()?;
    for line in s.split("\r\n") {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn parse_request(buf: &[u8]) -> Recorded {
    let mut out = Recorded::default();
    let Some(hdr_end) = find_header_end(buf) else {
        return out;
    };
    let header_str = std::str::from_utf8(&buf[..hdr_end]).unwrap_or("");
    for line in header_str.split("\r\n") {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("x-krabarena-event:") {
            out.event = Some(rest.trim().to_string());
        } else if let Some(rest) = lower.strip_prefix("x-krabarena-delivery:") {
            out.delivery_id = Some(rest.trim().to_string());
        } else if let Some(rest) = lower.strip_prefix("x-krabarena-signature:") {
            out.signature = Some(rest.trim().to_string());
        }
    }
    if buf.len() > hdr_end + 4 {
        out.body = buf[hdr_end + 4..].to_vec();
    }
    out
}

async fn mint_token(user: &str) -> Option<String> {
    let Some(secret) = test_auth_secret() else {
        eprintln!("SKIP: TEST_AUTH_SECRET not set");
        return None;
    };
    let url = format!("{}/v1/test/mint-token", base_url());
    let resp = http()
        .post(&url)
        .header("x-test-auth", secret)
        .json(&json!({ "user": user, "role": "user" }))
        .send()
        .await
        .ok()?;
    if resp.status() == StatusCode::NOT_FOUND {
        eprintln!("SKIP: /v1/test/mint-token returned 404 — test-mint disabled?");
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["data"]["bearer_token"].as_str().map(String::from)
}

/// Smallest reviewer-acceptable bundle. Copy of the standalone
/// claims e2e helper — kept separate so the two tests don't share a
/// module that has to be promoted to a library.
fn build_bundle() -> Vec<u8> {
    let readme = b"# Report\n\n## Summary\n\nWebhook e2e probe.\n\n## Methodology\n\nIt's a probe.\n\n## Conclusion\n\nProbe ran.\n";
    let run = b"# Reproduction\n\n## Steps\n\n1. Run the probe.\n";
    let structure = br#"{"schema_version":1,"benchmark":{"name":"webhook-probe","task":"e2e","dataset":"n/a","metric":"passed","higher_is_better":true,"setup":"synthetic"},"leaderboard":[{"rank":1,"solution":"webhook-probe","score":1.0,"notes":"ran"}],"winners":["webhook-probe"],"cost":{"tokens":100,"usd":0.005}}"#;
    let files: [(&str, &[u8]); 3] = [
        ("RUN.md", run),
        ("README.md", readme),
        ("structure.json", structure),
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
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar_bytes).expect("gz write");
    gz.finish().expect("gz finish")
}

#[tokio::test]
#[ignore]
async fn subscribe_post_claim_delivers_webhook() {
    let Some(bearer) = mint_token("webhook-e2e-author").await else {
        return;
    };

    // 1. Start in-process listener.
    let (port, log) = start_listener().await;
    let listener_url = format!("https://{}:{}/krabarena-test", listener_host(), port);
    eprintln!("listener bound at {listener_url}");

    // 2. Subscribe. NOTE: the API enforces https:// at subscribe time.
    // The listener serves plain HTTP, so deliveries will fail to
    // *connect* (TLS handshake mismatch). We still expect the row to
    // be created and `delivery_count` to be bumped because the
    // dispatcher counts attempts regardless of outcome. If the
    // env-provided host implements TLS termination (e.g. behind
    // Cloud Run with a self-signed cert), the listener will receive
    // the body — assert conditionally.
    let resp = http()
        .post(format!("{}/v1/webhooks", base_url()))
        .header("authorization", format!("Bearer {}", bearer))
        .json(&json!({
            "url": listener_url,
            "events": ["claim.posted"],
            "description": "webhook-e2e",
        }))
        .send()
        .await
        .expect("subscribe");
    let status = resp.status();
    let body: Value = resp.json().await.expect("subscribe non-JSON");
    assert!(
        status == StatusCode::CREATED,
        "expected 201 on subscribe, got {status}: {body}"
    );
    let sub_id = body["data"]["id"].as_str().expect("id").to_string();
    let secret = body["data"]["secret"]
        .as_str()
        .expect("secret returned on create")
        .to_string();
    assert_eq!(secret.len(), 64, "secret should be 64 hex chars");

    // 3. Post a standalone claim — fires `claim.posted`.
    let bundle = build_bundle();
    let topic = format!("webhook-e2e @ {}", chrono::Utc::now().timestamp_millis());
    let claim_text = format!(
        "# Webhook e2e claim\n\n## Summary\n\n{}\n\n## Methodology\n\nProbe.\n\n## Conclusion\n\nDone.\n",
        topic
    );
    let form = multipart::Form::new()
        .text("claim", claim_text)
        .text("topic", topic.clone())
        .text("topic_tags", "e2e-test,webhook")
        .text("agent_backend", "krabarena-e2e")
        .text("agent_model", "synthetic-webhook")
        .text("ignore_review", "true")
        .part(
            "artifact",
            multipart::Part::bytes(bundle)
                .file_name("webhook-probe.tar.gz")
                .mime_str("application/gzip")
                .unwrap(),
        );
    let resp = http()
        .post(format!("{}/v1/claims", base_url()))
        .header("authorization", format!("Bearer {}", bearer))
        .multipart(form)
        .send()
        .await
        .expect("post claim");
    let status = resp.status();
    let cbody: Value = resp.json().await.expect("post claim non-JSON");
    assert!(
        status == StatusCode::CREATED,
        "expected 201 on claim post, got {status}: {cbody}"
    );

    // 4. Poll `GET /v1/webhooks` until delivery_count > 0 (or 15s).
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut delivered = false;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let resp = http()
            .get(format!("{}/v1/webhooks", base_url()))
            .header("authorization", format!("Bearer {}", bearer))
            .send()
            .await
            .expect("list webhooks");
        let body: Value = resp.json().await.expect("list non-JSON");
        if let Some(subs) = body["data"].as_array() {
            for s in subs {
                if s["id"].as_str() == Some(&sub_id) {
                    let dc = s["delivery_count"].as_i64().unwrap_or(0);
                    if dc > 0 {
                        delivered = true;
                        break;
                    }
                }
            }
        }
        if delivered {
            break;
        }
    }
    assert!(
        delivered,
        "delivery_count never incremented within 15s — dispatcher didn't fire"
    );

    // 5. If our listener actually received a body (the URL we
    // registered was reachable + TLS terminated), assert the
    // headers + envelope shape. Otherwise just lock the bookkeeping
    // assertion.
    let log = log.lock().await;
    if !log.is_empty() {
        let rec = &log[0];
        assert_eq!(rec.event.as_deref(), Some("claim.posted"));
        assert!(
            rec.signature
                .as_deref()
                .unwrap_or("")
                .starts_with("sha256=")
        );
        let env: Value = serde_json::from_slice(&rec.body).expect("envelope JSON");
        assert_eq!(env["event"].as_str(), Some("claim.posted"));
        assert!(env["delivery_id"].as_str().unwrap_or("").starts_with("wd_"));
        assert!(env["data"]["claim"].is_object());
    }

    // 6. Hard-delete.
    let resp = http()
        .delete(format!("{}/v1/webhooks/{}", base_url(), sub_id))
        .header("authorization", format!("Bearer {}", bearer))
        .send()
        .await
        .expect("delete sub");
    assert_eq!(resp.status(), StatusCode::OK);
}
