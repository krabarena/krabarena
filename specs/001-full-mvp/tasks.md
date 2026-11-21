# Tasks: Krabbit Full MVP

**Input**: Design documents from `/specs/001-full-mvp/`
**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md, contracts/

**Tests**: Constitution mandates test-first development (Principle IV). Test tasks are included.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **Workspace**: Cargo workspace at repo root
- **API crate**: `crates/krabbit-api/src/`
- **CLI crate**: `crates/krabbit-cli/src/`
- **Shared crate**: `crates/krabbit-shared/src/`
- **Tests**: `tests/contract/`, `tests/integration/`
- **Vercel**: `api/`

---

## Phase 1: Setup

**Purpose**: Project initialization, workspace structure, and tooling

- [x] T001 Create Cargo workspace root `Cargo.toml` with members: `crates/krabbit-api`, `crates/krabbit-cli`, `crates/krabbit-shared`
- [x] T002 [P] Initialize `crates/krabbit-shared/Cargo.toml` with dependencies: serde, uuid
- [x] T003 [P] Initialize `crates/krabbit-api/Cargo.toml` with dependencies: axum, sqlx (postgres), serde, serde_json, tracing, tracing-subscriber, uuid, tokio, vercel_runtime, tower-http
- [x] T004 [P] Initialize `crates/krabbit-cli/Cargo.toml` with dependencies: clap (derive), reqwest, serde, serde_json, toml, dirs
- [x] T005 [P] Create `tests/` directory structure: `tests/contract/`, `tests/integration/`, `tests/fixtures/`
- [x] T006 [P] Create Vercel config `vercel.json` with Rust build settings and `api/handler.rs` entry point stub
- [x] T007 [P] Configure `clippy.toml` and `.cargo/config.toml` with deny-warnings, and add `rustfmt.toml`
- [x] T008 [P] Create `.env.example` with `DATABASE_URL` and `BLOB_STORE_URL` placeholders

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Shared types, database schema, error handling, and middleware that ALL user stories depend on

**CRITICAL**: No user story work can begin until this phase is complete

- [x] T009 Define shared newtypes in `crates/krabbit-shared/src/types.rs`: `HoleId`, `ShellId`, `DigId`, `BuryId`, `UserId`, `EntityId` (UUID wrappers with Serialize/Deserialize)
- [x] T010 [P] Define shared enums in `crates/krabbit-shared/src/domain.rs`: `HoleStatus` (Open, Contested, Holding, Settled), `AuthMethod` (Email, Dns), `VerificationMethod` (Email, Dns)
- [x] T011 [P] Define shared `lib.rs` in `crates/krabbit-shared/src/lib.rs` re-exporting types and domain modules
- [x] T012 Create JSON response envelope types in `crates/krabbit-api/src/envelope.rs`: `ApiResponse<T>`, `ApiError`, `PaginatedResponse<T>`, `PaginationMeta`
- [x] T013 [P] Create error types and error codes enum in `crates/krabbit-api/src/error.rs`: map each error code from `contracts/api-v1.md` to HTTP status, implement `IntoResponse` for axum
- [x] T014 Create database connection pool setup in `crates/krabbit-api/src/db/mod.rs` using sqlx `PgPool`
- [x] T015 Create SQL migration: `crates/krabbit-api/src/db/migrations/001_initial.sql` with all tables (entities, users, holes, shells, digs, buries) per `data-model.md`, including indexes and constraints
- [x] T016 [P] Create auth middleware extractor in `crates/krabbit-api/src/routes/auth.rs`: extract bearer token from `Authorization` header, validate JWT, inject `UserId` into request extensions. Allow unauthenticated requests to pass through for read-only endpoints
- [x] T017 Create axum app skeleton in `crates/krabbit-api/src/main.rs`: initialize tracing, create PgPool, build Router with shared state, bind to port
- [x] T018 [P] Create CLI config module in `crates/krabbit-cli/src/config.rs`: read/write `~/.krabbit/config.toml` (api_url, bearer_token)
- [x] T019 [P] Create CLI HTTP client wrapper in `crates/krabbit-cli/src/client.rs`: reqwest client with auth header injection, JSON envelope parsing, error handling
- [x] T020 Create CLI entry point in `crates/krabbit-cli/src/main.rs`: clap derive app with global options (--api-url, --token, --format, --verbose), subcommand routing

