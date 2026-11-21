fn required_string(
    tool: &str,
    arguments: &JsonObject,
    field: &str,
) -> std::result::Result<String, AdminError> {
    let Some(value) = arguments.get(field) else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            format!("{field} is required."),
            json!({ "field": field }),
        ));
    };
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
    Ok(trimmed.to_string())
}

fn optional_string(
    tool: &str,
    arguments: &JsonObject,
    field: &str,
) -> std::result::Result<Option<String>, AdminError> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
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
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn optional_i64(
    tool: &str,
    arguments: &JsonObject,
    field: &str,
) -> std::result::Result<Option<i64>, AdminError> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    if let Some(raw) = value.as_i64() {
        return Ok(Some(raw));
    }
    if let Some(raw) = value.as_u64() {
        return raw.try_into().map(Some).map_err(|_| {
            AdminError::new(
                tool,
                "VALIDATION_ERROR",
                format!("{field} must fit into a signed 64-bit integer."),
                json!({ "field": field }),
            )
        });
    }
    if let Some(raw) = value.as_str() {
        return raw.trim().parse::<i64>().map(Some).map_err(|_| {
            AdminError::new(
                tool,
                "VALIDATION_ERROR",
                format!("{field} must be an integer."),
                json!({ "field": field }),
            )
        });
    }
    Err(AdminError::new(
        tool,
        "VALIDATION_ERROR",
        format!("{field} must be an integer."),
        json!({ "field": field }),
    ))
}

fn prepare_plan(
    spec: &AdminToolSpec,
    arguments: &JsonObject,
) -> std::result::Result<Value, AdminError> {
    match spec.schema {
        SchemaKind::PrepareMutation => {
            let id = required_string(spec.name, arguments, "id")?;
            let requested_changes = optional_object_value(spec.name, arguments, "changes")?;
            Ok(build_prepare_plan(
                spec,
                json!({
                    "type": entity_type(spec.name),
                    "id": id
                }),
                requested_changes,
            ))
        }
        SchemaKind::PrepareCreateBattle => {
            let title = required_string(spec.name, arguments, "title")?;
            let description = required_string(spec.name, arguments, "description")?;
            let tags = optional_tags_value(spec.name, arguments)?;
            let mut requested_changes = serde_json::Map::new();
            requested_changes.insert("title".to_string(), Value::String(title));
            requested_changes.insert("description".to_string(), Value::String(description));
            if let Some(tags) = tags {
                requested_changes.insert("tags".to_string(), tags);
            }
            Ok(build_prepare_plan(
                spec,
                json!({
                    "type": "battle",
                    "id": null,
                    "kind": "new"
                }),
                Value::Object(requested_changes),
            ))
        }
        _ => Err(AdminError::new(
            spec.name,
            "VALIDATION_ERROR",
            "This tool does not support prepare input.",
            json!({ "schema": format!("{:?}", spec.schema) }),
        )),
    }
}

fn validate_execute_request(
    spec: &AdminToolSpec,
    arguments: &JsonObject,
) -> std::result::Result<Value, AdminError> {
    match spec.schema {
        SchemaKind::ExecuteMutation => {
            let id = required_string(spec.name, arguments, "id")?;
            let changes = optional_object_value(spec.name, arguments, "changes")?;
            let reason = required_reason(spec.name, arguments)?;
            require_confirmed(spec.name, arguments)?;
            Ok(json!({
                "target": {
                    "type": entity_type(spec.name),
                    "id": id
                },
                "requested_changes": changes,
                "reason": reason,
                "confirmed": true,
            }))
        }
        SchemaKind::ExecuteCreateBattle => {
            let title = required_string(spec.name, arguments, "title")?;
            let description = required_string(spec.name, arguments, "description")?;
            let tags = optional_tags_value(spec.name, arguments)?;
            let reason = required_reason(spec.name, arguments)?;
            require_confirmed(spec.name, arguments)?;

            let mut requested_changes = serde_json::Map::new();
            requested_changes.insert("title".to_string(), Value::String(title));
            requested_changes.insert("description".to_string(), Value::String(description));
            if let Some(tags) = tags {
                requested_changes.insert("tags".to_string(), tags);
            }

            Ok(json!({
                "target": {
                    "type": "battle",
                    "id": null,
                    "kind": "new"
                },
                "requested_changes": Value::Object(requested_changes),
                "reason": reason,
                "confirmed": true,
            }))
        }
        _ => Err(AdminError::new(
            spec.name,
            "VALIDATION_ERROR",
            "This tool does not support execute input.",
            json!({ "schema": format!("{:?}", spec.schema) }),
        )),
    }
}

fn optional_object_value(
    tool: &str,
    arguments: &JsonObject,
    field: &str,
) -> std::result::Result<Value, AdminError> {
    let Some(value) = arguments.get(field) else {
        return Ok(json!({}));
    };
    if value.is_object() {
        return Ok(value.clone());
    }
    Err(AdminError::new(
        tool,
        "VALIDATION_ERROR",
        format!("{field} must be an object."),
        json!({ "field": field }),
    ))
}

fn optional_tags_value(
    tool: &str,
    arguments: &JsonObject,
) -> std::result::Result<Option<Value>, AdminError> {
    let Some(value) = arguments.get("tags") else {
        return Ok(None);
    };
    let Some(tags) = value.as_array() else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "tags must be an array of strings.",
            json!({ "field": "tags" }),
        ));
    };
    let mut normalized = Vec::new();
    for tag in tags {
        let Some(raw) = tag.as_str() else {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "tags must be an array of strings.",
                json!({ "field": "tags" }),
            ));
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AdminError::new(
                tool,
                "VALIDATION_ERROR",
                "tags must not contain empty strings.",
                json!({ "field": "tags" }),
            ));
        }
        normalized.push(Value::String(trimmed.to_string()));
    }
    Ok(Some(Value::Array(normalized)))
}

fn required_reason(tool: &str, arguments: &JsonObject) -> std::result::Result<String, AdminError> {
    let Some(value) = arguments.get("reason") else {
        return Err(AdminError::new(
            tool,
            "REASON_REQUIRED",
            "Administrative reason is required before executing this action.",
            json!({ "field": "reason" }),
        ));
    };
    let Some(raw) = value.as_str() else {
        return Err(AdminError::new(
            tool,
            "VALIDATION_ERROR",
            "reason must be a string.",
            json!({ "field": "reason" }),
        ));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AdminError::new(
            tool,
            "REASON_REQUIRED",
            "Administrative reason is required before executing this action.",
            json!({ "field": "reason" }),
        ));
    }
    Ok(trimmed.to_string())
}

fn require_confirmed(tool: &str, arguments: &JsonObject) -> std::result::Result<(), AdminError> {
    if arguments
        .get("confirmed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(());
    }
    Err(AdminError::new(
        tool,
        "CONFIRMATION_REQUIRED",
        "confirmed=true is required after explicit administrator approval.",
        json!({ "field": "confirmed" }),
    ))
}

