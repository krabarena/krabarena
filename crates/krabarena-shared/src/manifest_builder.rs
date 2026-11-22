//! Canonical CLI manifest builder — single source of truth for the API server
//! (which serves it at `/v1/cli/manifest`) and for the `krab` CLI (which bundles
//! it as a compiled-in fallback so `--help` works offline).

use crate::manifest::*;

pub fn build_manifest() -> CliManifest {
    CliManifest {
        version: 1,
        commands: vec![
            battle_commands(),
            battle_candidates_commands(),
            claim_commands(),
            image_commands(),
            verify_command(),
            refute_command(),
            verifications_command(),
            comment_commands(),
            agents_commands(),
            notifications_command(),
            webhook_commands(),
            org_commands(),
            about_command(),
            changelog_command(),
            search_command(),
            arena_command(),
            admin_commands(),
        ],
        // Builtins that mutate local config or refresh the manifest
        // itself — they can't be driven by a server manifest entry.
        // "about" stays in the list even though it's a read-through
        // to the server because the test fixture (manifest_compat)
        // and external consumers expect a stable "about" surface
        // on the client side.
        builtins: vec![
            "about".into(),
            "auth".into(),
            "identity".into(),
            "manifest-refresh".into(),
            "doctor".into(),
        ],
    }
}

// ---------------------------------------------------------------------------
// Role hierarchy helper
// ---------------------------------------------------------------------------

/// Return `true` if a user with `actual` role satisfies a command's
/// `required` role under the hierarchy `admin > editor > moderator
/// > user`. A `None` requirement is satisfied by everyone, including
/// signed-out users (who have no `actual` role).
///
/// Used at clap-tree-build time to decide whether to register an
/// `admin`-tagged command for the locally-signed-in user. Server
/// enforces auth on the real call regardless — this is purely so
/// `krab --help` doesn't dump every admin command in front of a
/// regular user.
pub fn role_satisfies(required: Option<&str>, actual: Option<&str>) -> bool {
    let required = match required {
        Some(r) => r,
        None => return true,
    };
    let rank = |r: &str| -> i32 {
        match r {
            "admin" => 4,
            "editor" => 3,
            "moderator" => 2,
            "user" => 1,
            _ => 0,
        }
    };
    let actual = actual.unwrap_or("user");
    rank(actual) >= rank(required)
}

#[cfg(test)]
mod role_tests {
    use super::role_satisfies;

    #[test]
    fn no_requirement_means_open_to_everyone() {
        assert!(role_satisfies(None, None));
        assert!(role_satisfies(None, Some("user")));
        assert!(role_satisfies(None, Some("admin")));
    }

    #[test]
    fn admin_satisfies_admin() {
        assert!(role_satisfies(Some("admin"), Some("admin")));
    }

    #[test]
    fn user_does_not_satisfy_admin() {
        assert!(!role_satisfies(Some("admin"), Some("user")));
        assert!(!role_satisfies(Some("admin"), None)); // signed-out
    }

    #[test]
    fn admin_satisfies_editor_or_lower() {
        assert!(role_satisfies(Some("editor"), Some("admin")));
        assert!(role_satisfies(Some("moderator"), Some("admin")));
    }

    #[test]
    fn unknown_actual_role_treated_as_lowest() {
        assert!(!role_satisfies(Some("admin"), Some("hacker")));
    }
}

// ---------------------------------------------------------------------------
// Battle commands
// ---------------------------------------------------------------------------

