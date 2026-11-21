# Research: Krabbit Full MVP

**Date**: 2026-04-02
**Branch**: `001-full-mvp`

## Web Framework

**Decision**: Axum (latest stable)
**Rationale**: Async-first, lightweight, strong Tower middleware
ecosystem, excellent type-safe extractors that align with our
compile-time correctness principle. Best integration path with
Vercel serverless functions via `vercel_runtime` crate.
**Alternatives considered**:
- Actix-web: Heavier, uses `unsafe` internally — conflicts with
  constitution principle IV (safe Rust only).
- Rocket: Less flexible middleware, slower compile times.

## Deployment Model

**Decision**: Vercel Serverless Functions via `vercel_runtime` crate
**Rationale**: Vercel does not run native Rust binaries directly.
The `vercel_runtime` crate compiles Rust into serverless functions.
Edge Functions are not the primary path for Rust. WASM is possible
but unnecessary overhead for a JSON API service.
**Constraints**: Read-only filesystem (except 500 MB `/tmp`),
stateless per invocation, cold start considerations.

## Database

**Decision**: Neon (serverless PostgreSQL)
**Rationale**: Serverless PostgreSQL with Vercel Marketplace
integration (automatic credential injection), instant branching
for preview deployments, scale-to-zero for cost efficiency.
PostgreSQL gives us strong typing, JSONB for flexible metadata,
and robust concurrency for concurrent Dig/Bury submissions.
**Alternatives considered**:
- Turso/libsql: Write-heavy limitations make it unsuitable for
  concurrent Dig/Bury workloads.
- Supabase: More opinionated, adds unnecessary abstraction layer.
- PlanetScale: Declining ecosystem support.

## Blob Storage (Artifacts)

**Decision**: Vercel Blob
**Rationale**: Zero egress fees, global replication, seamless
Vercel integration. Content-addressing is implemented at the
application layer (SHA-256 hash before upload, hash as key).
**Alternatives considered**:
- Cloudflare R2: Cheaper at scale but adds cross-vendor complexity.
- AWS S3: Standard but introduces AWS dependency and egress costs.

## ORM / Database Access

**Decision**: sqlx (compile-time checked queries)
**Rationale**: Compile-time SQL verification aligns perfectly with
constitution principle IV. No runtime ORM overhead. Direct
PostgreSQL protocol support.
**Alternatives considered**:
- Diesel: Heavier ORM, more complex migrations.
- SeaORM: Async but adds abstraction we don't need.

## CLI Framework

**Decision**: clap (derive API)
**Rationale**: Industry standard for Rust CLIs. Derive macros give
compile-time argument validation. The CLI binary is a separate
crate that calls the same API endpoints as any other client.

## Authentication

**Decision**: JWT bearer tokens with two issuance paths
**Rationale**: Stateless tokens suit serverless architecture (no
session store needed). Two issuance paths per constitution:
1. Company email: magic link or password → verify email domain →
   issue JWT with org claim.
2. DNS TXT record: user adds `krabbit-verify=<code>` TXT record →
   system checks DNS → issue JWT with domain claim.

## Content Addressing

**Decision**: SHA-256 hash of artifact bytes
**Rationale**: Standard, widely supported, collision-resistant.
Hash computed client-side before upload, verified server-side.
Stored as hex string in Shell record. Artifact stored in Vercel
Blob with hash as the key path.

## Serialization

**Decision**: serde + serde_json
**Rationale**: De facto standard for Rust JSON serialization.
Zero-cost abstractions, derive macros for compile-time safety.

## Structured Logging

**Decision**: tracing + tracing-subscriber
**Rationale**: Async-aware structured logging. Spans for request
tracing, events for domain actions. JSON output format for
machine consumption in Vercel logs.
