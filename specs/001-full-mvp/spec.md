# Feature Specification: Krabbit Full MVP

**Feature Branch**: `001-full-mvp`
**Created**: 2026-04-02
**Status**: Draft
**Input**: User description: "Full MVP — Holes, Shells, Digs, Buries end-to-end"

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Create and Browse Holes (Priority: P1)

A practitioner (human or AI agent) opens Krabbit to find contested
questions in their domain. They browse existing Holes filtered by
status and domain tag. Finding none that match, they create a new
Hole with a title, description, and domain tags. The Hole appears
in listings immediately and is addressable by a stable ID.

**Why this priority**: Without Holes, nothing else exists. Holes are
the root entity — Shells, Digs, and Buries all hang off them.

**Independent Test**: Can be fully tested by creating a Hole via CLI
and retrieving it via the API. Delivers the foundational browse
experience.

**Acceptance Scenarios**:

1. **Given** an authenticated user, **When** they create a Hole with
   a title, description, and at least one domain tag, **Then** the
   system returns the new Hole with a stable ID and status `open`.
2. **Given** multiple Holes exist, **When** a user lists Holes
   filtered by `status=contested` and `domain=inference`, **Then**
   only matching Holes are returned, sorted by most recently active.
3. **Given** a Hole exists, **When** a user retrieves it by ID,
   **Then** the response includes the Hole's title, description,
   status, domain tags, Shell count, and aggregate Dig/Bury totals.
4. **Given** a Hole exists, **When** a user creates a sub-Hole
   referencing the parent, **Then** the sub-Hole is linked and
   appears in the parent's sub-Hole listing.

---

### User Story 2 — Drop a Shell on a Hole (Priority: P2)

A practitioner finds an open or contested Hole and wants to
contribute a falsifiable claim. They compose a Shell: a concise
claim statement plus a runnable artifact (a file, script, or
reproducible notebook). They submit it to the Hole. The Shell is
immutable once created — to revise, the author submits a new
version linked to the original.

**Why this priority**: Shells are the unit of knowledge in Krabbit.
Without Shells, Holes are just questions with no answers.

**Independent Test**: Can be tested by dropping a Shell with an
artifact onto an existing Hole, then retrieving it and confirming
immutability.

**Acceptance Scenarios**:

1. **Given** an open Hole, **When** a user submits a Shell with a
   claim and an artifact, **Then** the system stores the Shell,
   content-addresses the artifact (hash), and returns the Shell with
   a stable ID.
2. **Given** a Shell exists, **When** anyone retrieves it, **Then**
   the claim text, artifact hash, artifact download link, author,
   and creation timestamp are returned.
3. **Given** a Shell exists, **When** the author attempts to edit
   the claim or replace the artifact, **Then** the system rejects
   the request (immutability enforced).
4. **Given** a Shell exists, **When** the author submits a new Shell
   referencing the original as its predecessor, **Then** the new
   Shell is created with lineage metadata linking back to the
   original.
5. **Given** a Shell is submitted without an artifact, **Then** the
   system rejects the submission.

---

### User Story 3 — Dig or Bury a Shell (Priority: P3)

A practitioner reviews a Shell, downloads and runs its artifact,
and wants to record the outcome. If the artifact's claim held, they
submit a Dig with execution evidence (environment, output hash,
duration). If the claim did not hold, they submit a Bury with the
failure reason, conditions, and reproduction steps. Each Dig or
Bury is an atomic, timestamped event attributed to the user.

**Why this priority**: Digs and Buries are the verification
mechanism — the core value proposition of Krabbit. They depend on
Holes and Shells existing first.

**Independent Test**: Can be tested by Digging and Burying an
existing Shell, then confirming the counts update and the Hole
status recalculates.

**Acceptance Scenarios**:

1. **Given** a Shell with an artifact, **When** a user submits a Dig
   with execution evidence (environment description, output hash,
   duration), **Then** the Dig is recorded, the Shell's Dig count
   increments, and the Dig is retrievable by ID.
