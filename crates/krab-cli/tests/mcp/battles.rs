use serde_json::json;

use crate::support::{jwt_with_role, mcp_call, spawn_mock, unique_home, write_config};

#[test]
fn mcp_admin_battle_list_calls_admin_api() {
    let home = unique_home("list-battles");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "items": [{
                    "id": "00000000-0000-0000-0000-000000000020",
                    "slug": "example-battle",
                    "url": "https://krabarena.com/battles/example-battle",
                    "title": "Example Battle",
                    "status": "open",
                    "domain_tags": ["rust"],
                    "deleted_at": null
                }],
                "pagination": {
                    "limit": 10,
                    "offset": 20,
                    "count": 42,
                    "count_only": false
                }
            },
            "meta": { "request_id": "mock-battle-list", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_battle_list",
        json!({
            "status": "open",
            "domain_tag": "rust",
            "missing_solutions": true,
            "include_deleted": true,
            "limit": 10,
            "offset": 20,
            "count_only": false,
            "order_by": "-updated_at,+id"
        }),
        20,
        Some(&token),
    );
    let _request = server.expect_request(
        "GET",
        "/v1/admin/battles?status=open&domain_tag=rust&missing_solutions=true&include_deleted=true&limit=10&offset=20&count_only=false&order_by=-updated_at%2C%2Bid",
    );

    assert_eq!(response["id"], 20);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["code"], "OK");
    assert_eq!(
        response["result"]["structuredContent"]["data"]["items"][0]["slug"],
        "example-battle"
    );
    let data = serde_json::to_string(&response["result"]["structuredContent"]["data"]).unwrap();
    assert!(
        !data.contains("\"url\""),
        "MCP Battle read responses must not expose browser URL fields: {data}"
    );
}

#[test]
fn mcp_admin_battle_inspect_normalizes_full_url() {
    let home = unique_home("inspect-battle");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "id": "00000000-0000-0000-0000-000000000021",
                "slug": "example-battle",
                "title": "Example Battle",
                "status": "open",
                "claims": [],
                "solutions": []
            },
            "meta": { "request_id": "mock-battle-inspect", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_battle_inspect",
        json!({ "id": "http://127.0.0.1:3000/battles/example-battle?x=1#top" }),
        21,
        Some(&token),
    );
    let _request = server.expect_request("GET", "/v1/admin/battles/example-battle");

    assert_eq!(response["id"], 21);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["code"], "OK");
    assert_eq!(
        response["result"]["structuredContent"]["data"]["slug"],
        "example-battle"
    );
}

#[test]
fn mcp_admin_battle_prepare_loads_current_values_without_mutation() {
    let home = unique_home("prepare-battle");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "id": "battle-1",
                "slug": "battle-1",
                "url": "https://krabarena.com/battles/battle-1",
                "title": "Old Battle",
                "description": "Old description",
                "domain_tags": ["old"],
                "status": "open",
                "proxy_spec": null,
                "solutions": [],
                "summaries": { "has_executive_summary": false },
                "counts": { "live_claim_count": 0 },
                "deleted_at": null,
                "warnings": [],
                "risk_signals": []
            },
            "meta": { "request_id": "mock-battle-prepare", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_battle_prepare_update",
        json!({
            "id": "battle-1",
            "changes": {
                "title": "New Battle",
                "tags": ["new"]
            }
        }),
        22,
        Some(&token),
    );
    let _request = server.expect_request("GET", "/v1/admin/battles/battle-1");

    assert_eq!(response["id"], 22);
    assert_eq!(response["result"]["isError"], false);
    let data = &response["result"]["structuredContent"]["data"];
    assert_eq!(data["operation"], "battle.update");
    assert!(data["target"].get("url").is_none());
    assert_eq!(data["current_values"]["title"], "Old Battle");
    assert_eq!(data["requested_changes"]["title"], "New Battle");
    assert_eq!(data["requested_changes"]["domain_tags"], json!(["new"]));
    assert_eq!(data["execute_tool"], "admin_battle_execute_update");
    assert_eq!(data["mutation_performed"], false);
}

