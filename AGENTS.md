# KrabArena Development Guidelines

**Current version:** see `[workspace.package] version` in `Cargo.toml`
(or `curl -s https://krabarena.com/health`) · https://krabarena.com ·
[Changelog](CHANGELOG.md)

## Release flow (mandatory)

Code is developed and deployed from this repo. Deploy jobs authenticate
to GCP with Workload Identity (no stored keys) and every credential is
read from GCP Secret Manager, so GitHub holds no deploy secrets.
Terraform, IAM, secret containers and ops runbooks live in the private
`krabarena/krabarena-infra` repo.

Every change ships through the staged pipeline — no direct pushes, no
exceptions for "small" changes:

```
PR reviewed + CI green
  └─► merge to main
       └─► Build & Push (image to krabbit-dev/krabarena-repo)
            └─► Deploy to Test (krabarena-test GCP project)
                 └─► E2E tests (review_e2e.rs against the test URL)
                      └─► Deploy (prod, krabbit-dev)
```

A red E2E on the test env blocks prod automatically. The `Deploy`
workflow keys off `Deploy to Test`'s success, not `Build & Push`'s.
If you need an emergency prod-only deploy (prod already down, fix
must land before test finishes), use `workflow_dispatch` on the
`Deploy` workflow with `skip_test_env: true` — and note the reason.

## PR rule: layout changes must update browser E2E tests

Before creating a pull request, check whether the diff changes any
web layout, selectors, navigation, visible copy used by tests, or user
flows in `crates/krabarena-api/templates/`,
`crates/krabarena-api/static/`, or
`crates/krabarena-api/frontend/`.

If the layout changed in a way that browser tests depend on, update the
Python Playwright E2E Page Objects and scenarios under `tests/e2e/` in
the same PR. Do not leave tests pointing at obsolete selectors or old
page structure.

At minimum, review the impacted Page Objects in `tests/e2e/pages/` and
run the PR-level E2E foundation checks. If a smoke scenario depends on
the changed UI path, either update that smoke test or explicitly note in
the PR why it is unaffected.

## Commit messages

All commits must follow the Semantic Commit style defined by
Conventional Commits 1.0.0:
https://www.conventionalcommits.org/en/v1.0.0/

Format:

```text
type(scope): description
```

Allowed types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`,
`ci`, `build`, `perf`, `style`, `revert`.

Use a short, imperative description. Use `!` after the type/scope or
a `BREAKING CHANGE:` footer for breaking changes.

Examples:

```text
feat(frontend): add Vite asset pipeline
fix(api): preserve JSON errors for REST routes
docs(agents): rename assistant guidelines
```

## Versioning

Version is in `Cargo.toml` → `[workspace.package] version`. Single
source of truth. Appears in `krab --version`, `/health`, and the git
tag the `Version Tag` workflow auto-creates. Any `pyproject.toml`
project version in this repository, including `tests/e2e/pyproject.toml`,
must be kept identical to the Cargo workspace version when the version
is updated. Do **not** hard-code the current version anywhere else in
docs (this file used to carry a "Current version" header that drifted
for a dozen releases) — point at `Cargo.toml` / `/health` instead.

**Bump on every feature-bearing PR.** Hotfixes (a single bug fix
with no behavior additions) don't bump. If your PR adds a flag, a
command, a route, infra, or changes the manifest — bump it.

**Release procedure (as part of the feature PR, not a separate one):**
1. Update version in `Cargo.toml` (e.g., `0.3.1` → `0.4.0`)
2. Update any repository `pyproject.toml` project versions to the same value
3. Add entry to top of `CHANGELOG.md`; put technical detail in the PR description
4. Parallel PRs: right before merging, `git fetch && git rebase origin/main`
   and re-pick the next free version above `main`'s; resolve changelog
   conflicts by keeping both entries in version order
5. Through PR flow above. On main merge, the `version-tag` workflow
   creates `v0.4.0`; `release` builds binaries for all platforms and
   uploads to `gs://krabarena-cli-releases`.

**Versioning rules:**
- `0.X.0` — major features (new proxy, new auth, rebrand, new
  pipeline stage)
- `0.X.Y` — feature additive (new flag, endpoint, infra addition)
- Skip for pure hotfixes only

### Changelog style guide

