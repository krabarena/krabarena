fn claim_list_path(arguments: &JsonObject) -> std::result::Result<String, AdminError> {
    let tool = "admin_claim_list";
    let mut query = Vec::new();
    for field in [
        "battle",
        "author",
        "visibility",
        "created_after",
        "created_before",
        "review_verdict",
    ] {
        if let Some(value) = optional_filter_string(tool, arguments, field)? {
            if field == "visibility" && !["public", "private", "adopted"].contains(&value.as_str())
            {
                return Err(AdminError::new(
                    tool,
                    "VALIDATION_ERROR",
                    "visibility must be one of public, private, adopted.",
                    json!({ "field": field, "value": value }),
                ));
            }
            if field == "review_verdict" && !["pass", "warn", "block"].contains(&value.as_str()) {
                return Err(AdminError::new(
                    tool,
                    "VALIDATION_ERROR",
                    "review_verdict must be one of pass, warn, block.",
                    json!({ "field": field, "value": value }),
                ));
            }
            query.push(format!("{field}={}", urlencoding::encode(&value)));
        }
    }

    for field in [
        "has_warnings",
        "standalone",
        "without_verifications",
        "include_deleted",
    ] {
        if let Some(value) = optional_filter_bool(tool, arguments, field)? {
            query.push(format!("{field}={value}"));
        }
    }

    append_admin_list_params(tool, arguments, &mut query, CLAIM_ORDER_BY_FIELDS)?;

    if query.is_empty() {
        Ok("/v1/admin/claims".to_string())
    } else {
        Ok(format!("/v1/admin/claims?{}", query.join("&")))
    }
}

const CLAIM_ORDER_BY_FIELDS: &[&str] = &[
    "created_at",
    "updated_at",
    "title",
    "visibility",
    "review_verdict",
    "battle_title",
    "author",
    "id",
];

const BATTLE_ORDER_BY_FIELDS: &[&str] = &[
    "created_at",
    "updated_at",
    "title",
    "status",
    "claim_count",
    "id",
];

const USER_ORDER_BY_FIELDS: &[&str] = &[
    "created_at",
    "updated_at",
    "email",
    "role",
    "entity_domain",
    "org_role",
    "kind",
    "suspended",
    "user_id",
];

fn claim_inspect_path(id: &str) -> String {
    let normalized = normalize_path_param_value(id);
    format!("/v1/admin/claims/{}", urlencoding::encode(&normalized))
}

fn optional_filter_string(
    tool: &str,
    arguments: &JsonObject,
    field: &str,
) -> std::result::Result<Option<String>, AdminError> {
    match filter_value(arguments, field) {
        Some(value) => value
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| {
                AdminError::new(
                    tool,
                    "VALIDATION_ERROR",
                    format!("{field} must be a string."),
                    json!({ "field": field }),
                )
            }),
        None => Ok(None),
    }
}

fn optional_filter_bool(
    tool: &str,
    arguments: &JsonObject,
    field: &str,
) -> std::result::Result<Option<bool>, AdminError> {
    match filter_value(arguments, field) {
        Some(value) => value.as_bool().map(Some).ok_or_else(|| {
            AdminError::new(
                tool,
                "VALIDATION_ERROR",
                format!("{field} must be a boolean."),
                json!({ "field": field }),
            )
        }),
        None => Ok(None),
    }
}

fn optional_bool_argument(
    tool: &str,
    arguments: &JsonObject,
    field: &str,
) -> std::result::Result<Option<bool>, AdminError> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    value.as_bool().map(Some).ok_or_else(|| {
        AdminError::new(
            tool,
            "VALIDATION_ERROR",
            format!("{field} must be a boolean."),
            json!({ "field": field }),
        )
    })
}

fn optional_filter_i64(
    tool: &str,
    arguments: &JsonObject,
    field: &str,
) -> std::result::Result<Option<i64>, AdminError> {
    if arguments.contains_key(field) {
        return optional_i64(tool, arguments, field);
    }
    let Some(filter) = arguments.get("filter").and_then(Value::as_object) else {
        return Ok(None);
    };
    optional_i64(tool, filter, field)
}

fn append_admin_list_params(
    tool: &str,
    arguments: &JsonObject,
    query: &mut Vec<String>,
    allowed_order_fields: &[&str],
) -> std::result::Result<(), AdminError> {
    if let Some(limit) = optional_filter_i64(tool, arguments, "limit")? {
        if !(1..=200).contains(&limit) {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "limit must be between 1 and 200.",
                json!({ "field": "limit", "value": limit }),
            ));
        }
        query.push(format!("limit={limit}"));
    }
    if let Some(offset) = optional_filter_i64(tool, arguments, "offset")? {
        if offset < 0 {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "offset must be greater than or equal to 0.",
                json!({ "field": "offset", "value": offset }),
            ));
        }
        query.push(format!("offset={offset}"));
    }
    if let Some(count_only) = optional_filter_bool(tool, arguments, "count_only")? {
        query.push(format!("count_only={count_only}"));
    }
    if let Some(order_by) = optional_filter_string(tool, arguments, "order_by")? {
        validate_admin_order_by_arg(tool, &order_by, allowed_order_fields)?;
        query.push(format!("order_by={}", urlencoding::encode(&order_by)));
    }
    Ok(())
}

