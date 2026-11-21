fn admin_tools() -> Vec<Tool> {
    ADMIN_TOOL_SPECS.iter().map(tool_from_spec).collect()
}

fn tool_spec(name: &str) -> Option<&'static AdminToolSpec> {
    ADMIN_TOOL_SPECS.iter().find(|tool| tool.name == name)
}

fn tool_from_spec(spec: &AdminToolSpec) -> Tool {
    Tool::new_with_raw(
        spec.name,
        Some(tool_description(spec)),
        Arc::new(input_schema(spec.schema)),
    )
    .with_title(tool_title(spec.name))
    .with_annotations(tool_annotations(spec))
}

fn tool_title(name: &str) -> String {
    name.strip_prefix("admin_")
        .unwrap_or(name)
        .replace('_', " ")
}

fn tool_annotations(spec: &AdminToolSpec) -> ToolAnnotations {
    match spec.kind {
        ToolKind::Read | ToolKind::Prepare => ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(true),
        ToolKind::Execute => ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true),
    }
}

fn tool_description(spec: &AdminToolSpec) -> Cow<'static, str> {
    match spec.kind {
        ToolKind::Read => Cow::Owned(format!(
            "{} This read-only tool requires admin access but does not require confirmation and must not mutate KrabArena data.",
            spec.description
        )),
        ToolKind::Prepare => Cow::Owned(format!(
            "{} This prepare tool performs no mutation. The assistant must show its plan to the administrator before any execute tool is called.",
            spec.description
        )),
        ToolKind::Execute => Cow::Owned(format!(
            "{} This execute tool requires an explicit administrator approval: a non-empty reason and confirmed=true. The assistant must not set confirmed=true without direct human approval.",
            spec.description
        )),
    }
}


const ADMIN_TOOL_SPECS: &[AdminToolSpec] = &[
    AdminToolSpec {
        name: "admin_claim_list",
        description: "Read Claims visible to administrators with optional filters.",
        kind: ToolKind::Read,
        schema: SchemaKind::ClaimList,
    },
    AdminToolSpec {
        name: "admin_claim_inspect",
        description: "Read full administrative details for one Claim.",
        kind: ToolKind::Read,
        schema: SchemaKind::Inspect,
    },
    AdminToolSpec {
        name: "admin_claim_prepare_update",
        description: "Read-only preparation for editing Claim metadata or body.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_claim_execute_update",
        description: "Changing operation that edits Claim metadata or body after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_claim_prepare_visibility_change",
        description: "Read-only preparation for changing Claim visibility.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_claim_execute_visibility_change",
        description: "Changing operation that updates Claim visibility after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_claim_prepare_recheck",
        description: "Read-only preparation for running a new Claim check.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_claim_execute_recheck",
        description: "Changing operation that starts a new Claim check after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_claim_prepare_delete",
        description: "Read-only preparation for deleting a Claim.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_claim_execute_delete",
        description: "Destructive operation that deletes a Claim after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_claim_prepare_restore",
        description: "Read-only preparation for restoring a soft-deleted Claim.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_claim_execute_restore",
        description: "Changing operation that restores a soft-deleted Claim after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_battle_list",
        description: "Read Battles visible to administrators with optional filters.",
        kind: ToolKind::Read,
        schema: SchemaKind::BattleList,
    },
    AdminToolSpec {
        name: "admin_battle_inspect",
        description: "Read full administrative details for one Battle.",
        kind: ToolKind::Read,
        schema: SchemaKind::Inspect,
    },
    AdminToolSpec {
        name: "admin_battle_prepare_create",
        description: "Read-only preparation for creating a Battle.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareCreateBattle,
    },
    AdminToolSpec {
        name: "admin_battle_execute_create",
        description: "Changing operation that creates a Battle after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteCreateBattle,
    },
    AdminToolSpec {
        name: "admin_battle_prepare_update",
        description: "Read-only preparation for editing a Battle.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_battle_execute_update",
        description: "Changing operation that edits a Battle after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_battle_prepare_refresh_summary",
        description: "Read-only preparation for refreshing a Battle summary.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_battle_execute_refresh_summary",
        description: "Changing operation that refreshes a Battle summary after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_battle_prepare_backfill_solutions",
        description: "Read-only preparation for backfilling Battle solutions.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_battle_execute_backfill_solutions",
        description: "Changing operation that backfills Battle solutions after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_battle_prepare_delete",
        description: "Read-only preparation for deleting a Battle.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_battle_execute_delete",
        description: "Destructive operation that deletes a Battle after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_battle_prepare_restore",
        description: "Read-only preparation for restoring a soft-deleted Battle.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_battle_execute_restore",
        description: "Changing operation that restores a soft-deleted Battle after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_user_list",
        description: "Read users visible to administrators with optional filters.",
        kind: ToolKind::Read,
        schema: SchemaKind::UserList,
    },
    AdminToolSpec {
        name: "admin_user_inspect",
        description: "Read full administrative details for one user.",
        kind: ToolKind::Read,
        schema: SchemaKind::Inspect,
    },
    AdminToolSpec {
        name: "admin_user_prepare_update",
        description: "Read-only preparation for editing a user profile or admin metadata.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_user_execute_update",
        description: "Changing operation that edits a user after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_user_prepare_role_change",
        description: "Read-only preparation for changing a user role.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_user_execute_role_change",
        description: "Changing operation that changes a user role after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
    AdminToolSpec {
        name: "admin_user_prepare_suspension",
        description: "Read-only preparation for blocking or unblocking a user.",
        kind: ToolKind::Prepare,
        schema: SchemaKind::PrepareMutation,
    },
    AdminToolSpec {
        name: "admin_user_execute_suspension",
        description: "Changing operation that blocks or unblocks a user after approval.",
        kind: ToolKind::Execute,
        schema: SchemaKind::ExecuteMutation,
    },
];