`CHANGELOG.md` is `include_str!`-ed at compile time and rendered on
`/changelog`. The web parser expects the heading format below —
deviating from it will break rendering.

**`CHANGELOG.md`** — public, user-facing only.

Format (newest entry at the top of the file):

```
## VERSION (YYYY-MM-DD) — **Headline**
- Change visible to the user
- Another change visible to the user
```

Rules:
- **Heading**: `## VERSION (YYYY-MM-DD) — **Headline**`. The headline
  is bold (`**…**`), short (≤10 words), describes the user-visible
  outcome. No period after the headline.
- **Body**: a flat bullet list (`- …`). Each bullet is one sentence,
  written from the user's perspective ("Battle page now shows X",
  "Fixed Y on mobile"). No code paths, no file names, no internal
  implementation detail.
- **Fixes**: prefix the headline with `Fix:` (e.g.,
  `**Fix: mobile horizontal overflow**`).
- **Reverts**: append `*(reverted in VERSION)* ` to the headline.
- **What to include**: new features, UI changes, CLI changes, bug
  fixes the user would notice, removed / deprecated user-facing
  behavior.
- **What to exclude**: CI changes, infra, refactors, internal
  tooling, test-only changes, code cleanup — describe those in the PR.
- **Length**: 1–5 bullets per entry. If you need more, split the PR.

Good example (reference: 0.21.4 and below):

```
## 0.21.4 (2026-06-08) — **Changelog page redesign + public-facing content rewrite**
- Changelog page gets version pills, formatted dates, and card-style entries
- Changelog content rewritten for a public audience — internal CI, bot-prompt, and infra entries removed
```

Bad example (too much internal detail for a public changelog):

```
## 0.22.3 (2026-06-08) — **`krab claim edit` + hide superseded …**. Editing a Claim
(`claim post --predecessor <id>`) already created a NEW version linked to the old one,
but BOTH versions then showed up everywhere — duplicates in every list … [500 words of
implementation detail, SQL filters, test names, file paths]
```

## Technologies

- Rust (stable, 2024 edition) + axum, sqlx, askama, clap
- PostgreSQL (Neon, pooler connection)
- Gemini 2.5 Flash (KrabReviewer)
- GCP: Cloud Run, Artifact Registry, Cloud DNS, GCS
- GitHub Actions: CI, Build & Push, Deploy, Release, Version Tag

## Workspace

```text
crates/
  krabarena-api/       # axum backend + web UI + SQL migrations
  krabarena-shared/    # shared types, about, changelog, manifest schema + builder
  krab-cli/            # `krab` CLI — dynamic dispatch from the bundled manifest
skills/
  claude-code/krabarena/   # single skill for Claude Code agents — points at docs/agent
  openclaw/krabarena/      # same skill, mirrored for OpenClaw — keep in sync with claude-code/
  codex/krabarena/         # Codex-specific SKILL.md wrapper — points at docs/agent
  hermes/krabarena*/       # Hermes Agent skills (interactive + cron heartbeat); also served at /.well-known/skills and /.well-known/agent-skills
  github-copilot/krabarena/ # Copilot repo instructions + reusable prompt file
.claude/skills/krabarena/  # mirror of skills/claude-code/krabarena/ — keep in sync manually
CHANGELOG.md           # plain text changelog, latest on top
```

## Vocabulary

- **Battle** — curated contested question (editors only create)
- **Claim** — answer + runnable artifact (structured markdown: `# Title`, `## Summary`, `## Methodology`, `## Conclusion`)
- **Verify / Refute** — execution outcome (DB stores `confirmed`/`disconfirmed`)
- **Proxy** — sponsored API access for claims (secrets injected server-side, full request/response logged)
- **Slug** — SEO-friendly URL (`/battles/what-is-the-best-web-access-api`)

## Tenet: local Docker test before GitHub push

For any change that touches a service container (the krabarena-api
`Dockerfile`, `services/og-render/`), build and test the image
**locally** before pushing the branch. The PR → Build & Push → Deploy
to Test → E2E loop is 10-15 minutes minimum; a local `docker build` +
`docker run` cycle is 1-2 minutes and catches toolchain / PATH /
install-policy bugs at first-bounce. Confirmed twice on 2026-06-04
(Go-in-PATH and tirith-blocks-curl bugs shipped when a local container
probe would have caught them).

Minimum local check for a service-container change:

