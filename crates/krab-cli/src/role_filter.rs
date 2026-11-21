//! Role-aware manifest filtering (since 0.8.24).
//!
//! `--help` and the dispatched clap tree show only the commands the
//! locally-stored token's `role` claim is allowed to run. The server
//! still enforces auth on the actual call — this is purely a UX
//! filter so a regular user doesn't see `krab admin` at all.
//!
//!
//! Token role decoding is best-effort: we split a JWT on `.`,
//! base64-decode the middle segment, parse JSON, read `role`. No
//! signature verification, no expiry check — those are the server's
//! job. If the token is malformed / expired / not a JWT at all,
//! we treat the user as signed-out (`role = None`).

use base64::Engine;
use krabarena_shared::manifest::{CliManifest, CommandDef};
use krabarena_shared::manifest_builder::role_satisfies;

/// Pull the `role` claim out of a stored JWT without verifying. Best
/// effort — `None` means "treat as signed-out / regular user" and
/// hides every gated command from `--help`. The server still
/// enforces auth on the wire so a malformed-token user can't
/// actually call admin endpoints.
pub fn decode_role(token: Option<&str>) -> Option<String> {
    let token = token?;
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("role")?.as_str().map(str::to_string)
}

/// Walk the manifest and drop any command whose `required_role` is
/// not satisfied by the local user's role. Subcommand trees are
/// recursed into — gating a parent (`admin`) implicitly hides every
/// descendant, but we also gate descendants explicitly so a future
/// non-admin parent could host a single admin-only sub.
pub fn filter_by_role(mut manifest: CliManifest, actual: Option<&str>) -> CliManifest {
    manifest
        .commands
        .retain_mut(|c| filter_cmd_in_place(c, actual));
    manifest
}

fn filter_cmd_in_place(cmd: &mut CommandDef, actual: Option<&str>) -> bool {
    if !role_satisfies(cmd.required_role.as_deref(), actual) {
        return false;
    }
    cmd.subcommands
        .retain_mut(|sub| filter_cmd_in_place(sub, actual));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use krabarena_shared::manifest::ArgDef;

    fn jwt_with(role: &str) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"typ":"JWT","alg":"HS256"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"role":"{role}","sub":"x"}}"#).as_bytes());
        format!("{header}.{payload}.signature-not-checked")
    }

    #[test]
    fn decode_role_admin() {
        let t = jwt_with("admin");
        assert_eq!(decode_role(Some(&t)).as_deref(), Some("admin"));
    }

    #[test]
    fn decode_role_user() {
        let t = jwt_with("user");
        assert_eq!(decode_role(Some(&t)).as_deref(), Some("user"));
    }

    #[test]
    fn decode_role_empty_or_garbage_is_none() {
        assert_eq!(decode_role(None), None);
        assert_eq!(decode_role(Some("")), None);
        assert_eq!(decode_role(Some("notajwt")), None);
        assert_eq!(decode_role(Some("a.b.c")), None);
    }

    fn admin_only_cmd() -> CommandDef {
        CommandDef {
            name: "admin".into(),
            help: "admin tree".into(),
            required_role: Some("admin".into()),
            subcommands: vec![CommandDef {
                name: "user".into(),
                help: "user mgmt".into(),
                required_role: Some("admin".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn open_cmd() -> CommandDef {
        CommandDef {
            name: "battle".into(),
            help: "battle tree".into(),
            args: vec![ArgDef {
                name: "id".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn admin_user_sees_admin_tree() {
        let m = CliManifest {
            version: 1,
            commands: vec![open_cmd(), admin_only_cmd()],
            builtins: vec![],
        };
        let filtered = filter_by_role(m, Some("admin"));
        assert_eq!(filtered.commands.len(), 2);
        assert!(filtered.commands.iter().any(|c| c.name == "admin"));
    }

    #[test]
    fn regular_user_does_not_see_admin_tree() {
        let m = CliManifest {
            version: 1,
            commands: vec![open_cmd(), admin_only_cmd()],
            builtins: vec![],
        };
        let filtered = filter_by_role(m, Some("user"));
        assert_eq!(filtered.commands.len(), 1);
        assert_eq!(filtered.commands[0].name, "battle");
    }

    #[test]
    fn signed_out_does_not_see_admin_tree() {
        let m = CliManifest {
            version: 1,
            commands: vec![open_cmd(), admin_only_cmd()],
            builtins: vec![],
        };
        let filtered = filter_by_role(m, None);
        assert_eq!(filtered.commands.len(), 1);
    }
}
