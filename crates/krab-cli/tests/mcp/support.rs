use std::{
    cell::{Cell, RefCell},
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use serde_json::{Value, json};

const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);

thread_local! {
    static LAST_MCP_CONTEXT: RefCell<Option<McpFailureContext>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
struct McpFailureContext {
    tool_name: String,
    response: String,
    stderr: String,
}

pub(crate) fn unique_home(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("krab-mcp-admin-{label}-{nanos}"))
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

pub(crate) fn jwt_with_role(role: &str) -> String {
    jwt_with_role_and_exp(role, unix_timestamp() + 3600)
}

pub(crate) fn jwt_with_role_and_exp(role: &str, exp: i64) -> String {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"HS256"}"#);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        format!(
            r#"{{"role":"{role}","sub":"00000000-0000-0000-0000-000000000001","entity_id":"00000000-0000-0000-0000-000000000002","iat":{},"exp":{exp}}}"#,
            unix_timestamp()
        )
        .as_bytes(),
    );
    format!("{header}.{payload}.signature-not-checked")
}

pub(crate) fn expired_admin_jwt() -> String {
    jwt_with_role_and_exp("admin", unix_timestamp() - 60)
}

pub(crate) fn write_config(home: &PathBuf, api_url: &str, token: Option<&str>) {
    let config_dir = home.join(".krab");
    fs::create_dir_all(&config_dir).unwrap();
    let mut config = format!("api_url = \"{api_url}\"\n");
    if let Some(token) = token {
        config.push_str(&format!("bearer_token = \"{token}\"\n"));
    }
    fs::write(config_dir.join("config.toml"), config).unwrap();
}

#[derive(Clone, Debug)]
pub(crate) struct RecordedRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl RecordedRequest {
    pub(crate) fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn redacted_summary(&self) -> String {
        let headers = self
            .headers
            .iter()
            .map(|(name, value)| {
                if name.eq_ignore_ascii_case("authorization") {
                    format!("{name}: [REDACTED]")
                } else {
                    format!("{name}: {value}")
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "{} {} headers=[{}] body={}",
            self.method,
            self.path,
            headers,
            redact_authorization_fragments(&self.body)
        )
    }
}

pub(crate) struct MockHttpServer {
    pub(crate) api_url: String,
    addr: SocketAddr,
    receiver: mpsc::Receiver<RecordedRequest>,
    received: RefCell<Vec<RecordedRequest>>,
    cursor: Cell<usize>,
    shutdown: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

pub(crate) fn spawn_mock(status: u16, body: Value) -> MockHttpServer {
    spawn_sequence_mock(vec![(status, body)])
}

pub(crate) fn spawn_sequence_mock(responses: Vec<(u16, Value)>) -> MockHttpServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let api_url = format!("http://{addr}");
    let (sender, receiver) = mpsc::channel();
    let shutdown = Arc::new(AtomicBool::new(false));
    let thread_shutdown = Arc::clone(&shutdown);
    let handle =
        thread::spawn(move || serve_sequence(listener, thread_shutdown, sender, responses));

    MockHttpServer {
        api_url,
        addr,
        receiver,
        received: RefCell::new(Vec::new()),
        cursor: Cell::new(0),
        shutdown,
        handle: Some(handle),
    }
}

impl MockHttpServer {
    pub(crate) fn expect_request(&self, method: &str, path: &str) -> RecordedRequest {
        let request = self.next_request_or_panic(method, path);
        if request.method != method || request.path != path {
            panic!("{}", self.failure_report(method, path, Some(&request)));
        }
        request
    }

    pub(crate) fn expect_requests(&self, expected: &[(&str, &str)]) -> Vec<RecordedRequest> {
        expected
            .iter()
            .map(|(method, path)| self.expect_request(method, path))
            .collect()
    }

    pub(crate) fn assert_no_requests(&self) {
        self.drain_available();
        if self.cursor.get() < self.received.borrow().len() {
            panic!("{}", self.failure_report("<none>", "<none>", None));
        }
    }

    fn next_request_or_panic(&self, method: &str, path: &str) -> RecordedRequest {
        self.drain_available();
        if let Some(request) = self.next_recorded_request() {
            return request;
        }

        match self.receiver.recv_timeout(REQUEST_TIMEOUT) {
            Ok(request) => {
                self.received.borrow_mut().push(request.clone());
                self.cursor.set(self.cursor.get() + 1);
                request
            }
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                panic!("{}", self.failure_report(method, path, None));
            }
        }
    }

    fn next_recorded_request(&self) -> Option<RecordedRequest> {
        let cursor = self.cursor.get();
        let request = self.received.borrow().get(cursor).cloned()?;
        self.cursor.set(cursor + 1);
        Some(request)
    }

    fn drain_available(&self) {
        while let Ok(request) = self.receiver.try_recv() {
            self.received.borrow_mut().push(request);
        }
    }

    fn received_summaries(&self) -> Vec<String> {
        self.drain_available();
        self.received
            .borrow()
            .iter()
            .map(RecordedRequest::redacted_summary)
            .collect()
    }

    fn failure_report(
        &self,
        expected_method: &str,
        expected_path: &str,
        actual: Option<&RecordedRequest>,
    ) -> String {
        let actual_line = actual
            .map(|request| format!("{} {}", request.method, request.path))
            .unwrap_or_else(|| "<missing>".to_string());
        let received = self.received_summaries();
        let received = if received.is_empty() {
            "<none>".to_string()
        } else {
            received.join("\n")
        };
        let mcp_context = LAST_MCP_CONTEXT
            .with(|context| context.borrow().clone())
            .unwrap_or_else(|| McpFailureContext {
                tool_name: "<unknown>".to_string(),
                response: "<no MCP response captured>".to_string(),
                stderr: "<no MCP stderr captured>".to_string(),
            });

        format!(
            "MCP mock request expectation failed\n\
             tool: {}\n\
             expected request: {expected_method} {expected_path}\n\
             actual request: {actual_line}\n\
             received requests:\n{received}\n\
             structured MCP response:\n{}\n\
             MCP subprocess stderr:\n{}",
            mcp_context.tool_name, mcp_context.response, mcp_context.stderr
        )
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_sequence(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    sender: mpsc::Sender<RecordedRequest>,
    responses: Vec<(u16, Value)>,
) {
    for (status, body) in responses {
        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(_) => break,
        };
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let request = match read_request(&mut stream) {
            Ok(request) => request,
            Err(_) => break,
        };
        let _ = sender.send(request);
        let body = serde_json::to_string(&body).unwrap();
        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Krab-Server-Version: {}\r\n\r\n{body}",
            body.len(),
            env!("CARGO_PKG_VERSION")
        );
        let _ = stream.write_all(response.as_bytes());
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<RecordedRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut buffer = Vec::new();
    let mut tmp = [0_u8; 1024];
    loop {
        let n = stream.read(&mut tmp)?;
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
        .unwrap_or(buffer.len());
    let headers_raw = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let content_length = parse_content_length(&headers_raw).unwrap_or(0);
    while buffer.len().saturating_sub(header_end) < content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&tmp[..n]);
    }

    Ok(parse_recorded_request(&buffer, header_end))
}

fn parse_content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    })
}

