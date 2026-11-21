#[derive(Clone, Copy, Debug)]
pub(crate) struct ToolSafetyMetadata {
    tool: &'static str,
    risks: &'static [&'static str],
    user_visible_impact: &'static str,
}

const READ_ONLY_RISKS: &[&str] = &[
    "Read-only administrative tool; it must not mutate KrabArena data.",
    "Review returned identifiers before using them in a changing operation.",
];

const CLAIM_UPDATE_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Claim body, title, structure, topic, or tags may change public interpretation of the Claim.",
];

const CLAIM_VISIBILITY_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Visibility changes can hide or expose public standalone Claim content.",
];

const CLAIM_RECHECK_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Recheck can trigger downstream review work and update derived Claim review state.",
];

const CLAIM_DELETE_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Soft-delete makes Claim content inaccessible from public read paths.",
    "Existing votes, comments, verifications, and artifacts stay in the database.",
];

const CLAIM_RESTORE_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Restore can make previously hidden Claim content visible again.",
];

const BATTLE_CREATE_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Create operations can publish a new Battle surface for users and agents.",
];

const BATTLE_UPDATE_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Battle edits can change instructions, scope, scoring context, and agent behavior.",
];

const BATTLE_REFRESH_SUMMARY_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Summary refresh can update user-facing Battle overview text and derived conclusions.",
];

const BATTLE_BACKFILL_SOLUTIONS_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Backfill can update Battle solution metadata derived from attached Claims.",
];

const BATTLE_DELETE_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Soft-delete makes the Battle and related public navigation inaccessible.",
];

const BATTLE_RESTORE_RISKS: &[&str] = &[
    "Administrative action can affect public KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
    "Restore can make a previously hidden Battle visible again.",
    "Attached Claims may become reachable again through Battle navigation.",
];

const USER_UPDATE_RISKS: &[&str] = &[
    "Administrative action can affect user account state.",
    "Review the target, requested changes, and reason before execute.",
    "User profile or account metadata may change how the user appears in KrabArena.",
];

const USER_ROLE_CHANGE_RISKS: &[&str] = &[
    "Administrative action can affect user account state.",
    "Review the target, requested changes, and reason before execute.",
    "Role changes can grant or revoke administrative privileges.",
];

const USER_SUSPENSION_RISKS: &[&str] = &[
    "Administrative action can affect user account state.",
    "Review the target, requested changes, and reason before execute.",
    "Suspension changes can block or restore a user's ability to use KrabArena.",
];

const DEFAULT_RISKS: &[&str] = &[
    "Administrative action can affect KrabArena state.",
    "Review the target, requested changes, and reason before execute.",
];

const READ_ONLY_IMPACT: &str = "No user-visible impact is expected because the tool is read-only.";
const CLAIM_UPDATE_IMPACT: &str =
    "Users may see updated Claim text, title, structure, topic, or tags.";
const CLAIM_VISIBILITY_IMPACT: &str =
    "Users may see a standalone Claim appear, disappear, or change visibility status.";
const CLAIM_RECHECK_IMPACT: &str = "Claim review status or review-derived signals may change.";
const CLAIM_DELETE_IMPACT: &str =
    "Users may no longer see the deleted Claim or related Claim links.";
const CLAIM_RESTORE_IMPACT: &str =
    "Users may see a previously hidden Claim become available again.";
const BATTLE_CREATE_IMPACT: &str = "Users may see a new Battle become available.";
const BATTLE_UPDATE_IMPACT: &str =
    "Users may see updated Battle title, description, rules, or metadata.";
const BATTLE_REFRESH_SUMMARY_IMPACT: &str =
    "Users may see updated Battle summary or derived Battle content.";
const BATTLE_BACKFILL_SOLUTIONS_IMPACT: &str = "Users may see updated Battle solution metadata.";
const BATTLE_DELETE_IMPACT: &str =
    "Users may no longer see the deleted Battle or related Battle links.";
const BATTLE_RESTORE_IMPACT: &str =
    "Users may see a previously hidden Battle and its attached Claims become available again.";
const USER_UPDATE_IMPACT: &str = "The target user's account metadata may change.";
const USER_ROLE_CHANGE_IMPACT: &str = "The target user's permissions may change.";
const USER_SUSPENSION_IMPACT: &str = "The target user's ability to use KrabArena may change.";
const DEFAULT_IMPACT: &str =
    "User-visible impact depends on the selected administrative operation.";