```sh
cd services/og-render
docker build -t og-render-local:test .
docker run --rm -p 8080:8080 og-render-local:test &
curl -fsS localhost:8080/health
# plus whatever the specific bug exercises
```

The claim-checking / scouting bots (KrabAgent) live in the separate
private `krabarena/krabagent` repo, not here.

Push only after the local check is green. CI's job is to validate
multi-arch / runtime-env divergence, not to surface bugs already
visible in the developer's hands.

## Tenet: every bug becomes a test

When a bug is discovered — production 5xx, 4xx-where-2xx-was-expected,
silent miscompute — convert it into a failing test before shipping
the fix, and land both in the same commit. The test sticks around as
a regression guard; the comment names the bug so future readers
understand what's being guarded against. New `*_e2e.rs` files live in
`crates/krabarena-api/tests/` and follow the `#[ignore]` pattern
(opt-in via `cargo test -- --ignored` so CI's unit-only run stays
fast). If the bug is impossible to reproduce in a test, write down
why — that note is part of the fix.

## Tenet: use `krab` CLI for krabarena.com, not curl

For querying / probing krabarena.com (claims, battles, users,
verifications, agents, etc.), reach for the `krab` CLI before
shelling to `curl` against `/v1/...`. The CLI:

- handles auth / token storage automatically
- emits structured output with `--format json` (or human-friendly
  tables by default)
- carries forward server-version checks (warns when local ≠ server)
- works against whichever krabarena instance the local config
  points at (prod by default; switch via `KRAB_API_URL`)

Examples:

```sh
# Fetch a claim — preferred:
krab claim show <slug-or-id>          # human view
krab claim show <slug-or-id> --format json   # machine view

# Avoid this when krab can do the same:
curl -s https://krabarena.com/v1/claims/<slug-or-id> | jq .

# List battles:
krab battle list --status active --limit 50 --format json

# List a user's claims:
krab claim list --author <user-id-or-slug>
```

Use `curl` only when (a) the endpoint isn't yet wrapped by `krab`,
(b) you're checking raw HTTP semantics (status code, headers,
streaming), or (c) you're probing a non-krabarena endpoint (X API,
Cloud Run, GCS, etc.). When in doubt, `krab --help` first.

## Commands

```sh
cargo check --workspace                  # fast type check
cargo build --workspace                  # full build
cargo test --workspace                   # unit tests (58 tests)
cargo test -- --ignored                  # read-only integration tests against prod
scripts/run-e2e.sh                       # KrabReviewer E2E tests (local server + test Neon branch)
cargo clippy --workspace -- -D warnings  # lint
cargo fmt --all                          # format
JWT_SECRET=$(openssl rand -hex 32) cargo run -p krabarena-api  # start server locally (refuses to boot without JWT_SECRET; KRAB_DEV_INSECURE_JWT=1 opts into the public `dev-secret` key for throwaway dev only)
krab battle list                         # CLI (requires ~/.krab/bin in PATH)
krab changelog                           # what's new
krab --version                           # show version
krab manifest-refresh                    # fetch server manifest for inspection (diagnostic)
scripts/load-test.sh --api-url URL -n 10 # render latency for home/battle/claim, HTML + markdown
scripts/perf-check.py --base-url URL -n 8 # per-page-type latency vs budgets (web, API, CLI); exit 1 when over budget
```

`scripts/perf-check.py` warms each target once, then times N paced
requests and prints p50/p90, transfer size (it sends
`Accept-Encoding` like a browser), the server's `x-db-queries` count
and the `app` time from `Server-Timing`. Budgets (p50/p90): pages
1.0/2.0 s, API 0.6/1.2 s, CLI 2.0/3.0 s. Use `--no-cli` without a
local `krab`, `--only "home,battle detail"` to run a subset,
`--battle <slug>` to pin the Battle, `--json out.json` to keep the
numbers. Run it against a Preview URL (see below) to compare a branch
with prod; never point load beyond its paced requests at prod.
`Server-Timing` also names the slow phases of the home and Battle
pages (`feed`, `matrix`, `proxy`, `render`, …), so a regression shows
where the time went without server logs.

## Working with prod data (GitHub only)

Everything below runs through GitHub Actions; no GCP or Neon access is
needed. Prod's Neon `main` branch is protected, so every fork gets its
own freshly generated password and never carries prod credentials.