fn validate_admin_order_by_arg(
    tool: &str,
    order_by: &str,
    allowed_fields: &[&str],
) -> std::result::Result<(), AdminError> {
    for token in order_by.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "order_by must not contain empty sort fields.",
                json!({ "field": "order_by" }),
            ));
        }
        let field = match token.as_bytes()[0] {
            b'+' | b'-' => token[1..].trim(),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => token,
            _ => {
                return Err(AdminError::new(
                    tool,
                    "VALIDATION_ERROR",
                    "order_by sort tokens must start with '+', '-', or a field name.",
                    json!({ "field": "order_by", "value": token }),
                ));
            }
        };
        if field.is_empty() || !allowed_fields.contains(&field) {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "order_by contains an unsupported sort field.",
                json!({ "field": "order_by", "value": token, "allowed": allowed_fields }),
            ));
        }
    }
    Ok(())
}

fn filter_value<'a>(arguments: &'a JsonObject, field: &str) -> Option<&'a Value> {
    arguments.get(field).or_else(|| {
        arguments
            .get("filter")
            .and_then(Value::as_object)
            .and_then(|filter| filter.get(field))
    })
}

fn claim_prepare_request(
    spec: &AdminToolSpec,
    arguments: &JsonObject,
) -> std::result::Result<ClaimPrepareRequest, AdminError> {
    let id = required_string(spec.name, arguments, "id")?;
    let changes = optional_object_value(spec.name, arguments, "changes")?;
    let requested_changes = match spec.name {
        "admin_claim_prepare_update" => {
            if changes
                .as_object()
                .map(serde_json::Map::is_empty)
                .unwrap_or(true)
            {
                return Err(AdminError::new(
                    spec.name,
                    "VALIDATION_ERROR",
                    "changes must contain at least one Claim field for update.",
                    json!({ "field": "changes" }),
                ));
            }
            changes
        }
        "admin_claim_prepare_visibility_change" => {
            let visibility = required_change_string(spec.name, &changes, "visibility")?;
            if !["public", "private"].contains(&visibility.as_str()) {
                return Err(AdminError::new(
                    spec.name,
                    "VALIDATION_ERROR",
                    "visibility must be public or private.",
                    json!({ "field": "changes.visibility", "value": visibility }),
                ));
            }
            json!({ "visibility": visibility })
        }
        "admin_claim_prepare_recheck" => json!({ "recheck": true }),
        "admin_claim_prepare_delete" => json!({ "deleted": true }),
        "admin_claim_prepare_restore" => json!({ "deleted": false }),
        _ => changes,
    };
    Ok(ClaimPrepareRequest {
        id,
        requested_changes,
    })
}

fn build_claim_prepare_plan(
    spec: &AdminToolSpec,
    request: &ClaimPrepareRequest,
    current: Value,
) -> Value {
    let current_values = claim_current_values(&current);
    let diff = diff_from_current_values(&current_values, &request.requested_changes);
    json!({
        "operation": operation_name(spec.name),
        "target": {
            "type": "claim",
            "id": current.get("id").cloned().unwrap_or_else(|| Value::String(request.id.clone())),
            "slug": current.get("slug").cloned().unwrap_or(Value::Null),
            "title": current.get("title").cloned().unwrap_or(Value::Null),
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

fn claim_current_values(current: &Value) -> Value {
    json!({
        "title": current.get("title").cloned().unwrap_or(Value::Null),
        "claim": current.get("claim").cloned().unwrap_or(Value::Null),
        "claim_structure": current.get("claim_structure").cloned().unwrap_or(Value::Null),
        "topic": current.get("topic").cloned().unwrap_or(Value::Null),
        "topic_tags": current.get("topic_tags").cloned().unwrap_or(Value::Null),
        "produced_by": current.get("produced_by").cloned().unwrap_or(Value::Null),
        "visibility": current.get("visibility").cloned().unwrap_or(Value::Null),
        "deleted": current.get("deleted_at").is_some_and(|deleted_at| !deleted_at.is_null()),
        "deleted_at": current.get("deleted_at").cloned().unwrap_or(Value::Null),
        "latest_review": current.get("latest_review").cloned().unwrap_or(Value::Null),
        "warnings": current.get("warnings").cloned().unwrap_or_else(|| json!([])),
        "risk_signals": current.get("risk_signals").cloned().unwrap_or_else(|| json!([])),
    })
}

fn diff_from_current_values(current_values: &Value, requested_changes: &Value) -> Value {
    let Some(changes) = requested_changes.as_object() else {
        return json!([]);
    };
    Value::Array(
        changes
            .iter()
            .map(|(field, requested)| {
                json!({
                    "field": field,
                    "current": current_values.get(field).cloned().unwrap_or(Value::Null),
                    "requested": requested,
                    "status": "pending_confirmation"
                })
            })
            .collect(),
    )
}

fn required_change_string(
    tool: &str,
    changes: &Value,
    field: &str,
) -> std::result::Result<String, AdminError> {
    let Some(value) = changes.get(field) else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            format!("changes.{field} is required."),
            json!({ "field": format!("changes.{field}") }),
        ));
    };
    let Some(raw) = value.as_str() else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            format!("changes.{field} must be a string."),
            json!({ "field": format!("changes.{field}") }),
        ));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            format!("changes.{field} must not be empty."),
            json!({ "field": format!("changes.{field}") }),
        ));
    }
    Ok(trimmed.to_string())
}
