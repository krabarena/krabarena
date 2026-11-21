fn battle_list_path(arguments: &JsonObject) -> std::result::Result<String, AdminError> {
    let tool = "admin_battle_list";
    let mut query = Vec::new();
    for field in ["status", "domain_tag", "updated_after", "updated_before"] {
        if let Some(value) = optional_filter_string(tool, arguments, field)? {
            query.push(format!("{field}={}", urlencoding::encode(&value)));
        }
    }

    for field in [
        "has_claims",
        "missing_solutions",
        "stale_summary",
        "include_deleted",
    ] {
        if let Some(value) = optional_filter_bool(tool, arguments, field)? {
            query.push(format!("{field}={value}"));
        }
    }

    append_admin_list_params(tool, arguments, &mut query, BATTLE_ORDER_BY_FIELDS)?;

    if query.is_empty() {
        Ok("/v1/admin/battles".to_string())
    } else {
        Ok(format!("/v1/admin/battles?{}", query.join("&")))
    }
}

fn battle_inspect_path(id: &str) -> String {
    let normalized = normalize_path_param_value(id);
    format!("/v1/admin/battles/{}", urlencoding::encode(&normalized))
}

fn battle_public_summary_path(id: &str) -> String {
    let normalized = normalize_path_param_value(id);
    format!(
        "/v1/battles/{}/regenerate-summary",
        urlencoding::encode(&normalized)
    )
}

fn battle_prepare_request(
    spec: &AdminToolSpec,
    arguments: &JsonObject,
) -> std::result::Result<BattlePrepareRequest, AdminError> {
    match spec.name {
        "admin_battle_prepare_create" => Ok(BattlePrepareRequest {
            id: None,
            requested_changes: battle_create_changes(spec.name, arguments)?,
        }),
        "admin_battle_prepare_update" => {
            let id = required_string(spec.name, arguments, "id")?;
            let changes = battle_update_changes(spec.name, arguments)?;
            if changes
                .as_object()
                .map(serde_json::Map::is_empty)
                .unwrap_or(true)
            {
                return Err(AdminError::new(
                    spec.name,
                    "VALIDATION_ERROR",
                    "changes must contain at least one Battle field for update.",
                    json!({ "field": "changes" }),
                ));
            }
            Ok(BattlePrepareRequest {
                id: Some(id),
                requested_changes: changes,
            })
        }
        "admin_battle_prepare_refresh_summary" => Ok(BattlePrepareRequest {
            id: Some(required_string(spec.name, arguments, "id")?),
            requested_changes: json!({ "summary_refreshed": true }),
        }),
        "admin_battle_prepare_backfill_solutions" => Ok(BattlePrepareRequest {
            id: Some(required_string(spec.name, arguments, "id")?),
            requested_changes: json!({ "solutions_backfilled": true }),
        }),
        "admin_battle_prepare_delete" => Ok(BattlePrepareRequest {
            id: Some(required_string(spec.name, arguments, "id")?),
            requested_changes: json!({ "deleted": true }),
        }),
        "admin_battle_prepare_restore" => Ok(BattlePrepareRequest {
            id: Some(required_string(spec.name, arguments, "id")?),
            requested_changes: json!({ "restored": true }),
        }),
        _ => Err(AdminError::new(
            spec.name,
            "NOT_IMPLEMENTED",
            "Battle prepare tool is registered but not implemented.",
            json!({ "tool": spec.name }),
        )),
    }
}

fn battle_execute_request(
    spec: &AdminToolSpec,
    arguments: &JsonObject,
) -> std::result::Result<BattleExecuteRequest, AdminError> {
    let reason = required_reason(spec.name, arguments)?;
    require_confirmed(spec.name, arguments)?;
    match spec.name {
        "admin_battle_execute_create" => Ok(BattleExecuteRequest {
            id: None,
            requested_changes: battle_create_changes(spec.name, arguments)?,
            reason,
        }),
        "admin_battle_execute_update" => {
            let changes = battle_update_changes(spec.name, arguments)?;
            if changes
                .as_object()
                .map(serde_json::Map::is_empty)
                .unwrap_or(true)
            {
                return Err(AdminError::new(
                    spec.name,
                    "VALIDATION_ERROR",
                    "changes must contain at least one Battle field for update.",
                    json!({ "field": "changes" }),
                ));
            }
            Ok(BattleExecuteRequest {
                id: Some(required_string(spec.name, arguments, "id")?),
                requested_changes: changes,
                reason,
            })
        }
        "admin_battle_execute_refresh_summary"
        | "admin_battle_execute_backfill_solutions"
        | "admin_battle_execute_delete"
        | "admin_battle_execute_restore" => Ok(BattleExecuteRequest {
            id: Some(required_string(spec.name, arguments, "id")?),
            requested_changes: match spec.name {
                "admin_battle_execute_refresh_summary" => json!({ "summary_refreshed": true }),
                "admin_battle_execute_backfill_solutions" => {
                    json!({ "solutions_backfilled": true })
                }
                "admin_battle_execute_delete" => json!({ "deleted": true }),
                "admin_battle_execute_restore" => json!({ "restored": true }),
                _ => json!({}),
            },
            reason,
        }),
        _ => Err(AdminError::new(
            spec.name,
            "NOT_IMPLEMENTED",
            "Battle execute tool is registered but not implemented.",
            json!({ "tool": spec.name }),
        )),
    }
}