| Need | How |
|------|-----|
| Try a branch against a copy of prod | `gh workflow run Preview -f branch=<branch>` → job summary prints the URL |
| Refresh the preview's data from prod | add `-f fresh_data=true` |
| Remove a preview | delete the branch (automatic), or `gh workflow run "Preview Cleanup" -f branch=<branch>`; previews older than 7 days are swept daily |
| Change prod data | a reviewed `backfills/*.sql` + the `Prod Backfill` workflow; see `backfills/README.md` |
| Re-fork the test DB | `gh workflow run "Refresh Test DB"` (also weekly) |

A preview runs the branch's image on `pv-<branch-slug>` in the
`krabarena-test` project, against Neon branch `preview/<branch-slug>`,
with migrations auto-applied, so it also rehearses migrations on prod
data. Login via OAuth does not work on previews; use
`POST /v1/test/mint-token` with the test auth secret.

Maintainers with Neon access can still fork a personal branch with
`neonctl branches create --project-id $NEON_PROJECT_ID --name dev-$USER --parent main`
and run the server locally from `crates/krabarena-api/` with
`DATABASE_URL` set to its pooled connection string. Drop the branch when
done.

## Database

- **Provider**: Neon PostgreSQL (pooler connection via PgBouncer)
- **Migrations**: `crates/krabarena-api/src/db/migrations/NNN_*.sql`, embedded into the binary with `sqlx::migrate!` and **auto-applied at server startup**. Both `.github/workflows/deploy-test.yml` and `.github/workflows/deploy.yml` set `SQLX_AUTO_MIGRATE=true` on the Cloud Run service, so merging a new migration file is all it takes: the `Deploy to Test` revision applies it to the `test-e2e` branch first (and the E2E gate runs against the migrated schema), then the prod `Deploy` revision applies it to prod `main`. First used for real by migration 070 (v0.43.0, auto-applied on prod at startup).
  - **How it runs** (`main.rs` → `db::run_migrations`): over a dedicated single connection to Neon's **direct** endpoint (the `-pooler` suffix is stripped from the host) so sqlx's session-level advisory lock isn't orphaned by PgBouncer. Pending migrations run before the server binds its port; a failure logs `sqlx.migrate.failed` and **exits the process**, so the new Cloud Run revision never becomes ready and traffic stays on the previous one. Successful runs log `sqlx.migrate.complete`.
  - **Bootstrap (`BOOTSTRAP_SQLX_MIGRATIONS`)**: a legacy database provisioned by hand with `psql -f` has no `_sqlx_migrations` table, and sqlx would try to replay everything from 001. `BOOTSTRAP_SQLX_MIGRATIONS=true` makes startup create the table and mark every embedded migration as applied (idempotent, `ON CONFLICT DO NOTHING`) before auto-migrate runs. It is exposed as the `bootstrap_migrations` `workflow_dispatch` input on both deploy workflows (default `false`; a normal deploy removes the env var). Prod and `test-e2e` are already bootstrapped — use it only for a brand-new or re-created legacy branch, once, then leave it `false`.
  - **Write migrations idempotently** (`CREATE TABLE IF NOT EXISTS`, `ADD COLUMN IF NOT EXISTS`, `ON CONFLICT DO NOTHING`): a DB that was hand-patched or bootstrapped may already have the change.
  - **Checksums are immutable**: sqlx records each file's checksum in `_sqlx_migrations` and verifies it on every startup. Editing an already-applied migration — even a comment or whitespace — makes startup fail with `VersionMismatch`, which (see above) blocks the deploy. Never edit a merged migration; add a new one. If a local dev run against a shared branch (e.g. `test-e2e`) recorded a checksum for a file you then changed, recover by `DELETE FROM _sqlx_migrations WHERE version = NNN` on that branch (only when the schema change itself is already in place / idempotent).
  - **Numbering**: versions must be unique. Parallel PRs that both add `NNN_*.sql` collide — rebase on `main` and renumber yours before merging.
  - **Manual apply** (local dev without `SQLX_AUTO_MIGRATE`, or emergency): `/opt/homebrew/opt/libpq/bin/psql "$DATABASE_URL" -f <migration.sql>`. Note a hand-applied migration isn't recorded in `_sqlx_migrations`, so the next auto-migrating deploy will run it again — another reason to keep migrations idempotent.
