use std::{borrow::Cow, process::ExitCode, sync::Arc};

use anyhow::{Context, Result};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, JsonObject,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
        ToolAnnotations, object,
    },
    service::{RequestContext, RoleServer},
    transport::stdio,
};
use serde_json::{Value, json};

use crate::commands::admin_core::{AdminContext, AdminError};
use crate::commands::mcp_admin_metadata::{risks_for_tool, user_visible_impact};
use crate::path_params::normalize_path_param_value;

const SERVER_NAME: &str = "krabarena-admin";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolKind {
    Read,
    Prepare,
    Execute,
}

#[derive(Clone, Copy, Debug)]
struct AdminToolSpec {
    name: &'static str,
    description: &'static str,
    kind: ToolKind,
    schema: SchemaKind,
}

#[derive(Clone, Copy, Debug)]
enum SchemaKind {
    ClaimList,
    BattleList,
    UserList,
    Inspect,
    PrepareMutation,
    ExecuteMutation,
    PrepareCreateBattle,
    ExecuteCreateBattle,
}

include!("router.rs");
include!("spec.rs");
include!("responses.rs");
include!("types.rs");
include!("tools/battles.rs");
include!("tools/claims.rs");
include!("tools/users.rs");
include!("common.rs");
include!("plans.rs");
include!("schemas.rs");

#[cfg(test)]
include!("tests.rs");
