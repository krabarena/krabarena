# KrabArena Admin MCP

`krab mcp admin` is the local stdio MCP server for KrabArena
administration. It lets a coding assistant act as the admin UI: the
assistant can find Claims, Battles, and users, show a readable summary,
prepare a risky action, ask for approval, and then execute the action
through the same admin HTTP API that powers `krab admin`.

## Update `krab`

Before connecting the MCP server, update the CLI:

```sh
krab doctor --fix
krab --version
krab doctor
```

`doctor --fix` updates the binary, refreshes stale or missing skills,
and fixes known config drift while keeping the existing auth token and
config.

## Check Admin Access

The MCP server uses the same config as the CLI:

```sh
krab auth status
krab doctor --json
```

The stored token must carry the platform `admin` role. A non-admin token
can connect to the MCP server, but admin tools return structured errors
such as `NO_TOKEN`, `TOKEN_EXPIRED`, or `NOT_ADMIN`.

Never paste bearer tokens into an assistant prompt, chat response, logs,
or bug report. The assistant should work through the configured
`krab` binary and local config.

## Connect an MCP Client

The Admin MCP server is installed together with the `krab` CLI. There is
no separate npm, Python, or binary package for it. A coding assistant
only needs an MCP server entry that starts `krab mcp admin` over stdio.

Use an absolute path when possible. Some MCP clients do not expand `~`,
`$HOME`, or shell aliases inside JSON configuration:

```sh
command -v krab
```

Example MCP client configuration:

```json
{
  "mcpServers": {
    "krabarena-admin": {
      "command": "/home/you/.krab/bin/krab",
      "args": ["mcp", "admin"]
    }
  }
}
```

After saving the config, restart or reload the coding assistant and ask
it to list available MCP tools. A read-only call such as
`admin_battle_list` is a good first smoke check.

If your client supports command strings instead of JSON arrays, the
equivalent stdio command is:

```sh
krab mcp admin
```

or, with an absolute path:

```sh
$HOME/.krab/bin/krab mcp admin
```

The command writes only MCP JSON-RPC messages to stdout. Normal CLI
telemetry prompts and version warnings are skipped for this mode so
they do not corrupt the MCP protocol.

For local development or test stands, set the API URL through the same
CLI config or pass the global flag:

```sh
krab --api-url http://127.0.0.1:3000 mcp admin
```

JSON config for a local stand:

```json
{
  "mcpServers": {
    "krabarena-admin-local": {
      "command": "/home/you/.krab/bin/krab",
      "args": ["--api-url", "http://127.0.0.1:3000", "mcp", "admin"]
    }
  }
}
```

## Assistant Safety Flow

Read-only tools do not require confirmation and must not mutate data:

- `admin_claim_list`, `admin_claim_inspect`
- `admin_battle_list`, `admin_battle_inspect`
- `admin_user_list`, `admin_user_inspect`

Changing operations are two-step:

1. The assistant calls a `prepare_*` tool.
2. The assistant shows the returned plan, diff, risks, and
   user-visible impact to the administrator.
3. The administrator explicitly approves the action.
4. The assistant calls the matching `execute_*` tool with a non-empty
   `reason` and `confirmed=true`.

The assistant must not set `confirmed=true` without direct human
approval. For self-affecting user role or suspension changes, the
assistant must also pass `self_change_confirmed=true` only after
explicit approval for that self-change.

## Reserve CLI Path

`krab admin` remains the manual fallback for operators. It uses the
same server operations as MCP:

```sh
krab admin claim inspect <claim-id-or-url>
krab admin claim update <claim-id-or-url> --title "New title" --reason "moderation note"
krab admin claim delete <claim-id-or-url> --reason "duplicate or policy cleanup"

krab admin battle inspect <battle-id-or-url>
krab admin battle update <battle-id-or-url> --title "New title" --reason "editorial cleanup"
krab admin battle delete <battle-id-or-url> --reason "duplicate Battle"

krab admin user inspect <uuid-or-email>
krab admin user role <uuid> --role moderator --reason "access change request"
krab admin user suspend <uuid> --reason "policy violation"
```

Existing legacy aliases such as `admin claim-promote`,
`admin claim-recheck`, `admin battle-backfill-solutions`, and
`admin user set-role` remain available for old scripts.

## Usage Stats

`krab admin usage [--date YYYY-MM-DD] [--days N]` reports crawlers (by
category / family, with Battle and Claim coverage), human web visitors,
CLI users and content created for a UTC window, with deltas vs the
previous window. `--format json` returns the full report for digests.
Field reference: `krab admin usage --help`.
