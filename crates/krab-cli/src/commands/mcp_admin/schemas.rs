fn input_schema(kind: SchemaKind) -> JsonObject {
    match kind {
        SchemaKind::ClaimList => schema(
            json!({
                "filter": {
                    "type": "object",
                    "description": "Optional nested filters; direct top-level filter fields are also accepted.",
                    "additionalProperties": true
                },
                "battle": {
                    "type": "string",
                    "description": "Optional Battle UUID, slug, or URL filter."
                },
                "author": {
                    "type": "string",
                    "description": "Optional author UUID or exact email filter."
                },
                "visibility": {
                    "type": "string",
                    "enum": ["public", "private", "adopted"],
                    "description": "Optional exact Claim visibility filter."
                },
                "created_after": {
                    "type": "string",
                    "description": "Optional RFC3339 lower bound for creation time."
                },
                "created_before": {
                    "type": "string",
                    "description": "Optional RFC3339 upper bound for creation time."
                },
                "review_verdict": {
                    "type": "string",
                    "enum": ["pass", "warn", "block"],
                    "description": "Optional latest review verdict filter."
                },
                "has_warnings": {
                    "type": "boolean",
                    "description": "Filter Claims with or without review warnings/moderation flags."
                },
                "standalone": {
                    "type": "boolean",
                    "description": "true for standalone Claims, false for Battle-attached Claims."
                },
                "without_verifications": {
                    "type": "boolean",
                    "description": "When true, return Claims with no verification rows."
                },
                "include_deleted": {
                    "type": "boolean",
                    "description": "Include soft-deleted Claims."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "description": "Maximum number of Claims to return."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Zero-based result offset."
                },
                "count_only": {
                    "type": "boolean",
                    "description": "When true, return only pagination.count and no items."
                },
                "order_by": {
                    "type": "string",
                    "description": "Comma-separated sort fields using optional +/- prefixes. Valid fields: created_at, updated_at, title, visibility, review_verdict, battle_title, author, id."
                }
            }),
            &[],
        ),
        SchemaKind::BattleList => schema(
            json!({
                "filter": {
                    "type": "object",
                    "description": "Optional nested filters; direct top-level filter fields are also accepted.",
                    "additionalProperties": true
                },
                "status": {
                    "type": "string",
                    "description": "Optional Battle status filter. Legacy contested/holding values are normalized by the API."
                },
                "domain_tag": {
                    "type": "string",
                    "description": "Optional exact domain tag filter."
                },
                "has_claims": {
                    "type": "boolean",
                    "description": "Filter Battles with or without Claims."
                },
                "missing_solutions": {
                    "type": "boolean",
                    "description": "Filter Battles by whether their solutions list is empty."
                },
                "stale_summary": {
                    "type": "boolean",
                    "description": "Filter Battles missing generated summary fields."
                },
                "updated_after": {
                    "type": "string",
                    "description": "Optional RFC3339 lower bound for updated_at."
                },
                "updated_before": {
                    "type": "string",
                    "description": "Optional RFC3339 upper bound for updated_at."
                },
                "include_deleted": {
                    "type": "boolean",
                    "description": "Include soft-deleted Battles."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "description": "Maximum number of Battles to return."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Zero-based result offset."
                },
                "count_only": {
                    "type": "boolean",
                    "description": "When true, return only pagination.count and no items."
                },
                "order_by": {
                    "type": "string",
                    "description": "Comma-separated sort fields using optional +/- prefixes. Valid fields: created_at, updated_at, title, status, claim_count, id."
                }
            }),
            &[],
        ),
        SchemaKind::UserList => schema(
            json!({
                "filter": {
                    "type": "object",
                    "description": "Optional nested filters; direct top-level filter fields are also accepted.",
                    "additionalProperties": true
                },
                "email": {
                    "type": "string",
                    "description": "Optional exact user email filter."
                },
                "role": {
                    "type": "string",
                    "enum": ["admin", "editor", "moderator", "user"],
                    "description": "Optional exact user role filter."
                },
                "entity_id": {
                    "type": "string",
                    "description": "Optional linked entity UUID filter."
                },
                "entity_domain": {
                    "type": "string",
                    "description": "Optional linked entity domain filter."
                },
                "org_role": {
                    "type": "string",
                    "enum": ["admin", "member"],
                    "description": "Optional organization role filter."
                },
                "kind": {
                    "type": "string",
                    "enum": ["human", "bot"],
                    "description": "Optional entity kind filter."
                },
                "suspended": {
                    "type": "boolean",
                    "description": "Filter currently suspended or active users."
                },
                "created_after": {
                    "type": "string",
                    "description": "Optional RFC3339 lower bound for creation time."
                },
                "created_before": {
                    "type": "string",
                    "description": "Optional RFC3339 upper bound for creation time."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "description": "Maximum number of users to return."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Zero-based result offset."
                },
                "count_only": {
                    "type": "boolean",
                    "description": "When true, return only pagination.count and no items."
                },
                "order_by": {
                    "type": "string",
                    "description": "Comma-separated sort fields using optional +/- prefixes. Valid fields: created_at, updated_at, email, role, entity_domain, org_role, kind, suspended, user_id."
                }
            }),
            &[],
        ),
        SchemaKind::Inspect => schema(
            json!({
                "id": {
                    "type": "string",
                    "description": "Object id, slug, or URL to inspect."
                }
            }),
            &["id"],
        ),
        SchemaKind::PrepareMutation => schema(
            json!({
                "id": {
                    "type": "string",
                    "description": "Object id, slug, or URL to prepare a change for."
                },
                "changes": {
                    "type": "object",
                    "description": "Requested change set.",
                    "additionalProperties": true
                }
            }),
            &["id"],
        ),
        SchemaKind::ExecuteMutation => schema(
            json!({
                "id": {
                    "type": "string",
                    "description": "Object id, slug, or URL to change."
                },
                "changes": {
                    "type": "object",
                    "description": "Confirmed change set.",
                    "additionalProperties": true
                },
                "reason": {
                    "type": "string",
                    "description": "Administrative reason for this action."
                },
                "confirmed": {
                    "type": "boolean",
                    "description": "Must be true only after the administrator has explicitly approved the action."
                },
                "self_change_confirmed": {
                    "type": "boolean",
                    "description": "Required only for self role/suspension changes after explicit administrator approval."
                }
            }),
            &["id", "reason", "confirmed"],
        ),
        SchemaKind::PrepareCreateBattle => schema(
            json!({
                "title": {
                    "type": "string",
                    "description": "Battle title."
                },
                "description": {
                    "type": "string",
                    "description": "Battle description."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Alias for domain_tags."
                },
                "domain_tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Battle domain tags."
                },
                "parent_id": {
                    "type": "string",
                    "description": "Optional parent Battle UUID."
                },
                "proxy_spec": {
                    "type": "object",
                    "description": "Optional Battle proxy spec.",
                    "additionalProperties": true
                },
                "status": {
                    "type": "string",
                    "description": "Optional initial Battle status."
                },
                "produced_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional producer slugs (e.g. [\"fermatix\"]) labelling the Battle as 'produced by' registered brands. Admin-only."
                }
            }),
            &["title", "description"],
        ),
        SchemaKind::ExecuteCreateBattle => schema(
            json!({
                "title": {
                    "type": "string",
                    "description": "Battle title."
                },
                "description": {
                    "type": "string",
                    "description": "Battle description."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Alias for domain_tags."
                },
                "domain_tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Battle domain tags."
                },
                "parent_id": {
                    "type": "string",
                    "description": "Optional parent Battle UUID."
                },
                "proxy_spec": {
                    "type": "object",
                    "description": "Optional Battle proxy spec.",
                    "additionalProperties": true
                },
                "status": {
                    "type": "string",
                    "description": "Optional initial Battle status."
                },
                "produced_by": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional producer slugs (e.g. [\"fermatix\"]) labelling the Battle as 'produced by' registered brands. Admin-only."
                },
                "reason": {
                    "type": "string",
                    "description": "Administrative reason for creating the Battle."
                },
                "confirmed": {
                    "type": "boolean",
                    "description": "Must be true only after the administrator has explicitly approved creation."
                }
            }),
            &["title", "description", "reason", "confirmed"],
        ),
    }
}

fn schema(properties: Value, required: &[&str]) -> JsonObject {
    let mut root = object(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties
    }));
    if !required.is_empty() {
        root.insert(
            "required".to_string(),
            Value::Array(
                required
                    .iter()
                    .map(|field| Value::String((*field).to_string()))
                    .collect(),
            ),
        );
    }
    root
}
