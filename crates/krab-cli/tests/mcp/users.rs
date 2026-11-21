use serde_json::json;

use crate::support::{
    jwt_with_role, mcp_call, spawn_mock, spawn_sequence_mock, unique_home, write_config,
};

fn user_response(user_id: &str, role: &str) -> serde_json::Value {
    json!({
        "user_id": user_id,
        "entity_id": "00000000-0000-0000-0000-000000000099",
        "display_name": "Target User",
        "email": "target@example.com",
        "entity_name": "Target User",
        "entity_domain": "example.com",
        "role": role,
        "org_role": "member",
        "kind": "human",
        "suspended_until": null,
        "suspension_reason": null,
        "rate_limit_until": null,
        "created_at": "2026-01-01T00:00:00Z",
        "claim_count": 2,
        "battle_count": 1,
        "warnings": [],
        "risk_signals": []
    })
}

#[test]
fn mcp_admin_user_list_calls_admin_api() {
    let home = unique_home("list-users");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "items": [{
                    "user_id": "00000000-0000-0000-0000-000000000003",
                    "display_name": "Admin User",
                    "email": "admin@example.com",
                    "entity_domain": "example.com",
                    "role": "admin",
                    "suspended_until": null,
                    "suspension_reason": null,
                    "created_at": "2026-01-01T00:00:00Z"
                }],
                "pagination": {
                    "limit": 10,
                    "offset": 5,
                    "count": 1,
                    "count_only": false
                }
            },
            "meta": { "request_id": "mock-list", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_list",
        json!({
            "email": "admin@example.com",
            "role": "admin",
            "limit": 10,
            "offset": 5,
            "count_only": false,
            "order_by": "-created_at,+user_id"
        }),
        7,
        Some(&token),
    );
    let request = server.expect_request(
        "GET",
        "/v1/admin/users?email=admin%40example.com&role=admin&limit=10&offset=5&count_only=false&order_by=-created_at%2C%2Buser_id",
    );

    assert!(request.header_value("authorization").is_some());
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["code"], "OK");
    assert_eq!(
        response["result"]["structuredContent"]["data"]["items"][0]["email"],
        "admin@example.com"
    );
}

#[test]
fn mcp_admin_user_inspect_calls_admin_api() {
    let home = unique_home("inspect-user");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "user_id": "00000000-0000-0000-0000-000000000004",
                "display_name": "Target User",
                "email": "target@example.com",
                "entity_domain": "example.com",
                "role": "user",
                "suspended_until": null,
                "suspension_reason": null,
                "created_at": "2026-01-01T00:00:00Z"
            },
            "meta": { "request_id": "mock-inspect", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_inspect",
        json!({ "id": "target@example.com" }),
        8,
        Some(&token),
    );
    let _request = server.expect_request("GET", "/v1/admin/users/target%40example.com");

    assert_eq!(response["id"], 8);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["code"], "OK");
    assert_eq!(
        response["result"]["structuredContent"]["data"]["email"],
        "target@example.com"
    );
}

#[test]
fn mcp_admin_user_inspect_maps_404_to_not_found() {
    let home = unique_home("inspect-missing-user");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        404,
        json!({
            "error": { "code": "USER_NOT_FOUND", "message": "User not found" },
            "meta": { "request_id": "mock-404", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_inspect",
        json!({ "id": "missing@example.com" }),
        9,
        Some(&token),
    );
    let _request = server.expect_request("GET", "/v1/admin/users/missing%40example.com");

    assert_eq!(response["id"], 9);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(response["result"]["structuredContent"]["code"], "NOT_FOUND");
    assert_eq!(
        response["result"]["structuredContent"]["details"]["request_id"],
        "mock-404"
    );
}

#[test]
fn mcp_admin_user_list_maps_extended_filters() {
    let home = unique_home("list-users-extended");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "items": [],
                "pagination": {
                    "limit": 5,
                    "offset": 10,
                    "count": 0,
                    "count_only": true
                }
            },
            "meta": { "request_id": "mock-list-extended", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_list",
        json!({
            "filter": {
                "entity_domain": "example.com",
                "org_role": "member",
                "kind": "human",
                "suspended": false
            },
            "created_after": "2026-01-01T00:00:00Z",
            "limit": 5,
            "offset": 10,
            "count_only": true,
            "order_by": "email"
        }),
        30,
        Some(&token),
    );
    let _request = server.expect_request(
        "GET",
        "/v1/admin/users?entity_domain=example.com&org_role=member&kind=human&created_after=2026-01-01T00%3A00%3A00Z&suspended=false&limit=5&offset=10&count_only=true&order_by=email",
    );

    assert_eq!(response["id"], 30);
    assert_eq!(response["result"]["isError"], false);
}

#[test]
fn mcp_admin_user_prepare_update_loads_current_values_without_mutation() {
    let home = unique_home("prepare-user-update");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": user_response("00000000-0000-0000-0000-000000000010", "user"),
            "meta": { "request_id": "mock-user-prepare", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_prepare_update",
        json!({
            "id": "target@example.com",
            "changes": {
                "display_name": "Updated User",
                "email": "updated@example.com"
            }
        }),
        31,
        Some(&token),
    );
    let _request = server.expect_request("GET", "/v1/admin/users/target%40example.com");

    assert_eq!(response["id"], 31);
    assert_eq!(response["result"]["isError"], false);
    let data = &response["result"]["structuredContent"]["data"];
    assert_eq!(data["operation"], "user.update");
    assert_eq!(data["current_values"]["display_name"], "Target User");
    assert_eq!(data["requested_changes"]["display_name"], "Updated User");
    assert_eq!(data["execute_tool"], "admin_user_execute_update");
    assert_eq!(data["mutation_performed"], false);
}

#[test]
fn mcp_admin_confirmed_user_update_inspects_then_patches_uuid() {
    let home = unique_home("execute-user-update");
    let token = jwt_with_role("admin");
    let user_id = "00000000-0000-0000-0000-000000000010";
    let server = spawn_sequence_mock(vec![
        (
            200,
            json!({
                "data": user_response(user_id, "user"),
                "meta": { "request_id": "mock-user-inspect", "timestamp": "2026-01-01T00:00:00Z" }
            }),
        ),
        (
            200,
            json!({
                "data": {
                    "user_id": user_id,
                    "display_name": "Updated User",
                    "email": "updated@example.com",
                    "updated": true
                },
                "meta": { "request_id": "mock-user-patch", "timestamp": "2026-01-01T00:00:00Z" }
            }),
        ),
    ]);
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_execute_update",
        json!({
            "id": "target@example.com",
            "changes": {
                "display_name": "Updated User",
                "email": "updated@example.com"
            },
            "reason": "support requested correction",
            "confirmed": true
        }),
        32,
        Some(&token),
    );
    let requests = server.expect_requests(&[
        ("GET", "/v1/admin/users/target%40example.com"),
        ("PATCH", &format!("/v1/admin/users/{user_id}")),
    ]);

    assert!(
        requests[1]
            .body
            .contains(r#""reason":"support requested correction""#)
    );
    assert_eq!(response["id"], 32);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["data"]["mutation_performed"],
        true
    );
}

