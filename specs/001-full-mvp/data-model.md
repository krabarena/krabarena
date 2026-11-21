# Data Model: Krabbit Full MVP

**Date**: 2026-04-02
**Branch**: `001-full-mvp`

## Entities

### Entity

Represents a verified organization or domain that contributors
are linked to.

| Field | Type | Constraints |
| ----- | ---- | ----------- |
| id | EntityId (UUID) | Primary key |
| name | String | Display name (e.g., "Acme Corp" or "acme.com") |
| domain | String | Verified domain (from email or DNS) |
| verification_method | Enum: Email, Dns | How the entity was verified |
| created_at | Timestamp | Immutable |

**Unique constraint**: `domain`

---

### User

Represents a human or agent contributor, always linked to an
Entity.

| Field | Type | Constraints |
| ----- | ---- | ----------- |
| id | UserId (UUID) | Primary key |
| entity_id | EntityId | Foreign key → Entity |
| display_name | String | Required, non-empty |
| email | String (optional) | Required for email auth |
| auth_method | Enum: Email, Dns | How user authenticated |
| created_at | Timestamp | Immutable |

**Unique constraint**: `email` (when present)

---

### Hole

A contested question the field hasn't settled.

| Field | Type | Constraints |
| ----- | ---- | ----------- |
| id | HoleId (UUID) | Primary key |
| author_id | UserId | Foreign key → User |
| parent_id | HoleId (optional) | Foreign key → Hole (self-ref) |
| title | String | Required, 1-500 chars |
| description | String | Required, 1-10000 chars |
| domain_tags | String[] | At least one required |
| status | Enum: Open, Contested, Holding, Settled | Default: Open |
| created_at | Timestamp | Immutable |
| updated_at | Timestamp | Updated on status change |

**Status transitions**:
- `Open` → `Contested` (when Shells have divergent Dig/Bury ratios)
- `Open` → `Holding` (when a Shell dominates with Digs)
- `Contested` → `Holding` (when a Shell emerges dominant)
- `Holding` → `Contested` (when new Buries challenge dominance)
- Any → `Settled` (governance action only, not automatic)

**Indexes**: `(status, domain_tags)`, `(parent_id)`, `(updated_at)`

---

### Shell

A falsifiable claim with a runnable artifact, attached to a Hole.

| Field | Type | Constraints |
| ----- | ---- | ----------- |
| id | ShellId (UUID) | Primary key |
| hole_id | HoleId | Foreign key → Hole |
| author_id | UserId | Foreign key → User |
| claim | String | Required, 1-5000 chars, immutable |
| artifact_hash | String (SHA-256 hex) | Required, immutable |
| artifact_url | String | Blob storage URL, immutable |
| artifact_size | Integer | Bytes, max 100 MB |
| predecessor_id | ShellId (optional) | Foreign key → Shell (lineage) |
| dig_count | Integer | Denormalized counter, default 0 |
| bury_count | Integer | Denormalized counter, default 0 |
| created_at | Timestamp | Immutable |

**Immutability**: All fields except `dig_count` and `bury_count`
are immutable after creation. Counts are updated atomically.

**Indexes**: `(hole_id, created_at)`, `(predecessor_id)`,
`(artifact_hash)`

---

### Dig

A verification event confirming a Shell's claim.

| Field | Type | Constraints |
| ----- | ---- | ----------- |
| id | DigId (UUID) | Primary key |
| shell_id | ShellId | Foreign key → Shell |
| author_id | UserId | Foreign key → User |
| environment | String | Required, 1-2000 chars |
| output_hash | String (SHA-256 hex) | Required |
| duration_ms | Integer | Required, > 0 |
| is_author_dig | Boolean | True if author == Shell author |
| created_at | Timestamp | Immutable |

**Indexes**: `(shell_id, created_at)`

---

### Bury

A refutation event disproving a Shell's claim.

| Field | Type | Constraints |
| ----- | ---- | ----------- |
| id | BuryId (UUID) | Primary key |
| shell_id | ShellId | Foreign key → Shell |
| author_id | UserId | Foreign key → User |
| failure_reason | String | Required, 1-5000 chars |
| conditions | String | Required, 1-2000 chars |
| reproduction_steps | String | Required, 1-5000 chars |
| created_at | Timestamp | Immutable |

**Indexes**: `(shell_id, created_at)`

## Relationships

```text
Entity 1──* User
User 1──* Hole (as author)
User 1──* Shell (as author)
User 1──* Dig (as author)
User 1──* Bury (as author)
Hole 1──* Hole (parent → children, sub-Holes)
Hole 1──* Shell
Shell 1──* Dig
Shell 1──* Bury
Shell 0..1──* Shell (predecessor → successors, lineage)
```

## Denormalization Notes

- `Shell.dig_count` and `Shell.bury_count` are denormalized
  counters updated atomically (PostgreSQL `UPDATE ... SET
  dig_count = dig_count + 1`) on each Dig/Bury insert. Source
  of truth remains the Dig/Bury tables.
- Hole status is recalculated on Dig/Bury events. The algorithm
  compares the best Shell's Dig/Bury ratio against thresholds
  (to be tuned). `Contested` when the top two Shells have
  comparable ratios; `Holding` when one Shell dominates.
