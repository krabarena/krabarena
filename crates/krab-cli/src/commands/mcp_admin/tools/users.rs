fn user_inspect_path(id: &str) -> String {
    format!("/v1/admin/users/{}", urlencoding::encode(id))
}

fn user_prepare_request(
    spec: &AdminToolSpec,
    arguments: &JsonObject,
) -> std::result::Result<UserPrepareRequest, AdminError> {
    let id = required_string(spec.name, arguments, "id")?;
    let requested_changes = match spec.name {
        "admin_user_prepare_update" => user_update_changes(spec.name, arguments)?,
        "admin_user_prepare_role_change" => user_role_change_changes(spec.name, arguments)?,
        "admin_user_prepare_suspension" => user_suspension_changes(spec.name, arguments)?,
        _ => {
            return Err(AdminError::new(
                spec.name,
                "NOT_IMPLEMENTED",
                "User prepare tool is registered but not implemented.",
                json!({ "tool": spec.name }),
            ));
        }
    };
    Ok(UserPrepareRequest {
        id,
        requested_changes,
    })
}

fn user_execute_request(
    spec: &AdminToolSpec,
    arguments: &JsonObject,
) -> std::result::Result<UserExecuteRequest, AdminError> {
    let reason = required_reason(spec.name, arguments)?;
    require_confirmed(spec.name, arguments)?;
    let id = required_string(spec.name, arguments, "id")?;
    let requested_changes = match spec.name {
        "admin_user_execute_update" => user_update_changes(spec.name, arguments)?,
        "admin_user_execute_role_change" => user_role_change_changes(spec.name, arguments)?,
        "admin_user_execute_suspension" => user_suspension_changes(spec.name, arguments)?,
        _ => {
            return Err(AdminError::new(
                spec.name,
                "NOT_IMPLEMENTED",
                "User execute tool is registered but not implemented.",
                json!({ "tool": spec.name }),
            ));
        }
    };
    Ok(UserExecuteRequest {
        id,
        requested_changes,
        reason,
        self_change_confirmed: optional_bool_argument(
            spec.name,
            arguments,
            "self_change_confirmed",
        )?
        .unwrap_or(false),
    })
}

fn user_update_changes(
    tool: &str,
    arguments: &JsonObject,
) -> std::result::Result<Value, AdminError> {
    let mut changes = serde_json::Map::new();
    let raw_changes = optional_object_value(tool, arguments, "changes")?;
    let raw = raw_changes.as_object();

    for field in ["display_name", "email", "entity_id"] {
        let value = raw
            .and_then(|changes| changes.get(field))
            .or_else(|| arguments.get(field));
        let Some(value) = value else {
            continue;
        };
        match field {
            "display_name" | "email" | "entity_id" => {
                let Some(raw) = value.as_str() else {
                    return Err(AdminError::new(
                        tool,
                        "VALIDATION_ERROR",
                        format!("{field} must be a string."),
                        json!({ "field": field }),
                    ));
                };
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(AdminError::new(
                        tool,
                        "VALIDATION_ERROR",
                        format!("{field} must not be empty."),
                        json!({ "field": field }),
                    ));
                }
                changes.insert(field.to_string(), Value::String(trimmed.to_string()));
            }
            _ => {}
        }
    }

    if changes.is_empty() {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "changes must contain at least one user field for update.",
            json!({ "field": "changes" }),
        ));
    }
    Ok(Value::Object(changes))
}

fn user_role_change_changes(
    tool: &str,
    arguments: &JsonObject,
) -> std::result::Result<Value, AdminError> {
    let raw_changes = optional_object_value(tool, arguments, "changes")?;
    let role_value = raw_changes
        .get("role")
        .or_else(|| arguments.get("role"))
        .ok_or_else(|| {
            AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "role is required.",
                json!({ "field": "role" }),
            )
        })?;
    let Some(role) = role_value.as_str() else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "role must be a string.",
            json!({ "field": "role" }),
        ));
    };
    let role = role.trim();
    const VALID_ROLES: &[&str] = &["admin", "editor", "moderator", "user"];
    if !VALID_ROLES.contains(&role) {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "role must be one of admin, editor, moderator, user.",
            json!({ "field": "role", "value": role }),
        ));
    }
    Ok(json!({ "role": role }))
}

fn user_suspension_changes(
    tool: &str,
    arguments: &JsonObject,
) -> std::result::Result<Value, AdminError> {
    let raw_changes = optional_object_value(tool, arguments, "changes")?;
    let raw = raw_changes.as_object();
    let lift = raw
        .and_then(|changes| changes.get("lift_suspension"))
        .or_else(|| arguments.get("lift_suspension"));
    if let Some(value) = lift {
        let Some(lift_suspension) = value.as_bool() else {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "lift_suspension must be a boolean.",
                json!({ "field": "lift_suspension" }),
            ));
        };
        if lift_suspension {
            return Ok(json!({ "lift_suspension": true }));
        }
    }

    let reason_value = raw
        .and_then(|changes| changes.get("suspend_reason"))
        .or_else(|| arguments.get("suspend_reason"));
    let Some(value) = reason_value else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "suspend_reason or lift_suspension=true is required.",
            json!({ "field": "suspend_reason" }),
        ));
    };
    let Some(reason) = value.as_str() else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "suspend_reason must be a string.",
            json!({ "field": "suspend_reason" }),
        ));
    };
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "suspend_reason must not be empty.",
            json!({ "field": "suspend_reason" }),
        ));
    }
    Ok(json!({ "suspend_reason": reason }))
}