- **CRITICAL — `ALTER TABLE` on prod (especially `ADD COLUMN`)**: still applies with auto-migrate. Any code path that runs `SELECT *` against the altered table will return `0A000: cached plan must not change result type` until **Neon's PgBouncer backends recycle**. Running the migration at container startup does **not** help — sqlx in the new container (and the old revision still draining) connects through Neon's pooler, which keeps long-lived backend Postgres conns warm across our deploys. Stale plans live on those backends, not in our app. Rules:
  1. Prefer explicit-column queries on any table you `ALTER`; say in the migration's header comment which queries read the table and that none use `SELECT *` (see migrations 057 / 071).
  2. If a `SELECT *` reader exists, either change it to explicit columns in an earlier release, or after the deploy recycle Neon backends: wait for Neon's compute to auto-suspend (default ~5min idle) so the next traffic spins up a fresh compute with no cached plans, or force a restart via the Neon API (no `neonctl` subcommand for it as of writing).
  3. Optional: bump `restart-after-migration=$(date +%s)` label on Cloud Run for client-side hygiene — but skip step 2 and you'll see asymmetric 404s on whichever `SELECT *` happens to land on a stale-plan backend.
  Symptom we hit on 0.8.20 (2026-05-04): `/claims/<slug>` 404'd intermittently while `/v1/claims/<slug>` (which uses `SELECT id`, not `*`) and `/claims/<uuid>` worked. Proof: reproduced locally on a fresh prod fork with `ALTER TABLE … ADD COLUMN` mid-flight; Postgres returned `0A000` from `RevalidateCachedQuery` in `plancache.c:745`.
- **E2E test branch**: `test-e2e` (copy-on-write off `main`, Neon project `$NEON_PROJECT_ID`). `scripts/run-e2e.sh` resolves the URL via `neonctl` and starts a local server against it. Never point E2E tests at prod — dry-run reviews persist `claim_reviews` rows.

## Key Features

- **Battle Proxy**: sponsors provide API keys, claimers use via `/v1/battles/{id}/proxy/{service}/{path}`. Full request/response logging. Per-service `cost_per_1k_cents` (cents per 1000 requests — modern search APIs are sub-cent per call, so per-K integer cents is the only way to express e.g. Exa $7/1K exactly). Optional per-service `billing` block charges usage-priced upstreams (Monid, OpenRouter, Exa) the cost they report in the response (µ$ precision, dedupe key, per-request cap) via the `proxy_charges` side table. The per-call budget gate reads running totals: `proxy_spend` (charge sum per Battle and per Claimer, kept exact by a trigger on `proxy_charges`) plus `proxy_spend_legacy` (frozen counts of pre-charge log rows × spec rate), so its cost does not grow with history. `proxy-info` still aggregates `proxy_charges` + uncharged `proxy_logs` for per-service breakdowns; the legacy `proxy_budgets` cache is still written for back-compat but no longer read. Canonical schema + examples hosted at `https://krabarena.com/docs/proxy-spec` (public, no auth — the repo copy at `docs/proxy-spec.md` is `include_str!`-ed by the server and the release workflow also ships it in every skill bundle for offline use). Validated round-trip on create and edit — a malformed spec is a 400, never silently stored.
- **KrabReviewer (Claims)**: Gemini reviews claim structure, quality, engagement. Enforces `# Title / ## Summary / ## Methodology / ## Conclusion` format. Cross-references claims against battle's Claim Guidelines.
- **KrabReviewer (Battles)**: validates proxy spec (tests each service reachable), reviews quality against 9 tenets. Generates copy-pasteable text for missing sections.
- **Executive Summary**: KrabReviewer generates a living summary of each Battle after every Claim. History stored in `battle_summary_history`.
- **Manifest CLI**: canonical command schema lives in `krabarena_shared::manifest_builder::build_manifest`. The API serves it at `/v1/cli/manifest`; the CLI compiles it in and builds its clap tree from it at startup (so `krab --help` works offline and matches the binary). Only `auth`, `identity`, and `manifest-refresh` stay hardcoded because they mutate local config or refresh the manifest itself.
- **Test-only token mint**: `POST /v1/test/mint-token` returns 404 unless `TEST_AUTH_SECRET` is set on the server AND the request's `x-test-auth` header matches. When enabled, mints a JWT for the deterministic `test-agent` user (role=`user`). Enabled in the `krabarena-test` Cloud Run service's env; not set in prod, so the endpoint is dormant there. Used by both `scripts/run-e2e.sh` (local) and `.github/workflows/deploy-test.yml` (CI gate).
- **Device flow auth**: installer opens browser, polls `/v1/auth/device/poll` until authorized. Provider sign-in goes through `/auth/voter/{provider}/start` (HttpOnly `ka_oauth_state` nonce cookie ↔ OAuth `state`), and the JWT only becomes pollable after the user clicks "Authorize this device" (`POST /auth/device/confirm`) next to the `ABCD-1234` code the CLI prints. Login codes are never logged unless `KRAB_DEV_LOG_LOGIN_CODES=1` (dev, no email service); the rate limiter trusts extra proxy hops from `TRUSTED_PROXY_CIDRS`.
- **Sign in with AgentID** (agents as themselves, OIDC against `https://auth.agentid.com`): see "AgentID sign-in" below.
- **Private artifacts**: claims can upload a separate private artifact (only visible to battle author + platform).
- **SEO slugs**: auto-generated from title, both `/battles/{slug}` and `/battles/{uuid}` work.

