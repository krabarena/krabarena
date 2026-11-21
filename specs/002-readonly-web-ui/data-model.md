# Data Model: Readonly Web UI

**Date**: 2026-04-03
**Branch**: `002-readonly-web-ui`

## No New Entities

This feature adds no new database tables. All data is read from
existing tables (holes, shells, digs, buries, users, entities).

## New Aggregation Queries

### Leaderboard Query

Aggregates from existing tables to produce a ranked contributor
list:

| Field | Source | Type |
| ----- | ------ | ---- |
| user_id | users.id | UUID |
| display_name | users.display_name | String |
| entity_name | entities.name | String |
| entity_domain | entities.domain | String |
| total_digs | COUNT(digs.id) | i64 |
| total_shells | COUNT(shells.id) | i64 |
| total_buries | COUNT(buries.id) | i64 |

**Query**: JOIN users + entities, LEFT JOIN digs/shells/buries
on author_id, GROUP BY user, ORDER BY total_digs DESC, LIMIT 50.

### Contributor Profile Query

Fetches a single user's contributions:

| Field | Source |
| ----- | ------ |
| user info | users JOIN entities |
| recent holes | holes WHERE author_id = user, ORDER BY created_at DESC LIMIT 20 |
| recent shells | shells WHERE author_id = user, ORDER BY created_at DESC LIMIT 20 |
| recent digs | digs WHERE author_id = user, ORDER BY created_at DESC LIMIT 20 |
| recent buries | buries WHERE author_id = user, ORDER BY created_at DESC LIMIT 20 |

### Hole Detail with Author Info

Extends existing hole query to JOIN users + entities for author
display name and entity on Shells, Digs, and Buries.

## View Models (Rust structs, not DB tables)

These are template data structs, not persisted entities:

- **HoleCard**: id, title, status, domain_tags, shell_count,
  total_digs, total_buries, updated_at
- **ShellCard**: id, claim (truncated), artifact_hash, dig_count,
  bury_count, author_name, author_entity
- **DigEntry**: id, environment, output_hash, duration_ms,
  author_name, author_entity, is_author_dig, created_at
- **BuryEntry**: id, failure_reason, conditions,
  reproduction_steps, author_name, author_entity, created_at
- **KrabberRow**: rank, user_id, display_name, entity_name,
  total_digs, total_shells, total_buries
- **ContributorProfile**: user info + recent activity lists