fn build_user_prepare_plan(
    spec: &AdminToolSpec,
    request: &UserPrepareRequest,
    current: Value,
    admin_user_id: Option<&str>,
) -> Value {
    let current_values = user_current_values(&current);
    let diff = diff_from_current_values(&current_values, &request.requested_changes);
    let self_change_required =
        user_self_change_requires_extra_confirmation(spec.name, &current, admin_user_id);
    json!({
        "operation": operation_name(spec.name),
        "target": {
            "type": "user",
            "id": current.get("user_id").cloned().unwrap_or_else(|| Value::String(request.id.clone())),
            "email": current.get("email").cloned().unwrap_or(Value::Null),
            "display_name": current.get("display_name").cloned().unwrap_or(Value::Null),
            "entity_id": current.get("entity_id").cloned().unwrap_or(Value::Null),
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
            "self_change_confirmed": self_change_required,
            "policy": if self_change_required {
                "Only set confirmed=true and self_change_confirmed=true after explicit administrator approval for this self-affecting action."
            } else {
                "Only set confirmed=true after explicit administrator approval."
            }
        },
        "mutation_performed": false,
    })
}

fn user_current_values(current: &Value) -> Value {
    json!({
        "display_name": current.get("display_name").cloned().unwrap_or(Value::Null),
        "email": current.get("email").cloned().unwrap_or(Value::Null),
        "entity_id": current.get("entity_id").cloned().unwrap_or(Value::Null),
        "entity_name": current.get("entity_name").cloned().unwrap_or(Value::Null),
        "entity_domain": current.get("entity_domain").cloned().unwrap_or(Value::Null),
        "role": current.get("role").cloned().unwrap_or(Value::Null),
        "org_role": current.get("org_role").cloned().unwrap_or(Value::Null),
        "kind": current.get("kind").cloned().unwrap_or(Value::Null),
        "suspended_until": current.get("suspended_until").cloned().unwrap_or(Value::Null),
        "suspension_reason": current.get("suspension_reason").cloned().unwrap_or(Value::Null),
        "rate_limit_until": current.get("rate_limit_until").cloned().unwrap_or(Value::Null),
        "claim_count": current.get("claim_count").cloned().unwrap_or(Value::Null),
        "battle_count": current.get("battle_count").cloned().unwrap_or(Value::Null),
        "warnings": current.get("warnings").cloned().unwrap_or_else(|| json!([])),
        "risk_signals": current.get("risk_signals").cloned().unwrap_or_else(|| json!([])),
    })
}

fn user_self_change_requires_extra_confirmation(
    tool: &str,
    current: &Value,
    admin_user_id: Option<&str>,
) -> bool {
    if !matches!(
        tool,
        "admin_user_prepare_role_change"
            | "admin_user_execute_role_change"
            | "admin_user_prepare_suspension"
            | "admin_user_execute_suspension"
    ) {
        return false;
    }
    let Some(admin_user_id) = admin_user_id else {
        return false;
    };
    current
        .get("user_id")
        .and_then(Value::as_str)
        .is_some_and(|target_user_id| target_user_id == admin_user_id)
}

fn requested_change_can_remove_admin(requested_changes: &Value) -> bool {
    if let Some(role) = requested_changes.get("role").and_then(Value::as_str) {
        return role != "admin";
    }
    requested_changes
        .get("suspend_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.trim().is_empty())
}

fn user_id_from_current(tool: &str, current: &Value) -> std::result::Result<String, AdminError> {
    current
        .get("user_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "admin user inspect response did not include user_id.",
                json!({ "field": "user_id" }),
            )
        })
}

fn user_list_path(arguments: &JsonObject) -> std::result::Result<String, AdminError> {
    let tool = "admin_user_list";
    let mut query = Vec::new();
    for field in [
        "email",
        "role",
        "entity_id",
        "entity_domain",
        "org_role",
        "kind",
        "created_after",
        "created_before",
    ] {
        if let Some(value) = optional_filter_string(tool, arguments, field)? {
            match field {
                "role" if !["admin", "editor", "moderator", "user"].contains(&value.as_str()) => {
                    return Err(AdminError::new(
                        tool,
                        "VALIDATION_ERROR",
                        "role must be one of admin, editor, moderator, user.",
                        json!({ "field": field, "value": value }),
                    ));
                }
                "org_role" if !["admin", "member"].contains(&value.as_str()) => {
                    return Err(AdminError::new(
                        tool,
                        "VALIDATION_ERROR",
                        "org_role must be admin or member.",
                        json!({ "field": field, "value": value }),
                    ));
                }
                "kind" if !["human", "bot"].contains(&value.as_str()) => {
                    return Err(AdminError::new(
                        tool,
                        "VALIDATION_ERROR",
                        "kind must be human or bot.",
                        json!({ "field": field, "value": value }),
                    ));
                }
                _ => {}
            }
            query.push(format!("{field}={}", urlencoding::encode(&value)));
        }
    }
    if let Some(suspended) = optional_filter_bool(tool, arguments, "suspended")? {
        query.push(format!("suspended={suspended}"));
    }
    append_admin_list_params(tool, arguments, &mut query, USER_ORDER_BY_FIELDS)?;

    if query.is_empty() {
        Ok("/v1/admin/users".to_string())
    } else {
        Ok(format!("/v1/admin/users?{}", query.join("&")))
    }
}