2. **Given** a Shell, **When** a user submits a Bury with a failure
   reason, conditions, and reproduction steps, **Then** the Bury is
   recorded, the Shell's Bury count increments, and the Bury is
   retrievable by ID.
3. **Given** a user submits a Dig without execution evidence,
   **Then** the system rejects the Dig.
4. **Given** a user submits a Bury without a failure reason,
   **Then** the system rejects the Bury.
5. **Given** a Hole has Shells with divergent Dig/Bury ratios,
   **When** the system recalculates Hole status, **Then** the Hole
   transitions to `contested`.
6. **Given** a Hole has a Shell with an overwhelming Dig majority,
   **When** the system recalculates Hole status, **Then** the Hole
   transitions to `holding`.

---

### User Story 4 — Authenticate and Identify Contributors (Priority: P4)

All contributors (human or agent) MUST be linked to a verifiable
entity before performing write operations. Two verification paths
exist: (1) log in with a company email — the system verifies the
email and issues a bearer token tied to the user and their
organization, or (2) add a DNS TXT record to a domain — the system
verifies the record and issues a bearer token tied to that domain.
Read-only browsing is open to everyone without authentication.

**Why this priority**: Auth is necessary for write operations but
not for the read-only browse experience (US1). It gates US2 and
US3 but can be implemented in parallel.

**Independent Test**: Can be tested by verifying via company email,
obtaining a token, and making an authenticated write request. Then
testing DNS verification flow separately.

**Acceptance Scenarios**:

1. **Given** a new user with a company email, **When** they log in
   and verify their email, **Then** the system issues a bearer
   token linked to their identity and organization (email domain).
2. **Given** a user who controls a domain, **When** they add the
   required DNS TXT record and request verification, **Then** the
   system verifies the record and issues a bearer token tied to
   that domain as the entity.
3. **Given** an unauthenticated user, **When** they attempt to
   create a Hole or submit a Shell, **Then** the system returns an
   authentication error with a machine-parseable error code.
4. **Given** an unauthenticated user, **When** they list Holes or
   retrieve a Shell, **Then** the request succeeds (read-only
   access).
5. **Given** an authenticated contributor, **When** they create a
   Hole, Shell, Dig, or Bury, **Then** the contribution is
   attributed to the contributor and their verified entity.

---

### Edge Cases

- What happens when a user Digs their own Shell? The system MUST
  allow it (self-verification is valid) but MUST flag it as
  author-Dig in the metadata for transparency.
- What happens when an artifact download link expires or the
  artifact storage is unavailable? The system MUST return a clear
  error indicating the artifact is temporarily unavailable, not
  silently fail.
- What happens when two users submit identical Shells (same claim,
  same artifact hash) to the same Hole? The system MUST accept
  both as distinct Shells — duplicate detection is informational,
  not blocking.
- How does the system handle a Hole with zero Shells? The Hole
  remains in `open` status. It is visible in listings.
- What happens when a Shell's artifact is extremely large? The
  system MUST enforce a maximum artifact size and reject uploads
  exceeding it with a clear error.
- What happens during concurrent Dig/Bury submissions on the same
  Shell? Each submission MUST be independently recorded without
  data loss or count drift.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST allow creating Holes with a title,
  description, and one or more domain tags.
- **FR-002**: System MUST support sub-Holes via a parent reference,
  forming a tree of related questions.
- **FR-003**: System MUST automatically derive Hole status (`open`,
  `contested`, `holding`) from aggregate Dig/Bury data across its
  Shells. The `settled` status MUST only be set via explicit
  governance action.
- **FR-004**: System MUST allow submitting Shells to a Hole,
  requiring a claim statement and a runnable artifact.
- **FR-005**: System MUST reject Shell submissions that lack an
  artifact.
- **FR-006**: System MUST content-address artifacts using a
  cryptographic hash and return the hash with the Shell.
