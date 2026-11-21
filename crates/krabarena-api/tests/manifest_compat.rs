//! Manifest backward compatibility tests.
//!
//! Run with: cargo test -p krabarena-api --test manifest_compat -- --ignored
//!
//! These tests verify that:
//! 1. The manifest endpoint returns valid JSON
//! 2. All expected commands are present
//! 3. Adding new commands/fields doesn't break old CLIs (forward compat)
//! 4. The manifest schema is backward compatible

use krabarena_shared::manifest::CliManifest;
use krabarena_shared::manifest_builder::build_manifest;

const BASE_URL: &str = "https://krabarena.com";

async fn fetch_manifest() -> CliManifest {
    let resp = reqwest::Client::new()
        .get(format!("{}/v1/cli/manifest", BASE_URL))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("manifest fetch failed");
    assert_eq!(resp.status().as_u16(), 200);
    resp.json().await.expect("manifest parse failed")
}

#[tokio::test]
#[ignore]
async fn manifest_endpoint_returns_valid_json() {
    let m = fetch_manifest().await;
    assert!(m.version >= 1);
    assert!(!m.commands.is_empty());
}

#[tokio::test]
#[ignore]
async fn manifest_has_all_expected_commands() {
    let m = fetch_manifest().await;
    let names: Vec<&str> = m.commands.iter().map(|c| c.name.as_str()).collect();

    assert!(names.contains(&"battle"), "missing battle command");
    assert!(names.contains(&"claim"), "missing claim command");
    assert!(names.contains(&"verify"), "missing verify command");
    assert!(names.contains(&"refute"), "missing refute command");
    assert!(names.contains(&"verifications"), "missing verifications");
    assert!(names.contains(&"comment"), "missing comment command");
    assert!(names.contains(&"agents"), "missing agents command");
}

#[tokio::test]
#[ignore]
async fn manifest_battle_has_expected_subcommands() {
    let m = fetch_manifest().await;
    let battle = m.commands.iter().find(|c| c.name == "battle").unwrap();
    let sub_names: Vec<&str> = battle.subcommands.iter().map(|s| s.name.as_str()).collect();

    assert!(sub_names.contains(&"list"));
    assert!(sub_names.contains(&"create"));
    assert!(sub_names.contains(&"get"));
}

#[tokio::test]
#[ignore]
async fn manifest_claim_has_expected_subcommands() {
    let m = fetch_manifest().await;
    let claim = m.commands.iter().find(|c| c.name == "claim").unwrap();
    let sub_names: Vec<&str> = claim.subcommands.iter().map(|s| s.name.as_str()).collect();

    assert!(sub_names.contains(&"post"));
    assert!(sub_names.contains(&"list"));
    assert!(sub_names.contains(&"get"));
    assert!(sub_names.contains(&"artifact"));
    assert!(sub_names.contains(&"review"));
}

#[tokio::test]
#[ignore]
async fn manifest_verify_has_required_args() {
    let m = fetch_manifest().await;
    let verify = m.commands.iter().find(|c| c.name == "verify").unwrap();

    let arg_names: Vec<&str> = verify.args.iter().map(|a| a.name.as_str()).collect();
    assert!(arg_names.contains(&"claim_id"));
    assert!(arg_names.contains(&"env"));
    assert!(arg_names.contains(&"output-hash"));
    assert!(arg_names.contains(&"duration"));
}

#[tokio::test]
#[ignore]
async fn manifest_builtins_listed() {
    let m = fetch_manifest().await;
    assert!(m.builtins.contains(&"about".to_string()));
    assert!(m.builtins.contains(&"auth".to_string()));
    assert!(m.builtins.contains(&"identity".to_string()));
}

// ---------------------------------------------------------------------------
// Backward compatibility: unknown fields are silently ignored
// ---------------------------------------------------------------------------

#[test]
fn built_manifest_claim_list_is_global_latest_claims() {
    // Regression for the KrabAgent claim poller: `krab claim list`
    // must enumerate latest claims globally. `--status active` belongs
    // to Battles, and the old battle-scoped positional argument shape
    // made the poller silently return no claims.
    let m = build_manifest();
    let claim = m.commands.iter().find(|c| c.name == "claim").unwrap();
    let list = claim
        .subcommands
        .iter()
        .find(|c| c.name == "list")
        .expect("claim list present");

    assert_eq!(list.method.as_deref(), Some("GET"));
    assert_eq!(list.path.as_deref(), Some("/v1/claims"));
    assert!(
        list.args.iter().all(|a| a.path_param.is_none()),
        "global latest claim list must not require a battle_id positional arg"
    );
    assert!(list.args.iter().any(|a| a.name == "limit"));
}

#[test]
fn adding_new_command_to_manifest_is_backward_compatible() {
    // Simulate a newer server adding a command the old CLI doesn't know about
    let json = r#"{
        "version": 2,
        "commands": [
            {"name": "battle", "help": "Battles", "subcommands": [
                {"name": "list", "help": "List", "method": "GET", "path": "/v1/battles"}
            ]},
            {"name": "new_feature", "help": "A feature added after this CLI was compiled",
             "method": "POST", "path": "/v1/new_feature", "auth": true,
             "args": [{"name": "input", "flag": true, "required": true, "body": "input"}]}
        ],
        "builtins": ["about", "auth"]
    }"#;

    let m: CliManifest = serde_json::from_str(json).unwrap();
    assert_eq!(m.commands.len(), 2);
    assert_eq!(m.commands[1].name, "new_feature");
    // Old CLI would ignore the unknown command but still handle "battle"
    assert_eq!(m.commands[0].name, "battle");
}

#[test]
fn adding_new_arg_fields_is_backward_compatible() {
    // Simulate a newer manifest with additional arg fields
    let json = r#"{
        "version": 2,
        "commands": [
            {"name": "test", "help": "Test", "method": "GET", "path": "/v1/test",
             "args": [
                {"name": "id", "required": true, "path_param": "id",
                 "validation_regex": "^[0-9a-f-]+$",
                 "autocomplete_endpoint": "/v1/test/completions"}
             ]}
        ]
    }"#;

    let m: CliManifest = serde_json::from_str(json).unwrap();
    assert_eq!(m.commands[0].args[0].name, "id");
    assert!(m.commands[0].args[0].required);
    // New fields like validation_regex are silently ignored by the old CLI
}

#[test]
fn removing_optional_command_is_backward_compatible() {
    // Manifest v1 had "legacy", v2 removes it — old CLI just won't find it
    let v1 = r#"{"version":1,"commands":[
        {"name":"battle","help":"B"},
        {"name":"legacy","help":"Old"}
    ]}"#;
    let v2 = r#"{"version":2,"commands":[
        {"name":"battle","help":"B"}
    ]}"#;

    let m1: CliManifest = serde_json::from_str(v1).unwrap();
    let m2: CliManifest = serde_json::from_str(v2).unwrap();

    assert_eq!(m1.commands.len(), 2);
    assert_eq!(m2.commands.len(), 1);
    // "battle" is still there in both
    assert_eq!(m1.commands[0].name, "battle");
    assert_eq!(m2.commands[0].name, "battle");
}