**Checkpoint**: Foundation ready — user story implementation can now begin

---

## Phase 3: User Story 1 — Create and Browse Holes (Priority: P1) MVP

**Goal**: Practitioners can create Holes with title, description, and domain tags, browse them with filters, retrieve by ID, and create sub-Holes.

**Independent Test**: Create a Hole via CLI, list with filters, retrieve by ID — full browse experience works end-to-end.

### Tests for User Story 1

> **Write these tests FIRST, ensure they FAIL before implementation**

- [x] T021 [P] [US1] Contract test for `POST /v1/holes` in `tests/contract/test_holes_create.rs`: test creation with valid data returns 201 + HoleId, test missing title returns 400 `VALIDATION_ERROR`, test unauthenticated returns 401
- [x] T022 [P] [US1] Contract test for `GET /v1/holes` in `tests/contract/test_holes_list.rs`: test listing with status/domain filters, test pagination with cursor/limit, test unauthenticated read succeeds
- [x] T023 [P] [US1] Contract test for `GET /v1/holes/{id}` in `tests/contract/test_holes_get.rs`: test retrieval returns full Hole with shell_count and dig/bury totals, test 404 for missing ID
- [x] T024 [P] [US1] Integration test for Hole creation + sub-Hole flow in `tests/integration/test_holes_flow.rs`: create parent Hole, create sub-Hole with parent_id, list sub-holes of parent

### Implementation for User Story 1

- [x] T025 [P] [US1] Create Hole model in `crates/krabbit-api/src/models/hole.rs`: struct with all fields from data-model.md, sqlx FromRow derive, creation request/response DTOs
- [x] T026 [US1] Implement HoleService in `crates/krabbit-api/src/services/hole_service.rs`: `create_hole()`, `get_hole()`, `list_holes()` with filtering/pagination/sorting, `list_sub_holes()`, Hole status recalculation logic
- [x] T027 [US1] Implement Hole routes in `crates/krabbit-api/src/routes/holes.rs`: `POST /v1/holes` (auth required), `GET /v1/holes` (public), `GET /v1/holes/{id}` (public), `GET /v1/holes/{id}/sub-holes` (public). Wire into router in main.rs
- [x] T028 [US1] Implement CLI `hole` commands in `crates/krabbit-cli/src/commands/hole.rs`: `hole create`, `hole list`, `hole get`, `hole sub-holes` with --format json|human output
- [x] T029 [US1] Add structured logging for Hole domain events in `crates/krabbit-api/src/services/hole_service.rs`: log `hole.created` with hole_id, author_id, tags

**Checkpoint**: User Story 1 fully functional — Holes can be created, browsed, filtered, and retrieved

---

## Phase 4: User Story 2 — Drop a Shell on a Hole (Priority: P2)

**Goal**: Practitioners can submit Shells with claims and runnable artifacts to existing Holes. Shells are immutable. Versioning via lineage.

**Independent Test**: Drop a Shell with an artifact file onto a Hole, verify artifact hash, confirm immutability rejection, create a successor Shell with lineage.

### Tests for User Story 2

> **Write these tests FIRST, ensure they FAIL before implementation**

