# Tasks: Readonly Web UI

**Input**: Design documents from `/specs/002-readonly-web-ui/`
**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md, contracts/

**Tests**: Not explicitly requested for this feature. UI testing is manual per quickstart.md.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **API crate**: `crates/krabbit-api/src/`
- **Templates**: `crates/krabbit-api/src/templates/`
- **Static**: `crates/krabbit-api/src/static/`
- **Web module**: `crates/krabbit-api/src/web/`

---

## Phase 1: Setup

**Purpose**: Add askama dependency, create web module structure, static assets

- [ ] T001 Add `askama` and `askama_axum` dependencies to `crates/krabbit-api/Cargo.toml`
- [ ] T002 [P] Create base HTML template at `crates/krabbit-api/src/templates/base.html` with dark theme layout, nav (Home, Leaderboard), footer, responsive meta viewport
- [ ] T003 [P] Create CSS file at `crates/krabbit-api/src/static/style.css` with dark theme (#0d1117 bg, #e6edf3 text, #f97316 accent), status badge colors, responsive breakpoints (480px, 768px, 1024px), card layouts, mobile-first grid
- [ ] T004 [P] Create minimal JS at `crates/krabbit-api/src/static/app.js` for filter interactions (status dropdown, domain tag selection) without page reload
- [ ] T005 Create web module at `crates/krabbit-api/src/web/mod.rs` with submodules: pages, filters
- [ ] T006 [P] Create askama custom filters at `crates/krabbit-api/src/web/filters.rs`: relative time ("2 hours ago"), truncate text, format hash (monospace truncation)
- [ ] T007 Add static file serving for `/static/` directory and HTML page routes to axum router in `crates/krabbit-api/src/main.rs`

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: View model structs and service functions needed by all pages

- [ ] T008 Define view model structs in `crates/krabbit-api/src/web/mod.rs`: `HoleCard`, `ShellCard`, `DigEntry`, `BuryEntry`, `KrabberRow`, `ContributorProfile`
- [ ] T009 [P] Implement leaderboard service in `crates/krabbit-api/src/services/leaderboard_service.rs`: `get_top_krabbers(limit)` — JOIN users+entities, aggregate digs/shells/buries counts, ORDER BY total_digs DESC
- [ ] T010 [P] Implement contributor service in `crates/krabbit-api/src/services/contributor_service.rs`: `get_contributor_profile(user_id)` — user info + recent holes, shells, digs, buries (20 each)
- [ ] T011 Extend hole_service with `get_hole_with_authors(hole_id)` in `crates/krabbit-api/src/services/hole_service.rs`: JOIN users+entities for author display names on Hole and its Shells
- [ ] T012 Update `crates/krabbit-api/src/services/mod.rs` to include new leaderboard_service and contributor_service modules

**Checkpoint**: All data access functions ready for page handlers

---

## Phase 3: User Story 1 — Browse Top Holes (Priority: P1) MVP

**Goal**: Homepage listing Holes with filters, Hole detail page with Shells

**Independent Test**: Open browser, see Hole list, filter by status/domain, click to detail page

- [ ] T013 [P] [US1] Create homepage template at `crates/krabbit-api/src/templates/index.html`: extends base, Hole cards with status badges, domain tag chips, Shell/Dig/Bury counts, filter controls, pagination, empty state
- [ ] T014 [P] [US1] Create Hole detail template at `crates/krabbit-api/src/templates/hole.html`: extends base, Hole header with status badge + description, Shells list with claim preview + counts, sub-Holes section
- [ ] T015 [US1] Implement homepage and Hole detail page handlers in `crates/krabbit-api/src/web/pages.rs`: `GET /` (list holes with query params), `GET /holes/{hole_id}` (hole detail with shells). Wire into router
- [ ] T016 [US1] Add status badge color mapping and domain tag rendering in templates and CSS

**Checkpoint**: Homepage browse experience works — visitors can discover and explore Holes

---

## Phase 4: User Story 2 — Shell Details and Verification History (Priority: P2)

**Goal**: Shell detail page showing claim, artifact info, Digs with evidence, Buries with failure reasons

**Independent Test**: Click a Shell from Hole page, see full claim, Dig list with environments, Bury list with reasons

- [ ] T017 [P] [US2] Create Shell detail template at `crates/krabbit-api/src/templates/shell.html`: extends base, full claim, artifact hash (monospace), artifact size, author entity, lineage link, Digs section with author-Dig badge, Buries section with failure details
- [ ] T018 [US2] Implement Shell detail page handler in `crates/krabbit-api/src/web/pages.rs`: `GET /shells/{shell_id}` — fetch shell + digs + buries with author info. Wire into router
- [ ] T019 [US2] Style author-Dig badge and Bury cards in `crates/krabbit-api/src/static/style.css`: visual distinction for author-Digs (orange outline badge), Bury cards with warning styling

**Checkpoint**: Full verification history visible — visitors can assess Shell credibility

---

## Phase 5: User Story 3 — Top Krabbers Leaderboard (Priority: P3)

**Goal**: Leaderboard page ranking contributors, contributor profile page

**Independent Test**: Navigate to /krabbers, see ranked list, click contributor, see their activity

- [ ] T020 [P] [US3] Create leaderboard template at `crates/krabbit-api/src/templates/leaderboard.html`: extends base, ranked table/cards with display name, entity, Dig/Shell/Bury counts, linked to profile
- [ ] T021 [P] [US3] Create contributor profile template at `crates/krabbit-api/src/templates/contributor.html`: extends base, user header with entity and stats, recent activity timeline (holes, shells, digs, buries mixed)
- [ ] T022 [US3] Implement leaderboard and contributor profile page handlers in `crates/krabbit-api/src/web/pages.rs`: `GET /krabbers` (top 50), `GET /krabbers/{user_id}` (profile). Wire into router

**Checkpoint**: Community discovery works — visitors can find and explore top contributors

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Responsive testing, error states, edge cases

- [ ] T023 [P] Add error page template at `crates/krabbit-api/src/templates/error.html`: friendly "temporarily unavailable" or "not found" message with Krabbit branding
- [ ] T024 [P] Add 404 handler for unknown HTML routes (not API routes) returning the error template
- [ ] T025 Test responsive layout at 320px, 768px, and 1280px viewports — fix any layout breaks in `style.css`
- [ ] T026 Verify empty states: homepage with no Holes, Hole with no Shells, leaderboard with no users
- [ ] T027 Run `cargo clippy -- -D warnings` and fix all warnings
- [ ] T028 Run quickstart.md validation: follow all steps end-to-end

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Foundational (Phase 2)**: Depends on Setup (askama + web module)
- **US1 (Phase 3)**: Depends on Foundational (view models + services)
- **US2 (Phase 4)**: Depends on Foundational — independent of US1
- **US3 (Phase 5)**: Depends on Foundational (leaderboard service)
- **Polish (Phase 6)**: Depends on all user stories

### Parallel Opportunities

- Setup: T002+T003+T004+T006 in parallel
- Foundational: T009+T010 in parallel
- US1: T013+T014 in parallel (templates), then T015 (handlers)
- US2: T017 (template), then T018+T019
- US3: T020+T021 in parallel (templates), then T022 (handlers)
- US1 and US2 and US3 can start in parallel after Foundational

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Setup (Phase 1)
2. Complete Foundational (Phase 2)
3. Complete US1 (Phase 3) — homepage + hole detail
4. **STOP and VALIDATE**: Open browser, browse Holes
5. Deploy preview

### Incremental Delivery

1. Setup + Foundational → ready
2. US1 → homepage works → deploy
3. US2 → shell detail works → deploy
4. US3 → leaderboard works → deploy
5. Polish → final deploy

---

## Notes

- [P] tasks = different files, no dependencies
- All pages are read-only, no auth middleware needed for HTML routes
- Templates are compile-time checked by askama
- 28 total tasks across 6 phases
