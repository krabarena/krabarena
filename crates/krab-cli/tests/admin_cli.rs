use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use serde_json::{Value, json};

fn unique_home(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("krab-admin-cli-{label}-{nanos}"))
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

fn jwt_with_role(role: &str) -> String {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"HS256"}"#);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        format!(
            r#"{{"role":"{role}","sub":"00000000-0000-0000-0000-000000000001","iat":{},"exp":{}}}"#,
            unix_timestamp(),
            unix_timestamp() + 3600
        )
        .as_bytes(),
    );
    format!("{header}.{payload}.signature-not-checked")
}

fn write_config(home: &PathBuf, api_url: &str, token: &str) {
    let config_dir = home.join(".krab");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "api_url = \"{api_url}\"\nbearer_token = \"{token}\"\n\n[telemetry]\nshare = false\n"
        ),
    )
    .unwrap();
}

fn spawn_capture_mock(status: u16, body: Value) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let api_url = format!("http://{addr}");
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        panic!("mock HTTP server timed out waiting for request");
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("mock HTTP server accept failed: {err}"),
            }
        };

        let mut buffer = Vec::new();
        let mut tmp = [0_u8; 1024];
        loop {
            let n = stream.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&tmp[..n]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let header_end = buffer
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|pos| pos + 4)
            .unwrap();
        let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                (name.eq_ignore_ascii_case("content-length"))
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buffer.len() - header_end < content_length {
            let n = stream.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&tmp[..n]);
        }

        let request = String::from_utf8_lossy(&buffer).to_string();
        let body = serde_json::to_string(&body).unwrap();
        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Krab-Server-Version: {}\r\n\r\n{body}",
            body.len(),
            env!("CARGO_PKG_VERSION")
        );
        stream.write_all(response.as_bytes()).unwrap();
        request
    });
    (api_url, handle)
}

#[test]
fn reserve_admin_claim_delete_cli_sends_reason_body() {
    let home = unique_home("claim-delete");
    let token = jwt_with_role("admin");
    let (api_url, handle) = spawn_capture_mock(
        200,
        json!({
            "data": {
                "claim_id": "claim-1",
                "deleted": true
            },
            "meta": { "request_id": "mock-admin-cli", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &api_url, &token);

    let output = Command::new(env!("CARGO_BIN_EXE_krab"))
        .env("HOME", &home)
        .env("KRAB_NO_AUTO_UPDATE", "1")
        .args([
            "--format",
            "json",
            "admin",
            "claim",
            "delete",
            "https://krabarena.com/claims/claim-1?x=1#top",
            "--reason",
            "moderation cleanup",
        ])
        .output()
        .unwrap();
    let request = handle.join().unwrap();

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(request.starts_with("DELETE /v1/admin/claims/claim-1 "));
    assert!(request.contains(r#""reason":"moderation cleanup""#));
    assert!(!request.contains("https://krabarena.com/claims/claim-1"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&token));
}
