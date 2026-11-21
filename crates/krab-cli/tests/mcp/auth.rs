use serde_json::json;

use crate::support::{
    expired_admin_jwt, jwt_with_role, mcp_call, spawn_mock, unique_home, write_config,
};

#[test]
fn mcp_admin_no_token_returns_no_token() {
    let home = unique_home("no-token");
    let server = spawn_mock(200, json!({ "data": [] }));
    write_config(&home, &server.api_url, None);

    let response = mcp_call(&home, "admin_user_list", json!({}), 3, None);
    server.assert_no_requests();

    assert_eq!(response["id"], 3);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(response["result"]["structuredContent"]["code"], "NO_TOKEN");
}

#[test]
fn mcp_admin_non_admin_token_returns_not_admin() {
    let home = unique_home("user-token");
    let token = jwt_with_role("user");
    let server = spawn_mock(200, json!({ "data": [] }));
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(&home, "admin_user_list", json!({}), 4, Some(&token));
    server.assert_no_requests();

    assert_eq!(response["id"], 4);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(response["result"]["structuredContent"]["code"], "NOT_ADMIN");
}

#[test]
fn mcp_admin_expired_admin_token_returns_token_expired() {
    let home = unique_home("expired-token");
    let token = expired_admin_jwt();
    let server = spawn_mock(200, json!({ "data": [] }));
    write_config(&home, &server.api_url, Some(&token));

    let response = mcp_call(&home, "admin_user_list", json!({}), 5, Some(&token));
    server.assert_no_requests();

    assert_eq!(response["id"], 5);
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "TOKEN_EXPIRED"
    );
}
