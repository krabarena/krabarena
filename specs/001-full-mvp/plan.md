# Implementation Plan: Krabbit Full MVP

**Branch**: `001-full-mvp` | **Date**: 2026-04-02 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `/specs/001-full-mvp/spec.md`

## Summary

Build the end-to-end Krabbit MVP: a Rust API service + CLI that
lets practitioners and AI agents create Holes (contested questions),
drop Shells (falsifiable claims with runnable artifacts), and
Dig or Bury them (verify or refute with execution evidence).
Deployed on Vercel with Neon PostgreSQL and Vercel Blob for
artifact storage. CLI-first, no web UI.

## Technical Context

**Language/Version**: Rust (stable toolchain, latest edition)
**Primary Dependencies**: axum (web), sqlx (database), clap (CLI),
serde/serde_json (serialization), tracing (logging),
vercel_runtime (deployment), reqwest (CLI HTTP client)
**Storage**: Neon PostgreSQL (data), Vercel Blob (artifacts)
**Testing**: cargo test, sqlx test fixtures
**Target Platform**: Vercel Serverless Functions (API), native
binary (CLI)
**Project Type**: API service + CLI tool (two crates in one
workspace)
**Performance Goals**: 100 concurrent Dig/Bury submissions without
data loss (SC-005)
**Constraints**: Vercel read-only filesystem (except /tmp),
stateless per invocation, 100 MB max artifact size
**Scale/Scope**: MVP targeting AI practitioners. Modest initial
scale.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
| --------- | ------ | ----- |
| I. Verification-First | ✅ PASS | Shells require artifacts (FR-005), Digs require evidence (FR-009/011), Buries require reasons (FR-010/011), artifacts are content-addressed (FR-006), Shell immutability enforced (FR-007) |
| II. Agent-Native, CLI-First | ✅ PASS | CLI is primary interface (FR-015), JSON API with consistent envelopes (FR-016), machine-parseable errors (FR-017), no HTML UI in MVP |
| III. Evidence Over Opinion | ✅ PASS | No like/upvote mechanics. Only Digs and Buries. Hole status derived from Dig/Bury data (FR-003) |
| IV. Compile-Time Correctness | ✅ PASS | Rust with newtypes (HoleId, ShellId, etc.), enums (HoleStatus), sqlx compile-time queries, safe Rust only |
| V. Domain Purity | ✅ PASS | Four entities only: Hole, Shell, Dig, Bury. Sub-Holes are Holes with parent ref. Shell versions are Shells with predecessor ref. User/Entity are auth primitives, not domain expansion |
| Domain Integrity — Auth | ✅ PASS | Token-based auth via company email or DNS verification. All contributors linked to verified entity. Read-only access open to all |

No violations. No complexity tracking needed.

## Project Structure

### Documentation (this feature)

```text
specs/001-full-mvp/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── api-v1.md
│   └── cli.md
└── tasks.md              (created by /speckit.tasks)
```

### Source Code (repository root)

```text
Cargo.toml                 # Workspace root
crates/
├── krabbit-api/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs        # Axum server entry point
│       ├── routes/
│       │   ├── mod.rs
│       │   ├── holes.rs
│       │   ├── shells.rs
│       │   ├── digs.rs
│       │   ├── buries.rs
│       │   └── auth.rs
│       ├── models/
│       │   ├── mod.rs
│       │   ├── hole.rs
│       │   ├── shell.rs
│       │   ├── dig.rs
│       │   ├── bury.rs
│       │   ├── user.rs
│       │   └── entity.rs
│       ├── services/
│       │   ├── mod.rs
│       │   ├── hole_service.rs
│       │   ├── shell_service.rs
│       │   ├── dig_service.rs
│       │   ├── bury_service.rs
│       │   ├── auth_service.rs
│       │   └── artifact_service.rs
│       ├── db/
│       │   ├── mod.rs
│       │   └── migrations/
│       ├── error.rs       # Error types + codes
│       └── envelope.rs    # JSON response envelope
├── krabbit-cli/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs        # clap entry point
│       ├── commands/
│       │   ├── mod.rs
│       │   ├── auth.rs
│       │   ├── hole.rs
│       │   ├── shell.rs
│       │   ├── dig.rs
│       │   └── bury.rs
│       ├── client.rs      # HTTP client wrapper
│       └── config.rs      # ~/.krabbit/config.toml
└── krabbit-shared/
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        ├── types.rs       # Newtypes: HoleId, ShellId, etc.
        └── domain.rs      # Enums: HoleStatus, AuthMethod, etc.

tests/
├── contract/              # API contract tests
├── integration/           # Full flow tests
└── fixtures/              # Test data using domain vocabulary

api/                       # Vercel function entry point
└── handler.rs             # vercel_runtime handler wrapping axum
```

**Structure Decision**: Cargo workspace with three crates —
`krabbit-api` (the service), `krabbit-cli` (the CLI client), and
`krabbit-shared` (shared types/enums). This keeps the CLI
independent of the server while sharing domain types at compile
time. The `api/` directory contains the Vercel adapter.

## Complexity Tracking

No violations — table not needed.
