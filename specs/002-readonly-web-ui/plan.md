# Implementation Plan: Readonly Web UI

**Branch**: `002-readonly-web-ui` | **Date**: 2026-04-03 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `/specs/002-readonly-web-ui/spec.md`

## Summary

Add a read-only HTML web UI to krabbit-service so human visitors
can browse Holes, view Shells with verification history, and see a
Top Krabbers leaderboard — all without authentication. The UI is
server-rendered HTML served by the existing axum service using
askama templates. Two new read-only API endpoints are added for
leaderboard and contributor profile data.

## Technical Context

**Language/Version**: Rust (stable toolchain, same as existing)
**Primary Dependencies**: askama (HTML templating), axum (existing),
tower-http (existing, for static files)
**Storage**: Existing Neon PostgreSQL (read-only queries)
**Testing**: cargo test, browser manual testing
**Target Platform**: Vercel Serverless (same deployment)
**Project Type**: Server-rendered web pages added to existing API
**Performance Goals**: Homepage loads in <2 seconds (SC-002)
**Constraints**: Read-only, no auth, responsive down to 320px
**Scale/Scope**: 5 pages (homepage, hole detail, shell detail,
leaderboard, contributor profile)

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
| --------- | ------ | ----- |
| I. Verification-First | N/A | UI is read-only, no new write paths |
| II. Agent-Native, CLI-First | ✅ PASS | Constitution says "HTML UI is a secondary delivery channel. It MUST NOT introduce capabilities absent from the CLI/API." This UI is strictly read-only, consuming existing API data. No new capabilities. |
| III. Evidence Over Opinion | ✅ PASS | UI displays Dig/Bury data, not engagement metrics. No like/upvote. Leaderboard ranks by Digs (proof of work). |
| IV. Compile-Time Correctness | ✅ PASS | Askama templates are compile-time checked. Rust types enforced. |
| V. Domain Purity | ✅ PASS | "Krabber" is a display label for existing User entity, not a new domain concept. No new entities. |
| Domain Integrity — Auth | ✅ PASS | UI is read-only, no auth required. Matches "everyone can read" policy. |

No violations.

## Project Structure

### Documentation (this feature)

```text
specs/002-readonly-web-ui/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   └── pages.md
└── tasks.md              (created by /speckit.tasks)
```

### Source Code (repository root)

```text
crates/krabbit-api/src/
├── web/                    # NEW: Web UI module
│   ├── mod.rs
│   ├── pages.rs            # Route handlers for HTML pages
│   └── filters.rs          # Askama custom filters
├── templates/              # NEW: Askama HTML templates
│   ├── base.html           # Shared layout (nav, footer, dark theme)
│   ├── index.html          # Homepage: Hole listing
│   ├── hole.html           # Hole detail + Shells
│   ├── shell.html          # Shell detail + Digs/Buries
│   ├── leaderboard.html    # Top Krabbers
│   └── contributor.html    # Contributor profile
├── static/                 # NEW: CSS + minimal JS
│   ├── style.css           # Dark theme, responsive
│   └── app.js              # Filter interactions (minimal)
├── services/
│   ├── leaderboard_service.rs  # NEW: aggregation queries
│   └── contributor_service.rs  # NEW: user contribution queries
└── routes/
    └── (existing routes unchanged)
```

**Structure Decision**: Add a `web/` module to the existing
`krabbit-api` crate for HTML page handlers, plus `templates/` and
`static/` directories. No new crate needed — the UI is a thin
layer on existing services.

## Complexity Tracking

No violations — table not needed.