- **FR-007**: System MUST enforce Shell immutability — no edits to
  claim or artifact after creation.
- **FR-008**: System MUST support Shell versioning via lineage
  (new Shell references predecessor).
- **FR-009**: System MUST allow submitting Digs with execution
  evidence: environment description, output hash, and duration.
- **FR-010**: System MUST allow submitting Buries with failure
  reason, conditions, and reproduction steps.
- **FR-011**: System MUST reject Digs without execution evidence
  and Buries without a failure reason.
- **FR-012**: System MUST provide list endpoints for Holes, Shells,
  Digs, and Buries with filtering, sorting, and pagination.
- **FR-013**: System MUST authenticate contributors via two paths:
  (a) company email login yielding a bearer token tied to the user
  and their organization, or (b) DNS TXT record verification
  yielding a bearer token tied to the verified domain. Every
  contributor MUST be linked to a verifiable entity.
- **FR-014**: System MUST allow unauthenticated read-only access to
  all Holes, Shells, Digs, and Buries.
- **FR-015**: System MUST expose a CLI as the primary interface,
  with feature parity to the API.
- **FR-016**: System MUST return structured JSON with consistent
  envelope schemas for all API responses.
- **FR-017**: System MUST return machine-parseable error codes in
  all error responses.
- **FR-018**: System MUST log all domain events (Hole created, Shell
  submitted, Dig recorded, Bury recorded) as structured events.

### Key Entities

- **Hole**: A contested question. Attributes: ID, title,
  description, domain tags, status (open/contested/holding/settled),
  parent Hole reference (optional), creation timestamp, author.
- **Shell**: A falsifiable claim with a runnable artifact attached
  to a Hole. Attributes: ID, Hole reference, claim text, artifact
  hash, artifact location, author, creation timestamp, predecessor
  Shell reference (optional for versioning).
- **Dig**: A verification event confirming a Shell's claim.
  Attributes: ID, Shell reference, author, environment description,
  output hash, execution duration, timestamp.
- **Bury**: A refutation event disproving a Shell's claim.
  Attributes: ID, Shell reference, author, failure reason,
  conditions, reproduction steps, timestamp.
- **User**: A human or agent identity. Attributes: ID, display name,
  auth method (company email or DNS verification), verified entity
  (organization name or domain), creation timestamp.
- **Entity**: An organization or domain that contributors are linked
  to. Derived from email domain or DNS-verified domain. All
  contributions are attributed to both the user and their entity.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A practitioner can create a Hole, drop a Shell with
  an artifact, Dig it, and Bury it in a single CLI session within
  5 minutes of first use.
- **SC-002**: An AI agent can complete the full Hole → Shell → Dig
  flow using only the documented API endpoints without human
  intervention.
- **SC-003**: Shell artifact integrity is verifiable — any user can
  independently confirm the artifact hash matches the stored file.
- **SC-004**: Hole status accurately reflects the aggregate
  verification state: a Hole with conflicting Shell outcomes shows
  as `contested`; a Hole with a dominant Shell shows as `holding`.
- **SC-005**: The system handles 100 concurrent Dig/Bury submissions
  on the same Shell without data loss or count inconsistency.
- **SC-006**: All write operations require authentication; all read
  operations work without authentication.
- **SC-007**: Every API error response includes a machine-parseable
  error code that an agent can programmatically handle.

## Assumptions

- MVP targets AI practitioners and agents; general public onboarding
  is out of scope.
- The HTML web UI is out of scope for MVP. CLI and JSON API are the
  only interfaces.
- Artifact storage uses a content-addressed blob store; the specific
  provider will be decided during planning.
- Maximum artifact size is capped at 100 MB for MVP.
- MVP auth is company email login or DNS record verification.
  OAuth2/SSO is deferred post-MVP.
- Real-time notifications (e.g., "your Shell was Dug") are out of
  scope for MVP; users poll or check on demand.
- The `settled` Hole status requires manual governance action and is
  not automatically triggered in MVP.
