mod tests {
    use super::*;

    #[test]
    fn registers_expected_admin_tools() {
        let names = admin_tool_names();
        assert_eq!(names.len(), 34);
        assert!(names.contains(&"admin_claim_list"));
        assert!(names.contains(&"admin_claim_prepare_restore"));
        assert!(names.contains(&"admin_claim_execute_restore"));
        assert!(names.contains(&"admin_battle_execute_delete"));
        assert!(names.contains(&"admin_battle_prepare_restore"));
        assert!(names.contains(&"admin_battle_execute_restore"));
        assert!(names.contains(&"admin_user_execute_suspension"));
    }

    #[test]
    fn every_admin_tool_has_safety_metadata() {
        for spec in ADMIN_TOOL_SPECS {
            assert!(
                crate::commands::mcp_admin_metadata::has_metadata_for_tool(spec.name),
                "{} is missing safety metadata",
                spec.name
            );
        }
    }

    #[test]
    fn tool_descriptions_include_confirmation_policy() {
        let read = tool_description(tool_spec("admin_claim_list").unwrap()).to_string();
        assert!(read.contains("read-only"));
        assert!(read.contains("does not require confirmation"));

        let prepare =
            tool_description(tool_spec("admin_claim_prepare_delete").unwrap()).to_string();
        assert!(prepare.contains("performs no mutation"));
        assert!(prepare.contains("show its plan"));

        let execute =
            tool_description(tool_spec("admin_claim_execute_delete").unwrap()).to_string();
        assert!(execute.contains("confirmed=true"));
        assert!(execute.contains("direct human approval"));
    }

    #[test]
    fn claim_list_schema_has_admin_filters() {
        let tool = tool_from_spec(
            ADMIN_TOOL_SPECS
                .iter()
                .find(|tool| tool.name == "admin_claim_list")
                .unwrap(),
        );
        let schema = tool.schema_as_json_value();
        assert!(schema["properties"]["battle"].is_object());
        assert!(schema["properties"]["include_deleted"].is_object());
        assert_eq!(
            schema["properties"]["visibility"]["enum"],
            json!(["public", "private", "adopted"])
        );
        assert_eq!(schema["properties"]["limit"]["maximum"], json!(200));
        assert_eq!(schema["properties"]["offset"]["minimum"], json!(0));
        assert!(schema["properties"]["count_only"].is_object());
        assert!(schema["properties"]["order_by"].is_object());
    }

    #[test]
    fn claim_list_path_accepts_direct_and_nested_filters() {
        let args = object(json!({
            "battle": "https://krabarena.com/battles/example-battle",
            "visibility": "public",
            "filter": {
                "include_deleted": true,
                "without_verifications": true
            },
            "limit": 20,
            "offset": 40,
            "count_only": false,
            "order_by": "-created_at,+id"
        }));
        let path = claim_list_path(&args).unwrap();
        assert_eq!(
            path,
            "/v1/admin/claims?battle=https%3A%2F%2Fkrabarena.com%2Fbattles%2Fexample-battle&visibility=public&without_verifications=true&include_deleted=true&limit=20&offset=40&count_only=false&order_by=-created_at%2C%2Bid"
        );

        let args = object(json!({ "visibility": "hidden" }));
        let err = claim_list_path(&args).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");

        let args = object(json!({ "order_by": "-unknown" }));
        let err = claim_list_path(&args).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
    }

    #[test]
    fn battle_list_schema_has_admin_filters() {
        let tool = tool_from_spec(
            ADMIN_TOOL_SPECS
                .iter()
                .find(|tool| tool.name == "admin_battle_list")
                .unwrap(),
        );
        let schema = tool.schema_as_json_value();
        assert!(schema["properties"]["domain_tag"].is_object());
        assert!(schema["properties"]["missing_solutions"].is_object());
        assert!(schema["properties"]["stale_summary"].is_object());
        assert_eq!(schema["properties"]["limit"]["maximum"], json!(200));
        assert_eq!(schema["properties"]["offset"]["minimum"], json!(0));
        assert!(schema["properties"]["count_only"].is_object());
        assert!(schema["properties"]["order_by"].is_object());
    }

    #[test]
    fn battle_list_path_accepts_direct_and_nested_filters() {
        let args = object(json!({
            "status": "open",
            "domain_tag": "rust",
            "filter": {
                "missing_solutions": true,
                "include_deleted": true
            },
            "limit": 20,
            "offset": 10,
            "count_only": true,
            "order_by": "title,-id"
        }));
        let path = battle_list_path(&args).unwrap();
        assert_eq!(
            path,
            "/v1/admin/battles?status=open&domain_tag=rust&missing_solutions=true&include_deleted=true&limit=20&offset=10&count_only=true&order_by=title%2C-id"
        );

        let args = object(json!({ "limit": 201 }));
        let err = battle_list_path(&args).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");

        let args = object(json!({ "order_by": ":title" }));
        let err = battle_list_path(&args).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
    }

    #[test]
    fn battle_create_changes_accepts_domain_tags_aliases() {
        let args = object(json!({
            "title": "New Battle",
            "description": "Description",
            "tags": [" rust ", "cli"],
            "status": "active"
        }));
        let changes = battle_create_changes("admin_battle_prepare_create", &args).unwrap();
        assert_eq!(changes["domain_tags"], json!(["rust", "cli"]));
        assert_eq!(changes["status"], "active");

        let args = object(json!({
            "title": "New Battle",
            "description": "Description"
        }));
        let err = battle_create_changes("admin_battle_prepare_create", &args).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
    }