- [x] T030 [P] [US2] Contract test for `POST /v1/holes/{id}/shells` in `tests/contract/test_shells_create.rs`: test creation with claim + artifact returns 201 + artifact_hash, test missing artifact returns 400 `SHELL_MISSING_ARTIFACT`, test artifact >100MB returns 400 `ARTIFACT_TOO_LARGE`, test unauthenticated returns 401
- [x] T031 [P] [US2] Contract test for `GET /v1/shells/{id}` in `tests/contract/test_shells_get.rs`: test retrieval returns claim, artifact_hash, dig_count, bury_count, test 404 for missing ID
- [x] T032 [P] [US2] Contract test for `GET /v1/shells/{id}/artifact` in `tests/contract/test_shells_artifact.rs`: test binary download with correct Content-Disposition, test artifact hash matches stored hash
- [x] T033 [P] [US2] Integration test for Shell immutability + lineage in `tests/integration/test_shells_flow.rs`: create Shell, attempt edit (expect rejection), create successor with predecessor_id, verify lineage chain

### Implementation for User Story 2

- [x] T034 [P] [US2] Create Shell model in `crates/krabbit-api/src/models/shell.rs`: struct with all fields from data-model.md, sqlx FromRow derive, creation request DTO (multipart), response DTO
- [x] T035 [P] [US2] Implement ArtifactService in `crates/krabbit-api/src/services/artifact_service.rs`: `upload_artifact()` (compute SHA-256 hash, upload to Vercel Blob, return hash + URL), `download_artifact()`, validate size ≤100MB
- [x] T036 [US2] Implement ShellService in `crates/krabbit-api/src/services/shell_service.rs`: `create_shell()` (calls ArtifactService, stores Shell), `get_shell()`, `list_shells()` with sorting (dug/buried/newest), `get_artifact()`, `get_lineage()`. Reject edits (immutability)
- [x] T037 [US2] Implement Shell routes in `crates/krabbit-api/src/routes/shells.rs`: `POST /v1/holes/{id}/shells` (auth, multipart), `GET /v1/holes/{id}/shells` (public), `GET /v1/shells/{id}` (public), `GET /v1/shells/{id}/artifact` (public, binary), `GET /v1/shells/{id}/lineage` (public). Wire into router
- [x] T038 [US2] Implement CLI `shell` commands in `crates/krabbit-cli/src/commands/shell.rs`: `shell drop` (read file, compute hash, upload multipart), `shell list`, `shell get`, `shell artifact` (download to stdout or --output path), `shell lineage`
- [x] T039 [US2] Add structured logging for Shell domain events: log `shell.created` with shell_id, hole_id, artifact_hash, author_id

**Checkpoint**: User Stories 1 AND 2 both work independently — Holes have Shells with immutable artifacts

---

## Phase 5: User Story 3 — Dig or Bury a Shell (Priority: P3)

**Goal**: Practitioners can Dig (verify) or Bury (refute) Shells with execution evidence. Hole status recalculates automatically.

**Independent Test**: Dig a Shell with evidence, Bury a Shell with failure reason, verify counts increment, verify Hole status transitions.

### Tests for User Story 3

> **Write these tests FIRST, ensure they FAIL before implementation**

- [x] T040 [P] [US3] Contract test for `POST /v1/shells/{id}/digs` in `tests/contract/test_digs_create.rs`: test creation with evidence returns 201, test missing evidence returns 400 `MISSING_EXECUTION_EVIDENCE`, test is_author_dig flag when author Digs own Shell
- [x] T041 [P] [US3] Contract test for `POST /v1/shells/{id}/buries` in `tests/contract/test_buries_create.rs`: test creation with failure reason returns 201, test missing reason returns 400 `MISSING_FAILURE_REASON`
- [x] T042 [P] [US3] Integration test for Dig/Bury flow + status recalculation in `tests/integration/test_verification_flow.rs`: create Hole → Shell → multiple Digs → verify Hole status becomes `holding`; then add Buries → verify status becomes `contested`
- [x] T043 [P] [US3] Concurrency test in `tests/integration/test_concurrent_digs.rs`: submit 100 concurrent Digs on same Shell, verify final dig_count == 100 with no data loss (SC-005)

### Implementation for User Story 3

