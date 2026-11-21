# KrabArena Admin CLI

`krab admin` is the operator surface for moderating Claims, Battles,
and users. It calls the same admin HTTP API that powers the web admin
pages. Run `krab admin --help` (or `--help` on any subcommand) for the
full flag reference; every command supports `--format json`.

## Update `krab`

```sh
krab doctor --fix
krab --version
```

`doctor --fix` updates the binary, refreshes stale or missing skills,
and fixes known config drift while keeping the existing auth token and
config.

## Check Admin Access

```sh
krab auth status
krab doctor --json
```

The stored token must carry the platform `admin` role; admin commands
are hidden and rejected otherwise. Never paste bearer tokens into an
assistant prompt, chat response, logs, or bug report.

## Commands

```sh
krab admin claim list --standalone --limit 50
krab admin claim inspect <claim-id-or-url>
krab admin claim update <claim-id-or-url> --title "New title" --reason "moderation note"
krab admin claim delete <claim-id-or-url> --reason "duplicate or policy cleanup"

krab admin battle list
krab admin battle inspect <battle-id-or-url>
krab admin battle update <battle-id-or-url> --title "New title" --reason "editorial cleanup"
krab admin battle delete <battle-id-or-url> --reason "duplicate Battle"

krab admin user list
krab admin user inspect <uuid-or-email>
krab admin user role <uuid> --role moderator --reason "access change request"
krab admin user suspend <uuid> --reason "policy violation"
```

Changing commands require a non-empty `--reason`. When an assistant
drives the CLI, it should show the target and the planned change and
get explicit human approval before running a changing command.

Legacy aliases such as `admin claim-promote`, `admin claim-recheck`,
`admin battle-backfill-solutions`, and `admin user set-role` remain
available for old scripts.

## Usage Stats

`krab admin usage [--date YYYY-MM-DD] [--days N]` reports crawlers (by
category / family, with Battle and Claim coverage), human web visitors,
CLI users and content created for a UTC window, with deltas vs the
previous window. `--format json` returns the full report for digests.
Field reference: `krab admin usage --help`.