    #[test]
    fn user_list_schema_has_direct_filters() {
        let tool = tool_from_spec(
            ADMIN_TOOL_SPECS
                .iter()
                .find(|tool| tool.name == "admin_user_list")
                .unwrap(),
        );
        let schema = tool.schema_as_json_value();
        assert!(schema["properties"]["email"].is_object());
        assert_eq!(
            schema["properties"]["role"]["enum"],
            json!(["admin", "editor", "moderator", "user"])
        );
        assert_eq!(schema["properties"]["limit"]["maximum"], json!(200));
        assert_eq!(schema["properties"]["offset"]["minimum"], json!(0));
        assert!(schema["properties"]["count_only"].is_object());
        assert!(schema["properties"]["order_by"].is_object());
    }

    #[test]
    fn user_list_path_validates_arguments() {
        let args = object(json!({
            "email": "admin@example.com",
            "role": "admin",
            "limit": 25,
            "offset": 5,
            "count_only": false,
            "order_by": "-created_at,+user_id"
        }));
        let path = user_list_path(&args).unwrap();
        assert_eq!(
            path,
            "/v1/admin/users?email=admin%40example.com&role=admin&limit=25&offset=5&count_only=false&order_by=-created_at%2C%2Buser_id"
        );

        let args = object(json!({ "role": "owner" }));
        let err = user_list_path(&args).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");

        let args = object(json!({ "order_by": "-id" }));
        let err = user_list_path(&args).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
    }

    #[test]
    fn execute_tools_require_reason_and_confirmation() {
        for spec in ADMIN_TOOL_SPECS
            .iter()
            .filter(|tool| tool.kind == ToolKind::Execute)
        {
            let tool = tool_from_spec(spec);
            let schema = tool.schema_as_json_value();
            let required = schema["required"].as_array().unwrap();
            assert!(
                required.contains(&json!("reason")),
                "{} must require reason",
                spec.name
            );
            assert!(
                required.contains(&json!("confirmed")),
                "{} must require confirmed",
                spec.name
            );
            assert_eq!(tool.annotations.unwrap().destructive_hint, Some(true));
        }
    }

    #[test]
    fn prepare_tools_do_not_require_confirmation() {
        for spec in ADMIN_TOOL_SPECS
            .iter()
            .filter(|tool| tool.kind == ToolKind::Prepare)
        {
            let tool = tool_from_spec(spec);
            let schema = tool.schema_as_json_value();
            let required = schema["required"].as_array().unwrap();
            assert!(
                !required.contains(&json!("confirmed")),
                "{} must not require confirmed",
                spec.name
            );
            assert!(
                !required.contains(&json!("reason")),
                "{} must not require reason",
                spec.name
            );
            assert!(
                schema["properties"].get("confirmed").is_none(),
                "{} must not expose confirmed in prepare schema",
                spec.name
            );
        }
    }

    #[test]
    fn prepare_plan_contains_safety_fields() {
        let spec = tool_spec("admin_claim_prepare_delete").unwrap();
        let args = object(json!({
            "id": "claim-1",
            "changes": { "visibility": "hidden" }
        }));
        let plan = prepare_plan(spec, &args).unwrap();

        assert_eq!(plan["operation"], "claim.delete");
        assert_eq!(plan["target"]["type"], "claim");
        assert_eq!(plan["target"]["id"], "claim-1");
        assert_eq!(plan["requested_changes"]["visibility"], "hidden");
        assert!(plan["diff"].as_array().unwrap().len() == 1);
        assert!(plan["risks"].as_array().unwrap().len() >= 2);
        assert!(plan["user_visible_impact"].is_string());
        assert_eq!(plan["execute_tool"], "admin_claim_execute_delete");
        assert_eq!(plan["confirmation_required"]["reason"], true);
        assert_eq!(plan["confirmation_required"]["confirmed"], true);
        assert_eq!(plan["mutation_performed"], false);
    }

    #[test]
    fn execute_validation_rejects_missing_or_blank_reason() {
        let spec = tool_spec("admin_claim_execute_delete").unwrap();
        let args = object(json!({ "id": "claim-1", "confirmed": true }));
        let err = validate_execute_request(spec, &args).unwrap_err();
        assert_eq!(err.code(), "REASON_REQUIRED");

        let args = object(json!({ "id": "claim-1", "reason": "   ", "confirmed": true }));
        let err = validate_execute_request(spec, &args).unwrap_err();
        assert_eq!(err.code(), "REASON_REQUIRED");
    }

    #[test]
    fn execute_validation_rejects_missing_or_false_confirmation() {
        let spec = tool_spec("admin_claim_execute_delete").unwrap();
        let args = object(json!({ "id": "claim-1", "reason": "moderation cleanup" }));
        let err = validate_execute_request(spec, &args).unwrap_err();
        assert_eq!(err.code(), "CONFIRMATION_REQUIRED");

        let args =
            object(json!({ "id": "claim-1", "reason": "moderation cleanup", "confirmed": false }));
        let err = validate_execute_request(spec, &args).unwrap_err();
        assert_eq!(err.code(), "CONFIRMATION_REQUIRED");
    }

    #[test]
    fn execute_validation_accepts_confirmed_request() {
        let spec = tool_spec("admin_claim_execute_delete").unwrap();
        let args = object(json!({
            "id": "claim-1",
            "changes": { "visibility": "hidden" },
            "reason": "moderation cleanup",
            "confirmed": true
        }));
        let request = validate_execute_request(spec, &args).unwrap();
        assert_eq!(request["target"]["id"], "claim-1");
        assert_eq!(request["requested_changes"]["visibility"], "hidden");
        assert_eq!(request["reason"], "moderation cleanup");
        assert_eq!(request["confirmed"], true);
    }
}
