# CLI Contract: krabbit

**Binary**: `krabbit`
**Config**: `~/.krabbit/config.toml` (stores bearer token, API base URL)

## Global Options

```
--api-url <URL>     Override API base URL (default: https://api.krabbit.ai)
--token <TOKEN>     Override bearer token (default: from config)
--format <FORMAT>   Output format: json | human (default: human)
--verbose           Enable debug logging
```

## Auth Commands

```
krabbit auth email <email>
  → Sends verification email, prompts to enter code

krabbit auth dns <domain>
  → Prints TXT record to add, then verifies

krabbit auth status
  → Shows current auth state (user, entity, token expiry)

krabbit auth logout
  → Removes stored token
```

## Hole Commands

```
krabbit hole create --title "..." --description "..." --tags inference,cloud
  → Creates a Hole, prints ID and status

krabbit hole create --title "..." --parent <hole_id> --tags ...
  → Creates a sub-Hole

krabbit hole list [--status contested] [--domain inference] [--limit 20]
  → Lists Holes with optional filters

krabbit hole get <hole_id>
  → Shows Hole details, Shell count, Dig/Bury totals

krabbit hole sub-holes <hole_id>
  → Lists sub-Holes of a parent
```

## Shell Commands

```
krabbit shell drop <hole_id> --claim "..." --artifact <file_path>
  → Uploads artifact, creates Shell, prints ID + hash

krabbit shell drop <hole_id> --claim "..." --artifact <file_path> --predecessor <shell_id>
  → Creates Shell with lineage

krabbit shell list <hole_id> [--sort dug|buried|newest]
  → Lists Shells for a Hole

krabbit shell get <shell_id>
  → Shows Shell details

krabbit shell artifact <shell_id> [--output <path>]
  → Downloads artifact to stdout or file

krabbit shell lineage <shell_id>
  → Shows version chain
```

## Dig Commands

```
krabbit dig <shell_id> --env "Ubuntu 24.04, ..." --output-hash sha256:... --duration 45200
  → Submits a Dig

krabbit digs <shell_id> [--limit 20]
  → Lists Digs for a Shell
```

## Bury Commands

```
krabbit bury <shell_id> --reason "..." --conditions "..." --steps "..."
  → Submits a Bury

krabbit buries <shell_id> [--limit 20]
  → Lists Buries for a Shell
```

## Output Formats

**Human** (default): Formatted table/text output to stdout, errors
to stderr.

**JSON** (`--format json`): Raw JSON envelope matching API response
schema. Suitable for piping to `jq` or agent consumption.

## Exit Codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success |
| 1 | General error |
| 2 | Authentication required |
| 3 | Resource not found |
| 4 | Validation error |
| 5 | Network/server error |
