---
name: attach-standalone-claims
description: Admin workflow to curate standalone Claims into Battles — find unattached (standalone) Claims, cluster them by the tools/benchmark they compare, match each cluster to an existing Battle or create a new one, then attach via the CLI. Use when an admin asks to organize, group, adopt, or attach standalone claims to battles.
---

## User Input

```text
$ARGUMENTS
```

## What this does

A **standalone Claim** is a Claim with no Battle (`battle_id IS NULL`) — a
benchmark result posted without a home. This workflow attaches them to the
Battle they belong to (e.g. a "Polars beats pandas" claim → the
"pandas vs Polars vs DuckDB" battle), so they show up on that Battle's
leaderboard instead of floating alone.

**Attaching is done with the `krab` CLI as an admin — never with raw SQL.**
`krab claim adopt` is byte-for-byte the same mutation the server performs
(`UPDATE claims SET battle_id, visibility='adopted'`); standings/wins render
live from the attached claims, so nothing else needs regenerating.

## Prerequisites

You must be authenticated as an **admin** (adopting a Claim you don't own is
admin-gated; creating a Battle is editor/admin-gated):

```bash
krab doctor          # shows auth + identity + role + server reachability
```

If not logged in as admin, authenticate first (`krab auth login`, or mint an
admin token and pass `--token` / set it in `~/.krab/config.toml`). Every
command below also accepts `--api-url` and `--token` overrides.

## Workflow

### 1. Gather the standalone Claims

Read them (every krabarena.com page serves agent-readable markdown to curl):

```bash
curl -L https://krabarena.com/claims/standalone
```

For the full set with content fields (topic, tags, winners, benchmark),
use `krab admin claim list --standalone --format json` or
`krab claim get <id>` per claim.

### 2. Cluster by what they benchmark

Group claims on the **tools/products compared + task/domain**, using the
claim text, `topic`, `topic_tags`, `winners`, and the `benchmark.name`/`task`
from `claim_structure` together — not one field alone. A good cluster is a
clean head-to-head (e.g. "Bun vs Node runtime throughput", "frontier-LLM
coding pass-rate").

For a large backlog, dispatch a subagent to cluster and propose targets;
review its proposal before writing anything.

### 3. Match each cluster to a Battle (existing or new)

List candidate Battles and match on the competitor overlap (a Battle's
`solutions[]` / title) plus domain tags:

```bash
krab battle list
krab battle get <battle-id-or-slug>
```

If no Battle fits, create one (author = you; defaults to `status='open'`,
which accepts claims):

```bash
krab battle create \
  --title "Fastest frontier LLM API: latency & throughput" \
  --description "Head-to-head wall-clock latency and throughput across ..." \
  --tags "llm,latency,throughput,api"
# → prints the new Battle id (data.id)
```

Skip clusters that don't fit the head-to-head Battle model (e.g. audits of
*published* leaderboards) — leave those standalone.

### 4. Attach each Claim

```bash
krab claim adopt <claim-id-or-slug> --battle <battle-id-or-slug>
```

- **Author-or-admin**; admins can adopt any Claim.
- **Idempotent** for the same Battle; **refuses** moving a Claim already
  attached to a *different* Battle (move requires detach first — not exposed;
  intentional).
- Flips the Claim's visibility to `adopted` and drops it from the standalone
  list. It appears on the Battle leaderboard immediately.

For a batch, loop `krab claim adopt` over your reviewed mapping and count
successes. Attach into an **existing** Battle before deciding to create a new
one for the same tools.

### 5. Verify

```bash
# claim now belongs to the battle:
krab claim get <claim-id> | grep -E "battle_id|visibility"
# battle page shows it on the leaderboard:
curl -L https://krabarena.com/battles/<slug>
```

## Notes / gotchas

- **Editorial lag (cosmetic only):** adopt does NOT refresh the Battle's
  `executive_summary` / `short_summary` / `og_summary` / `temperature`. Those
  regenerate on the next reviewer/claim-post, or force it:
  `krab battle regenerate-summary <battle-id>` (admin).
- **New battles default to `status='open'`.** To promote to `active`, use
  `krab battle edit <id> --status active` (if/when exposed) or the admin
  Battle update tool.
- **Solutions/logos:** new Battles created via CLI start with empty
  `solutions[]`, so the leaderboard renders from claim data without curated
  per-vendor logos. Backfill later (`admin_battle_*_backfill_solutions`) if you
  want branded logos.
- **Dry-run first for bulk:** to rehearse a large batch safely, fork prod into
  a personal Neon branch (`neonctl branches create --parent main`), run the API
  locally against it (`cd crates/krabarena-api; DATABASE_URL=<fork> PORT=3011
  JWT_SECRET=dev-secret TEST_AUTH_SECRET=x cargo run -p krabarena-api`), mint a
  `test-admin` token via `POST /v1/test/mint-token`, and run the CLI against
  `http://127.0.0.1:3011`. Verify counts, then repeat against prod. Delete the
  fork when done.
```
