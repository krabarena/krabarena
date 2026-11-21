//! Regression guard for the field-length-limits source-of-truth
//! contract (introduced 2026-05-28 / v0.11.0).
//!
//! These tests don't need a database — they pin the *wire* surface
//! produced by `krabarena_shared::field_limits` so a future change to
//! the source-of-truth file can't silently break the public error
//! shape, the CLI manifest, or the docs render. The DB-side invariant
//! is enforced at server boot by `db::schema_check::verify_or_die`
//! against `pg_constraint`, which needs a live pool; see that module
//! for the runtime check.

use axum::body::to_bytes;
use axum::response::IntoResponse;
use krabarena_api::error::ApiError;
use krabarena_shared::field_limits::{ALL_FIELDS, Field, FieldLengthError, Table};
use krabarena_shared::manifest_builder::build_manifest;

#[tokio::test]
async fn api_error_field_too_long_is_structured_400() {
    // Direct construction stands in for "route returned FieldLength";
    // the IntoResponse impl is what every handler emits, so testing
    // the conversion is testing the wire contract.
    let err: ApiError = FieldLengthError::TooLong {
        table: "battle_candidates",
        field: "rationale",
        max: 2000,
        submitted: 2292,
    }
    .into();
    let resp = err.into_response();
    assert_eq!(resp.status(), 400, "field-too-long must be HTTP 400");

    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("collect body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("body is JSON");

    assert_eq!(body["error"]["code"], "FIELD_TOO_LONG");
    let details = &body["error"]["details"];
    assert_eq!(details["table"], "battle_candidates");
    assert_eq!(details["field"], "rationale");
    assert_eq!(details["limit_chars"], 2000);
    assert_eq!(details["submitted_chars"], 2292);
}

#[tokio::test]
async fn api_error_field_too_short_is_structured_400() {
    let err: ApiError = FieldLengthError::TooShort {
        table: "battle_candidates",
        field: "title",
        min: 8,
        submitted: 5,
    }
    .into();
    let resp = err.into_response();
    assert_eq!(resp.status(), 400);

    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("collect body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("body is JSON");

    assert_eq!(body["error"]["code"], "FIELD_TOO_SHORT");
    assert_eq!(body["error"]["details"]["limit_chars"], 8);
    assert_eq!(body["error"]["details"]["submitted_chars"], 5);
}

#[test]
fn manifest_carries_bounds_on_battle_create_title() {
    // The CLI builds `krab battle create --title <…>` from the manifest;
    // `--help` reads `min_chars`/`max_chars` from the arg. If a future
    // refactor drops `.with_bounds(Field::BattlesTitle)` from the
    // manifest_builder call site, this test catches it.
    let m = build_manifest();
    let battle = m
        .commands
        .iter()
        .find(|c| c.name == "battle")
        .expect("battle command present");
    let create = battle
        .subcommands
        .iter()
        .find(|c| c.name == "create")
        .expect("battle create present");
    let title_arg = create
        .args
        .iter()
        .find(|a| a.name == "title")
        .expect("title arg present");
    assert_eq!(
        title_arg.min_chars,
        Some(Field::BattlesTitle.min()),
        "battle create --title must advertise min_chars"
    );
    assert_eq!(
        title_arg.max_chars,
        Some(Field::BattlesTitle.max()),
        "battle create --title must advertise max_chars"
    );
}

#[test]
fn manifest_carries_bounds_on_comment_post_body() {
    let m = build_manifest();
    let comment = m
        .commands
        .iter()
        .find(|c| c.name == "comment")
        .expect("comment command present");
    let post = comment
        .subcommands
        .iter()
        .find(|c| c.name == "post")
        .expect("comment post present");
    let body_arg = post
        .args
        .iter()
        .find(|a| a.name == "body")
        .expect("body arg present");
    assert_eq!(body_arg.min_chars, Some(Field::CommentsBody.min()));
    assert_eq!(body_arg.max_chars, Some(Field::CommentsBody.max()));
}

/// Regression for the KrabScout-bailed-with-exit-2 bug: the prompt
/// referenced `krab battle comment <id>` but the manifest didn't
/// expose it, so Hermes' clap tree didn't have the subcommand and
/// the dispatcher exited 2. Migration 044 / 0.15.7 ships the
/// command — this test catches it disappearing.
#[test]
fn manifest_has_battle_comment_command_with_bounds() {
    let m = build_manifest();
    let battle = m
        .commands
        .iter()
        .find(|c| c.name == "battle")
        .expect("battle command present");
    let comment = battle
        .subcommands
        .iter()
        .find(|c| c.name == "comment")
        .expect("battle comment subcommand present");

    // Wire shape: POST /v1/battles/{battle_id}/comments with auth.
    assert_eq!(comment.method.as_deref(), Some("POST"));
    assert_eq!(
        comment.path.as_deref(),
        Some("/v1/battles/{battle_id}/comments")
    );
    assert!(comment.auth, "battle comment requires auth");

    // Body field is named `--text` (consistent with `claim post --text`)
    // and lands in the JSON body under `body` — matches what the server
    // route handler decodes into `CreateCommentRequest::body`.
    let text_arg = comment
        .args
        .iter()
        .find(|a| a.name == "text")
        .expect("--text arg present");
    assert!(text_arg.required, "comment text is required");
    assert!(text_arg.flag, "comment text is a flag, not positional");
    assert_eq!(text_arg.body.as_deref(), Some("body"));
    assert_eq!(text_arg.min_chars, Some(Field::CommentsBody.min()));
    assert_eq!(text_arg.max_chars, Some(Field::CommentsBody.max()));
}

#[test]
fn manifest_has_battle_comments_list_command() {
    let m = build_manifest();
    let battle = m
        .commands
        .iter()
        .find(|c| c.name == "battle")
        .expect("battle command present");
    let comments = battle
        .subcommands
        .iter()
        .find(|c| c.name == "comments")
        .expect("battle comments subcommand present");
    assert_eq!(comments.method.as_deref(), Some("GET"));
    assert_eq!(
        comments.path.as_deref(),
        Some("/v1/battles/{battle_id}/comments")
    );
}

#[test]
fn docs_section_renders_per_table_scope() {
    // Battle-structure scope = battles + battle_candidates only —
    // claim fields, verification fields, etc. must NOT appear.
    let md = krabarena_shared::field_limits::render_markdown_section(&[
        Table::Battles,
        Table::BattleCandidates,
    ]);
    assert!(md.contains("## Field length limits"));
    assert!(md.contains("### `battles`"));
    assert!(md.contains("### `battle_candidates`"));
    assert!(md.contains("`title`"));
    assert!(md.contains("`description`"));
    assert!(md.contains("500"), "battles.title cap 500 should appear");
    assert!(
        md.contains("2000"),
        "candidate rationale cap 2000 should appear"
    );
    // Other tables out of scope:
    assert!(!md.contains("### `claims`"));
    assert!(!md.contains("### `verifications`"));
    assert!(!md.contains("### `comments`"));
}

#[test]
fn docs_section_empty_scope_renders_all_tables() {
    let md = krabarena_shared::field_limits::render_markdown_section(&[]);
    // Every bounded table should be present.
    for f in ALL_FIELDS {
        let header = format!("### `{}`", f.table().name());
        assert!(md.contains(&header), "missing header for {header}");
    }
}
