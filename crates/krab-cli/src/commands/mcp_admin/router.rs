pub struct AdminMcpServer {
    ctx: AdminContext,
}

impl AdminMcpServer {
    fn new(ctx: AdminContext) -> Self {
        Self { ctx }
    }

    async fn tool_response(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        match spec.name {
            "admin_claim_list" => self.admin_claim_list(arguments).await,
            "admin_claim_inspect" => self.admin_claim_inspect(arguments).await,
            _ if spec.name.starts_with("admin_claim_prepare_") => {
                self.admin_claim_prepare(spec, arguments).await
            }
            _ if spec.name.starts_with("admin_claim_execute_") => {
                self.admin_claim_execute(spec, arguments).await
            }
            "admin_battle_list" => self.admin_battle_list(arguments).await,
            "admin_battle_inspect" => self.admin_battle_inspect(arguments).await,
            _ if spec.name.starts_with("admin_battle_prepare_") => {
                self.admin_battle_prepare(spec, arguments).await
            }
            _ if spec.name.starts_with("admin_battle_execute_") => {
                self.admin_battle_execute(spec, arguments).await
            }
            "admin_user_list" => self.admin_user_list(arguments).await,
            "admin_user_inspect" => self.admin_user_inspect(arguments).await,
            _ if spec.name.starts_with("admin_user_prepare_") => {
                self.admin_user_prepare(spec, arguments).await
            }
            _ if spec.name.starts_with("admin_user_execute_") => {
                self.admin_user_execute(spec, arguments).await
            }
            _ if spec.kind == ToolKind::Prepare => self.prepare_tool_response(spec, arguments),
            _ if spec.kind == ToolKind::Execute => self.execute_tool_response(spec, arguments),
            _ => {
                if let Err(err) = self.ctx.require_admin(spec.name) {
                    return structured_admin_error(err);
                }
                structured_not_implemented(spec.name, json!({}))
            }
        }
    }

    fn prepare_tool_response(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        if let Err(err) = self.ctx.require_admin(spec.name) {
            return structured_admin_error(err);
        }
        let arguments = arguments.unwrap_or_default();
        match prepare_plan(spec, &arguments) {
            Ok(plan) => structured_success(
                spec.name,
                "Prepared administrative action plan. No data was changed.",
                plan,
            ),
            Err(err) => structured_admin_error(err),
        }
    }

    fn execute_tool_response(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        if let Err(err) = self.ctx.require_admin(spec.name) {
            return structured_admin_error(err);
        }
        let arguments = arguments.unwrap_or_default();
        match validate_execute_request(spec, &arguments) {
            Ok(request) => structured_not_implemented(
                spec.name,
                json!({
                    "confirmation_validated": true,
                    "mutation_performed": false,
                    "request": request,
                }),
            ),
            Err(err) => structured_admin_error(err),
        }
    }

    async fn admin_user_list(&self, arguments: Option<JsonObject>) -> CallToolResult {
        let tool = "admin_user_list";
        let arguments = arguments.unwrap_or_default();
        let path = match user_list_path(&arguments) {
            Ok(path) => path,
            Err(err) => return structured_admin_error(err),
        };
        match self.ctx.get_data(tool, &path).await {
            Ok(data) => structured_success(tool, "Retrieved KrabArena admin users.", data),
            Err(err) => structured_admin_error(err),
        }
    }

    async fn admin_user_inspect(&self, arguments: Option<JsonObject>) -> CallToolResult {
        let tool = "admin_user_inspect";
        let arguments = arguments.unwrap_or_default();
        let id = match required_string(tool, &arguments, "id") {
            Ok(id) => id,
            Err(err) => return structured_admin_error(err),
        };
        let path = user_inspect_path(&id);
        match self.ctx.get_data(tool, &path).await {
            Ok(data) => structured_success(tool, "Retrieved KrabArena admin user.", data),
            Err(err) => structured_admin_error(err),
        }
    }