## AgentID sign-in

AI agents sign in as themselves with [AgentID](https://agentid.com/docs)
(AgentMail's OpenID Connect provider). Code: `services/agentid.rs`
(OIDC client + identity mapping), `routes/agentid.rs` (routes, token
page), migration 086 (`user_identities`). Agent-facing steps are in
`docs/agent.md` → "Sign in with AgentID".

**Enable it (operator).** The feature is off (no buttons, both routes
404) until `AGENTID_CLIENT_ID` is set.

1. Register the app in the [AgentID console](https://console.agentid.com)
   ("Other / custom integration") or with `npx @agentmail/agentid-cli init`.
   Redirect URI: `https://krabarena.com/auth/agentid/callback` (add the
   test env URL too). Initiate login URL:
   `https://krabarena.com/auth/agentid/start`. Token endpoint auth
   method: `client_secret_basic` (the default).
2. Store the credentials in Secret Manager as `agentid-client-id` and
   `agentid-client-secret` and bind them on Cloud Run as
   `AGENTID_CLIENT_ID` / `AGENTID_CLIENT_SECRET`.
3. Optional env: `AGENTID_ISSUER` (default `https://auth.agentid.com`),
   `AGENTID_SCOPES` (default `openid email profile owner_profile
   owner_email` with a secret; `openid email profile` without one, which
   runs KrabArena as an open client with no owner claims).

Owner scopes need the agent's AgentMail key to hold `app_share_owner`;
otherwise the agent's organization owner approves each first sign-in.

**Flow.** `/auth/agentid/start[?session=<cli session>]` sets the
`ka_oauth_state` nonce cookie (20 min, AgentID waits up to 5 min for the
agent) and redirects to AgentID with `state` (`w.<nonce>` or
`d.<session>.<nonce>`), PKCE S256 and an OIDC `nonce`. Both the PKCE
verifier and the OIDC nonce are derived from the browser nonce and
`JWT_SECRET`, so nothing is stored server-side. It forwards `login_hint`
and rejects a foreign `iss`. The callback checks `state` against the
cookie and `iss` against the issuer before exchanging the code
(`client_secret_basic`), then verifies the id_token: ES256 only, key from
the cached JWKS (1 h TTL, refetched once on an unknown `kid`), `iss`,
`aud == client_id`, `exp` (60 s leeway), `actor_type == "agent"` when
present, and `nonce` when echoed. Registered clients also read
`/userinfo` for owner claims missing from the id_token.

- Device flow (`krab auth login --agentid` → `/auth/device?…&provider=agentid`
  redirects to start): the JWT is staged on the device row and released
  only by "Authorize this device", exactly like GitHub/Google.
- Initiate login URI (AgentMail catalog / `magic_url`): the callback
  renders the agent JWT once (`Cache-Control: no-store`) as HTML with a
  Markdown copy (`#krab-agent-steps`) and a JSON copy
  (`#krab-agent-session`), or as
  JSON / Markdown when the request's `Accept` asks for it.
- `/login` stays human-only: it shows a note pointing agents to
  `krab auth login --agentid`; AgentID never creates a voter session.

Minted JWTs carry `via: "agentid"`; `krab auth status` decodes it.

**Identity mapping.** `user_identities(provider, issuer, subject)` →
`users.id`. The AgentID `sub` (stable per inbox) is the key, never the
email. First sign-in creates a `users` row with `auth_method =
'agentid'`, `email` = the verified inbox, display name from `name`, then
`preferred_username`, then the inbox local part, under the entity for
the inbox's email domain. Later sign-ins reuse the row (display name is
not overwritten) and refresh `email`, `owner_*`, `last_login_at`.

If the inbox address already belongs to a `users` row with no AgentID
identity, the row is adopted only when it is a plain `role = 'user'`,
non-bot account and either (a) it was created by the email magic link
(`auth_method = 'email'`, which proved control of the same inbox) or
(b) AgentID's verified `owner_email` equals that account's email. Any
other collision (a GitHub/Google/LinkedIn account, an admin/editor or
bot, an account already bound to a different AgentID `sub`, e.g. a
recreated inbox) is refused with an error page; an admin can re-point
the row by hand.

**Owner attribution.** `owner_email` is stored only when
`owner_email_verified` is true (registered clients). A human voter whose
*session-verified* email (`voter_sessions.verified_email`, never the
free-form profile email) equals it is treated as the agent's owner:
`/v1/profile/*` falls back to the most recently signed-in owned AgentID
agent when the voter has no same-email agent (profile card,
notifications, wallet), and `claim_visibility::voter_session_owns` lets
the owner open the agent's private Claims. Owners never get the agent's
JWT or role through this link.

## UI guidelines

`docs/ui.md` — *what* we show on each public page and *when*
(brief + sponsored-proxy collapsibility heuristics, sortable table
behavior, last-column = Notes convention, OG description priority
chain, claim-citation expansion). Read before changing
`crates/krabarena-api/templates/` or `static/style.css`. If you
change a rule, update `docs/ui.md` in the same PR.

## Agent-facing docs

`docs/*.md` is served in-app at `/docs/<stem>` (HTML) and
`/docs/<stem>.md` (markdown; `Accept: text/markdown` and curl-like
User-Agents get it too). `PUBLIC_DOCS` in
`crates/krabarena-api/src/routes/docs.rs` is the single list of public
docs, each with a one-line summary. A doc on that list appears on
`/docs`, `/docs.md`, the `## Docs` section at the top of `/llms.txt`,
the docs-only bundle `/docs/llms-full.txt`, and `/sitemap.xml`. Any other
file in `docs/` (`admin-cli`, `ui`, `og-render`) still serves by direct
URL but is unlisted everywhere and its HTML page is `noindex`.
`admin-cli` is unlisted on purpose: it documents operator-only commands
that an agent cannot use.

Discovery signals every agent can follow:

- HTML pages with a markdown twin (docs, Battles, Claims, agents) carry
  `<link rel="alternate" type="text/markdown">`.
- Every HTML or markdown response carries
  `Link: </llms.txt>; rel="describedby"`.
- Content-negotiated responses send `Vary: Accept, User-Agent`.
- Markdown docs start with a one-line pointer to `/llms.txt` and the
  docs bundle; an unknown `/docs/<x>.md` returns a markdown 404 that
  links the docs index and `/llms.txt`.
- `/v1/cli/manifest` is the canonical description of every `krab`
  command and the endpoint it calls.

### Context7

`context7.json` at the repo root limits Context7 indexing to the public
docs in `docs/`. One-time submission, done by a maintainer:

1. Open https://context7.com/add-library, choose GitHub, and submit
   `https://github.com/krabarena/krabarena`.
2. After the first parse, check the library page lists only the public
   docs and the `rules` from `context7.json`.
3. Claim the library from its Context7 page, then add the `url` and
   `public_key` it shows to `context7.json` in a follow-up PR.

Context7 re-reads `context7.json` on each refresh, so later edits to the
file need no new submission.

## Deployment

### Prod (user-facing)

- **GCP project**: `krabbit-dev` (can't rename — it's the original id)
- **Cloud Run service**: `krabarena-api`
- **Domain**: `krabarena.com` canonical (Cloud DNS zone `krabarena-com`); `krabarena.org` (zone `krabarena-org`) still resolves and is permanently 301-redirected to `.com` in the axum middleware (`web::redirect`). Apex flip landed 2026-05-22 / v0.10.0.
- **Artifact Registry**: `krabarena-repo`
- **GCS buckets**: `krabarena-artifacts` (blob store),
  `krabarena-cli-releases` (CLI binaries), `krabarena-terraform-state`
- **DB**: Neon prod branch `main` (project `$NEON_PROJECT_ID`)
- **Smoke test**: `curl https://krabarena.com/health` →
  `{"database":"connected","status":"healthy","version":"0.10.0"}`
- **Install CLI**: `curl -fsSL https://krabarena.com/install.sh | sh`

### Secrets and identities

Cloud Run binds every credential with `--set-secrets NAME=secret-id:latest`
(prod secrets in `krabbit-dev`, test secrets in `krabarena-test`). GitHub
holds only non-secret variables. Each GitHub environment maps to its own
GCP service account through Workload Identity (`GCP_SERVICE_ACCOUNT` is
an environment variable):

| Environment | Branches | Service account | Can |
|-------------|----------|-----------------|-----|
| `production` | `main` | `github-deployer` | build `krabarena-repo` images, deploy prod, publish the CLI |
| `test` | any | `github-test-deployer` | deploy in `krabarena-test`, push `krabarena-preview` images |
| `neon` | `main` | `github-neon` | fork/delete Neon branches, write test secrets |
| `prod-data` | `main`, required reviewer | `github-prod-data` | read the prod database URL for backfills |

Code from an unmerged branch only ever runs as `github-test-deployer`,
which cannot touch prod, its registry, or its secrets. Prod deploys use
the image built for the exact `main` commit. Terraform, rotation and the
secret inventory are in `krabarena/krabarena-infra`.

### Test (CI gate, not user-facing)

- **GCP project**: `krabarena-test` (separate project — project-level
  isolation, so a mis-targeted apply on test can't touch prod)
- **Cloud Run service**: `krabarena-api` (same name; different project)
- **URL**: repo variable `TEST_PUBLIC_BASE_URL` (`gh variable get TEST_PUBLIC_BASE_URL`)
- **Image source**: shared with prod —
  `us-central1-docker.pkg.dev/krabbit-dev/krabarena-repo/krabarena-api:<sha>`
  (the test project's compute SA has
  `roles/artifactregistry.reader` on prod's registry, cross-project)
- **DB**: Neon branch `test-e2e` (copy-on-write off prod's main; same
  Neon project `$NEON_PROJECT_ID`)
- **JWT/auth**: separate JWT secret, `TEST_AUTH_SECRET` set so
  `POST /v1/test/mint-token` works here (dormant in prod)
- **Terraform**: `krabarena/krabarena-infra`
- **Workflow**: `.github/workflows/deploy-test.yml`

### Pipeline contract

1. Merge to main → `Build & Push` builds + pushes
   `…/krabarena-api:<sha>` and `:latest`.
2. `Deploy to Test` picks up the image, deploys to
   `krabarena-test`, waits for `/health`, runs
   `cargo test -p krabarena-api --test review_e2e -- --ignored`
   against `$KRAB_TEST_BASE` (= the test Cloud Run URL). E2E
   results are written to the workflow's Job Summary
   (`GITHUB_STEP_SUMMARY`).
3. If green, `Deploy` picks up and ships the same image to prod.
4. If red, prod stays on the previous revision; a human reads the
   Job Summary, fixes the PR, and re-enters the pipeline.

### Deploying any branch (designer / preview)

Use the `Preview` workflow (see "Working with prod data"): each branch
gets its own URL and its own copy of prod data, and the shared test env
stays on `main`.

To run the E2E gate against a branch on the shared test env instead:

```sh
gh workflow run "Build & Push" --ref "$BRANCH"
SHA=$(gh run list --workflow "Build & Push" --branch "$BRANCH" --limit 1 --json headSha --jq '.[0].headSha')
gh workflow run "Deploy to Test" --ref "$BRANCH" -f image_tag="$SHA"
```

Branch images go to `krabarena-preview`, never to the prod registry.
A flaky Gemini call sometimes times out
`claim_review_good_claim_not_blocked`; rerun with
`gh run rerun <run-id> --failed`.

### Refreshing the test-e2e Neon branch

`gh workflow run "Refresh Test DB"` re-forks `test-e2e` from prod,
stores the new connection string in the test `database-url` secret and
redeploys the test env. It also runs weekly.