fn battle_create_changes(
    tool: &str,
    arguments: &JsonObject,
) -> std::result::Result<Value, AdminError> {
    let title = required_string(tool, arguments, "title")?;
    let description = required_string(tool, arguments, "description")?;
    let domain_tags = optional_domain_tags_value(tool, arguments)?.ok_or_else(|| {
        AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "domain_tags is required.",
            json!({ "field": "domain_tags" }),
        )
    })?;

    let mut changes = serde_json::Map::new();
    changes.insert("title".into(), Value::String(title));
    changes.insert("description".into(), Value::String(description));
    changes.insert("domain_tags".into(), domain_tags);
    if let Some(parent_id) = optional_string(tool, arguments, "parent_id")? {
        changes.insert("parent_id".into(), Value::String(parent_id));
    }
    if let Some(status) = optional_string(tool, arguments, "status")? {
        changes.insert("status".into(), Value::String(status));
    }
    if let Some(proxy_spec) = arguments.get("proxy_spec") {
        changes.insert("proxy_spec".into(), proxy_spec.clone());
    }
    if let Some(value) = arguments.get("produced_by") {
        changes.insert("produced_by".into(), value.clone());
    }
    Ok(Value::Object(changes))
}

fn battle_update_changes(
    tool: &str,
    arguments: &JsonObject,
) -> std::result::Result<Value, AdminError> {
    let raw_changes = optional_object_value(tool, arguments, "changes")?;
    let Some(raw) = raw_changes.as_object() else {
        return Ok(json!({}));
    };
    let mut changes = serde_json::Map::new();
    for field in ["title", "description", "status"] {
        if let Some(value) = raw.get(field) {
            if !value.is_string() {
                return Err(AdminError::new(
                    tool,
                    "VALIDATION_ERROR",
                    format!("changes.{field} must be a string."),
                    json!({ "field": format!("changes.{field}") }),
                ));
            }
            changes.insert(field.to_string(), value.clone());
        }
    }
    if let Some(value) = raw.get("proxy_spec") {
        changes.insert("proxy_spec".into(), value.clone());
    }
    if let Some(tags) =
        domain_tags_from_value(tool, raw.get("domain_tags").or_else(|| raw.get("tags")))?
    {
        changes.insert("domain_tags".into(), tags);
    }
    if let Some(value) = raw.get("produced_by") {
        changes.insert("produced_by".into(), value.clone());
    }
    Ok(Value::Object(changes))
}

fn optional_domain_tags_value(
    tool: &str,
    arguments: &JsonObject,
) -> std::result::Result<Option<Value>, AdminError> {
    domain_tags_from_value(
        tool,
        arguments
            .get("domain_tags")
            .or_else(|| arguments.get("tags")),
    )
}

fn domain_tags_from_value(
    tool: &str,
    value: Option<&Value>,
) -> std::result::Result<Option<Value>, AdminError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(tags) = value.as_array() else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "domain_tags must be an array of strings.",
            json!({ "field": "domain_tags" }),
        ));
    };
    let mut normalized = Vec::new();
    for tag in tags {
        let Some(raw) = tag.as_str() else {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "domain_tags must be an array of strings.",
                json!({ "field": "domain_tags" }),
            ));
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "domain_tags must not contain empty strings.",
                json!({ "field": "domain_tags" }),
            ));
        }
        normalized.push(Value::String(trimmed.to_string()));
    }
    if normalized.is_empty() {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "domain_tags must contain at least one tag.",
            json!({ "field": "domain_tags" }),
        ));
    }
    Ok(Some(Value::Array(normalized)))
}

fn build_battle_prepare_plan(
    spec: &AdminToolSpec,
    request: &BattlePrepareRequest,
    current: Value,
) -> Value {
    let current_values = if current.is_null() {
        Value::Null
    } else {
        battle_current_values(&current)
    };
    let diff = if current_values.is_null() {
        diff_from_requested_changes(&request.requested_changes)
    } else {
        diff_from_current_values(&current_values, &request.requested_changes)
    };
    json!({
        "operation": operation_name(spec.name),
        "target": {
            "type": "battle",
            "id": current.get("id").cloned().or_else(|| request.id.clone().map(Value::String)).unwrap_or(Value::Null),
            "slug": current.get("slug").cloned().unwrap_or(Value::Null),
            "title": current.get("title").cloned().unwrap_or(Value::Null),
            "kind": if request.id.is_some() { "existing" } else { "new" },
        },
        "current_values": current_values,
        "requested_changes": request.requested_changes.clone(),
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

fn battle_current_values(current: &Value) -> Value {
    json!({
        "title": current.get("title").cloned().unwrap_or(Value::Null),
        "description": current.get("description").cloned().unwrap_or(Value::Null),
        "domain_tags": current.get("domain_tags").cloned().unwrap_or(Value::Null),
        "status": current.get("status").cloned().unwrap_or(Value::Null),
        "proxy_spec": current.get("proxy_spec").cloned().unwrap_or(Value::Null),
        "produced_by": current.get("produced_by").cloned().unwrap_or(Value::Null),
        "solutions": current.get("solutions").cloned().unwrap_or(Value::Null),
        "summaries": current.get("summaries").cloned().unwrap_or(Value::Null),
        "counts": current.get("counts").cloned().unwrap_or(Value::Null),
        "deleted": current.get("deleted_at").is_some_and(|deleted_at| !deleted_at.is_null()),
        "deleted_at": current.get("deleted_at").cloned().unwrap_or(Value::Null),
        "warnings": current.get("warnings").cloned().unwrap_or_else(|| json!([])),
        "risk_signals": current.get("risk_signals").cloned().unwrap_or_else(|| json!([])),
    })
}