    async fn admin_user_prepare(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        if let Err(err) = self.ctx.require_admin(spec.name) {
            return structured_admin_error(err);
        }
        let arguments = arguments.unwrap_or_default();
        let request = match user_prepare_request(spec, &arguments) {
            Ok(request) => request,
            Err(err) => return structured_admin_error(err),
        };
        let current = match self
            .ctx
            .get_data(spec.name, &user_inspect_path(&request.id))
            .await
        {
            Ok(data) => data,
            Err(err) => return structured_admin_error(err),
        };
        let plan = build_user_prepare_plan(spec, &request, current, self.ctx.admin_user_id());
        structured_success(
            spec.name,
            "Prepared user administrative action plan. No data was changed.",
            plan,
        )
    }

    async fn admin_user_execute(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        if let Err(err) = self.ctx.require_admin(spec.name) {
            return structured_admin_error(err);
        }
        let arguments = arguments.unwrap_or_default();
        let request = match user_execute_request(spec, &arguments) {
            Ok(request) => request,
            Err(err) => return structured_admin_error(err),
        };
        let current = match self
            .ctx
            .get_data(spec.name, &user_inspect_path(&request.id))
            .await
        {
            Ok(data) => data,
            Err(err) => return structured_admin_error(err),
        };
        if user_self_change_requires_extra_confirmation(
            spec.name,
            &current,
            self.ctx.admin_user_id(),
        ) && !request.self_change_confirmed
        {
            return structured_admin_error(AdminError::new(
                spec.name,
                "SELF_CHANGE_CONFIRMATION_REQUIRED",
                "Self role or suspension changes require self_change_confirmed=true after explicit administrator approval.",
                json!({ "field": "self_change_confirmed" }),
            ));
        }
        if let Err(err) = self
            .validate_last_admin_user_change(spec.name, &current, &request.requested_changes)
            .await
        {
            return structured_admin_error(err);
        }

        let user_id = match user_id_from_current(spec.name, &current) {
            Ok(user_id) => user_id,
            Err(err) => return structured_admin_error(err),
        };
        let mut body = request
            .requested_changes
            .as_object()
            .cloned()
            .unwrap_or_default();
        body.insert("reason".into(), Value::String(request.reason.clone()));
        if request.self_change_confirmed {
            body.insert("self_change_confirmed".into(), Value::Bool(true));
        }
        match self
            .ctx
            .patch_data(
                spec.name,
                &user_inspect_path(&user_id),
                &Value::Object(body),
            )
            .await
        {
            Ok(data) => structured_success(
                spec.name,
                "Executed approved user administrative action.",
                json!({
                    "confirmation_validated": true,
                    "mutation_performed": true,
                    "request": request.as_value(),
                    "result": data,
                }),
            ),
            Err(err) => structured_admin_error(err),
        }
    }

    async fn validate_last_admin_user_change(
        &self,
        tool: &str,
        current: &Value,
        requested_changes: &Value,
    ) -> std::result::Result<(), AdminError> {
        let Some(current_role) = current.get("role").and_then(Value::as_str) else {
            return Ok(());
        };
        if current_role != "admin" || !requested_change_can_remove_admin(requested_changes) {
            return Ok(());
        }

        let target_user_id = current.get("user_id").and_then(Value::as_str);
        let admins = self
            .ctx
            .get_data(tool, "/v1/admin/users?role=admin&suspended=false&limit=200")
            .await?;
        let other_active_admin_exists = admins
            .get("items")
            .and_then(Value::as_array)
            .or_else(|| admins.as_array())
            .map(|items| {
                items.iter().any(|admin| {
                    admin
                        .get("user_id")
                        .and_then(Value::as_str)
                        .is_some_and(|admin_id| Some(admin_id) != target_user_id)
                })
            })
            .unwrap_or(false);
        if !other_active_admin_exists {
            return Err(AdminError::new(
                tool,
                "LAST_ADMIN",
                "This action would remove or suspend the last platform admin.",
                json!({ "role": current_role }),
            ));
        }
        Ok(())
    }