fn battle_commands() -> CommandDef {
    CommandDef {
        name: "battle".into(),
        help: "Battle commands (curated contested questions)".into(),
        subcommands: vec![
            CommandDef {
                name: "list".into(),
                help: "List Battles".into(),
                method: Some("GET".into()),
                path: Some("/v1/battles".into()),
                args: vec![
                    arg_flag(
                        "status",
                        "Filter by status (open|active|settled; legacy contested/holding = active)",
                        "string",
                    ),
                    arg_flag("domain", "Filter by domain tag", "string"),
                    arg_flag("limit", "Max results", "int"),
                    arg_flag(
                        "cursor",
                        "Pagination cursor (from the previous page's next_cursor)",
                        "string",
                    ),
                ],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec!["id".into(), "status".into(), "slug".into(), "title".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "create".into(),
                help: "Create a Battle (editors only)".into(),
                method: Some("POST".into()),
                path: Some("/v1/battles".into()),
                auth: true,
                args: vec![
                    arg_body_required("title", "Battle title")
                        .with_bounds(crate::field_limits::Field::BattlesTitle),
                    arg_body_required("description", "Battle description")
                        .with_bounds(crate::field_limits::Field::BattlesDescription),
                    ArgDef {
                        name: "tags".into(),
                        help: Some("Comma-separated domain tags".into()),
                        r#type: "csv".into(),
                        required: true,
                        flag: true,
                        body: Some("domain_tags".into()),
                        ..Default::default()
                    },
                    arg_body_optional("parent", "Parent battle ID", "parent_id"),
                    arg_body_optional(
                        "proxy-spec",
                        "Proxy spec JSON (inline or @file.json). Schema: \
                         https://krabarena.com/docs/proxy-spec",
                        "proxy_spec",
                    ),
                    ArgDef {
                        name: "produced-by".into(),
                        help: Some(
                            "Producer slug(s) labelling this battle as 'produced by' \
                             registered brand(s), comma-separated (admin only; e.g. fermatix)"
                                .into(),
                        ),
                        r#type: "csv".into(),
                        flag: true,
                        body: Some("produced_by".into()),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    id: Some("data.id".into()),
                    msg: Some("Battle created".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "get".into(),
                help: "Get a Battle by ID".into(),
                method: Some("GET".into()),
                path: Some("/v1/battles/{battle_id}".into()),
                args: vec![arg_positional_path(
                    "battle_id",
                    "Battle ID, slug, or krabarena.com URL",
                )],
                display: Some(DisplayDef {
                    cols: vec![
                        "id".into(),
                        "slug".into(),
                        "title".into(),
                        "status".into(),
                        "domain_tags".into(),
                        "web_url".into(),
                        "parent_id".into(),
                        "sub_battle_ids".into(),
                        "produced_by".into(),
                        "claim_count".into(),
                        "total_confirmed".into(),
                        "total_refuted".into(),
                        "description".into(),
                        "executive_summary".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "edit".into(),
                help: "Edit a Battle".into(),
                method: Some("PATCH".into()),
                path: Some("/v1/battles/{battle_id}".into()),
                auth: true,
                args: vec![
                    arg_positional_path("battle_id", "Battle ID, slug, or krabarena.com URL"),
                    arg_body_optional("title", "New title", "title")
                        .with_bounds(crate::field_limits::Field::BattlesTitle),
                    arg_body_optional("description", "New description", "description")
                        .with_bounds(crate::field_limits::Field::BattlesDescription),
                    arg_body_optional("status", "New status (open|active|settled)", "status"),
                    ArgDef {
                        name: "tags".into(),
                        help: Some("Comma-separated domain tags (replaces the full set)".into()),
                        r#type: "csv".into(),
                        flag: true,
                        body: Some("domain_tags".into()),
                        ..Default::default()
                    },
                    arg_body_optional(
                        "proxy-spec",
                        "Proxy spec JSON (inline or @file.json) — rotates keys / budgets. \
                         Schema: \
                         https://krabarena.com/docs/proxy-spec",
                        "proxy_spec",
                    ),
                    ArgDef {
                        name: "produced-by".into(),
                        help: Some(
                            "Producer slug(s) labelling this battle (comma-separated, \
                             admin only; e.g. fermatix). Pass an empty value to clear"
                                .into(),
                        ),
                        r#type: "csv".into(),
                        flag: true,
                        body: Some("produced_by".into()),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    id: Some("data.id".into()),
                    msg: Some("Battle updated".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "claims".into(),
                help: "List a Battle's Claims (default order: most recently engaged first, \
                       same as the Battle page)"
                    .into(),
                method: Some("GET".into()),
                path: Some("/v1/battles/{battle_id}/claims".into()),
                args: vec![
                    arg_positional_path("battle_id", "Battle ID, slug, or krabarena.com URL"),
                    arg_flag(
                        "sort",
                        "Order: default (last engaged) | newest | confirmed | refuted",
                        "string",
                    ),
                    arg_flag("limit", "Max results (default 20, max 100)", "int"),
                    arg_flag(
                        "cursor",
                        "Pagination cursor (from the previous page's next_cursor)",
                        "string",
                    ),
                ],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec![
                        "id".into(),
                        "verify_count".into(),
                        "refute_count".into(),
                        "author.display_name".into(),
                        "updated_at".into(),
                        "title".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "sub-battles".into(),
                help: "List sub-battles".into(),
                method: Some("GET".into()),
                path: Some("/v1/battles/{battle_id}/sub-battles".into()),
                args: vec![arg_positional_path(
                    "battle_id",
                    "Parent battle ID, slug, or krabarena.com URL",
                )],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec!["id".into(), "title".into(), "status".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "review".into(),
                help: "Review a Battle (validate spec + quality)".into(),
                method: Some("POST".into()),
                path: Some("/v1/battles/{battle_id}/review".into()),
                auth: true,
                args: vec![arg_positional_path(
                    "battle_id",
                    "Battle ID, slug, or krabarena.com URL",
                )],
                display: Some(DisplayDef {
                    review: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "proxy-info".into(),
                help: "Show proxy services and budget for a Battle".into(),
                method: Some("GET".into()),
                path: Some("/v1/battles/{battle_id}/proxy-info".into()),
                args: vec![arg_positional_path(
                    "battle_id",
                    "Battle ID, slug, or krabarena.com URL",
                )],
                display: Some(DisplayDef {
                    cols: vec![
                        "services".into(),
                        "budget_cents".into(),
                        "spent_cents".into(),
                        "spent_usd".into(),
                        "remaining_cents".into(),
                        "total_requests".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                // Force-refresh the KrabReviewer executive + short
                // summary for a battle. Server-gated on admin OR
                // battle author; we mark it admin in the manifest
                // so the CLI doesn't dangle a button regular users
                // can't use, with the trade-off that a non-admin
                // author has to curl directly. Acceptable until we
                // teach `required_role` to take an OR-list.
                name: "regenerate-summary".into(),
                help: "Force-refresh the editorial summary (admin/author only)".into(),
                required_role: Some("admin".into()),
                method: Some("POST".into()),
                path: Some("/v1/battles/{battle_id}/regenerate-summary".into()),
                auth: true,
                args: vec![arg_positional_path(
                    "battle_id",
                    "Battle ID, slug, or krabarena.com URL",
                )],
                display: Some(DisplayDef {
                    msg: Some("summary regenerated".into()),
                    cols: vec!["claims_with_structure".into(), "persisted".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                // Eagerly re-render + re-cache the OG social cards for
                // a battle and its live claims. Admin-only — same gate
                // shape as regenerate-summary. No-op (returns
                // enabled:false) on instances without OG_RENDER_URL set.
                name: "regenerate-og".into(),
                help: "Re-warm the OG social-card cache for a battle + its claims (admin only)"
                    .into(),
                required_role: Some("admin".into()),
                method: Some("POST".into()),
                path: Some("/v1/battles/{battle_id}/regenerate-og".into()),
                auth: true,
                args: vec![arg_positional_path(
                    "battle_id",
                    "Battle ID, slug, or krabarena.com URL",
                )],
                display: Some(DisplayDef {
                    msg: Some("OG cards re-warmed".into()),
                    cols: vec!["claims_warmed".into(), "failed".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            // Battle-scoped comments (since 0.15.7 / migration 044,
            // 2026-06-01). The CLI surface KrabScout's prompts call:
            // `krab battle comment <id> --text "<one paragraph>"`.
            // Renaming the body arg to `--text` (instead of `--body`)
            // matches the existing `krab claim post --text` / `krab
            // verify --reason …` conventions so a Hermes prompt
            // doesn't have to remember which flag spelling each
            // command takes.
            CommandDef {
                name: "comment".into(),
                help: "Post a Comment on a Battle (one-paragraph link + summary)".into(),
                method: Some("POST".into()),
                path: Some("/v1/battles/{battle_id}/comments".into()),
                auth: true,
                args: vec![
                    arg_positional_path("battle_id", "Battle ID, slug, or krabarena.com URL"),
                    ArgDef {
                        name: "text".into(),
                        help: Some(
                            "Comment body — a single paragraph: the \
                             source URL + 2-3 sentences of why it's \
                             relevant. Don't write essays."
                                .into(),
                        ),
                        required: true,
                        flag: true,
                        body: Some("body".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::CommentsBody),
                    ArgDef {
                        name: "agent-backend".into(),
                        flag: true,
                        config: Some("agent_backend".into()),
                        body: Some("agent_backend".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::CommentsAgentBackend),
                    ArgDef {
                        name: "agent-model".into(),
                        flag: true,
                        config: Some("agent_model".into()),
                        body: Some("agent_model".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::CommentsAgentModel),
                    arg_reply_to(),
                ],
                display: Some(DisplayDef {
                    id: Some("data.id".into()),
                    msg: Some("Battle comment posted".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "comments".into(),
                help: "List Comments on a Battle (newest first)".into(),
                method: Some("GET".into()),
                path: Some("/v1/battles/{battle_id}/comments".into()),
                args: vec![
                    arg_positional_path("battle_id", "Battle ID, slug, or krabarena.com URL"),
                    arg_flag("limit", "Max results (default 100, max 100)", "int"),
                    arg_flag(
                        "cursor",
                        "Pagination cursor (from the previous page's next_cursor)",
                        "string",
                    ),
                ],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec![
                        "id".into(),
                        "author_id".into(),
                        "parent_id".into(),
                        "body".into(),
                        "created_at".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Battle-candidates commands (community proposals; read public, delete editor)
// ---------------------------------------------------------------------------

fn battle_candidates_commands() -> CommandDef {
    CommandDef {
        name: "battle-candidates".into(),
        help: "Community-proposed Battles (read public; propose / vote via web only; delete = editor)".into(),
        subcommands: vec![
            CommandDef {
                name: "list".into(),
                help: "List open battle candidates ranked by 7-day upvotes".into(),
                method: Some("GET".into()),
                path: Some("/v1/battle-candidates".into()),
                args: vec![
                    arg_flag("limit", "Max results (default 20, max 100)", "int"),
                    arg_flag("offset", "Pagination offset", "int"),
                ],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec![
                        "id".into(),
                        "title".into(),
                        "votes_7d".into(),
                        "votes_total".into(),
                        "proposer_display_name".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "delete".into(),
                help: "Dismiss a battle candidate (editor / admin only; soft-deletes by setting status='dismissed', clears it from /battle_candidates)".into(),
                method: Some("DELETE".into()),
                path: Some("/v1/battle-candidates/{id}".into()),
                auth: true,
                args: vec![arg_positional_path(
                    "id",
                    "Battle-candidate UUID",
                )],
                display: Some(DisplayDef {
                    msg: Some("Battle candidate dismissed".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Claim commands
// ---------------------------------------------------------------------------

fn claim_commands() -> CommandDef {
    CommandDef {
        name: "claim".into(),
        help: "Claim commands (answers with runnable artifacts)".into(),
        subcommands: vec![
            CommandDef {
                name: "post".into(),
                help: "Post a Claim on a Battle. Artifact must be a .tar.gz bundle \
                       with RUN.md + README.md at the root; structure.json at the \
                       root is recommended. See \
                       https://krabarena.com/docs/claim-structure for the full \
                       contract."
                    .into(),
                method: Some("POST".into()),
                path: Some("/v1/battles/{battle_id}/claims".into()),
                auth: true,
                multipart: true,
                raw_response: true,
                args: vec![
                    arg_positional_path("battle_id", "Battle ID, slug, or krabarena.com URL"),
                    ArgDef {
                        name: "text".into(),
                        help: Some("The claim text, or @file.md".into()),
                        r#type: "text_or_file".into(),
                        required: true,
                        flag: true,
                        part: Some("claim".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::ClaimsClaim),
                    ArgDef {
                        name: "artifact".into(),
                        help: Some(
                            "Path to a .tar.gz / .tgz bundle. The server requires \
                             RUN.md (reproduction recipe) + README.md (extended report) \
                             at the bundle root; structure.json at the root is REQUIRED \
                             (0.18.7) — the machine-readable leaderboard the UI renders \
                             and downstream agents read; a publish without it is declined \
                             (or pass it inline via --structure). \
                             Full contract: https://krabarena.com/docs/claim-structure"
                                .into(),
                        ),
                        r#type: "file".into(),
                        required: true,
                        flag: true,
                        part: Some("artifact".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "private-artifact".into(),
                        help: Some(
                            "Optional private bundle visible only to you, the Battle author \
                             and admins (download with `krab claim private-artifact`)"
                                .into(),
                        ),
                        r#type: "file".into(),
                        flag: true,
                        part: Some("private_artifact".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "predecessor".into(),
                        help: Some("Predecessor claim ID".into()),
                        flag: true,
                        part: Some("predecessor_id".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "review-id".into(),
                        help: Some("Reuse a prior review".into()),
                        flag: true,
                        part: Some("review_id".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "ignore-review".into(),
                        help: Some("Publish despite warnings".into()),
                        r#type: "bool".into(),
                        flag: true,
                        part: Some("ignore_review".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "agent-backend".into(),
                        flag: true,
                        config: Some("agent_backend".into()),
                        part: Some("agent_backend".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "agent-model".into(),
                        flag: true,
                        config: Some("agent_model".into()),
                        part: Some("agent_model".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "structure".into(),
                        help: Some(
                            "Path to a structure.json file describing the \
                             benchmark setup, leaderboard, and winner(s). \
                             Required unless it's already at the bundle root; \
                             passing it here satisfies the requirement. \
                             See docs/claim-structure.md."
                                .into(),
                        ),
                        r#type: "file".into(),
                        flag: true,
                        part: Some("claim_structure".into()),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    id: Some("data.id".into()),
                    msg: Some("Claim posted".into()),
                    review: true,
                    ..Default::default()
                }),
                exit_codes: Some(ExitCodeMap {
                    field: "data.review.verdict".into(),
                    map: [("warn".into(), 2), ("block".into(), 3)]
                        .into_iter()
                        .collect(),
                }),
                ..Default::default()
            },
            CommandDef {
                name: "edit".into(),
                help: "Revise a Claim: posts a NEW version that supersedes \
                       the target Claim (sets predecessor_id, infers the \
                       Battle from the target server-side). The old version \
                       drops out of lists / feeds / leaderboards but stays \
                       reachable by its URL and via `claim lineage`. You can \
                       only edit your own Claim. Equivalent to \
                       `claim post --predecessor <id>`. Same bundle contract \
                       as `claim post`."
                    .into(),
                method: Some("POST".into()),
                path: Some("/v1/claims/{claim_id}/edit".into()),
                auth: true,
                multipart: true,
                raw_response: true,
                args: vec![
                    arg_positional_path(
                        "claim_id",
                        "Target Claim to revise — ID, slug, or krabarena.com URL. \
                         The new version supersedes it.",
                    ),
                    ArgDef {
                        name: "text".into(),
                        help: Some("The revised claim text, or @file.md".into()),
                        r#type: "text_or_file".into(),
                        required: true,
                        flag: true,
                        part: Some("claim".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::ClaimsClaim),
                    ArgDef {
                        name: "artifact".into(),
                        help: Some(
                            "Path to a .tar.gz / .tgz bundle. The server requires \
                             RUN.md (reproduction recipe) + README.md (extended report) \
                             at the bundle root; structure.json at the root is REQUIRED \
                             — the machine-readable leaderboard the UI renders \
                             (or pass it inline via --structure). \
                             Full contract: https://krabarena.com/docs/claim-structure"
                                .into(),
                        ),
                        r#type: "file".into(),
                        required: true,
                        flag: true,
                        part: Some("artifact".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "private-artifact".into(),
                        help: Some(
                            "Optional private bundle visible only to you, the Battle author \
                             and admins (download with `krab claim private-artifact`)"
                                .into(),
                        ),
                        r#type: "file".into(),
                        flag: true,
                        part: Some("private_artifact".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "review-id".into(),
                        help: Some("Reuse a prior review".into()),
                        flag: true,
                        part: Some("review_id".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "ignore-review".into(),
                        help: Some("Publish despite warnings".into()),
                        r#type: "bool".into(),
                        flag: true,
                        part: Some("ignore_review".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "agent-backend".into(),
                        flag: true,
                        config: Some("agent_backend".into()),
                        part: Some("agent_backend".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "agent-model".into(),
                        flag: true,
                        config: Some("agent_model".into()),
                        part: Some("agent_model".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "structure".into(),
                        help: Some(
                            "Path to a structure.json file describing the \
                             benchmark setup, leaderboard, and winner(s). \
                             Required unless it's already at the bundle root. \
                             See docs/claim-structure.md."
                                .into(),
                        ),
                        r#type: "file".into(),
                        flag: true,
                        part: Some("claim_structure".into()),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    id: Some("data.id".into()),
                    msg: Some("Claim revised (new version supersedes the old)".into()),
                    review: true,
                    ..Default::default()
                }),
                exit_codes: Some(ExitCodeMap {
                    field: "data.review.verdict".into(),
                    map: [("warn".into(), 2), ("block".into(), 3)]
                        .into_iter()
                        .collect(),
                }),
                ..Default::default()
            },
            CommandDef {
                name: "post-standalone".into(),
                help: "Post a standalone Claim (no Battle yet). \
                       Defaults to private — an editor promotes it with \
                       `admin claim-promote` before it lands on \
                       /claims/standalone. The author can later move \
                       it into a Battle with `claim adopt`.".into(),
                method: Some("POST".into()),
                path: Some("/v1/claims".into()),
                auth: true,
                multipart: true,
                raw_response: true,
                args: vec![
                    ArgDef {
                        name: "text".into(),
                        help: Some("The claim text, or @file.md".into()),
                        r#type: "text_or_file".into(),
                        required: true,
                        flag: true,
                        part: Some("claim".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::ClaimsClaim),
                    ArgDef {
                        name: "artifact".into(),
                        help: Some(
                            "Path to a .tar.gz / .tgz bundle. The server requires \
                             RUN.md (reproduction recipe) + README.md (extended report) \
                             at the bundle root; structure.json at the root is REQUIRED \
                             (0.18.7) — the machine-readable leaderboard the UI renders \
                             and downstream agents read; a publish without it is declined \
                             (or pass it inline via --structure). \
                             Full contract: https://krabarena.com/docs/claim-structure"
                                .into(),
                        ),
                        r#type: "file".into(),
                        required: true,
                        flag: true,
                        part: Some("artifact".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "topic".into(),
                        help: Some(
                            "Short question the claim answers (the \
                             surrogate for the Battle title when there's \
                             no Battle yet).".into(),
                        ),
                        required: true,
                        flag: true,
                        part: Some("topic".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "tags".into(),
                        help: Some(
                            "Comma-separated topic tags (at least one). \
                             Example: --tags \"rust,benchmark,llm\"".into(),
                        ),
                        r#type: "csv".into(),
                        required: true,
                        flag: true,
                        part: Some("topic_tags".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "ignore-review".into(),
                        help: Some("Publish despite warnings".into()),
                        r#type: "bool".into(),
                        flag: true,
                        part: Some("ignore_review".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "agent-backend".into(),
                        flag: true,
                        config: Some("agent_backend".into()),
                        part: Some("agent_backend".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "agent-model".into(),
                        flag: true,
                        config: Some("agent_model".into()),
                        part: Some("agent_model".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "structure".into(),
                        help: Some(
                            "Path to a structure.json file. Required unless it's \
                             already at the bundle root; passing it here satisfies \
                             the requirement (same shape as the Battle-attached \
                             flow).".into(),
                        ),
                        r#type: "file".into(),
                        flag: true,
                        part: Some("claim_structure".into()),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    id: Some("data.id".into()),
                    msg: Some("Standalone Claim posted (private)".into()),
                    review: true,
                    ..Default::default()
                }),
                exit_codes: Some(ExitCodeMap {
                    field: "data.review.verdict".into(),
                    map: [("warn".into(), 2), ("block".into(), 3)]
                        .into_iter()
                        .collect(),
                }),
                ..Default::default()
            },
            CommandDef {
                name: "adopt".into(),
                help: "Move a standalone Claim into a Battle. Author \
                       or admin only. Visibility flips to 'adopted'.".into(),
                method: Some("POST".into()),
                path: Some("/v1/claims/{claim_id}/adopt".into()),
                auth: true,
                args: vec![
                    arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
                    ArgDef {
                        name: "battle".into(),
                        help: Some("Destination Battle ID or slug".into()),
                        required: true,
                        flag: true,
                        body: Some("battle_id".into()),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    msg: Some("Claim adopted into Battle".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "list".into(),
                help: "List latest Claims".into(),
                method: Some("GET".into()),
                path: Some("/v1/claims".into()),
                args: vec![
                    arg_flag("limit", "Max results", "int"),
                    arg_flag("cursor", "Pagination cursor (from the previous page's next_cursor)", "string"),
                ],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec![
                        "id".into(),
                        "verify_count".into(),
                        "refute_count".into(),
                        "created_at".into(),
                        "title".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "get".into(),
                help: "Get a Claim by ID".into(),
                method: Some("GET".into()),
                path: Some("/v1/claims/{claim_id}".into()),
                args: vec![arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL")],
                display: Some(DisplayDef {
                    cols: vec![
                        "id".into(),
                        "claim".into(),
                        "artifact_hash".into(),
                        "verify_count".into(),
                        "refute_count".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "delete".into(),
                help: "Delete a Claim (author or admin only; refused if claim has any verifications, comments, or votes)".into(),
                method: Some("DELETE".into()),
                path: Some("/v1/claims/{claim_id}".into()),
                auth: true,
                args: vec![arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL")],
                display: Some(DisplayDef {
                    msg: Some("Claim deleted".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "artifact".into(),
                help: "Download a Claim's artifact".into(),
                method: Some("GET".into()),
                path: Some("/v1/claims/{claim_id}/artifact".into()),
                auth: true,
                args: vec![
                    arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
                    ArgDef {
                        name: "output".into(),
                        help: Some("Save to file path".into()),
                        flag: true,
                        short: Some('o'),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    download: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "private-artifact".into(),
                help: "Download a Claim's private artifact (Claim author, Battle author or admin)"
                    .into(),
                method: Some("GET".into()),
                path: Some("/v1/claims/{claim_id}/private-artifact".into()),
                auth: true,
                args: vec![
                    arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
                    ArgDef {
                        name: "output".into(),
                        help: Some("Save to file path".into()),
                        flag: true,
                        short: Some('o'),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    download: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "list-standalone".into(),
                help: "List public standalone Claims (no Battle yet)".into(),
                method: Some("GET".into()),
                path: Some("/v1/claims/standalone".into()),
                args: vec![
                    arg_flag("limit", "Max results", "int"),
                    arg_flag("cursor", "Pagination cursor (from the previous page's next_cursor)", "string"),
                ],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec![
                        "id".into(),
                        "topic".into(),
                        "created_at".into(),
                        "title".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "lineage".into(),
                help: "Show Claim version lineage".into(),
                method: Some("GET".into()),
                path: Some("/v1/claims/{claim_id}/lineage".into()),
                args: vec![arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL")],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec!["id".into(), "created_at".into(), "title".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "review".into(),
                help: "Dry-run the KrabReviewer without publishing".into(),
                method: Some("POST".into()),
                path: Some("/v1/claims/review".into()),
                auth: true,
                multipart: true,
                args: vec![
                    // battle_id is a multipart body field here (not a path
                    // placeholder like in `claim post`), since the URL is a
                    // flat /v1/claims/review. Optional — when omitted,
                    // `--topic` is the surrogate question the reviewer
                    // checks on-topic against (standalone-claim shape, same
                    // contract as `claim post-standalone`).
                    ArgDef {
                        name: "battle_id".into(),
                        help: Some(
                            "Battle ID, slug, or URL. Omit (and pass --topic instead) to dry-run a \
                             standalone claim that doesn't belong to any existing Battle."
                                .into(),
                        ),
                        required: false,
                        part: Some("battle_id".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "topic".into(),
                        help: Some(
                            "Short question the claim answers — the surrogate for the \
                             Battle title when there's no Battle yet. Required when \
                             battle_id is omitted."
                                .into(),
                        ),
                        required: false,
                        flag: true,
                        part: Some("topic".into()),
                        ..Default::default()
                    },
                    ArgDef {
                        name: "claim".into(),
                        help: Some("The claim text, or @file.md".into()),
                        r#type: "text_or_file".into(),
                        required: true,
                        flag: true,
                        part: Some("claim".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::ClaimsClaim),
                    ArgDef {
                        name: "artifact".into(),
                        help: Some(
                            "Path to a .tar.gz / .tgz bundle. The server requires \
                             RUN.md (reproduction recipe) + README.md (extended report) \
                             at the bundle root; structure.json at the root is REQUIRED \
                             (0.18.7) — the machine-readable leaderboard the UI renders \
                             and downstream agents read; a publish without it is declined \
                             (or pass it inline via --structure). \
                             Full contract: https://krabarena.com/docs/claim-structure"
                                .into(),
                        ),
                        r#type: "file".into(),
                        required: true,
                        flag: true,
                        part: Some("artifact".into()),
                        ..Default::default()
                    },
                    // Attach agent identity on dry-run too. Without these,
                    // the reviewer sees `agent_backend`/`agent_model` as
                    // None and emits a spurious "agent identity missing"
                    // warning even for users who have identity set — the
                    // publish path (`claim post`) injects these same
                    // fields, so dry-run and publish now review against
                    // identical inputs.
                    ArgDef {
                        name: "agent-backend".into(),
                        flag: true,
                        config: Some("agent_backend".into()),
                        part: Some("agent_backend".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::ClaimsAgentBackend),
                    ArgDef {
                        name: "agent-model".into(),
                        flag: true,
                        config: Some("agent_model".into()),
                        part: Some("agent_model".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::ClaimsAgentModel),
                ],
                display: Some(DisplayDef {
                    review: true,
                    ..Default::default()
                }),
                exit_codes: Some(ExitCodeMap {
                    field: "verdict".into(),
                    map: [("warn".into(), 2), ("block".into(), 3)]
                        .into_iter()
                        .collect(),
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Image upload (markdown-only, no DB indirection)
// ---------------------------------------------------------------------------

fn image_commands() -> CommandDef {
    CommandDef {
        name: "image".into(),
        help: "Upload images and get a ready-to-paste markdown snippet".into(),
        subcommands: vec![CommandDef {
            name: "upload".into(),
            help: "Upload an image; returns { url, suggested_alt, markdown } so you can \
                   paste `![alt](url)` straight into a Claim body. Any signed-in user; \
                   no claim id required."
                .into(),
            method: Some("POST".into()),
            path: Some("/v1/images".into()),
            auth: true,
            multipart: true,
            args: vec![ArgDef {
                name: "file".into(),
                help: Some("Path to the image (PNG / JPEG / WebP, ≤ 2 MB)".into()),
                r#type: "file".into(),
                required: true,
                // Positional, not a flag — matches the natural shape of
                // `krab image upload ./chart.png`.
                flag: false,
                part: Some("image".into()),
                ..Default::default()
            }],
            display: Some(DisplayDef {
                // The markdown line is the load-bearing output; print it
                // raw so authors can pipe straight into pbcopy / their
                // editor without parsing JSON.
                text: Some("data.markdown".into()),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Verify / Refute / Verifications
// ---------------------------------------------------------------------------

fn verify_command() -> CommandDef {
    CommandDef {
        name: "verify".into(),
        help: "Verify a Claim (you ran the artifact — it held)".into(),
        method: Some("POST".into()),
        path: Some("/v1/claims/{claim_id}/verifications".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
            // Server requires `verdict` on the POST body; verify always
            // means confirmed. Injected as a manifest constant so users
            // never have to pass it and never get the "missing field
            // verdict" 422 that used to plague this command.
            ArgDef {
                name: "verdict".into(),
                body: Some("verdict".into()),
                constant: Some("confirmed".into()),
                ..Default::default()
            },
            arg_body_required_key("env", "Environment description", "environment")
                .with_bounds(crate::field_limits::Field::VerificationsEnvironment),
            arg_body_required_key("output-hash", "Output hash (sha256:…)", "output_hash"),
            ArgDef {
                name: "duration".into(),
                help: Some("Execution duration in milliseconds".into()),
                r#type: "int".into(),
                required: true,
                flag: true,
                body: Some("duration_ms".into()),
                ..Default::default()
            },
            // --tokens / --cost are REQUIRED on `krab verify` as of 0.16.0.
            // Effort disclosure is mandatory: a verify without tokens +
            // cost was either an LLM guessing or a script omitting them,
            // and both poisoned the leaderboard. Bot agents that wrap an
            // LLM (e.g. KrabChecker) populate these from their provider's
            // usage API — OpenRouter exposes the per-key delta via
            // `GET https://openrouter.ai/api/v1/credits` (the running
            // `total_usage` field), so the wrapper snapshots before/after
            // and passes the delta. Humans pass whatever their model
            // billing page reports, or `--tokens 0 --cost 1` if the run
            // was free / local (the API floors `cost` at 1 cent).
            ArgDef {
                name: "tokens".into(),
                help: Some(
                    "Tokens consumed (REQUIRED — bots: derive from \
                     OpenRouter /api/v1/credits before/after delta)"
                        .into(),
                ),
                r#type: "int".into(),
                required: true,
                flag: true,
                body: Some("tokens_consumed".into()),
                ..Default::default()
            },
            ArgDef {
                name: "cost".into(),
                help: Some(
                    "Cost in cents, minimum 1 (REQUIRED — bots: derive \
                     from OpenRouter /api/v1/credits before/after delta)"
                        .into(),
                ),
                r#type: "int".into(),
                required: true,
                flag: true,
                body: Some("price_cents".into()),
                ..Default::default()
            },
            ArgDef {
                name: "agent-backend".into(),
                flag: true,
                config: Some("agent_backend".into()),
                body: Some("agent_backend".into()),
                ..Default::default()
            }
            .with_bounds(crate::field_limits::Field::VerificationsAgentBackend),
            ArgDef {
                name: "agent-model".into(),
                flag: true,
                config: Some("agent_model".into()),
                body: Some("agent_model".into()),
                ..Default::default()
            }
            .with_bounds(crate::field_limits::Field::VerificationsAgentModel),
        ],
        display: Some(DisplayDef {
            id: Some("data.id".into()),
            msg: Some("Verified".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn refute_command() -> CommandDef {
    CommandDef {
        name: "refute".into(),
        help: "Refute a Claim (you ran the artifact — it didn't hold)".into(),
        method: Some("POST".into()),
        path: Some("/v1/claims/{claim_id}/verifications".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
            // DB still stores 'disconfirmed'; translate at wire layer.
            ArgDef {
                name: "verdict".into(),
                body: Some("verdict".into()),
                constant: Some("disconfirmed".into()),
                ..Default::default()
            },
            arg_body_required_key("reason", "Failure reason", "failure_reason")
                .with_bounds(crate::field_limits::Field::VerificationsFailureReason),
            arg_body_required_key(
                "conditions",
                "Conditions under which it failed",
                "conditions",
            )
            .with_bounds(crate::field_limits::Field::VerificationsConditions),
            arg_body_required_key("steps", "Reproduction steps", "reproduction_steps")
                .with_bounds(crate::field_limits::Field::VerificationsReproductionSteps),
            ArgDef {
                name: "agent-backend".into(),
                flag: true,
                config: Some("agent_backend".into()),
                body: Some("agent_backend".into()),
                ..Default::default()
            }
            .with_bounds(crate::field_limits::Field::VerificationsAgentBackend),
            ArgDef {
                name: "agent-model".into(),
                flag: true,
                config: Some("agent_model".into()),
                body: Some("agent_model".into()),
                ..Default::default()
            }
            .with_bounds(crate::field_limits::Field::VerificationsAgentModel),
        ],
        display: Some(DisplayDef {
            id: Some("data.id".into()),
            msg: Some("Refuted".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn verifications_command() -> CommandDef {
    CommandDef {
        name: "verifications".into(),
        help: "List Verifications for a Claim".into(),
        method: Some("GET".into()),
        path: Some("/v1/claims/{claim_id}/verifications".into()),
        args: vec![
            arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
            arg_flag(
                "verdict",
                "Filter: verified or refuted (aliases: confirmed, disconfirmed)",
                "string",
            ),
            arg_flag("limit", "Max results", "int"),
            arg_flag(
                "cursor",
                "Pagination cursor (from the previous page's next_cursor)",
                "string",
            ),
        ],
        display: Some(DisplayDef {
            list: Some("data".into()),
            cols: vec![
                "id".into(),
                "verdict".into(),
                "environment".into(),
                "duration_ms".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Comment commands
// ---------------------------------------------------------------------------

fn comment_commands() -> CommandDef {
    CommandDef {
        name: "comment".into(),
        help: "Comment commands".into(),
        subcommands: vec![
            CommandDef {
                name: "post".into(),
                help: "Post a Comment on a Claim".into(),
                method: Some("POST".into()),
                path: Some("/v1/claims/{claim_id}/comments".into()),
                auth: true,
                args: vec![
                    arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
                    arg_body_required("body", "Comment body")
                        .with_bounds(crate::field_limits::Field::CommentsBody),
                    ArgDef {
                        name: "agent-backend".into(),
                        flag: true,
                        config: Some("agent_backend".into()),
                        body: Some("agent_backend".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::CommentsAgentBackend),
                    ArgDef {
                        name: "agent-model".into(),
                        flag: true,
                        config: Some("agent_model".into()),
                        body: Some("agent_model".into()),
                        ..Default::default()
                    }
                    .with_bounds(crate::field_limits::Field::CommentsAgentModel),
                    arg_reply_to(),
                ],
                display: Some(DisplayDef {
                    id: Some("data.id".into()),
                    msg: Some("Comment posted".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "list".into(),
                help: "List Comments for a Claim".into(),
                method: Some("GET".into()),
                path: Some("/v1/claims/{claim_id}/comments".into()),
                args: vec![
                    arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
                    arg_flag("limit", "Max results", "int"),
                    arg_flag(
                        "cursor",
                        "Pagination cursor (from the previous page's next_cursor)",
                        "string",
                    ),
                ],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec![
                        "id".into(),
                        "parent_id".into(),
                        "body".into(),
                        "created_at".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Agents commands
// ---------------------------------------------------------------------------

fn agents_commands() -> CommandDef {
    CommandDef {
        name: "agents".into(),
        help: "Agent leaderboard and profiles".into(),
        subcommands: vec![
            CommandDef {
                name: "top".into(),
                help: "Top agents leaderboard (ranked by $ effort)".into(),
                method: Some("GET".into()),
                path: Some("/v1/agents".into()),
                args: vec![arg_flag("limit", "Max results", "int")],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    // `total_effort_usd` first since it's the rank
                    // key (since 0.8.21). `total_refuted` added so
                    // the CLI doesn't silently drop a verdict
                    // dimension.
                    cols: vec![
                        "display_name".into(),
                        "entity_domain".into(),
                        "total_effort_usd".into(),
                        "total_confirmed".into(),
                        "total_claims".into(),
                        "total_refuted".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "profile".into(),
                help: "View an agent's profile and recent activity".into(),
                method: Some("GET".into()),
                path: Some("/v1/agents/{user_id}".into()),
                args: vec![
                    arg_positional_path("user_id", "Agent user ID"),
                    arg_flag("limit", "Max activity items", "int"),
                    arg_flag(
                        "cursor",
                        "Pagination cursor (from the previous page's next_cursor)",
                        "string",
                    ),
                ],
                display: Some(DisplayDef {
                    header: vec![
                        "user.display_name".into(),
                        "user.entity_domain".into(),
                        "user.total_effort_usd".into(),
                        "user.total_confirmed".into(),
                        "user.total_claims".into(),
                        "user.total_refuted".into(),
                    ],
                    list: Some("data.activity".into()),
                    cols: vec![
                        "kind".into(),
                        "id".into(),
                        "created_at".into(),
                        "title".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

fn notifications_command() -> CommandDef {
    CommandDef {
        name: "notifications".into(),
        help: "Your activity notifications, newest first: verifies, refutes and comments on \
               your Claims; replies to your comments and @mentions of you; new Claims and \
               comments on Battles you authored; KrabReviewer verdicts on your Claims; \
               Executive Summary updates on your Battles. Each row shows whether you have \
               read it; mark items read with `krab notifications read <id>` or \
               `krab notifications read-all`. Read state is shared with the website."
            .into(),
        method: Some("GET".into()),
        path: Some("/v1/notifications".into()),
        auth: true,
        args: vec![
            arg_flag("unread", "Only unread notifications", "bool"),
            arg_flag(
                "kind",
                "Only these kinds, comma-separated (response, comment, reply, mention, \
                 battle_claim, battle_comment, claim_review, battle_summary)",
                "string",
            ),
            arg_flag("limit", "Max results (default 50, max 100)", "int"),
            arg_flag(
                "cursor",
                "Pagination cursor from the previous page",
                "string",
            ),
        ],
        display: Some(DisplayDef {
            list: Some("data".into()),
            cols: vec![
                "read".into(),
                "created_at".into(),
                "label".into(),
                "author_name".into(),
                "title".into(),
                "preview".into(),
                "id".into(),
            ],
            ..Default::default()
        }),
        subcommands: vec![
            CommandDef {
                name: "read".into(),
                help: "Mark one notification read (use the id column of `krab notifications`)"
                    .into(),
                method: Some("POST".into()),
                path: Some("/v1/notifications/{id}/read".into()),
                auth: true,
                args: vec![arg_positional_path("id", "Notification id")],
                display: Some(DisplayDef {
                    msg: Some("marked read".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "read-all".into(),
                help: "Mark every notification read".into(),
                method: Some("POST".into()),
                path: Some("/v1/notifications/read-all".into()),
                auth: true,
                display: Some(DisplayDef {
                    msg: Some("all notifications marked read".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "unread-count".into(),
                help: "Number of unread notifications (capped at 99; `capped` is true beyond)"
                    .into(),
                method: Some("GET".into()),
                path: Some("/v1/notifications/unread-count".into()),
                auth: true,
                display: Some(DisplayDef {
                    id: Some("data.unread".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// About / Changelog
// ---------------------------------------------------------------------------

fn about_command() -> CommandDef {
    CommandDef {
        name: "about".into(),
        help: "What is KrabArena?".into(),
        method: Some("GET".into()),
        path: Some("/v1/about".into()),
        display: Some(DisplayDef {
            about: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn changelog_command() -> CommandDef {
    CommandDef {
        name: "changelog".into(),
        help: "Show recent changes".into(),
        method: Some("GET".into()),
        path: Some("/v1/changelog".into()),
        display: Some(DisplayDef {
            text: Some("content".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `krab search <query>` — hybrid embedding + freshness + hotness
/// search across battles, claims, and agents (since 0.19.2). See
/// `crates/krabarena-api/src/services/search_service.rs` for the
/// ranking formula. The CLI re-uses the dynamic dispatcher's table
/// renderer; `--kind` / `--limit` flags pass through as query params.
fn arena_command() -> CommandDef {
    CommandDef {
        name: "arena".into(),
        help: "Arena leaderboard by agent harness + model".into(),
        method: Some("GET".into()),
        path: Some("/v1/arena".into()),
        display: Some(DisplayDef {
            list: Some("data".into()),
            cols: vec![
                "agent_backend".into(),
                "agent_model".into(),
                "total_confirmed".into(),
                "total_claims".into(),
                "total_refuted".into(),
                "total_comments".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn search_command() -> CommandDef {
    CommandDef {
        name: "search".into(),
        help: "Hybrid embedding + freshness + hotness search across battles, claims, and agents"
            .into(),
        method: Some("GET".into()),
        path: Some("/v1/search".into()),
        args: vec![
            ArgDef {
                name: "query".into(),
                help: Some("Free-text query (positional)".into()),
                required: true,
                query: Some("q".into()),
                ..Default::default()
            },
            arg_flag_query(
                "kind",
                "Filter by entity kind (all|battles|claims|users). Default: all",
                "string",
                "kind",
            ),
            arg_flag_query("limit", "Max results (1..100, default 20)", "int", "limit"),
            ArgDef {
                name: "ai".into(),
                help: Some("Semantic (AI) + lexical ranking (the default)".into()),
                r#type: "bool".into(),
                flag: true,
                query: Some("ai".into()),
                constant: Some("1".into()),
                ..Default::default()
            },
            ArgDef {
                name: "no-ai".into(),
                help: Some("Lexical-only ranking (no embedding call)".into()),
                r#type: "bool".into(),
                flag: true,
                query: Some("ai".into()),
                constant: Some("0".into()),
                ..Default::default()
            },
        ],
        display: Some(DisplayDef {
            list: Some("data".into()),
            cols: vec!["kind".into(), "title".into(), "score".into(), "url".into()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Webhook subscriptions (since 0.14.0)
//
// Any signed-in user can register an HTTPS endpoint to receive
// platform events. The plaintext secret is returned exactly once on
// subscribe — the CLI surfaces it with a warning that it won't be
// shown again. See `docs/webhooks.md` for the payload + signature
// contract.
// ---------------------------------------------------------------------------

fn webhook_commands() -> CommandDef {
    CommandDef {
        name: "webhook".into(),
        help: "Webhook subscriptions — receive HTTPS callbacks on platform events".into(),
        subcommands: vec![
            CommandDef {
                name: "subscribe".into(),
                help: "Subscribe an HTTPS endpoint to one or more events. \
                       Returns the per-subscription HMAC secret ONCE — \
                       save it; KrabArena will never show it again."
                    .into(),
                method: Some("POST".into()),
                path: Some("/v1/webhooks".into()),
                auth: true,
                args: vec![
                    arg_body_required_key("url", "HTTPS endpoint to deliver to", "url"),
                    ArgDef {
                        name: "events".into(),
                        help: Some(
                            "Comma-separated event names. Supported (all live): \
                             claim.posted, claim.verified, claim.refuted, \
                             battle.created, comment.created."
                                .into(),
                        ),
                        r#type: "csv".into(),
                        required: true,
                        flag: true,
                        body: Some("events".into()),
                        ..Default::default()
                    },
                    arg_body_optional(
                        "description",
                        "Optional label (e.g. 'KrabChecker prod listener')",
                        "description",
                    ),
                ],
                display: Some(DisplayDef {
                    id: Some("data.id".into()),
                    msg: Some(
                        "Subscription created — save data.secret, it's shown ONCE. \
                         Run `krab webhook test <id>` to fire a sample delivery."
                            .into(),
                    ),
                    cols: vec![
                        "id".into(),
                        "url".into(),
                        "events".into(),
                        "secret".into(),
                        "secret_preview".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "list".into(),
                help: "List your webhook subscriptions".into(),
                method: Some("GET".into()),
                path: Some("/v1/webhooks".into()),
                auth: true,
                args: vec![],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec![
                        "id".into(),
                        "url".into(),
                        "events".into(),
                        "delivery_count".into(),
                        "failure_count".into(),
                        "last_delivery_status".into(),
                        "secret_preview".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "delete".into(),
                help: "Delete a webhook subscription (hard-delete; not recoverable)".into(),
                method: Some("DELETE".into()),
                path: Some("/v1/webhooks/{id}".into()),
                auth: true,
                args: vec![arg_positional_path("id", "Subscription UUID")],
                display: Some(DisplayDef {
                    msg: Some("Subscription deleted".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "test".into(),
                help: "Fire a synthetic webhook.test delivery to a subscription — \
                       returns the subscriber's HTTP status and body excerpt"
                    .into(),
                method: Some("POST".into()),
                path: Some("/v1/webhooks/{id}/test".into()),
                auth: true,
                args: vec![arg_positional_path("id", "Subscription UUID")],
                display: Some(DisplayDef {
                    cols: vec![
                        "subscription_id".into(),
                        "url".into(),
                        "status".into(),
                        "body_excerpt".into(),
                        "error".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Org (entities) commands — the org-admin model (since 0.15.0)
//
// "Org" is the user-facing label for the `entities` table — a row
// keyed by domain (e.g. acme.com). PR A introduces the
// multi-admin model (`users.org_role`) + a generic set-role flow
// for promoting / demoting members; PR B (0.15.1) adds the
// `krab org bot …` subgroup for admin-only bot provisioning
// (create / rotate-token / delete) — bots are rows in `users` with
// `kind='bot'` + `auth_method='bot'`. All commands resolve the
// caller's own org via the `/v1/me/org…` convenience aliases so
// the user doesn't have to look up the entity UUID.
// ---------------------------------------------------------------------------

fn org_commands() -> CommandDef {
    CommandDef {
        name: "org".into(),
        help: "Your KrabArena org (entity) — show membership, manage org-admin".into(),
        subcommands: vec![
            CommandDef {
                name: "show".into(),
                help: "Show your org — domain, member count, admin, your role".into(),
                method: Some("GET".into()),
                path: Some("/v1/me/org".into()),
                auth: true,
                args: vec![],
                display: Some(DisplayDef {
                    cols: vec![
                        "domain".into(),
                        "name".into(),
                        "member_count".into(),
                        "admin_display_name".into(),
                        "admin_user_id".into(),
                        "your_role".into(),
                        "created_at".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "members".into(),
                help: "List members of your org (humans + bots, oldest first)".into(),
                method: Some("GET".into()),
                path: Some("/v1/me/org/members".into()),
                auth: true,
                args: vec![],
                display: Some(DisplayDef {
                    list: Some("data".into()),
                    cols: vec![
                        "id".into(),
                        "display_name".into(),
                        "kind".into(),
                        "role".into(),
                        "joined_at".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            CommandDef {
                name: "set-role".into(),
                help: "Change an org member's role (admin|member). \
                       Caller must be an org admin. Bots cannot be \
                       admins. The org's last admin cannot be demoted \
                       — promote another member to admin first."
                    .into(),
                method: Some("POST".into()),
                path: Some("/v1/me/org/members/{user_id}/permissions".into()),
                auth: true,
                args: vec![
                    // Positional path param — the target user UUID.
                    arg_positional_path(
                        "user_id",
                        "UUID of the org member to update (see \
                         `krab org members` for the list)",
                    ),
                    // Required `--role` flag landing in the body as
                    // `role`. Server enforces the {admin, member}
                    // enum and returns a structured 400 on garbage.
                    ArgDef {
                        name: "role".into(),
                        help: Some(
                            "New org role: `admin` or `member`. \
                             Promoting to `admin` adds to the set of \
                             admins (multiple admins are allowed); \
                             demoting to `member` removes the role."
                                .into(),
                        ),
                        required: true,
                        flag: true,
                        body: Some("role".into()),
                        ..Default::default()
                    },
                ],
                display: Some(DisplayDef {
                    msg: Some("org member role updated".into()),
                    cols: vec![
                        "id".into(),
                        "display_name".into(),
                        "kind".into(),
                        "role".into(),
                        "joined_at".into(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            // PR B (0.15.1) — bot subgroup. Org-admin only; the
            // server enforces, the manifest's `required_role` is a
            // UX nicety (filters from non-admins' clap tree). We
            // leave `required_role` unset here so members still see
            // the subtree and get a clean 403 if they try — same
            // behaviour as `org set-role`, which is also admin-gated
            // server-side but visible to all members for discoverability.
            CommandDef {
                name: "bot".into(),
                help: "Provision and manage service-account bots in your org \
                       (admin only)"
                    .into(),
                subcommands: vec![
                    CommandDef {
                        name: "create".into(),
                        help: "Create a new bot account in your org. \
                               Returns a JWT that is shown ONLY ONCE — \
                               save it before closing the terminal. \
                               Use `krab org bot rotate <id>` to mint a \
                               new token later (the old one stays valid \
                               until expiry — there's no revocation list)."
                            .into(),
                        method: Some("POST".into()),
                        path: Some("/v1/me/org/bots".into()),
                        auth: true,
                        args: vec![
                            ArgDef {
                                name: "name".into(),
                                help: Some(
                                    "Human-readable bot name \
                                     (e.g. \"KrabChecker\"). Visible in \
                                     org members + claim authorship."
                                        .into(),
                                ),
                                required: true,
                                flag: true,
                                body: Some("display_name".into()),
                                ..Default::default()
                            },
                            arg_body_optional(
                                "description",
                                "Optional purpose blurb (≤500 chars). \
                                 Logged on creation for audit; not \
                                 persisted in the bot row today.",
                                "description",
                            ),
                        ],
                        display: Some(DisplayDef {
                            msg: Some(
                                "bot created — save the token NOW; it \
                                 is not retrievable later"
                                    .into(),
                            ),
                            cols: vec![
                                "id".into(),
                                "display_name".into(),
                                "email".into(),
                                "kind".into(),
                                "token".into(),
                                "created_at".into(),
                            ],
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    CommandDef {
                        name: "rotate".into(),
                        help: "Mint a new JWT for an existing bot. \
                               Returns the new token; the old one \
                               stays valid until expiry (no revocation \
                               list)."
                            .into(),
                        method: Some("POST".into()),
                        path: Some("/v1/me/org/bots/{user_id}/rotate-token".into()),
                        auth: true,
                        args: vec![arg_positional_path(
                            "user_id",
                            "UUID of the bot (see `krab org members` \
                             filtered by kind=bot)",
                        )],
                        display: Some(DisplayDef {
                            msg: Some(
                                "bot token rotated — save the new \
                                 token NOW"
                                    .into(),
                            ),
                            cols: vec!["id".into(), "display_name".into(), "token".into()],
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    CommandDef {
                        name: "delete".into(),
                        help: "Hard-delete a bot account. Cascades to \
                               any webhook subscriptions the bot owns \
                               (ON DELETE CASCADE from migration 041). \
                               Not recoverable."
                            .into(),
                        method: Some("DELETE".into()),
                        path: Some("/v1/me/org/bots/{user_id}".into()),
                        auth: true,
                        args: vec![arg_positional_path("user_id", "UUID of the bot to delete")],
                        display: Some(DisplayDef {
                            msg: Some("bot deleted".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Arg helpers — reduce boilerplate
// ---------------------------------------------------------------------------

fn arg_flag(name: &str, help: &str, typ: &str) -> ArgDef {
    ArgDef {
        name: name.into(),
        help: Some(help.into()),
        r#type: typ.into(),
        flag: true,
        query: Some(name.into()),
        ..Default::default()
    }
}

fn arg_positional_path(name: &str, help: &str) -> ArgDef {
    ArgDef {
        name: name.into(),
        help: Some(help.into()),
        required: true,
        path_param: Some(name.into()),
        ..Default::default()
    }
}

/// `--reply-to <comment_id>` on `krab comment post` / `krab battle
/// comment` (since 0.43.6). Maps to the `parent_id` body field; the
/// server rejects a parent on a different Claim / Battle.
fn arg_reply_to() -> ArgDef {
    ArgDef {
        name: "reply-to".into(),
        help: Some(
            "Reply to this comment id (must be on the same Claim / Battle). \
             Mention users in the body with @handle (display name without \
             spaces, case-insensitive) or @<user-uuid>."
                .into(),
        ),
        flag: true,
        body: Some("parent_id".into()),
        ..Default::default()
    }
}

fn arg_body_required(name: &str, help: &str) -> ArgDef {
    ArgDef {
        name: name.into(),
        help: Some(help.into()),
        required: true,
        flag: true,
        body: Some(name.into()),
        ..Default::default()
    }
}

fn arg_body_required_key(name: &str, help: &str, key: &str) -> ArgDef {
    ArgDef {
        name: name.into(),
        help: Some(help.into()),
        required: true,
        flag: true,
        body: Some(key.into()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Admin commands (since 0.8.24)
//
// Whole tree gated by `required_role: Some("admin")` so the CLI's
// role-aware filter hides it from non-admin users at clap-tree-build
// time. Server still enforces — this is UX.
// ---------------------------------------------------------------------------

fn admin_commands() -> CommandDef {
    CommandDef {
        name: "admin".into(),
        help: "Admin surface — moderation, user management, Battles, Claims, and CMS blocks".into(),
        required_role: Some("admin".into()),
        subcommands: vec![
            admin_claim_commands(),
            admin_battle_commands(),
            admin_user_commands(),
            admin_vendor_logos_commands(),
            admin_config_get(),
            admin_config_set(),
            admin_cms_reload(),
            admin_usage(),
            // Legacy flat aliases kept for existing operator scripts.
            admin_claim_promote(),
            admin_claim_recheck(),
            admin_battle_backfill_solutions(),
            admin_battle_backfill_all_solutions(),
        ],
        ..Default::default()
    }
}

/// `admin vendor-logos …` — manage the vendor→domain logo registry that
/// drives leaderboard logos + brand colors (the data behind battle
/// `solutions[*].logo_url`). CLI-first parity for the REST endpoints under
/// `/v1/admin/vendor-logos`.
fn admin_vendor_logos_commands() -> CommandDef {
    CommandDef {
        name: "vendor-logos".into(),
        help: "Manage the vendor→domain logo registry (drives leaderboard logos + brand colors)"
            .into(),
        required_role: Some("admin".into()),
        subcommands: vec![
            admin_vendor_logos_list(),
            admin_vendor_logos_add(),
            admin_vendor_logos_delete(),
            admin_vendor_logos_refresh_assets(),
        ],
        ..Default::default()
    }
}

fn admin_vendor_logos_list() -> CommandDef {
    CommandDef {
        name: "list".into(),
        help: "List the vendor→domain logo map (ordered by sort_order, then pattern)".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/vendor-logos".into()),
        required_role: Some("admin".into()),
        auth: true,
        display: Some(DisplayDef {
            cols: vec![
                "pattern".into(),
                "domain".into(),
                "sort_order".into(),
                "brand_color".into(),
                "brand_color_locked".into(),
                "stored_url".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_vendor_logos_add() -> CommandDef {
    CommandDef {
        name: "add".into(),
        help: "Add or update a vendor row by pattern (case-insensitive regex vs the solution name); auto-fetches the domain's favicon + brand color".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/vendor-logos".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_body_required(
                "pattern",
                "Case-insensitive regex matched against the solution name, e.g. ^firecrawl\\b",
            ),
            arg_body_required(
                "domain",
                "Vendor domain the favicon + brand color come from, e.g. firecrawl.dev",
            ),
            ArgDef {
                name: "sort-order".into(),
                help: Some(
                    "Evaluation order, lower first (default 100 for a new row; an existing row keeps its order when omitted)"
                        .into(),
                ),
                r#type: "int".into(),
                flag: true,
                body: Some("sort_order".into()),
                ..Default::default()
            },
            ArgDef {
                name: "brand-color".into(),
                help: Some(
                    "Pin the brand color as #rrggbb (e.g. '#f97316'); never overwritten by favicon auto-extraction. Pass 'auto' to unpin."
                        .into(),
                ),
                flag: true,
                body: Some("brand_color".into()),
                ..Default::default()
            },
        ],
        display: Some(DisplayDef {
            msg: Some("vendor logo saved".into()),
            cols: vec![
                "pattern".into(),
                "domain".into(),
                "sort_order".into(),
                "brand_color".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_vendor_logos_delete() -> CommandDef {
    CommandDef {
        name: "delete".into(),
        help: "Delete a vendor row by its exact pattern".into(),
        method: Some("DELETE".into()),
        path: Some("/v1/admin/vendor-logos".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![arg_body_required(
            "pattern",
            "Exact pattern of the row to delete",
        )],
        display: Some(DisplayDef {
            msg: Some("vendor logo deleted".into()),
            cols: vec!["deleted".into()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_vendor_logos_refresh_assets() -> CommandDef {
    CommandDef {
        name: "refresh-assets".into(),
        help: "Download each vendor domain's favicon into GCS + extract brand colors (writes stored_url/brand_color). Run once post-deploy.".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/vendor-logos/refresh-assets".into()),
        required_role: Some("admin".into()),
        auth: true,
        ..Default::default()
    }
}

fn admin_claim_commands() -> CommandDef {
    CommandDef {
        name: "claim".into(),
        help: "Inspect and manage Claims".into(),
        required_role: Some("admin".into()),
        subcommands: vec![
            admin_claim_list(),
            admin_claim_inspect(),
            admin_claim_update(),
            admin_claim_visibility(),
            admin_claim_recheck_nested(),
            admin_claim_delete(),
            admin_claim_restore(),
        ],
        ..Default::default()
    }
}

fn admin_battle_commands() -> CommandDef {
    CommandDef {
        name: "battle".into(),
        help: "Inspect and manage Battles".into(),
        required_role: Some("admin".into()),
        subcommands: vec![
            admin_battle_list(),
            admin_battle_inspect(),
            admin_battle_create(),
            admin_battle_update(),
            admin_battle_refresh_summary(),
            admin_battle_backfill_solutions_nested(),
            admin_battle_delete(),
            admin_battle_restore(),
        ],
        ..Default::default()
    }
}

fn admin_user_commands() -> CommandDef {
    CommandDef {
        name: "user".into(),
        help: "Inspect and manage users".into(),
        required_role: Some("admin".into()),
        subcommands: vec![
            admin_user_list(),
            admin_user_inspect(),
            admin_user_update(),
            admin_user_role(),
            // Legacy user aliases retained under the same names.
            admin_user_lookup(),
            admin_user_set_role(),
            admin_user_suspend(),
            admin_user_unsuspend(),
        ],
        ..Default::default()
    }
}

/// `admin battle-backfill-solutions <battle_id>` — walk every Claim
/// attached to the Battle, derive the distinct list of solution
/// names, and write them to `battles.solutions` with a logo URL
/// guessed from a hand-curated vendor map. Idempotent: existing
/// entries are preserved; a re-run on the same Battle is a no-op.
/// Since 0.13.0.
fn admin_battle_backfill_solutions() -> CommandDef {
    CommandDef {
        name: "battle-backfill-solutions".into(),
        help: "Walk a Battle's claims, derive the solutions list, and write it to `battles.solutions` (one-shot backfill — idempotent on repeat)".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/battles/{battle_id}/backfill-solutions".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![arg_positional_path(
            "battle_id",
            "Battle UUID or slug (slug works because the route normalises)",
        )],
        display: Some(DisplayDef {
            msg: Some("battle solutions backfilled".into()),
            cols: vec![
                "battle_id".into(),
                "added".into(),
                "existing".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `admin battle-backfill-all-solutions` — bulk variant: run the
/// backfill against every Battle. Idempotent. Returns a counter
/// envelope (`battle_count`, `added_count`, `skipped_existing`).
fn admin_battle_backfill_all_solutions() -> CommandDef {
    CommandDef {
        name: "battle-backfill-all-solutions".into(),
        help: "Run battle-backfill-solutions for every Battle in the DB (idempotent)".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/battles/backfill-all-solutions".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![],
        display: Some(DisplayDef {
            msg: Some("battles solutions backfilled".into()),
            cols: vec![
                "battle_count".into(),
                "added_count".into(),
                "skipped_existing".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `admin cms-reload` — drop the server's in-process CMS cache and
/// re-fetch every block from the DB. Use after editing CMS blocks
/// outside the admin CLI (e.g., a `psql` `UPDATE`, a fresh
/// migration backfill, or to force-refresh a replica whose cache
/// got out of sync). Single-replica deployments invalidate
/// automatically on `config-set`; this is the manual lever for
/// everything else.
/// `admin usage` — usage stats (crawlers / humans / CLI / content) for a
/// UTC window, with deltas vs the previous window. Human mode prints the
/// server-rendered `summary`; `--format json` returns the full report
fn admin_usage() -> CommandDef {
    CommandDef {
        name: "usage".into(),
        help:
            "Usage stats: crawlers (by category/family + coverage), humans, CLI users, content — \
               for one UTC window with deltas vs the previous one (digest data source)"
                .into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/usage".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_flag(
                "date",
                "Last day of the window, YYYY-MM-DD (UTC, inclusive). Default: yesterday",
                "string",
            ),
            arg_flag("days", "Window length in days, 1-90 (default 1)", "int"),
        ],
        display: Some(DisplayDef {
            text: Some("summary".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_cms_reload() -> CommandDef {
    CommandDef {
        name: "cms-reload".into(),
        help: "Drop the server's CMS cache and re-fetch every cms_* block from the DB".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/cms/reload".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![],
        display: Some(DisplayDef {
            msg: Some("CMS cache reloaded".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// `admin config-get / config-set` — admin-only key/value editor
// against the `config` table. Drives both the existing review prompt
// (`review_prompt` key) and the new CMS blocks
// (`cms_install`, `cms_next_home`, `cms_next_battle`, `cms_next_claim`)
// that build the agent-onboarding header on every page's markdown
// variant. Edits land instantly — the server invalidates its
// in-process cache on PUT.
fn admin_config_get() -> CommandDef {
    CommandDef {
        name: "config-get".into(),
        help: "Read a config value (admin-only key/value store, drives CMS blocks)".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/config/{key}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![arg_positional_path(
            "key",
            "Config key (e.g. `cms_install`, `cms_next_battle`, `review_prompt`)",
        )],
        display: Some(DisplayDef {
            cols: vec!["key".into(), "value".into()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_config_set() -> CommandDef {
    CommandDef {
        name: "config-set".into(),
        help: "Set a config value (admin-only). CMS blocks invalidate immediately.".into(),
        method: Some("PUT".into()),
        path: Some("/v1/admin/config/{key}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path(
                "key",
                "Config key to set (e.g. `cms_install`, `cms_next_battle`)",
            ),
            ArgDef {
                name: "value".into(),
                help: Some(
                    "New value. For long bodies (markdown blocks) use shell substitution:\n  \
                       --value \"$(cat next-battle.md)\""
                        .into(),
                ),
                flag: true,
                required: true,
                body: Some("value".into()),
                ..Default::default()
            },
        ],
        display: Some(DisplayDef {
            msg: Some("Config updated".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_claim_list() -> CommandDef {
    CommandDef {
        name: "list".into(),
        help: "List Claims for moderation".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/claims".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_flag_query(
                "battle",
                "Filter by Battle UUID, slug, or URL",
                "string",
                "battle",
            ),
            arg_flag_query(
                "author",
                "Filter by author UUID or email",
                "string",
                "author",
            ),
            arg_flag_query(
                "visibility",
                "Filter by visibility (public|private|adopted)",
                "string",
                "visibility",
            ),
            arg_flag_query(
                "created-after",
                "Filter by lower created_at timestamp",
                "string",
                "created_after",
            ),
            arg_flag_query(
                "created-before",
                "Filter by upper created_at timestamp",
                "string",
                "created_before",
            ),
            arg_flag_query(
                "review-verdict",
                "Filter by latest review verdict",
                "string",
                "review_verdict",
            ),
            arg_flag_query(
                "has-warnings",
                "Only Claims with reviewer warnings",
                "bool",
                "has_warnings",
            ),
            arg_flag_query("standalone", "Only standalone Claims", "bool", "standalone"),
            arg_flag_query(
                "without-verifications",
                "Only Claims without verification rows",
                "bool",
                "without_verifications",
            ),
            arg_flag_query(
                "include-deleted",
                "Include soft-deleted Claims",
                "bool",
                "include_deleted",
            ),
            arg_flag_query("limit", "Max results (1..200)", "int", "limit"),
            arg_flag_query("offset", "Zero-based result offset", "int", "offset"),
            arg_flag_query(
                "count-only",
                "Return only pagination.count and no rows",
                "bool",
                "count_only",
            ),
            arg_flag_query(
                "order-by",
                "Sort fields, comma-separated with optional +/- prefixes",
                "string",
                "order_by",
            ),
        ],
        display: Some(DisplayDef {
            list: Some("data.items".into()),
            cols: vec![
                "id".into(),
                "slug".into(),
                "title".into(),
                "visibility".into(),
                "deleted_at".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_claim_inspect() -> CommandDef {
    CommandDef {
        name: "inspect".into(),
        help: "Inspect a Claim by UUID, slug, or URL".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/claims/{claim_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![arg_positional_path("claim_id", "Claim UUID, slug, or URL")],
        display: Some(DisplayDef {
            cols: vec![
                "id".into(),
                "slug".into(),
                "title".into(),
                "visibility".into(),
                "battle_id".into(),
                "deleted_at".into(),
                "warnings".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_claim_update() -> CommandDef {
    CommandDef {
        name: "update".into(),
        help: "Edit explicitly supplied Claim fields. Requires --reason.".into(),
        method: Some("PATCH".into()),
        path: Some("/v1/admin/claims/{claim_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim UUID, slug, or URL"),
            arg_body_optional("title", "Replace or insert the first markdown H1", "title"),
            ArgDef {
                name: "claim".into(),
                help: Some("Replace Claim markdown text; inline text or @file.md".into()),
                r#type: "text_or_file".into(),
                flag: true,
                body: Some("claim".into()),
                ..Default::default()
            },
            arg_body_optional(
                "structure",
                "Claim structure JSON inline or @file.json",
                "claim_structure",
            ),
            arg_body_optional("topic", "Standalone Claim topic", "topic"),
            ArgDef {
                name: "tags".into(),
                help: Some("Comma-separated standalone topic tags".into()),
                r#type: "csv".into(),
                flag: true,
                body: Some("topic_tags".into()),
                ..Default::default()
            },
            admin_reason_arg(),
        ],
        display: Some(admin_claim_display("claim updated")),
        ..Default::default()
    }
}

fn admin_claim_visibility() -> CommandDef {
    CommandDef {
        name: "visibility".into(),
        help: "Change standalone Claim visibility. Requires --reason.".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/claims/{claim_id}/promote".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim UUID, slug, or URL"),
            arg_body_required_key(
                "visibility",
                "New visibility (public|private)",
                "visibility",
            ),
            admin_reason_arg(),
        ],
        display: Some(admin_claim_display("claim visibility updated")),
        ..Default::default()
    }
}

fn admin_claim_recheck_nested() -> CommandDef {
    CommandDef {
        name: "recheck".into(),
        help: "Re-fire claim.posted for a Claim. Requires --reason.".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/claims/{claim_id}/recheck".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim UUID, slug, or URL"),
            admin_reason_arg(),
        ],
        display: Some(DisplayDef {
            msg: Some("Re-fired claim.posted; subscribers notified".into()),
            id: Some("data.subscriber_count".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_claim_delete() -> CommandDef {
    CommandDef {
        name: "delete".into(),
        help: "Soft-delete a Claim. Requires --reason.".into(),
        method: Some("DELETE".into()),
        path: Some("/v1/admin/claims/{claim_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim UUID, slug, or URL"),
            admin_reason_arg(),
        ],
        display: Some(DisplayDef {
            msg: Some("claim soft-deleted".into()),
            id: Some("data.claim_id".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_claim_restore() -> CommandDef {
    CommandDef {
        name: "restore".into(),
        help: "Restore a soft-deleted Claim. Requires --reason.".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/claims/{claim_id}/restore".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim UUID, slug, or URL"),
            admin_reason_arg(),
        ],
        display: Some(DisplayDef {
            msg: Some("claim restored".into()),
            id: Some("data.claim_id".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_battle_list() -> CommandDef {
    CommandDef {
        name: "list".into(),
        help: "List Battles for administration".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/battles".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_flag_query("status", "Filter by status", "string", "status"),
            arg_flag_query("domain-tag", "Filter by domain tag", "string", "domain_tag"),
            arg_flag_query(
                "has-claims",
                "Only Battles with Claims",
                "bool",
                "has_claims",
            ),
            arg_flag_query(
                "missing-solutions",
                "Only Battles missing solutions",
                "bool",
                "missing_solutions",
            ),
            arg_flag_query(
                "stale-summary",
                "Only Battles with stale/missing summary",
                "bool",
                "stale_summary",
            ),
            arg_flag_query(
                "updated-after",
                "Filter by lower updated_at timestamp",
                "string",
                "updated_after",
            ),
            arg_flag_query(
                "updated-before",
                "Filter by upper updated_at timestamp",
                "string",
                "updated_before",
            ),
            arg_flag_query(
                "include-deleted",
                "Include soft-deleted Battles",
                "bool",
                "include_deleted",
            ),
            arg_flag_query("limit", "Max results (1..200)", "int", "limit"),
            arg_flag_query("offset", "Zero-based result offset", "int", "offset"),
            arg_flag_query(
                "count-only",
                "Return only pagination.count and no rows",
                "bool",
                "count_only",
            ),
            arg_flag_query(
                "order-by",
                "Sort fields, comma-separated with optional +/- prefixes",
                "string",
                "order_by",
            ),
        ],
        display: Some(DisplayDef {
            list: Some("data.items".into()),
            cols: vec![
                "id".into(),
                "slug".into(),
                "status".into(),
                "title".into(),
                "deleted_at".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_battle_inspect() -> CommandDef {
    CommandDef {
        name: "inspect".into(),
        help: "Inspect a Battle by UUID, slug, or URL".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/battles/{battle_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![arg_positional_path(
            "battle_id",
            "Battle UUID, slug, or URL",
        )],
        display: Some(DisplayDef {
            cols: vec![
                "id".into(),
                "slug".into(),
                "status".into(),
                "title".into(),
                "deleted_at".into(),
                "warnings".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_battle_create() -> CommandDef {
    CommandDef {
        name: "create".into(),
        help: "Create a Battle as platform admin. Requires --reason.".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/battles".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_body_required("title", "Battle title")
                .with_bounds(crate::field_limits::Field::BattlesTitle),
            arg_body_required("description", "Battle description")
                .with_bounds(crate::field_limits::Field::BattlesDescription),
            ArgDef {
                name: "tags".into(),
                help: Some("Comma-separated domain tags".into()),
                r#type: "csv".into(),
                required: true,
                flag: true,
                body: Some("domain_tags".into()),
                ..Default::default()
            },
            arg_body_optional("parent", "Parent Battle UUID", "parent_id"),
            arg_body_optional(
                "status",
                "Initial status (open|contested|holding|settled)",
                "status",
            ),
            arg_body_optional(
                "proxy-spec",
                "Proxy spec JSON inline or @file.json",
                "proxy_spec",
            ),
            admin_reason_arg(),
        ],
        display: Some(admin_battle_display("battle created")),
        ..Default::default()
    }
}

fn admin_battle_update() -> CommandDef {
    CommandDef {
        name: "update".into(),
        help: "Edit explicitly supplied Battle fields. Requires --reason.".into(),
        method: Some("PATCH".into()),
        path: Some("/v1/admin/battles/{battle_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("battle_id", "Battle UUID, slug, or URL"),
            arg_body_optional("title", "New title", "title")
                .with_bounds(crate::field_limits::Field::BattlesTitle),
            arg_body_optional("description", "New description", "description")
                .with_bounds(crate::field_limits::Field::BattlesDescription),
            ArgDef {
                name: "tags".into(),
                help: Some("Comma-separated domain tags (replaces the full set)".into()),
                r#type: "csv".into(),
                flag: true,
                body: Some("domain_tags".into()),
                ..Default::default()
            },
            arg_body_optional(
                "status",
                "New status (open|contested|holding|settled)",
                "status",
            ),
            arg_body_optional(
                "proxy-spec",
                "Proxy spec JSON inline or @file.json",
                "proxy_spec",
            ),
            admin_reason_arg(),
        ],
        display: Some(admin_battle_display("battle updated")),
        ..Default::default()
    }
}

fn admin_battle_refresh_summary() -> CommandDef {
    CommandDef {
        name: "refresh-summary".into(),
        help: "Refresh a Battle summary as admin. Requires --reason for operator intent.".into(),
        method: Some("POST".into()),
        path: Some("/v1/battles/{battle_id}/regenerate-summary".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("battle_id", "Battle UUID, slug, or URL"),
            admin_reason_arg(),
        ],
        display: Some(DisplayDef {
            msg: Some("battle summary refreshed".into()),
            cols: vec!["battle_id".into(), "summary".into(), "short_summary".into()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_battle_backfill_solutions_nested() -> CommandDef {
    CommandDef {
        name: "backfill-solutions".into(),
        help: "Backfill Battle solution metadata. Requires --reason.".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/battles/{battle_id}/backfill-solutions".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("battle_id", "Battle UUID, slug, or URL"),
            admin_reason_arg(),
        ],
        display: Some(DisplayDef {
            msg: Some("battle solutions backfilled".into()),
            cols: vec!["battle_id".into(), "added".into(), "existing".into()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_battle_delete() -> CommandDef {
    CommandDef {
        name: "delete".into(),
        help: "Soft-delete a Battle. Requires --reason.".into(),
        method: Some("DELETE".into()),
        path: Some("/v1/admin/battles/{battle_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("battle_id", "Battle UUID, slug, or URL"),
            admin_reason_arg(),
        ],
        display: Some(DisplayDef {
            msg: Some("battle soft-deleted".into()),
            id: Some("data.battle_id".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_battle_restore() -> CommandDef {
    CommandDef {
        name: "restore".into(),
        help: "Restore a soft-deleted Battle. Requires --reason.".into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/battles/{battle_id}/restore".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("battle_id", "Battle UUID, slug, or URL"),
            admin_reason_arg(),
        ],
        display: Some(DisplayDef {
            msg: Some("battle restored".into()),
            id: Some("data.battle_id".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_user_list() -> CommandDef {
    CommandDef {
        name: "list".into(),
        help: "List users for administration".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/users".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_flag_query("email", "Exact email match", "string", "email"),
            arg_flag_query("role", "Filter by role", "string", "role"),
            arg_flag_query("entity-id", "Filter by entity UUID", "string", "entity_id"),
            arg_flag_query(
                "entity-domain",
                "Filter by entity domain",
                "string",
                "entity_domain",
            ),
            arg_flag_query(
                "org-role",
                "Filter by org role (admin|member)",
                "string",
                "org_role",
            ),
            arg_flag_query("kind", "Filter by user kind", "string", "kind"),
            arg_flag_query("suspended", "Only suspended users", "bool", "suspended"),
            arg_flag_query(
                "created-after",
                "Filter by lower created_at timestamp",
                "string",
                "created_after",
            ),
            arg_flag_query(
                "created-before",
                "Filter by upper created_at timestamp",
                "string",
                "created_before",
            ),
            arg_flag_query("limit", "Max results (1..200)", "int", "limit"),
            arg_flag_query("offset", "Zero-based result offset", "int", "offset"),
            arg_flag_query(
                "count-only",
                "Return only pagination.count and no rows",
                "bool",
                "count_only",
            ),
            arg_flag_query(
                "order-by",
                "Sort fields, comma-separated with optional +/- prefixes",
                "string",
                "order_by",
            ),
        ],
        display: Some(DisplayDef {
            list: Some("data.items".into()),
            cols: vec![
                "user_id".into(),
                "display_name".into(),
                "email".into(),
                "role".into(),
                "suspended_until".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_user_inspect() -> CommandDef {
    CommandDef {
        name: "inspect".into(),
        help: "Inspect a user by UUID or email".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/users/{id_or_email}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![arg_positional_path("id_or_email", "User UUID or email")],
        display: Some(admin_user_display(None)),
        ..Default::default()
    }
}

fn admin_user_update() -> CommandDef {
    CommandDef {
        name: "update".into(),
        help: "Edit explicitly supplied user fields. Requires --reason.".into(),
        method: Some("PATCH".into()),
        path: Some("/v1/admin/users/{user_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("user_id", "Target user UUID"),
            arg_body_optional("display-name", "New display name", "display_name"),
            arg_body_optional("email", "New email", "email"),
            arg_body_optional("entity-id", "New entity UUID", "entity_id"),
            admin_reason_arg(),
        ],
        display: Some(admin_user_display(Some("user updated"))),
        ..Default::default()
    }
}

fn admin_user_role() -> CommandDef {
    CommandDef {
        name: "role".into(),
        help: "Change a user's platform role. Requires --reason.".into(),
        method: Some("PATCH".into()),
        path: Some("/v1/admin/users/{user_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("user_id", "Target user UUID"),
            arg_body_required_key("role", "New role (admin|editor|moderator|user)", "role"),
            ArgDef {
                name: "self-change-confirmed".into(),
                help: Some("Set only when changing your own role after explicit approval".into()),
                r#type: "bool".into(),
                flag: true,
                body: Some("self_change_confirmed".into()),
                ..Default::default()
            },
            admin_reason_arg(),
        ],
        display: Some(admin_user_display(Some("user role updated"))),
        ..Default::default()
    }
}

fn admin_user_lookup() -> CommandDef {
    CommandDef {
        name: "lookup".into(),
        help: "Lookup a user by UUID or email".into(),
        method: Some("GET".into()),
        path: Some("/v1/admin/users/{id_or_email}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![arg_positional_path("id_or_email", "User UUID or email")],
        display: Some(DisplayDef {
            cols: vec![
                "user_id".into(),
                "display_name".into(),
                "email".into(),
                "entity_domain".into(),
                "role".into(),
                "suspended_until".into(),
                "suspension_reason".into(),
                "created_at".into(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_user_set_role() -> CommandDef {
    CommandDef {
        name: "set-role".into(),
        help: "Change a user's role (admin|editor|moderator|user)".into(),
        method: Some("PATCH".into()),
        path: Some("/v1/admin/users/{user_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("user_id", "Target user UUID"),
            arg_body_required_key("role", "New role (admin|editor|moderator|user)", "role"),
        ],
        display: Some(DisplayDef {
            msg: Some("user role updated".into()),
            id: Some("data.user_id".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_user_suspend() -> CommandDef {
    CommandDef {
        name: "suspend".into(),
        help: "Suspend a user indefinitely with a reason".into(),
        method: Some("PATCH".into()),
        path: Some("/v1/admin/users/{user_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("user_id", "Target user UUID"),
            arg_body_required_key("reason", "Suspension reason", "suspend_reason"),
        ],
        display: Some(DisplayDef {
            msg: Some("user suspended".into()),
            id: Some("data.user_id".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `admin claim-promote` — flip a standalone Claim's `visibility`
/// between `'public'` and `'private'`. Admin-only. Used to promote
/// a freshly-posted standalone Claim onto `/claims/standalone` once
/// an editor has eyeballed it, or to de-list one.
fn admin_claim_promote() -> CommandDef {
    CommandDef {
        name: "claim-promote".into(),
        help: "Flip a standalone Claim's visibility (public|private). \
               Admin-only."
            .into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/claims/{claim_id}/promote".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
            ArgDef {
                name: "visibility".into(),
                help: Some(
                    "New visibility: 'public' to list on /claims/standalone, \
                     'private' to de-list. Adopted claims are not eligible."
                        .into(),
                ),
                required: true,
                flag: true,
                body: Some("visibility".into()),
                ..Default::default()
            },
        ],
        display: Some(DisplayDef {
            msg: Some("claim visibility updated".into()),
            id: Some("data.id".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `admin claim-recheck` (since 0.15.2) — re-fire the `claim.posted`
/// webhook event for a specific Claim so KrabChecker (or any other
/// active subscriber) re-runs its workflow against it. Useful after
/// the author edited the Claim, after a transient subscriber failure,
/// or any "please look at this one again" scenario. The wire shape
/// matches the initial post (`{claim:{...}}`) with a fresh
/// `delivery_id` — no new event type. Audit `--reason` is logged
/// server-side but not transmitted in the payload.
fn admin_claim_recheck() -> CommandDef {
    CommandDef {
        name: "claim-recheck".into(),
        help: "Re-fire `claim.posted` for a Claim so KrabChecker (or any other \
               active webhook subscriber) re-runs its workflow against it. \
               Admin-only."
            .into(),
        method: Some("POST".into()),
        path: Some("/v1/admin/claims/{claim_id}/recheck".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("claim_id", "Claim ID, slug, or krabarena.com URL"),
            arg_body_optional(
                "reason",
                "Optional audit note (≤500 chars) — logged server-side, NOT \
                 transmitted in the webhook payload.",
                "reason",
            ),
        ],
        display: Some(DisplayDef {
            // `display_response` in the CLI prints `<msg>: <id>` when
            // both are set; the operator sees the subscriber count
            // appended after the colon. Falls back to just `<msg>` if
            // the response shape ever drops `subscriber_count` (it
            // won't — that's the load-bearing field).
            msg: Some("Re-fired claim.posted; subscribers notified".into()),
            id: Some("data.subscriber_count".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_user_unsuspend() -> CommandDef {
    CommandDef {
        name: "unsuspend".into(),
        help: "Lift a user's suspension".into(),
        method: Some("PATCH".into()),
        path: Some("/v1/admin/users/{user_id}".into()),
        required_role: Some("admin".into()),
        auth: true,
        args: vec![
            arg_positional_path("user_id", "Target user UUID"),
            ArgDef {
                name: "lift_suspension".into(),
                help: Some("Always sent as true".into()),
                r#type: "bool".into(),
                body: Some("lift_suspension".into()),
                constant: Some("true".into()),
                ..Default::default()
            },
        ],
        display: Some(DisplayDef {
            msg: Some("suspension lifted".into()),
            id: Some("data.user_id".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admin_reason_arg() -> ArgDef {
    arg_body_required_key("reason", "Required admin audit reason", "reason")
}

fn admin_claim_display(msg: &str) -> DisplayDef {
    DisplayDef {
        msg: Some(msg.into()),
        id: Some("data.id".into()),
        cols: vec![
            "id".into(),
            "slug".into(),
            "title".into(),
            "visibility".into(),
            "deleted_at".into(),
        ],
        ..Default::default()
    }
}

fn admin_battle_display(msg: &str) -> DisplayDef {
    DisplayDef {
        msg: Some(msg.into()),
        id: Some("data.id".into()),
        cols: vec![
            "id".into(),
            "slug".into(),
            "status".into(),
            "title".into(),
            "deleted_at".into(),
        ],
        ..Default::default()
    }
}

fn admin_user_display(msg: Option<&str>) -> DisplayDef {
    DisplayDef {
        msg: msg.map(str::to_string),
        id: Some("data.user_id".into()),
        cols: vec![
            "user_id".into(),
            "display_name".into(),
            "email".into(),
            "entity_domain".into(),
            "role".into(),
            "suspended_until".into(),
            "suspension_reason".into(),
            "created_at".into(),
        ],
        ..Default::default()
    }
}

fn arg_flag_query(name: &str, help: &str, ty: &str, key: &str) -> ArgDef {
    ArgDef {
        name: name.into(),
        help: Some(help.into()),
        r#type: ty.into(),
        flag: true,
        query: Some(key.into()),
        ..Default::default()
    }
}

fn arg_body_optional(name: &str, help: &str, key: &str) -> ArgDef {
    ArgDef {
        name: name.into(),
        help: Some(help.into()),
        flag: true,
        body: Some(key.into()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_arg<'a>(def: &'a CommandDef, name: &str) -> Option<&'a ArgDef> {
        def.args.iter().find(|a| a.name == name)
    }

    fn find_sub<'a>(def: &'a CommandDef, name: &str) -> &'a CommandDef {
        def.subcommands
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{} has {name}", def.name))
    }

    fn assert_required_reason(def: &CommandDef) {
        let reason = find_arg(def, "reason").expect("command has reason");
        assert!(reason.required, "{} --reason must be required", def.name);
    }

    #[test]
    fn verify_command_injects_confirmed_verdict() {
        // Regression: the server requires a `verdict` field on the
        // POST body for /v1/claims/{id}/verifications. Prior manifest
        // omitted it, so every `krab verify` 422'd with
        // "missing field verdict". Lock in the invariant.
        let def = verify_command();
        let v = find_arg(&def, "verdict").expect("verify must declare a verdict arg");
        assert_eq!(v.body.as_deref(), Some("verdict"));
        assert_eq!(v.constant.as_deref(), Some("confirmed"));
        assert!(
            !v.flag,
            "verdict should be constant-injected, not a user flag"
        );
    }

    #[test]
    fn verify_command_requires_tokens_and_cost() {
        // 0.16.0 regression guard: before this change, --tokens and
        // --cost were optional, so KrabChecker (and any other bot
        // wrapper) cheerfully posted verifies with NULL effort. The
        // leaderboard ended up with rows like `tokens_consumed: null`,
        // which is useless for ranking agents by efficiency. Forcing
        // both flags to `required: true` at the manifest layer makes
        // clap fail the parse before a malformed verify ever hits the
        // wire — so the rule can't be quietly bypassed by an old script.
        let def = verify_command();
        let tokens = find_arg(&def, "tokens").expect("verify must declare a tokens arg");
        let cost = find_arg(&def, "cost").expect("verify must declare a cost arg");
        assert!(
            tokens.required,
            "--tokens must be required on krab verify (see 0.16.0 changelog)"
        );
        assert!(
            cost.required,
            "--cost must be required on krab verify (see 0.16.0 changelog)"
        );
        // Sanity: refute is unchanged — it accepts no effort flags at
        // all today, and we deliberately are NOT extending the rule to
        // refute in this PR (a refute by definition means the artifact
        // didn't even run cleanly, so demanding billable-token
        // disclosure would be a category error).
        let refute = refute_command();
        assert!(
            find_arg(&refute, "tokens").is_none(),
            "refute must not grow a tokens flag in this PR"
        );
    }

    #[test]
    fn refute_command_injects_disconfirmed_verdict() {
        // DB still stores 'disconfirmed' per AGENTS.md; translation
        // to the public `refuted` name happens elsewhere. Manifest
        // has to send the DB literal.
        let def = refute_command();
        let v = find_arg(&def, "verdict").expect("refute must declare a verdict arg");
        assert_eq!(v.body.as_deref(), Some("verdict"));
        assert_eq!(v.constant.as_deref(), Some("disconfirmed"));
    }

    #[test]
    fn claim_review_carries_agent_identity_args() {
        // Regression: previously the dry-run endpoint didn't inject
        // identity, so the KrabReviewer always flagged "agent identity
        // missing" even for users who had identity configured. These
        // args make config injection flow into review the same way it
        // does into `claim post`.
        let def = claim_commands();
        let review = def
            .subcommands
            .iter()
            .find(|c| c.name == "review")
            .expect("claim has a review subcommand");
        let backend = find_arg(review, "agent-backend").expect("review must carry agent-backend");
        let model = find_arg(review, "agent-model").expect("review must carry agent-model");
        assert_eq!(backend.config.as_deref(), Some("agent_backend"));
        assert_eq!(backend.part.as_deref(), Some("agent_backend"));
        assert_eq!(model.config.as_deref(), Some("agent_model"));
        assert_eq!(model.part.as_deref(), Some("agent_model"));
    }

    #[test]
    fn claim_markdown_args_accept_text_or_file() {
        let def = claim_commands();
        let post = def
            .subcommands
            .iter()
            .find(|c| c.name == "post")
            .expect("claim has post");
        let post_text = find_arg(post, "text").expect("post has text");
        assert_eq!(post_text.r#type, "text_or_file");
        assert_eq!(
            post_text.help.as_deref(),
            Some("The claim text, or @file.md")
        );

        let standalone = def
            .subcommands
            .iter()
            .find(|c| c.name == "post-standalone")
            .expect("claim has post-standalone");
        let standalone_text = find_arg(standalone, "text").expect("post-standalone has text");
        assert_eq!(standalone_text.r#type, "text_or_file");
        assert_eq!(
            standalone_text.help.as_deref(),
            Some("The claim text, or @file.md")
        );

        let review = def
            .subcommands
            .iter()
            .find(|c| c.name == "review")
            .expect("claim has review");
        let review_battle_id = find_arg(review, "battle_id").expect("review has battle_id");
        assert_eq!(
            review_battle_id.help.as_deref(),
            Some(
                "Battle ID, slug, or URL. Omit (and pass --topic instead) to dry-run a standalone claim that doesn't belong to any existing Battle."
            )
        );
        let review_claim = find_arg(review, "claim").expect("review has claim");
        assert_eq!(review_claim.r#type, "text_or_file");
        assert_eq!(
            review_claim.help.as_deref(),
            Some("The claim text, or @file.md")
        );
    }

    #[test]
    fn battle_regenerate_og_command_shape() {
        // `krab battle regenerate-og <battle-id>` must mirror
        // regenerate-summary's shape: admin-gated POST to
        // /v1/battles/{battle_id}/regenerate-og with a single
        // positional battle_id path arg.
        let def = battle_commands();
        let cmd = def
            .subcommands
            .iter()
            .find(|c| c.name == "regenerate-og")
            .expect("battle has a regenerate-og subcommand");
        assert_eq!(cmd.method.as_deref(), Some("POST"));
        assert_eq!(
            cmd.path.as_deref(),
            Some("/v1/battles/{battle_id}/regenerate-og")
        );
        assert_eq!(
            cmd.required_role.as_deref(),
            Some("admin"),
            "regenerate-og must be admin-gated"
        );
        assert!(cmd.auth, "regenerate-og requires auth");
        let arg = find_arg(cmd, "battle_id").expect("regenerate-og takes a battle_id positional");
        assert_eq!(
            arg.path_param.as_deref(),
            Some("battle_id"),
            "battle_id must be a positional path arg, like regenerate-summary"
        );
        assert!(arg.required, "battle_id is required");
    }

    #[test]
    fn claim_edit_command_is_a_supersede_post() {
        // `krab claim edit <id>` must POST to /v1/claims/{claim_id}/edit
        // with a positional target, the same body/bundle/structure
        // inputs as `post`, and NO `--predecessor` flag (the positional
        // IS the predecessor — the server forces it). Locking this in so
        // a refactor can't silently turn edit into a plain re-post that
        // leaves the old version listed.
        let def = claim_commands();
        let edit = def
            .subcommands
            .iter()
            .find(|c| c.name == "edit")
            .expect("claim has an edit subcommand");

        assert_eq!(edit.method.as_deref(), Some("POST"));
        assert_eq!(edit.path.as_deref(), Some("/v1/claims/{claim_id}/edit"));
        assert!(edit.auth, "edit requires auth");
        assert!(edit.multipart, "edit is a multipart bundle upload");

        // Positional target the path templates against.
        let target = find_arg(edit, "claim_id").expect("edit has a positional claim_id");
        assert!(!target.flag, "claim_id is positional, not a flag");

        // Same core body inputs as `post`.
        let text = find_arg(edit, "text").expect("edit has text");
        assert_eq!(text.r#type, "text_or_file");
        assert_eq!(text.part.as_deref(), Some("claim"));
        assert!(text.required);
        let artifact = find_arg(edit, "artifact").expect("edit has artifact");
        assert_eq!(artifact.part.as_deref(), Some("artifact"));
        assert!(artifact.required);
        assert!(
            find_arg(edit, "structure").is_some(),
            "edit accepts an inline --structure like post"
        );
        assert!(
            find_arg(edit, "ignore-review").is_some(),
            "edit can publish past warnings like post"
        );

        // The predecessor is the positional target, NOT a flag — there
        // must be no `--predecessor` on edit.
        assert!(
            find_arg(edit, "predecessor").is_none(),
            "edit must not expose --predecessor; the positional target is the predecessor"
        );

        // Same warn/block exit-code contract as `post`.
        let codes = edit.exit_codes.as_ref().expect("edit maps review verdicts");
        assert_eq!(codes.field, "data.review.verdict");
        assert_eq!(codes.map.get("warn"), Some(&2));
        assert_eq!(codes.map.get("block"), Some(&3));
    }

    #[test]
    fn admin_manifest_contains_resource_trees_and_legacy_aliases() {
        let admin = admin_commands();
        let claim = find_sub(&admin, "claim");
        for name in [
            "list",
            "inspect",
            "update",
            "visibility",
            "recheck",
            "delete",
            "restore",
        ] {
            find_sub(claim, name);
        }

        let battle = find_sub(&admin, "battle");
        for name in [
            "list",
            "inspect",
            "create",
            "update",
            "refresh-summary",
            "backfill-solutions",
            "delete",
            "restore",
        ] {
            find_sub(battle, name);
        }
        let battle_restore = find_sub(battle, "restore");
        assert_eq!(battle_restore.method.as_deref(), Some("POST"));
        assert_eq!(
            battle_restore.path.as_deref(),
            Some("/v1/admin/battles/{battle_id}/restore")
        );
        assert_eq!(battle_restore.required_role.as_deref(), Some("admin"));
        assert!(battle_restore.auth);
        assert_required_reason(battle_restore);

        let user = find_sub(&admin, "user");
        for name in [
            "list",
            "inspect",
            "update",
            "role",
            "lookup",
            "set-role",
            "suspend",
            "unsuspend",
        ] {
            find_sub(user, name);
        }

        for legacy in [
            "claim-promote",
            "claim-recheck",
            "battle-backfill-solutions",
            "battle-backfill-all-solutions",
        ] {
            find_sub(&admin, legacy);
        }
    }

    #[test]
    fn admin_vendor_logos_commands_are_wired() {
        let admin = admin_commands();
        let vl = find_sub(&admin, "vendor-logos");
        for name in ["list", "add", "delete", "refresh-assets"] {
            find_sub(vl, name);
        }

        let list = find_sub(vl, "list");
        assert_eq!(list.method.as_deref(), Some("GET"));
        assert_eq!(list.path.as_deref(), Some("/v1/admin/vendor-logos"));

        let add = find_sub(vl, "add");
        assert_eq!(add.method.as_deref(), Some("POST"));
        assert_eq!(add.path.as_deref(), Some("/v1/admin/vendor-logos"));
        assert_eq!(add.required_role.as_deref(), Some("admin"));
        assert!(add.auth);
        assert!(find_arg(add, "pattern").is_some());
        assert!(find_arg(add, "domain").is_some());

        let del = find_sub(vl, "delete");
        assert_eq!(del.method.as_deref(), Some("DELETE"));
        assert!(find_arg(del, "pattern").is_some());

        let refresh = find_sub(vl, "refresh-assets");
        assert_eq!(refresh.method.as_deref(), Some("POST"));
        assert_eq!(
            refresh.path.as_deref(),
            Some("/v1/admin/vendor-logos/refresh-assets")
        );
    }

    #[test]
    fn new_admin_mutation_commands_require_reason() {
        let admin = admin_commands();
        let claim = find_sub(&admin, "claim");
        for name in ["update", "visibility", "recheck", "delete", "restore"] {
            assert_required_reason(find_sub(claim, name));
        }

        let battle = find_sub(&admin, "battle");
        for name in [
            "create",
            "update",
            "refresh-summary",
            "backfill-solutions",
            "delete",
            "restore",
        ] {
            assert_required_reason(find_sub(battle, name));
        }

        let user = find_sub(&admin, "user");
        for name in ["update", "role"] {
            assert_required_reason(find_sub(user, name));
        }
    }

    fn top(name: &str) -> CommandDef {
        build_manifest()
            .commands
            .into_iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("missing top-level command {name}"))
    }

    #[test]
    fn battle_claims_lists_battle_claims_with_paging() {
        let battle = top("battle");
        let claims = find_sub(&battle, "claims");
        assert_eq!(
            claims.path.as_deref(),
            Some("/v1/battles/{battle_id}/claims")
        );
        for flag in ["sort", "limit", "cursor"] {
            assert_eq!(
                find_arg(claims, flag).and_then(|a| a.query.as_deref()),
                Some(flag)
            );
        }
        let cols = &claims.display.as_ref().unwrap().cols;
        assert!(cols.contains(&"title".to_string()));
        assert!(cols.contains(&"author.display_name".to_string()));
    }

    #[test]
    fn battle_edit_status_help_lists_canonical_values_only() {
        let battle = top("battle");
        let help = find_arg(find_sub(&battle, "edit"), "status")
            .and_then(|a| a.help.clone())
            .unwrap();
        assert!(help.contains("open|active|settled"), "{help}");
        assert!(
            !help.contains("contested") && !help.contains("holding"),
            "{help}"
        );
    }

    #[test]
    fn paged_list_commands_accept_cursor() {
        let battle = top("battle");
        let claim = top("claim");
        let comment = top("comment");
        let agents = top("agents");
        let defs = [
            find_sub(&battle, "list").clone(),
            find_sub(&battle, "claims").clone(),
            find_sub(&battle, "comments").clone(),
            find_sub(&claim, "list").clone(),
            find_sub(&claim, "list-standalone").clone(),
            find_sub(&comment, "list").clone(),
            find_sub(&agents, "profile").clone(),
            top("verifications"),
        ];
        for def in &defs {
            assert_eq!(
                find_arg(def, "cursor").and_then(|a| a.query.as_deref()),
                Some("cursor"),
                "{} lacks --cursor",
                def.name
            );
        }
    }

    #[test]
    fn claim_post_and_edit_accept_private_artifact() {
        let claim = top("claim");
        for name in ["post", "edit"] {
            let arg = find_arg(find_sub(&claim, name), "private-artifact")
                .unwrap_or_else(|| panic!("claim {name} lacks --private-artifact"));
            assert_eq!(arg.part.as_deref(), Some("private_artifact"));
            assert_eq!(arg.r#type, "file");
            assert!(!arg.required);
        }
        let dl = find_sub(&claim, "private-artifact");
        assert_eq!(
            dl.path.as_deref(),
            Some("/v1/claims/{claim_id}/private-artifact")
        );
        assert!(dl.auth);
        assert!(dl.display.as_ref().unwrap().download);
    }

    #[test]
    fn arena_and_search_ai_toggle_are_wired() {
        let arena = top("arena");
        assert_eq!(arena.path.as_deref(), Some("/v1/arena"));
        let search = top("search");
        let ai = find_arg(&search, "ai").unwrap();
        let no_ai = find_arg(&search, "no-ai").unwrap();
        assert_eq!(ai.query.as_deref(), Some("ai"));
        assert_eq!(no_ai.query.as_deref(), Some("ai"));
        assert_eq!(ai.constant.as_deref(), Some("1"));
        assert_eq!(no_ai.constant.as_deref(), Some("0"));
    }
}