- [x] T044 [P] [US3] Create Dig model in `crates/krabbit-api/src/models/dig.rs`: struct with all fields from data-model.md, sqlx FromRow derive, creation request/response DTOs
- [x] T045 [P] [US3] Create Bury model in `crates/krabbit-api/src/models/bury.rs`: struct with all fields from data-model.md, sqlx FromRow derive, creation request/response DTOs
- [x] T046 [US3] Implement DigService in `crates/krabbit-api/src/services/dig_service.rs`: `create_dig()` (validate evidence, detect author-dig, atomically increment Shell.dig_count, trigger Hole status recalc), `list_digs()`
- [x] T047 [US3] Implement BuryService in `crates/krabbit-api/src/services/bury_service.rs`: `create_bury()` (validate failure_reason/conditions/steps, atomically increment Shell.bury_count, trigger Hole status recalc), `list_buries()`
- [x] T048 [US3] Implement Hole status recalculation in `crates/krabbit-api/src/services/hole_service.rs`: add `recalculate_status()` — compare top Shells' Dig/Bury ratios, transition to `contested` or `holding` as appropriate
- [x] T049 [US3] Implement Dig and Bury routes in `crates/krabbit-api/src/routes/digs.rs` and `crates/krabbit-api/src/routes/buries.rs`: `POST /v1/shells/{id}/digs` (auth), `GET /v1/shells/{id}/digs` (public), `POST /v1/shells/{id}/buries` (auth), `GET /v1/shells/{id}/buries` (public). Wire into router
- [x] T050 [US3] Implement CLI `dig` and `bury` commands in `crates/krabbit-cli/src/commands/dig.rs` and `crates/krabbit-cli/src/commands/bury.rs`: `dig <shell_id>` with --env/--output-hash/--duration, `bury <shell_id>` with --reason/--conditions/--steps, `digs <shell_id>`, `buries <shell_id>`
- [x] T051 [US3] Add structured logging for verification events: log `dig.recorded` and `bury.recorded` with IDs, shell_id, author_id

**Checkpoint**: All core domain flows work — Holes → Shells → Digs/Buries with status recalculation

---

## Phase 6: User Story 4 — Authenticate and Identify Contributors (Priority: P4)

**Goal**: Contributors verify their identity via company email or DNS TXT record, receive bearer tokens, and all contributions are linked to their verified entity.

**Independent Test**: Verify via email → get token → make authenticated write. Verify via DNS → get token → make authenticated write. Unauthenticated writes rejected.

### Tests for User Story 4

> **Write these tests FIRST, ensure they FAIL before implementation**

- [x] T052 [P] [US4] Contract test for `POST /v1/auth/email` and `/v1/auth/email/verify` in `tests/contract/test_auth_email.rs`: test email sends verification, test valid token returns bearer + user + entity, test invalid token returns 400
- [x] T053 [P] [US4] Contract test for `POST /v1/auth/dns/initiate` and `/v1/auth/dns/verify` in `tests/contract/test_auth_dns.rs`: test initiate returns TXT record value, test successful DNS verification returns bearer + entity
- [x] T054 [P] [US4] Integration test for auth + write flow in `tests/integration/test_auth_flow.rs`: authenticate, create Hole, verify contribution attributed to user and entity

### Implementation for User Story 4

- [x] T055 [P] [US4] Create User and Entity models in `crates/krabbit-api/src/models/user.rs` and `crates/krabbit-api/src/models/entity.rs`: structs per data-model.md, sqlx FromRow, DTOs
- [x] T056 [US4] Implement AuthService in `crates/krabbit-api/src/services/auth_service.rs`: `initiate_email_auth()` (send verification email, store pending verification), `verify_email()` (validate token, find-or-create Entity from email domain, create User, issue JWT), `initiate_dns_auth()` (generate TXT record code), `verify_dns()` (resolve DNS, find-or-create Entity from domain, create User, issue JWT)
- [x] T057 [US4] Implement auth routes in `crates/krabbit-api/src/routes/auth.rs`: add `POST /v1/auth/email`, `POST /v1/auth/email/verify`, `POST /v1/auth/dns/initiate`, `POST /v1/auth/dns/verify`. Wire into router
- [x] T058 [US4] Implement CLI `auth` commands in `crates/krabbit-cli/src/commands/auth.rs`: `auth email <email>` (initiate + prompt for code), `auth dns <domain>` (initiate + print TXT record + verify), `auth status`, `auth logout`. Save token to config
- [x] T059 [US4] Update auth middleware (T016) to validate JWT claims and extract UserId + EntityId, reject expired tokens