    async fn admin_claim_list(&self, arguments: Option<JsonObject>) -> CallToolResult {
        let tool = "admin_claim_list";
        let arguments = arguments.unwrap_or_default();
        let path = match claim_list_path(&arguments) {
            Ok(path) => path,
            Err(err) => return structured_admin_error(err),
        };
        match self.ctx.get_data(tool, &path).await {
            Ok(data) => structured_success(
                tool,
                "Retrieved KrabArena admin Claims.",
                without_browser_urls(data),
            ),
            Err(err) => structured_admin_error(err),
        }
    }

    async fn admin_claim_inspect(&self, arguments: Option<JsonObject>) -> CallToolResult {
        let tool = "admin_claim_inspect";
        let arguments = arguments.unwrap_or_default();
        let id = match required_string(tool, &arguments, "id") {
            Ok(id) => id,
            Err(err) => return structured_admin_error(err),
        };
        let path = claim_inspect_path(&id);
        match self.ctx.get_data(tool, &path).await {
            Ok(data) => structured_success(
                tool,
                "Retrieved KrabArena admin Claim.",
                without_browser_urls(data),
            ),
            Err(err) => structured_admin_error(err),
        }
    }

    async fn admin_claim_prepare(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        if let Err(err) = self.ctx.require_admin(spec.name) {
            return structured_admin_error(err);
        }
        let arguments = arguments.unwrap_or_default();
        let request = match claim_prepare_request(spec, &arguments) {
            Ok(request) => request,
            Err(err) => return structured_admin_error(err),
        };
        let path = claim_inspect_path(&request.id);
        let current = match self.ctx.get_data(spec.name, &path).await {
            Ok(data) => without_browser_urls(data),
            Err(err) => return structured_admin_error(err),
        };
        let plan = build_claim_prepare_plan(spec, &request, current);
        structured_success(
            spec.name,
            "Prepared Claim administrative action plan. No data was changed.",
            plan,
        )
    }

    async fn admin_claim_execute(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        if let Err(err) = self.ctx.require_admin(spec.name) {
            return structured_admin_error(err);
        }
        let arguments = arguments.unwrap_or_default();
        let request = match validate_execute_request(spec, &arguments) {
            Ok(request) => request,
            Err(err) => return structured_admin_error(err),
        };
        let id = request["target"]["id"].as_str().unwrap_or_default();
        let reason = request["reason"].as_str().unwrap_or_default();
        let changes = request
            .get("requested_changes")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let result = match spec.name {
            "admin_claim_execute_update" => {
                let changes_empty = changes
                    .as_object()
                    .map(serde_json::Map::is_empty)
                    .unwrap_or(true);
                if changes_empty {
                    Err(AdminError::new(
                        spec.name,
                        "VALIDATION_ERROR",
                        "changes must contain at least one Claim field for update.",
                        json!({ "field": "changes" }),
                    ))
                } else {
                    let mut body = changes.as_object().cloned().unwrap_or_default();
                    body.insert("reason".into(), Value::String(reason.to_string()));
                    self.ctx
                        .patch_data(spec.name, &claim_inspect_path(id), &Value::Object(body))
                        .await
                }
            }
            "admin_claim_execute_visibility_change" => {
                match required_change_string(spec.name, &changes, "visibility") {
                    Ok(visibility) => {
                        self.ctx
                            .post_data(
                                spec.name,
                                &format!("{}/promote", claim_inspect_path(id)),
                                &json!({ "visibility": visibility, "reason": reason }),
                            )
                            .await
                    }
                    Err(err) => Err(err),
                }
            }
            "admin_claim_execute_recheck" => {
                self.ctx
                    .post_data(
                        spec.name,
                        &format!("{}/recheck", claim_inspect_path(id)),
                        &json!({ "reason": reason }),
                    )
                    .await
            }
            "admin_claim_execute_delete" => {
                self.ctx
                    .delete_data(
                        spec.name,
                        &claim_inspect_path(id),
                        &json!({ "reason": reason }),
                    )
                    .await
            }
            "admin_claim_execute_restore" => {
                self.ctx
                    .post_data(
                        spec.name,
                        &format!("{}/restore", claim_inspect_path(id)),
                        &json!({ "reason": reason }),
                    )
                    .await
            }
            _ => Err(AdminError::new(
                spec.name,
                "NOT_IMPLEMENTED",
                "Claim tool is registered but not implemented.",
                json!({ "tool": spec.name }),
            )),
        };

        match result {
            Ok(data) => structured_success(
                spec.name,
                "Executed approved Claim administrative action.",
                json!({
                    "confirmation_validated": true,
                    "mutation_performed": true,
                    "request": request,
                    "result": data,
                }),
            ),
            Err(err) if err.structured_content()["code"] == "NOT_IMPLEMENTED" => {
                structured_not_implemented(spec.name, json!({ "request": request }))
            }
            Err(err) => structured_admin_error(err),
        }
    }