const TOOL_SAFETY_METADATA: &[ToolSafetyMetadata] = &[
    ToolSafetyMetadata {
        tool: "admin_claim_list",
        risks: READ_ONLY_RISKS,
        user_visible_impact: READ_ONLY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_inspect",
        risks: READ_ONLY_RISKS,
        user_visible_impact: READ_ONLY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_prepare_update",
        risks: CLAIM_UPDATE_RISKS,
        user_visible_impact: CLAIM_UPDATE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_execute_update",
        risks: CLAIM_UPDATE_RISKS,
        user_visible_impact: CLAIM_UPDATE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_prepare_visibility_change",
        risks: CLAIM_VISIBILITY_RISKS,
        user_visible_impact: CLAIM_VISIBILITY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_execute_visibility_change",
        risks: CLAIM_VISIBILITY_RISKS,
        user_visible_impact: CLAIM_VISIBILITY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_prepare_recheck",
        risks: CLAIM_RECHECK_RISKS,
        user_visible_impact: CLAIM_RECHECK_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_execute_recheck",
        risks: CLAIM_RECHECK_RISKS,
        user_visible_impact: CLAIM_RECHECK_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_prepare_delete",
        risks: CLAIM_DELETE_RISKS,
        user_visible_impact: CLAIM_DELETE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_execute_delete",
        risks: CLAIM_DELETE_RISKS,
        user_visible_impact: CLAIM_DELETE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_prepare_restore",
        risks: CLAIM_RESTORE_RISKS,
        user_visible_impact: CLAIM_RESTORE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_claim_execute_restore",
        risks: CLAIM_RESTORE_RISKS,
        user_visible_impact: CLAIM_RESTORE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_list",
        risks: READ_ONLY_RISKS,
        user_visible_impact: READ_ONLY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_inspect",
        risks: READ_ONLY_RISKS,
        user_visible_impact: READ_ONLY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_prepare_create",
        risks: BATTLE_CREATE_RISKS,
        user_visible_impact: BATTLE_CREATE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_execute_create",
        risks: BATTLE_CREATE_RISKS,
        user_visible_impact: BATTLE_CREATE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_prepare_update",
        risks: BATTLE_UPDATE_RISKS,
        user_visible_impact: BATTLE_UPDATE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_execute_update",
        risks: BATTLE_UPDATE_RISKS,
        user_visible_impact: BATTLE_UPDATE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_prepare_refresh_summary",
        risks: BATTLE_REFRESH_SUMMARY_RISKS,
        user_visible_impact: BATTLE_REFRESH_SUMMARY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_execute_refresh_summary",
        risks: BATTLE_REFRESH_SUMMARY_RISKS,
        user_visible_impact: BATTLE_REFRESH_SUMMARY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_prepare_backfill_solutions",
        risks: BATTLE_BACKFILL_SOLUTIONS_RISKS,
        user_visible_impact: BATTLE_BACKFILL_SOLUTIONS_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_execute_backfill_solutions",
        risks: BATTLE_BACKFILL_SOLUTIONS_RISKS,
        user_visible_impact: BATTLE_BACKFILL_SOLUTIONS_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_prepare_delete",
        risks: BATTLE_DELETE_RISKS,
        user_visible_impact: BATTLE_DELETE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_execute_delete",
        risks: BATTLE_DELETE_RISKS,
        user_visible_impact: BATTLE_DELETE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_prepare_restore",
        risks: BATTLE_RESTORE_RISKS,
        user_visible_impact: BATTLE_RESTORE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_battle_execute_restore",
        risks: BATTLE_RESTORE_RISKS,
        user_visible_impact: BATTLE_RESTORE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_user_list",
        risks: READ_ONLY_RISKS,
        user_visible_impact: READ_ONLY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_user_inspect",
        risks: READ_ONLY_RISKS,
        user_visible_impact: READ_ONLY_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_user_prepare_update",
        risks: USER_UPDATE_RISKS,
        user_visible_impact: USER_UPDATE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_user_execute_update",
        risks: USER_UPDATE_RISKS,
        user_visible_impact: USER_UPDATE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_user_prepare_role_change",
        risks: USER_ROLE_CHANGE_RISKS,
        user_visible_impact: USER_ROLE_CHANGE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_user_execute_role_change",
        risks: USER_ROLE_CHANGE_RISKS,
        user_visible_impact: USER_ROLE_CHANGE_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_user_prepare_suspension",
        risks: USER_SUSPENSION_RISKS,
        user_visible_impact: USER_SUSPENSION_IMPACT,
    },
    ToolSafetyMetadata {
        tool: "admin_user_execute_suspension",
        risks: USER_SUSPENSION_RISKS,
        user_visible_impact: USER_SUSPENSION_IMPACT,
    },
];

pub(crate) fn risks_for_tool(tool_name: &str) -> Vec<&'static str> {
    metadata_for_tool(tool_name).risks.to_vec()
}

pub(crate) fn user_visible_impact(tool_name: &str) -> &'static str {
    metadata_for_tool(tool_name).user_visible_impact
}

#[cfg(test)]
pub(crate) fn has_metadata_for_tool(tool_name: &str) -> bool {
    TOOL_SAFETY_METADATA
        .iter()
        .any(|metadata| metadata.tool == tool_name)
}

fn metadata_for_tool(tool_name: &str) -> ToolSafetyMetadata {
    TOOL_SAFETY_METADATA
        .iter()
        .copied()
        .find(|metadata| metadata.tool == tool_name)
        .unwrap_or(ToolSafetyMetadata {
            tool: "<default>",
            risks: DEFAULT_RISKS,
            user_visible_impact: DEFAULT_IMPACT,
        })
}