**Checkpoint**: Full auth flow works — all write operations require verified contributors linked to entities

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [x] T060 [P] Create Vercel serverless handler in `api/handler.rs`: wrap axum app with `vercel_runtime`, handle request/response conversion
- [x] T061 [P] Add request ID middleware in `crates/krabbit-api/src/main.rs`: generate UUID per request, include in all log spans and response `meta.request_id`
- [x] T062 [P] Add latency logging middleware: record and log request duration for all endpoints
- [x] T063 Run all contract tests against quickstart.md scenarios to validate end-to-end flow
- [x] T064 Run `cargo clippy -- -D warnings` and fix all warnings
- [x] T065 [P] Add `crates/krabbit-api/src/routes/mod.rs` and `crates/krabbit-api/src/models/mod.rs` and `crates/krabbit-api/src/services/mod.rs` module re-exports (if not already created during story phases)
- [x] T066 Verify `vercel deploy` works with preview deployment
- [x] T067 Run quickstart.md validation: follow all steps end-to-end on a clean environment

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion — BLOCKS all user stories
- **User Stories (Phase 3-6)**: All depend on Foundational phase completion
  - US1 (Holes) can start immediately after Foundational
  - US2 (Shells) can start after Foundational — independent of US1 at code level, but logically Shells attach to Holes
  - US3 (Digs/Buries) depends on US2 (needs Shells to exist)
  - US4 (Auth) can start after Foundational — independent of US1-3
- **Polish (Phase 7)**: Depends on all user stories being complete

### User Story Dependencies

- **US1 (P1)**: Can start after Foundational — no dependencies on other stories
- **US2 (P2)**: Can start after Foundational — needs Holes to exist for testing but code is independent
- **US3 (P3)**: Depends on US2 — Digs/Buries attach to Shells
- **US4 (P4)**: Can start after Foundational — independent, but integrates with all stories via auth middleware

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Models before services
- Services before routes
- Routes before CLI commands
- Logging after core implementation

### Parallel Opportunities

- All Setup tasks T002-T008 can run in parallel
- Foundational: T009-T011 (shared types) in parallel, then T012-T013 in parallel, T016+T018+T019 in parallel
- US1: Tests T021-T024 in parallel, then T025 (model), then T026-T029 sequentially
- US2: Tests T030-T033 in parallel, then T034+T035 in parallel, then T036-T039 sequentially
- US3: Tests T040-T043 in parallel, then T044+T045 in parallel, then T046-T051 sequentially
- US4: Tests T052-T054 in parallel, then T055 (models), then T056-T059 sequentially
- US1 and US4 can run fully in parallel with each other
- Polish T060-T062 can run in parallel

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL — blocks all stories)
3. Complete Phase 3: User Story 1 (Holes)
4. **STOP and VALIDATE**: Create a Hole via CLI, list, filter, retrieve
5. Deploy preview to Vercel

### Incremental Delivery

1. Setup + Foundational → Foundation ready
2. Add US1 (Holes) → Test independently → Deploy (browse experience!)
3. Add US2 (Shells) → Test independently → Deploy (claims + artifacts!)
4. Add US3 (Digs/Buries) → Test independently → Deploy (verification loop!)
5. Add US4 (Auth) → Test independently → Deploy (real auth replaces dev tokens!)
6. Polish → Final deploy

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: US1 (Holes) + US2 (Shells) → US3 (Digs/Buries)
   - Developer B: US4 (Auth)
3. Polish together after stories merge

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story is independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Constitution Principle IV: tests first, compile-time checks, cargo clippy clean