    async fn admin_battle_list(&self, arguments: Option<JsonObject>) -> CallToolResult {
        let tool = "admin_battle_list";
        let arguments = arguments.unwrap_or_default();
        let path = match battle_list_path(&arguments) {
            Ok(path) => path,
            Err(err) => return structured_admin_error(err),
        };
        match self.ctx.get_data(tool, &path).await {
            Ok(data) => structured_success(
                tool,
                "Retrieved KrabArena admin Battles.",
                without_browser_urls(data),
            ),
            Err(err) => structured_admin_error(err),
        }
    }

    async fn admin_battle_inspect(&self, arguments: Option<JsonObject>) -> CallToolResult {
        let tool = "admin_battle_inspect";
        let arguments = arguments.unwrap_or_default();
        let id = match required_string(tool, &arguments, "id") {
            Ok(id) => id,
            Err(err) => return structured_admin_error(err),
        };
        let path = battle_inspect_path(&id);
        match self.ctx.get_data(tool, &path).await {
            Ok(data) => structured_success(
                tool,
                "Retrieved KrabArena admin Battle.",
                without_browser_urls(data),
            ),
            Err(err) => structured_admin_error(err),
        }
    }

    async fn admin_battle_prepare(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        if let Err(err) = self.ctx.require_admin(spec.name) {
            return structured_admin_error(err);
        }
        let arguments = arguments.unwrap_or_default();
        let request = match battle_prepare_request(spec, &arguments) {
            Ok(request) => request,
            Err(err) => return structured_admin_error(err),
        };
        let current = match request.id.as_deref() {
            Some(id) => match self.ctx.get_data(spec.name, &battle_inspect_path(id)).await {
                Ok(data) => without_browser_urls(data),
                Err(err) => return structured_admin_error(err),
            },
            None => Value::Null,
        };
        let plan = build_battle_prepare_plan(spec, &request, current);
        structured_success(
            spec.name,
            "Prepared Battle administrative action plan. No data was changed.",
            plan,
        )
    }

