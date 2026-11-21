use std::{
    any::Any,
    io::{Read, Write},
    net::TcpStream,
    panic::{AssertUnwindSafe, catch_unwind},
    process::Command,
    time::{Duration, Instant},
};

use serde_json::json;

use crate::support::{McpSession, jwt_with_role, mcp_call, spawn_mock, unique_home, write_config};

#[test]
fn mcp_mock_expect_request_reports_missing_request() {
    let server = spawn_mock(200, json!({ "data": [] }));

    let message = panic_message(catch_unwind(AssertUnwindSafe(|| {
        server.expect_request("GET", "/v1/admin/users");
    })));

    assert!(message.contains("expected request: GET /v1/admin/users"));
    assert!(message.contains("actual request: <missing>"));
    assert!(message.contains("received requests:\n<none>"));
}

#[test]
fn mcp_mock_expect_request_reports_unexpected_request_with_received_requests() {
    let server = spawn_mock(200, json!({ "data": [] }));
    send_raw_request(
        &server.api_url,
        "POST /v1/admin/claims/claim-1 HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
    );

    let message = panic_message(catch_unwind(AssertUnwindSafe(|| {
        server.expect_request("GET", "/v1/admin/users");
    })));

    assert!(message.contains("expected request: GET /v1/admin/users"));
    assert!(message.contains("actual request: POST /v1/admin/claims/claim-1"));
    assert!(message.contains("POST /v1/admin/claims/claim-1"));
}

#[test]
fn mcp_mock_failure_report_redacts_authorization_headers() {
    let server = spawn_mock(200, json!({ "data": [] }));
    let token = "secret-token-that-must-not-leak";
    send_raw_request(
        &server.api_url,
        &format!(
            "POST /actual HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\n\r\n"
        ),
    );

    let message = panic_message(catch_unwind(AssertUnwindSafe(|| {
        server.expect_request("GET", "/expected");
    })));

    assert!(!message.contains(token));
    assert!(message.contains("Authorization: [REDACTED]"));
}

#[test]
fn mcp_error_response_can_assert_no_requests_without_timeout_wait() {
    let home = unique_home("harness-no-request");
    let token = jwt_with_role("admin");
    let server = spawn_mock(200, json!({ "data": [] }));
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_claim_execute_delete",
        json!({
            "id": "claim-1",
            "confirmed": true
        }),
        101,
        Some(&token),
    );

    let started = Instant::now();
    server.assert_no_requests();
    assert!(started.elapsed() < Duration::from_millis(200));
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "REASON_REQUIRED"
    );
}

#[test]
fn mcp_session_drop_terminates_subprocess() {
    let home = unique_home("session-drop");
    write_config(&home, "http://127.0.0.1:3000", None);

    let pid = {
        let session = McpSession::start(&home);
        session.process_id()
    };

    assert!(
        !process_exists(pid),
        "MCP subprocess {pid} is still running"
    );
}

fn send_raw_request(api_url: &str, request: &str) {
    let addr = api_url.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
}

fn process_exists(pid: u32) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("kill -0 {pid} 2>/dev/null"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn panic_message(result: Result<(), Box<dyn Any + Send>>) -> String {
    let err = result.expect_err("expected panic");
    if let Some(message) = err.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = err.downcast_ref::<&'static str>() {
        return (*message).to_string();
    }
    "<unknown panic>".to_string()
}
