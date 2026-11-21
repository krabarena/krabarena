fn structured_error(message: &str, value: Value) -> CallToolResult {
    let mut result = CallToolResult::structured_error(value);
    result.content = vec![Content::text(message.to_string())];
    result
}

fn structured_admin_error(err: AdminError) -> CallToolResult {
    structured_error(err.message(), err.structured_content())
}

fn structured_not_implemented(tool: &str, details: Value) -> CallToolResult {
    structured_error(
        "Admin MCP tool is registered but not implemented yet.",
        json!({
            "code": "NOT_IMPLEMENTED",
            "tool": tool,
            "message": "Admin MCP infrastructure is available; this tool's business behavior is implemented in a later task.",
            "details": details
        }),
    )
}

fn structured_success(tool: &str, message: &str, data: Value) -> CallToolResult {
    let mut result = CallToolResult::structured(json!({
        "code": "OK",
        "tool": tool,
        "data": data,
    }));
    result.content = vec![Content::text(message.to_string())];
    result
}

fn without_browser_urls(mut data: Value) -> Value {
    remove_browser_urls(&mut data);
    data
}

fn remove_browser_urls(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("url");
            for value in object.values_mut() {
                remove_browser_urls(value);
            }
        }
        Value::Array(items) => {
            for value in items {
                remove_browser_urls(value);
            }
        }
        _ => {}
    }
}
