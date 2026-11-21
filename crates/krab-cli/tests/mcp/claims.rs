use serde_json::json;

use crate::support::{jwt_with_role, mcp_call, spawn_mock, unique_home, write_config};

#[test]
fn mcp_admin_claim_list_calls_admin_api() {
    let home = unique_home("list-claims");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "items": [{
                    "id": "00000000-0000-0000-0000-000000000010",
                    "slug": "example-claim",
                    "title": "Example Claim",
                    "url": "https://krabarena.com/claims/example-claim",
                    "battle": {
                        "battle_id": "00000000-0000-0000-0000-000000000020",
                        "slug": "example-battle",
                        "url": "https://krabarena.com/battles/example-battle"
                    },
                    "visibility": "public",
                    "deleted_at": null
                }],
                "pagination": {
                    "limit": 10,
                    "offset": 5,
                    "count": 17,
                    "count_only": false
                }
            },
            "meta": { "request_id": "mock-claim-list", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_claim_list",
        json!({
            "battle": "https://krabarena.com/battles/example-battle",
            "visibility": "public",
            "include_deleted": true,
            "limit": 10,
            "offset": 5,
            "count_only": false,
            "order_by": "-created_at,+id"
        }),
        15,
        Some(&token),
    );
    let _request = server.expect_request(
        "GET",
        "/v1/admin/claims?battle=https%3A%2F%2Fkrabarena.com%2Fbattles%2Fexample-battle&visibility=public&include_deleted=true&limit=10&offset=5&count_only=false&order_by=-created_at%2C%2Bid",
    );

    assert_eq!(response["id"], 15);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["code"], "OK");
    assert_eq!(
        response["result"]["structuredContent"]["data"]["items"][0]["slug"],
        "example-claim"
    );
    let data = serde_json::to_string(&response["result"]["structuredContent"]["data"]).unwrap();
    assert!(
        !data.contains("\"url\""),
        "MCP Claim read responses must not expose browser URL fields: {data}"
    );
}

#[test]
fn mcp_admin_claim_prepare_loads_current_values_without_mutation() {
    let home = unique_home("prepare-plan");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "id": "claim-1",
                "slug": "claim-1",
                "title": "Old Claim",
                "url": "https://krabarena.com/claims/claim-1",
                "claim": "# Old Claim\n\n## Summary\nOld.",
                "claim_structure": null,
                "topic": "Old topic",
                "topic_tags": ["old"],
                "visibility": "public",
                "deleted_at": null,
                "latest_review": null,
                "warnings": [],
                "risk_signals": []
            },
            "meta": { "request_id": "mock-claim-inspect", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_claim_prepare_delete",
        json!({
            "id": "claim-1"
        }),
        13,
        Some(&token),
    );
    let _request = server.expect_request("GET", "/v1/admin/claims/claim-1");

    assert_eq!(response["id"], 13);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["code"], "OK");
    let data = &response["result"]["structuredContent"]["data"];
    assert_eq!(data["operation"], "claim.delete");
    assert_eq!(data["target"]["id"], "claim-1");
    assert!(data["target"].get("url").is_none());
    assert_eq!(data["current_values"]["visibility"], "public");
    assert_eq!(data["requested_changes"]["deleted"], true);
    assert_eq!(data["execute_tool"], "admin_claim_execute_delete");
    assert_eq!(data["confirmation_required"]["confirmed"], true);
    assert_eq!(data["mutation_performed"], false);
}

#[test]
fn mcp_admin_confirmed_claim_delete_calls_admin_api_without_token_leak() {
    let home = unique_home("confirmed-execute");
    let token = jwt_with_role("admin");
    let server = spawn_mock(
        200,
        json!({
            "data": {
                "claim_id": "claim-1",
                "deleted": true
            },
            "meta": { "request_id": "mock-delete", "timestamp": "2026-01-01T00:00:00Z" }
        }),
    );
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(
        &home,
        "admin_claim_execute_delete",
        json!({
            "id": "claim-1",
            "changes": { "visibility": "hidden" },
            "reason": "moderation cleanup",
            "confirmed": true
        }),
        14,
        Some(&token),
    );
    let request = server.expect_request("DELETE", "/v1/admin/claims/claim-1");

    assert!(request.body.contains(r#""reason":"moderation cleanup""#));
    assert_eq!(response["id"], 14);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(response["result"]["structuredContent"]["code"], "OK");
    assert_eq!(
        response["result"]["structuredContent"]["data"]["confirmation_validated"],
        true
    );
    assert_eq!(
        response["result"]["structuredContent"]["data"]["mutation_performed"],
        true
    );
    assert_eq!(
        response["result"]["structuredContent"]["data"]["result"]["deleted"],
        true
    );
}
