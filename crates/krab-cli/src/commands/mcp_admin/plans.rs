fn build_prepare_plan(spec: &AdminToolSpec, target: Value, requested_changes: Value) -> Value {
    let diff = diff_from_requested_changes(&requested_changes);
    json!({
        "operation": operation_name(spec.name),
        "target": target,
        "current_values": Value::Null,
        "current_values_status": "not_loaded_until_business_tool_is_implemented",
        "requested_changes": requested_changes,
        "diff": diff,
        "risks": risks_for_tool(spec.name),
        "user_visible_impact": user_visible_impact(spec.name),
        "execute_tool": execute_tool_name(spec.name),
        "confirmation_required": {
            "reason": true,
            "confirmed": true,
            "policy": "Only set confirmed=true after explicit administrator approval."
        },
        "mutation_performed": false,
    })
}

fn operation_name(tool_name: &str) -> String {
    tool_name
        .strip_prefix("admin_")
        .unwrap_or(tool_name)
        .replace("_prepare_", ".")
        .replace("_execute_", ".")
}

fn execute_tool_name(prepare_tool_name: &str) -> String {
    prepare_tool_name.replace("_prepare_", "_execute_")
}

fn entity_type(tool_name: &str) -> &'static str {
    if tool_name.starts_with("admin_claim_") {
        "claim"
    } else if tool_name.starts_with("admin_battle_") {
        "battle"
    } else if tool_name.starts_with("admin_user_") {
        "user"
    } else {
        "unknown"
    }
}

fn diff_from_requested_changes(requested_changes: &Value) -> Value {
    let Some(changes) = requested_changes.as_object() else {
        return json!([]);
    };
    Value::Array(
        changes
            .iter()
            .map(|(field, requested)| {
                json!({
                    "field": field,
                    "current": Value::Null,
                    "requested": requested,
                    "status": "pending_business_tool_current_value_load"
                })
            })
            .collect(),
    )
}

