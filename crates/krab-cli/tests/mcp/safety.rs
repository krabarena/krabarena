use serde_json::json;

use crate::support::{jwt_with_role, mcp_call, spawn_mock, unique_home, write_config};

#[test]
fn mcp_admin_non_admin_execute_returns_not_admin_before_confirmation_validation() {
    let home = unique_home("user-execute");
    let token = jwt_with_role("user");
    let server = spawn_mock(200, json!({ "data": [] }));
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_claim_execute_delete",
        json!({
            "id": "claim-1",
            "reason": "moderation cleanup",
            "confirmed": true
        }),
        10,
        Some(&token),
    );
    server.assert_no_requests();

    assert_eq!(response["id"], 10);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(response["result"]["structuredContent"]["code"], "NOT_ADMIN");
}

#[test]
fn mcp_admin_execute_without_reason_returns_reason_required() {
    let home = unique_home("execute-no-reason");
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
        11,
        Some(&token),
    );
    server.assert_no_requests();

    assert_eq!(response["id"], 11);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "REASON_REQUIRED"
    );
}

#[test]
fn mcp_admin_execute_without_confirmed_returns_confirmation_required() {
    let home = unique_home("execute-no-confirmed");
    let token = jwt_with_role("admin");
    let server = spawn_mock(200, json!({ "data": [] }));
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_claim_execute_delete",
        json!({
            "id": "claim-1",
            "reason": "moderation cleanup"
        }),
        12,
        Some(&token),
    );
    server.assert_no_requests();

    assert_eq!(response["id"], 12);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "CONFIRMATION_REQUIRED"
    );
}