fn parse_recorded_request(buffer: &[u8], header_end: usize) -> RecordedRequest {
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = header_text.lines();
    let first_line = lines.next().unwrap_or_default();
    let mut first_parts = first_line.split_whitespace();
    let method = first_parts.next().unwrap_or_default().to_string();
    let path = first_parts.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    let body = String::from_utf8_lossy(buffer.get(header_end..).unwrap_or_default()).to_string();

    RecordedRequest {
        method,
        path,
        headers,
        body,
    }
}

pub(crate) struct McpSession {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: BufReader<ChildStdout>,
    stderr_handle: Option<thread::JoinHandle<String>>,
    finished: bool,
}

impl McpSession {
    pub(crate) fn start(home: &PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_krab"))
            .arg("mcp")
            .arg("admin")
            .env("HOME", home)
            .env("KRAB_NO_AUTO_UPDATE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stderr_handle = thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buffer = String::new();
            let _ = reader.read_to_string(&mut buffer);
            buffer
        });

        let mut session = Self {
            child,
            stdin: Some(stdin),
            reader: BufReader::new(stdout),
            stderr_handle: Some(stderr_handle),
            finished: false,
        };
        session.initialize();
        session
    }

    pub(crate) fn process_id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn call_tool(&mut self, tool_name: &str, arguments: Value, call_id: i64) -> Value {
        self.send_json(json!({
            "jsonrpc": "2.0",
            "id": call_id,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            }
        }));
        self.read_json()
    }

    pub(crate) fn finish(&mut self) -> String {
        drop(self.stdin.take());
        let _ = self.child.wait().unwrap();
        self.finished = true;
        self.stderr_handle
            .take()
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default()
    }

    fn initialize(&mut self) {
        self.send_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "krab-cli-test", "version": "0" }
            }
        }));
        let init = self.read_json();
        assert_eq!(init["result"]["serverInfo"]["name"], "krabarena-admin");

        self.send_json(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }));

        self.send_json(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }));
        let tools = self.read_json();
        let tool_names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(tool_names.contains(&"admin_claim_list"));
        assert!(tool_names.contains(&"admin_claim_prepare_restore"));
        assert!(tool_names.contains(&"admin_claim_execute_restore"));
        assert!(tool_names.contains(&"admin_battle_execute_delete"));
        assert!(tool_names.contains(&"admin_battle_prepare_restore"));
        assert!(tool_names.contains(&"admin_battle_execute_restore"));
        assert!(tool_names.contains(&"admin_user_list"));
        assert!(tool_names.contains(&"admin_user_inspect"));
    }

    fn send_json(&mut self, payload: Value) {
        writeln!(self.stdin.as_mut().unwrap(), "{payload}").unwrap();
    }

    fn read_json(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.stderr_handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn mcp_call(
    home: &PathBuf,
    tool_name: &str,
    arguments: Value,
    call_id: i64,
    token_to_check_absent: Option<&str>,
) -> Value {
    let mut session = McpSession::start(home);
    let response = session.call_tool(tool_name, arguments, call_id);
    let stderr = session.finish();
    let response_text = serde_json::to_string_pretty(&response).unwrap();
    let redacted_response = redact_with_optional_token(&response_text, token_to_check_absent);
    let redacted_stderr = redact_with_optional_token(&stderr, token_to_check_absent);

    if let Some(token) = token_to_check_absent {
        assert!(!response_text.contains(token));
        assert!(!stderr.contains(token));
    }

    LAST_MCP_CONTEXT.with(|context| {
        *context.borrow_mut() = Some(McpFailureContext {
            tool_name: tool_name.to_string(),
            response: redacted_response,
            stderr: redacted_stderr,
        });
    });

    response
}

fn redact_with_optional_token(input: &str, token: Option<&str>) -> String {
    let mut redacted = redact_authorization_fragments(input);
    if let Some(token) = token.filter(|token| !token.is_empty()) {
        redacted = redacted.replace(token, "[REDACTED_TOKEN]");
    }
    redacted
}

fn redact_authorization_fragments(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            if line.to_ascii_lowercase().contains("authorization") {
                "Authorization: [REDACTED]".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