    async fn admin_battle_execute(
        &self,
        spec: &AdminToolSpec,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        if let Err(err) = self.ctx.require_admin(spec.name) {
            return structured_admin_error(err);
        }
        let arguments = arguments.unwrap_or_default();
        let request = match battle_execute_request(spec, &arguments) {
            Ok(request) => request,
            Err(err) => return structured_admin_error(err),
        };
        let reason = request.reason.as_str();

        let result = match spec.name {
            "admin_battle_execute_create" => {
                let mut body = request
                    .requested_changes
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                body.insert("reason".into(), Value::String(reason.to_string()));
                self.ctx
                    .post_data(spec.name, "/v1/admin/battles", &Value::Object(body))
                    .await
            }
            "admin_battle_execute_update" => {
                let id = request.id.as_deref().unwrap_or_default();
                let changes_empty = request
                    .requested_changes
                    .as_object()
                    .map(serde_json::Map::is_empty)
                    .unwrap_or(true);
                if changes_empty {
                    Err(AdminError::new(
                        spec.name,
                        "VALIDATION_ERROR",
                        "changes must contain at least one Battle field for update.",
                        json!({ "field": "changes" }),
                    ))
                } else {
                    let mut body = request
                        .requested_changes
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                    body.insert("reason".into(), Value::String(reason.to_string()));
                    self.ctx
                        .patch_data(spec.name, &battle_inspect_path(id), &Value::Object(body))
                        .await
                }
            }
            "admin_battle_execute_refresh_summary" => {
                let id = request.id.as_deref().unwrap_or_default();
                self.ctx
                    .post_data(
                        spec.name,
                        &battle_public_summary_path(id),
                        &json!({ "reason": reason }),
                    )
                    .await
            }
            "admin_battle_execute_backfill_solutions" => {
                let id = request.id.as_deref().unwrap_or_default();
                self.ctx
                    .post_data(
                        spec.name,
                        &format!("{}/backfill-solutions", battle_inspect_path(id)),
                        &json!({ "reason": reason }),
                    )
                    .await
            }
            "admin_battle_execute_delete" => {
                let id = request.id.as_deref().unwrap_or_default();
                self.ctx
                    .delete_data(
                        spec.name,
                        &battle_inspect_path(id),
                        &json!({ "reason": reason }),
                    )
                    .await
            }
            "admin_battle_execute_restore" => {
                let id = request.id.as_deref().unwrap_or_default();
                self.ctx
                    .post_data(
                        spec.name,
                        &format!("{}/restore", battle_inspect_path(id)),
                        &json!({ "reason": reason }),
                    )
                    .await
            }
            _ => Err(AdminError::new(
                spec.name,
                "NOT_IMPLEMENTED",
                "Battle tool is registered but not implemented.",
                json!({ "tool": spec.name }),
            )),
        };

        match result {
            Ok(data) => structured_success(
                spec.name,
                "Executed approved Battle administrative action.",
                json!({
                    "confirmation_validated": true,
                    "mutation_performed": true,
                    "request": request.as_value(),
                    "result": data,
                }),
            ),
            Err(err) => structured_admin_error(err),
        }
    }
}

impl ServerHandler for AdminMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "KrabArena Admin MCP server. Read tools inspect admin state; execute tools require explicit reason and confirmation.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(admin_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let tool_name = request.name.as_ref();
        let Some(spec) = tool_spec(tool_name) else {
            return Ok(structured_error(
                "Unknown KrabArena admin MCP tool.",
                json!({
                    "code": "UNKNOWN_TOOL",
                    "tool": tool_name,
                    "message": "This tool is not registered by the KrabArena Admin MCP server."
                }),
            ));
        };

        Ok(self.tool_response(spec, request.arguments).await)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        ADMIN_TOOL_SPECS
            .iter()
            .find(|tool| tool.name == name)
            .map(tool_from_spec)
    }
}

pub async fn run(api_url_override: Option<&str>, token_override: Option<&str>) -> Result<ExitCode> {
    let ctx = AdminContext::from_config(api_url_override, token_override)?;
    let server = AdminMcpServer::new(ctx);
    let running = server
        .serve(stdio())
        .await
        .context("failed to start admin MCP stdio server")?;
    running
        .waiting()
        .await
        .context("admin MCP stdio server stopped with an error")?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
pub fn admin_tool_names() -> Vec<&'static str> {
    ADMIN_TOOL_SPECS.iter().map(|tool| tool.name).collect()
}
