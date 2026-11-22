//! CLI command manifest — shared types between server and CLI.
//!
//! The server builds a `CliManifest` and serves it at `GET /v1/cli/manifest`.
//! The CLI fetches it, builds a clap command tree, and dispatches generically.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliManifest {
    pub version: u32,
    pub commands: Vec<CommandDef>,
    /// Names of builtin commands handled by hardcoded CLI logic (auth, about, identity).
    #[serde(default)]
    pub builtins: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandDef {
    pub name: String,
    pub help: String,
    #[serde(default)]
    pub subcommands: Vec<CommandDef>,
    /// HTTP method (GET, POST, PATCH, DELETE). Absent for parent-only commands.
    #[serde(default)]
    pub method: Option<String>,
    /// URL path template, e.g. "/v1/battles/{battle_id}".
    #[serde(default)]
    pub path: Option<String>,
    /// Whether this command requires authentication.
    #[serde(default)]
    pub auth: bool,
    /// Send as multipart form instead of JSON.
    #[serde(default)]
    pub multipart: bool,
    /// Return raw (status, body) for branching on error codes.
    #[serde(default)]
    pub raw_response: bool,
    #[serde(default)]
    pub args: Vec<ArgDef>,
    #[serde(default)]
    pub display: Option<DisplayDef>,
    /// Map response field values to non-zero exit codes.
    #[serde(default)]
    pub exit_codes: Option<ExitCodeMap>,
    /// Minimum role this command requires. `None` = available to
    /// everyone. `Some("admin")` = only the local token's `role`
    /// claim of `admin` (or higher in the hierarchy) gets to see it
    /// in `--help` and dispatch it. The server still enforces auth
    /// on the actual call — this is a UX filter, not security.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgDef {
    pub name: String,
    #[serde(default)]
    pub help: Option<String>,
    /// Argument type: string, int, bool, csv, file.
    #[serde(default = "default_arg_type")]
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    /// If true, this is a --flag. If false, it's positional.
    #[serde(default)]
    pub flag: bool,
    /// Short flag character (e.g., 'o' for -o).
    #[serde(default)]
    pub short: Option<char>,
    // Placement — at most one of these should be set:
    /// URL query parameter key.
    #[serde(default)]
    pub query: Option<String>,
    /// JSON body field key.
    #[serde(default)]
    pub body: Option<String>,
    /// URL path parameter key (replaces {key} in path).
    #[serde(default)]
    pub path_param: Option<String>,
    /// Multipart form field key.
    #[serde(default)]
    pub part: Option<String>,
    /// Inject value from config (e.g., "agent_backend").
    #[serde(default)]
    pub config: Option<String>,
    /// A constant string to inject unconditionally. Used when the
    /// endpoint needs a fixed body field that the user doesn't
    /// control — e.g. `verdict=confirmed` on `krab verify` and
    /// `verdict=disconfirmed` on `krab refute`. The wire value is
    /// bound whether or not the user passes a flag; a `constant` arg
    /// shouldn't also be a flag.
    #[serde(default)]
    pub constant: Option<String>,
    /// Inclusive minimum `chars().count()` if this arg maps to a
    /// length-bounded text column. Derived from
    /// `krabarena_shared::field_limits::Field::min`; emitted so the
    /// CLI can render the constraint in `--help` and pre-validate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_chars: Option<usize>,
    /// Inclusive maximum `chars().count()`. See [`Self::min_chars`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
}

fn default_arg_type() -> String {
    "string".to_string()
}

impl ArgDef {
    /// Attach length bounds from a `field_limits::Field` to this arg
    /// so the CLI / agent tooling can pre-validate and render the
    /// constraint in `--help`. Returns `self` for fluent chaining at
    /// the call site:
    ///
    /// ```ignore
    /// arg_body_required("title", "Battle title")
    ///     .with_bounds(Field::BattlesTitle)
    /// ```
    pub fn with_bounds(mut self, field: crate::field_limits::Field) -> Self {
        let spec = field.spec();
        self.min_chars = Some(spec.min);
        self.max_chars = Some(spec.max);
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisplayDef {
    /// For list responses: JSON path to the array (e.g., "data").
    #[serde(default)]
    pub list: Option<String>,
    /// Columns to display in list mode.
    #[serde(default)]
    pub cols: Vec<String>,
    /// Single-object fields printed as `label: value` before a list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header: Vec<String>,
    /// JSON path to an ID field to print to stdout.
    #[serde(default)]
    pub id: Option<String>,
    /// Success message for stderr.
    #[serde(default)]
    pub msg: Option<String>,
    /// Binary download mode.
    #[serde(default)]
    pub download: bool,
    /// Special review rendering mode (hardcoded display).
    #[serde(default)]
    pub review: bool,
    /// About page rendering mode.
    #[serde(default)]
    pub about: bool,
    /// Raw text rendering mode (prints a text field as-is).
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitCodeMap {
    /// JSON path to the field that determines exit code (e.g., "data.verdict").
    pub field: String,
    /// Map of field values to exit codes.
    pub map: std::collections::HashMap<String, u8>,
}

impl Default for ArgDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            help: None,
            r#type: "string".into(),
            required: false,
            flag: false,
            short: None,
            query: None,
            body: None,
            path_param: None,
            part: None,
            config: None,
            constant: None,
            min_chars: None,
            max_chars: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trip() {
        let manifest = CliManifest {
            version: 1,
            commands: vec![CommandDef {
                name: "test".into(),
                help: "A test command".into(),
                subcommands: vec![],
                method: Some("GET".into()),
                path: Some("/v1/test".into()),
                auth: false,
                multipart: false,
                raw_response: false,
                args: vec![ArgDef {
                    name: "id".into(),
                    help: Some("Test ID".into()),
                    r#type: "string".into(),
                    required: true,
                    flag: false,
                    short: None,
                    query: None,
                    body: None,
                    path_param: Some("id".into()),
                    part: None,
                    config: None,
                    constant: None,
                    min_chars: None,
                    max_chars: None,
                }],
                display: Some(DisplayDef {
                    list: None,
                    cols: vec![],
                    header: vec![],
                    id: Some("data.id".into()),
                    msg: Some("Done".into()),
                    download: false,
                    review: false,
                    about: false,
                    text: None,
                }),
                exit_codes: None,
                required_role: None,
            }],
            builtins: vec!["about".into()],
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: CliManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(parsed.commands[0].name, "test");
        assert_eq!(parsed.commands[0].args[0].path_param.as_deref(), Some("id"));
    }

    #[test]
    fn manifest_deserialize_with_defaults() {
        let json = r#"{"version":1,"commands":[{"name":"x","help":"y"}]}"#;
        let m: CliManifest = serde_json::from_str(json).unwrap();
        assert!(m.commands[0].subcommands.is_empty());
        assert!(!m.commands[0].auth);
        assert!(m.builtins.is_empty());
    }

    #[test]
    fn new_fields_are_backward_compatible() {
        // Simulate a manifest with an unknown field — should deserialize fine
        let json = r#"{
            "version": 2,
            "commands": [{"name": "x", "help": "y", "new_future_field": true}],
            "builtins": [],
            "some_new_top_level": "ignored"
        }"#;
        let m: CliManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.version, 2);
        assert_eq!(m.commands[0].name, "x");
    }
}