#[test]
fn mcp_admin_user_role_self_change_requires_extra_confirmation() {
    let home = unique_home("self-role-change");
    let token = jwt_with_role("admin");
    let self_id = "00000000-0000-0000-0000-000000000001";
    let server = spawn_mock(
        200,
        json!({
            "data": user_response(self_id, "admin"),
            "meta": { "request_id": "mock-self-inspect", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_execute_role_change",
        json!({
            "id": self_id,
            "role": "user",
            "reason": "testing self safety",
            "confirmed": true
        }),
        33,
        Some(&token),
    );
    let _request = server.expect_request("GET", &format!("/v1/admin/users/{self_id}"));

    assert_eq!(response["id"], 33);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "SELF_CHANGE_CONFIRMATION_REQUIRED"
    );
}

#[test]
fn mcp_admin_user_role_change_requires_other_active_admin() {
    let home = unique_home("last-active-admin");
    let token = jwt_with_role("admin");
    let target_id = "00000000-0000-0000-0000-000000000010";
    let server = spawn_sequence_mock(vec![
        (
            200,
            json!({
                "data": user_response(target_id, "admin"),
                "meta": { "request_id": "mock-target-admin", "timestamp": "2026-01-01T00:00:00Z" }
            }),
        ),
        (
            200,
            json!({
                "data": {
                    "items": [user_response(target_id, "admin")],
                    "pagination": {
                        "limit": 200,
                        "offset": 0,
                        "count": 1,
                        "count_only": false
                    }
                },
                "meta": { "request_id": "mock-active-admins", "timestamp": "2026-01-01T00:00:00Z" }
            }),
        ),
    ]);
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_execute_role_change",
        json!({
            "id": target_id,
            "role": "user",
            "reason": "test last-admin guard",
            "confirmed": true
        }),
        36,
        Some(&token),
    );
    let requests = server.expect_requests(&[
        ("GET", &format!("/v1/admin/users/{target_id}")),
        (
            "GET",
            "/v1/admin/users?role=admin&suspended=false&limit=200",
        ),
    ]);

    assert_eq!(requests.len(), 2);
    assert_eq!(response["id"], 36);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "LAST_ADMIN"
    );
}

#[test]
fn mcp_admin_user_suspension_requires_confirmation_fields_before_api_patch() {
    let home = unique_home("user-suspend-no-reason");
    let token = jwt_with_role("admin");
    let server = spawn_mock(200, json!({ "data": [] }));
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_user_execute_suspension",
        json!({
            "id": "target@example.com",
            "suspend_reason": "policy violation",
            "confirmed": true
        }),
        34,
        Some(&token),
    );
    server.assert_no_requests();
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "REASON_REQUIRED"
    );

    let response = mcp_call(
        &home,
        "admin_user_execute_suspension",
        json!({
            "id": "target@example.com",
            "suspend_reason": "policy violation",
            "reason": "admin moderation"
        }),
        35,
        Some(&token),
    );
    server.assert_no_requests();
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "CONFIRMATION_REQUIRED"
    );
}