#[test]
fn mcp_admin_confirmed_battle_update_calls_admin_api_without_token_leak() {
    let home = unique_home("confirmed-battle-update");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "id": "battle-1",
                "title": "New Battle",
                "status": "active"
            },
            "meta": { "request_id": "mock-battle-update", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_battle_execute_update",
        json!({
            "id": "battle-1",
            "changes": {
                "title": "New Battle",
                "status": "active"
            },
            "reason": "editorial cleanup",
            "confirmed": true
        }),
        23,
        Some(&token),
    );
    let request = server.expect_request("PATCH", "/v1/admin/battles/battle-1");

    assert!(request.body.contains(r#""reason":"editorial cleanup""#));
    assert_eq!(response["id"], 23);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["code"], "OK");
    assert_eq!(
        response["result"]["structuredContent"]["data"]["mutation_performed"],
        true
    );
}

#[test]
fn mcp_admin_confirmed_battle_delete_calls_admin_api_without_token_leak() {
    let home = unique_home("confirmed-battle-delete");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "battle_id": "battle-1",
                "deleted": true
            },
            "meta": { "request_id": "mock-battle-delete", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_battle_execute_delete",
        json!({
            "id": "battle-1",
            "reason": "duplicate Battle cleanup",
            "confirmed": true
        }),
        24,
        Some(&token),
    );
    let request = server.expect_request("DELETE", "/v1/admin/battles/battle-1");

    assert!(
        request
            .body
            .contains(r#""reason":"duplicate Battle cleanup""#)
    );
    assert_eq!(response["id"], 24);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["data"]["result"]["deleted"],
        true
    );
}

#[test]
fn mcp_admin_battle_prepare_restore_loads_current_values_without_mutation() {
    let home = unique_home("prepare-battle-restore");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "id": "battle-1",
                "slug": "battle-1",
                "url": "https://krabarena.com/battles/battle-1",
                "title": "Deleted Battle",
                "description": "Hidden description",
                "domain_tags": ["rust"],
                "status": "open",
                "proxy_spec": null,
                "solutions": [],
                "summaries": { "has_executive_summary": false },
                "counts": { "live_claim_count": 0 },
                "deleted_at": "2026-01-01T00:00:00Z",
                "warnings": [],
                "risk_signals": []
            },
            "meta": { "request_id": "mock-battle-prepare-restore", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_battle_prepare_restore",
        json!({ "id": "battle-1" }),
        25,
        Some(&token),
    );
    let _request = server.expect_request("GET", "/v1/admin/battles/battle-1");

    assert_eq!(response["id"], 25);
    assert_eq!(response["result"]["isError"], false);
    let data = &response["result"]["structuredContent"]["data"];
    assert_eq!(data["operation"], "battle.restore");
    assert_eq!(data["target"]["id"], "battle-1");
    assert!(data["target"].get("url").is_none());
    assert_eq!(data["current_values"]["deleted"], true);
    assert_eq!(data["requested_changes"]["restored"], true);
    assert_eq!(data["execute_tool"], "admin_battle_execute_restore");
    assert_eq!(data["mutation_performed"], false);
}

#[test]
fn mcp_admin_confirmed_battle_restore_calls_admin_api_without_token_leak() {
    let home = unique_home("confirmed-battle-restore");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "battle_id": "battle-1",
                "restored": true
            },
            "meta": { "request_id": "mock-battle-restore", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_battle_execute_restore",
        json!({
            "id": "battle-1",
            "reason": "restore after mistaken cleanup",
            "confirmed": true
        }),
        26,
        Some(&token),
    );
    let request = server.expect_request("POST", "/v1/admin/battles/battle-1/restore");

    assert!(
        request
            .body
            .contains(r#""reason":"restore after mistaken cleanup""#)
    );
    assert_eq!(response["id"], 26);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["data"]["result"]["restored"],
        true
    );
}

#[test]
fn mcp_admin_battle_restore_execute_requires_reason_and_confirmation() {
    let home = unique_home("battle-restore-safety");
    let token = jwt_with_role("admin");
    let server = spawn_mock(200, json!({ "data": [] }));
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_battle_execute_restore",
        json!({
            "id": "battle-1",
            "confirmed": true
        }),
        27,
        Some(&token),
    );
    server.assert_no_requests();

    assert_eq!(response["id"], 27);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "REASON_REQUIRED"
    );

    let response = mcp_call(
        &home,
        "admin_battle_execute_restore",
        json!({
            "id": "battle-1",
            "reason": "restore after mistaken cleanup"
        }),
        28,
        Some(&token),
    );
    server.assert_no_requests();

    assert_eq!(response["id"], 28);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "CONFIRMATION_REQUIRED"
    );
}
