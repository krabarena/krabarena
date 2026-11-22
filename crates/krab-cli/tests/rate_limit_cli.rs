use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn unique_home() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("krab-rate-limit-cli-{nanos}"))
}

fn write_config(home: &Path, api_url: &str) {
    let config_dir = home.join(".krab");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!("api_url = \"{api_url}\"\n\n[telemetry]\nshare = false\n"),
    )
    .unwrap();
}

fn read_request(stream: &mut TcpStream) {
    let mut buffer = Vec::new();
    let mut tmp = [0_u8; 1024];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut tmp).unwrap();
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&tmp[..n]);
    }
}

fn spawn_rate_limited_mock(body: &'static str) -> (String, thread::JoinHandle<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let api_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut served = 0;
        while served < 2 {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "mock timed out");
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(err) => panic!("accept failed: {err}"),
            };
            stream.set_nonblocking(false).unwrap();
            read_request(&mut stream);
            let response = if served == 0 {
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_string()
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Krab-Server-Version: {}\r\n\r\n{body}",
                    body.len(),
                    env!("CARGO_PKG_VERSION")
                )
            };
            stream.write_all(response.as_bytes()).unwrap();
            served += 1;
        }
        served
    });
    (api_url, handle)
}

#[test]
fn rate_limited_request_reports_wait_on_stderr_and_keeps_json_stdout_clean() {
    let home = unique_home();
    let body = r#"{"data":[],"pagination":{"has_more":false,"next_cursor":null},"meta":{"request_id":"rl","timestamp":"2026-01-01T00:00:00Z"}}"#;
    let (api_url, handle) = spawn_rate_limited_mock(body);
    write_config(&home, &api_url);

    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_krab"))
        .env("HOME", &home)
        .env("KRAB_NO_AUTO_UPDATE", "1")
        .args(["--format", "json", "battle", "list"])
        .output()
        .unwrap();
    let served = handle.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert_eq!(served, 2);
    assert!(started.elapsed() >= Duration::from_secs(1));
    let host = api_url.trim_start_matches("http://");
    let notices: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("rate limited"))
        .collect();
    assert_eq!(
        notices,
        vec![format!("rate limited by {host}, retrying in 1 s").as_str()],
        "stderr={stderr}"
    );
    assert!(!stdout.contains("rate limited"));
    serde_json::from_str::<serde_json::Value>(&stdout).expect("stdout stays valid JSON");
}
