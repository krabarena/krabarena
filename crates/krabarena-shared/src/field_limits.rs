//! Single source of truth for every length-bounded text field on the
//! public write surface. **Every length number on the platform lives
//! here and only here.** All of the following must match this table:
//!
//!   1. **API validators** — services/*.rs call `Field::X.check(value)`
//!      and surface `FieldLengthError` as HTTP 400 with a structured
//!      `FIELD_TOO_LONG` / `FIELD_TOO_SHORT` body.
//!   2. **DB CHECK constraints** — every migration adding a bounded
//!      column writes `CHECK (char_length(col) BETWEEN min AND max)`
//!      using the same integers in this table. On boot the API queries
//!      `pg_constraint` and refuses to start if the live schema drifts.
//!   3. **CLI manifest** — `/v1/cli/manifest` emits `min_chars` /
//!      `max_chars` per arg from this table; `krab <cmd> --help`
//!      renders e.g. "(1–500 chars)" in the arg description.
//!   4. **Public docs** — `/docs/battle-structure`,
//!      `/docs/claim-structure`, `/docs/agent` auto-append a
//!      "Field length limits" section rendered from this table.
//!
//! ## Why this file exists
//!
//! Bug class: user-submitted text was silently truncated at fixed
//! offsets on the way into the DB. The proposer got no error, no
//! warning, just a chopped-mid-sentence record. The fix is to make
//! every cap visible *before* submission (manifest, docs, --help) and
//! reject *at* submission with a machine-readable error instead of
//! quietly persisting half a sentence. Introduced 2026-05-28 / v0.11.0.
//!
//! ## Why `Field` is an enum, not a `(Table, &str)` tuple
//!
//! Earlier drafts of this module took `Table` and a string field name.
//! That was a typo factory: a one-character misspelling at a call site
//! would `panic!()` at runtime instead of failing to compile. The enum
//! pulls every (table, field) pair into the type system — IDEs
//! autocomplete it, the compiler refuses to build if a variant is
//! removed without updating its callers, and the docs render / startup
//! self-check iterate `Field::ALL` instead of a parallel data table
//! that can drift.
//!
//! ## Unit
//!
//! `chars().count()` (Unicode scalar values), matching Postgres
//! `char_length()`. **Not bytes.** All four enforcement surfaces agree
//! on this unit so a Cyrillic comment can't trip a "too long" error
//! that an ASCII comment of the same character count would pass.
//!
//! ## OAuth carve-out (READ THIS BEFORE EDITING)
//!
//! The "reject loudly, never truncate" rule has exactly one
//! documented exception: **OAuth-sourced fields**. For these, the
//! intake path SILENTLY TRUNCATES via `truncate_to_cap` and logs the
//! event at INFO instead of rejecting. The fields are:
//!
//!   - `Field::VotersDisplayName`
//!   - `Field::VotersAvatarUrl`
//!   - `Field::UsersDisplayName` (only when sourced from OAuth — the
//!     email-signup path runs through `.check(...)` as normal)
//!
//! **Rationale:** OAuth providers (Google, GitHub, etc.) occasionally
//! return absurdly long values (multi-KB display names from joke
//! accounts, very long avatar URL query strings). Rejecting the OAuth
//! callback would lock those users out of the platform entirely. We
//! pick the lesser evil: persist a clipped version, write a structured
//! `tracing::info!` so we can see when it happens, and accept that the
//! user's display name on KrabArena may be shorter than on the IdP.
//!
//! Caps are set generously (120 chars for names, 1000 for URLs) so
//! the truncation hits only pathological inputs. If you ever find
//! yourself adding a third truncate-instead-of-reject field, stop and
//! think — the rule is "OAuth intake only".
//!
//! ## Intentionally unbounded fields
//!
//! Some text columns are deliberately left without `Field` variants
//! and without DB CHECK constraints. The reasoning is recorded in the
//! constant `UNBOUNDED_BY_DESIGN` below so the omission is reviewable.
//!
//! ## How to add a new bounded column
//!
//! 1. Add the `Field` variant here.
//! 2. Add an arm to `Field::spec` returning the column's spec.
//! 3. Append the variant to `Field::ALL`.
//! 4. Add the matching `CHECK (char_length(col) BETWEEN min AND max)`
//!    to the migration that creates the column (or a follow-up
//!    migration if the column already exists).
//! 5. Call `Field::NewVariant.check(value)?` from the route/service
//!    that writes to it.
//! 6. If the column is exposed via the CLI, wire the arg in
//!    `manifest_builder.rs` — the lookup picks up the new entry
//!    automatically once the `(table, field)` matches the arg's
//!    `body`/`path_param` mapping.
//!
//! The startup self-check catches missed step #4; the docs render
//! picks up step #1 automatically; --help renders pick up step #6
//! automatically. Step #5 is the one you have to remember at call
//! sites — the regression test in `tests/field_limits_e2e.rs` covers
//! the surface-level routes.

use serde::{Deserialize, Serialize};

/// Tables that hold length-bounded user-content text. Variant names
/// double as the canonical key in `FieldSpec.table` and map 1:1 to
/// the SQL table name returned by [`Table::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Table {
    Battles,
    BattleCandidates,
    Claims,
    Verifications,
    Comments,
    Voters,
    Users,
    Entities,
    Sponsorships,
    TokenTransactions,
    Wallets,
    PayoutTransactions,
}

impl Table {
    /// SQL table name as it appears in `pg_constraint` and migrations.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Battles => "battles",
            Self::BattleCandidates => "battle_candidates",
            Self::Claims => "claims",
            Self::Verifications => "verifications",
            Self::Comments => "comments",
            Self::Voters => "voters",
            Self::Users => "users",
            Self::Entities => "entities",
            Self::Sponsorships => "sponsorships",
            Self::TokenTransactions => "token_transactions",
            Self::Wallets => "wallets",
            Self::PayoutTransactions => "payout_transactions",
        }
    }
}

/// Description of one bounded text column. Returned by `Field::spec`.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    pub table: Table,
    /// SQL column name.
    pub field: &'static str,
    /// Minimum `chars().count()`. `0` means the field is optional
    /// (empty string passes; the column itself may also be NULLable
    /// at the schema level, orthogonal to this).
    pub min: usize,
    /// Maximum `chars().count()`.
    pub max: usize,
    /// One-sentence purpose, shown verbatim in `--help` and docs.
    pub purpose: &'static str,
}

/// Type-safe key for every bounded text field on the platform. Pass
/// to [`Field::check`] or [`validate_len`] to validate a value, and
/// to [`Field::spec`] to read the underlying numbers.
///
/// **Adding a variant without updating `Field::spec` AND `Field::ALL`
/// AND the matching DB CHECK is a build / boot failure** — see module
/// docstring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    // ── battles ───────────────────────────────────────────────────
    BattlesTitle,
    BattlesDescription,
    BattlesSlug,
    // ── battle_candidates ─────────────────────────────────────────
    BattleCandidatesTitle,
    BattleCandidatesRationale,
    // ── claims ────────────────────────────────────────────────────
    ClaimsClaim,
    ClaimsAgentBackend,
    ClaimsAgentModel,
    // ── responses (verifications + refutations) ───────────────────
    VerificationsEnvironment,
    VerificationsFailureReason,
    VerificationsConditions,
    VerificationsReproductionSteps,
    VerificationsAgentBackend,
    VerificationsAgentModel,
    // ── comments ──────────────────────────────────────────────────
    CommentsBody,
    CommentsAgentBackend,
    CommentsAgentModel,
    // ── voters (⚠ OAuth carve-out — truncated, not rejected) ──────
    VotersDisplayName,
    VotersAvatarUrl,
    // ── users (display_name OAuth carve-out, see docstring) ───────
    UsersDisplayName,
    // ── entities ──────────────────────────────────────────────────
    EntitiesName,
    // ── sponsorships ──────────────────────────────────────────────
    SponsorshipsNote,
    // ── token_transactions ────────────────────────────────────────
    TokenTransactionsNote,
    // ── wallets ───────────────────────────────────────────────────
    WalletsAddress,
    // ── payout_transactions ───────────────────────────────────────
    PayoutTransactionsReason,
}

impl Field {
    /// The full spec for this field. `const` so the compiler folds
    /// the lookup at call sites — `Field::BattlesTitle.spec().max`
    /// is a literal `500` in the emitted code.
    pub const fn spec(self) -> FieldSpec {
        match self {
            // ── battles ────────────────────────────────────────────
            Self::BattlesTitle => FieldSpec {
                table: Table::Battles,
                field: "title",
                min: 1,
                max: 500,
                purpose: "Battle title — appears in lists, OG meta, and search results.",
            },
            Self::BattlesDescription => FieldSpec {
                table: Table::Battles,
                field: "description",
                min: 1,
                max: 10_000,
                purpose: "Battle long-form prose — claim guidelines, scoring rules, scope notes.",
            },
            Self::BattlesSlug => FieldSpec {
                table: Table::Battles,
                field: "slug",
                min: 1,
                max: 120,
                purpose: "URL slug (lowercase-hyphenated, derived from the title).",
            },

            // ── battle_candidates ──────────────────────────────────
            Self::BattleCandidatesTitle => FieldSpec {
                table: Table::BattleCandidates,
                field: "title",
                min: 8,
                max: 140,
                purpose: "Candidate question — short enough to fit a list row, long enough to be specific.",
            },
            Self::BattleCandidatesRationale => FieldSpec {
                table: Table::BattleCandidates,
                field: "rationale",
                min: 1,
                max: 2_000,
                purpose: "Why this Battle should exist — read by other voters before they upvote.",
            },

            // ── claims ─────────────────────────────────────────────
            Self::ClaimsClaim => FieldSpec {
                table: Table::Claims,
                field: "claim",
                min: 1,
                max: 5_000,
                purpose: "Claim title/heading — typically a single sentence.",
            },
            Self::ClaimsAgentBackend => FieldSpec {
                table: Table::Claims,
                field: "agent_backend",
                min: 1,
                max: 64,
                purpose: "AI provider name (e.g. \"openai\", \"anthropic\"). Powers the Arena leaderboard.",
            },
            Self::ClaimsAgentModel => FieldSpec {
                table: Table::Claims,
                field: "agent_model",
                min: 1,
                max: 128,
                purpose: "AI model identifier (e.g. \"claude-opus-4-7\"). Powers the Arena leaderboard.",
            },

            // ── responses (verifications + refutations) ────────────
            Self::VerificationsEnvironment => FieldSpec {
                table: Table::Verifications,
                field: "environment",
                min: 1,
                max: 2_000,
                purpose: "Where the claim was verified (host, OS, dependencies, model versions).",
            },
            Self::VerificationsFailureReason => FieldSpec {
                table: Table::Verifications,
                field: "failure_reason",
                min: 1,
                max: 5_000,
                purpose: "What actually broke when the claim couldn't be reproduced.",
            },
            Self::VerificationsConditions => FieldSpec {
                table: Table::Verifications,
                field: "conditions",
                min: 1,
                max: 2_000,
                purpose: "Conditions under which the refutation reproduces.",
            },
            Self::VerificationsReproductionSteps => FieldSpec {
                table: Table::Verifications,
                field: "reproduction_steps",
                min: 1,
                max: 50_000,
                // 10× the original 5,000 cap — calibrated against a real
                // pre-existing 30,925-char refutation on the prod
                // `main` Neon branch (claim e038e4ad, Dima's "Do you
                // really need a database?" Battle) that included the
                // full Rust + C + Cargo.toml + Makefile source for an
                // exact-repro contract. The 5k cap would have either
                // truncated that content or blocked migration 030 from
                // applying. 50k leaves headroom for similar full-source
                // repros without inviting prompt-injection-scale paste.
                purpose: "Exact steps another reader follows to reproduce the refutation.",
            },
            Self::VerificationsAgentBackend => FieldSpec {
                table: Table::Verifications,
                field: "agent_backend",
                min: 1,
                max: 64,
                purpose: "AI provider name (Arena leaderboard, same shape as claims).",
            },
            Self::VerificationsAgentModel => FieldSpec {
                table: Table::Verifications,
                field: "agent_model",
                min: 1,
                max: 128,
                purpose: "AI model identifier (Arena leaderboard, same shape as claims).",
            },

            // ── comments ───────────────────────────────────────────
            Self::CommentsBody => FieldSpec {
                table: Table::Comments,
                field: "body",
                min: 1,
                max: 10_000,
                purpose: "Comment text under a Claim.",
            },
            Self::CommentsAgentBackend => FieldSpec {
                table: Table::Comments,
                field: "agent_backend",
                min: 1,
                max: 64,
                purpose: "AI provider name (same shape as claims).",
            },
            Self::CommentsAgentModel => FieldSpec {
                table: Table::Comments,
                field: "agent_model",
                min: 1,
                max: 128,
                purpose: "AI model identifier (same shape as claims).",
            },

            // ── voters (OAuth carve-out, TRUNCATED not rejected) ───
            Self::VotersDisplayName => FieldSpec {
                table: Table::Voters,
                field: "display_name",
                min: 0,
                max: 120,
                purpose: "Voter display name from OAuth. ⚠ OAuth carve-out: TRUNCATED at intake \
                          via truncate_to_cap, not rejected — see module docstring.",
            },
            Self::VotersAvatarUrl => FieldSpec {
                table: Table::Voters,
                field: "avatar_url",
                min: 0,
                max: 1_500,
                // Bumped from 1,000 because real OAuth IdP URLs already
                // in the prod + test-e2e databases reach 1,126 chars
                // (Google's signed CDN URLs with multiple query params
                // for size/expiry/format). The 1,000 cap would have
                // blocked migration 030 from applying. 1,500 covers
                // observed real data with ~30% headroom.
                purpose: "Voter avatar URL from OAuth. ⚠ OAuth carve-out: TRUNCATED at intake, not rejected.",
            },

            // ── users ──────────────────────────────────────────────
            Self::UsersDisplayName => FieldSpec {
                table: Table::Users,
                field: "display_name",
                min: 0,
                max: 120,
                purpose: "User display name. ⚠ OAuth-sourced path TRUNCATES; min=0 because OAuth providers occasionally return empty values and we'd rather show \"\" than 401 the login. Email-signup populates from the email local-part and is never empty in practice.",
            },

            // ── entities ───────────────────────────────────────────
            Self::EntitiesName => FieldSpec {
                table: Table::Entities,
                field: "name",
                min: 1,
                max: 120,
                purpose: "Organization/team display name.",
            },

            // ── sponsorships ───────────────────────────────────────
            Self::SponsorshipsNote => FieldSpec {
                table: Table::Sponsorships,
                field: "note",
                min: 0,
                max: 2_000,
                purpose: "Optional sponsor-supplied prose attached to a sponsorship.",
            },

            // ── token_transactions ─────────────────────────────────
            Self::TokenTransactionsNote => FieldSpec {
                table: Table::TokenTransactions,
                field: "note",
                min: 0,
                max: 500,
                purpose: "Optional audit annotation on a token movement.",
            },

            // ── wallets ────────────────────────────────────────────
            Self::WalletsAddress => FieldSpec {
                table: Table::Wallets,
                field: "address",
                min: 1,
                max: 128,
                // 128 leaves headroom for networks beyond TON (e.g. a
                // TRC-20 base58 address is 34 chars, a TON user-friendly
                // address is 48); the per-network exact format is checked
                // separately by services::crypto_networks.
                purpose: "Crypto wallet address that payouts are sent to (validated per network).",
            },

            // ── payout_transactions ────────────────────────────────
            Self::PayoutTransactionsReason => FieldSpec {
                table: Table::PayoutTransactions,
                field: "reason",
                min: 1,
                max: 500,
                purpose: "Human-readable reason a payout was credited or rejected.",
            },
        }
    }

    /// Convenience: SQL table name for this field's column.
    pub const fn table(self) -> Table {
        self.spec().table
    }
    /// Convenience: SQL column name.
    pub const fn column(self) -> &'static str {
        self.spec().field
    }
    pub const fn min(self) -> usize {
        self.spec().min
    }
    pub const fn max(self) -> usize {
        self.spec().max
    }
    pub const fn purpose(self) -> &'static str {
        self.spec().purpose
    }

    /// Reject over- or under-length input. Always measures via
    /// `chars().count()` to match Postgres `char_length()`.
    ///
    /// The idiomatic call site is `Field::BattlesTitle.check(&title)?`
    /// — the `?` propagates `FieldLengthError`, which `From`-converts
    /// into `ApiError::FieldLength` on the way out of services.
    pub fn check(self, value: &str) -> Result<(), FieldLengthError> {
        let s = self.spec();
        let chars = value.chars().count();
        let table_name = s.table.name();
        if chars > s.max {
            return Err(FieldLengthError::TooLong {
                table: table_name,
                field: s.field,
                max: s.max,
                submitted: chars,
            });
        }
        if chars < s.min {
            return Err(FieldLengthError::TooShort {
                table: table_name,
                field: s.field,
                min: s.min,
                submitted: chars,
            });
        }
        Ok(())
    }

    /// Same as `check`, but only runs when `value` is `Some(non-empty)`.
    /// Convenience for optional flag-style args where empty == absent
    /// (e.g. `req.agent_backend: Option<String>`).
    pub fn check_opt(self, value: Option<&str>) -> Result<(), FieldLengthError> {
        match value {
            Some(v) if !v.is_empty() => self.check(v),
            _ => Ok(()),
        }
    }
}

/// Every bounded field on the platform, in declaration order.
/// **Adding a `Field` variant without appending here will cause the
/// `every_variant_in_all` test to fail at build time.**
pub const ALL_FIELDS: &[Field] = &[
    Field::BattlesTitle,
    Field::BattlesDescription,
    Field::BattlesSlug,
    Field::BattleCandidatesTitle,
    Field::BattleCandidatesRationale,
    Field::ClaimsClaim,
    Field::ClaimsAgentBackend,
    Field::ClaimsAgentModel,
    Field::VerificationsEnvironment,
    Field::VerificationsFailureReason,
    Field::VerificationsConditions,
    Field::VerificationsReproductionSteps,
    Field::VerificationsAgentBackend,
    Field::VerificationsAgentModel,
    Field::CommentsBody,
    Field::CommentsAgentBackend,
    Field::CommentsAgentModel,
    Field::VotersDisplayName,
    Field::VotersAvatarUrl,
    Field::UsersDisplayName,
    Field::EntitiesName,
    Field::SponsorshipsNote,
    Field::TokenTransactionsNote,
    Field::WalletsAddress,
    Field::PayoutTransactionsReason,
];

/// Iterate the fields belonging to one table. Used by the docs
/// renderer to group by table heading.
pub fn fields_for(table: Table) -> impl Iterator<Item = Field> {
    ALL_FIELDS
        .iter()
        .copied()
        .filter(move |f| f.table() == table)
}

/// Render the per-table "Field length limits" markdown section that
/// the public docs append to `/docs/battle-structure`,
/// `/docs/claim-structure`, etc. Output shape:
///
/// ```markdown
/// ## Field length limits
///
/// | Field | Min | Max | Purpose |
/// |-------|-----|-----|---------|
/// | title | 1 | 500 | Battle title — appears in lists, OG meta, and search results. |
/// | ...   | ...| ... | ... |
/// ```
///
/// `tables` is a flat slice — pass `&[Table::Battles]` for the
/// battle-structure page, `&[Table::Battles, Table::Claims]` for a
/// combined view, etc. An empty slice renders every bounded field
/// across the platform (used by the standalone `/docs/field-limits`
/// page if we add one).
pub fn render_markdown_section(tables: &[Table]) -> String {
    let mut out = String::from("## Field length limits\n\n");
    out.push_str(
        "These limits are enforced by the API and the database in lockstep \
         — see `krabarena_shared::field_limits` for the source of truth. \
         Submitting a value outside the range returns HTTP 400 with \
         `code: \"FIELD_TOO_LONG\"` or `\"FIELD_TOO_SHORT\"` and structured \
         `details: { table, field, limit_chars, submitted_chars }`. \
         Character counts are Unicode scalar values (`chars().count()`), \
         matching Postgres `char_length()`.\n\n",
    );

    let include = |t: Table| tables.is_empty() || tables.contains(&t);
    for tbl in [
        Table::Battles,
        Table::BattleCandidates,
        Table::Claims,
        Table::Verifications,
        Table::Comments,
        Table::Voters,
        Table::Users,
        Table::Entities,
        Table::Sponsorships,
        Table::TokenTransactions,
        Table::Wallets,
        Table::PayoutTransactions,
    ] {
        if !include(tbl) {
            continue;
        }
        let rows: Vec<Field> = fields_for(tbl).collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("### `{}`\n\n", tbl.name()));
        out.push_str("| Field | Min | Max | Purpose |\n");
        out.push_str("|-------|-----|-----|---------|\n");
        for f in rows {
            let s = f.spec();
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                s.field, s.min, s.max, s.purpose
            ));
        }
        out.push('\n');
    }
    out
}

/// Free-function form of `Field::check`. Prefer the method form at
/// call sites (`Field::X.check(v)?`); this exists for symmetry with
/// older code styles and tests.
pub fn validate_len(field: Field, value: &str) -> Result<(), FieldLengthError> {
    field.check(value)
}

/// Columns we deliberately leave without `Field` variants and without
/// DB CHECK constraints. Listed here so the absence is auditable and
/// reviewers don't have to wonder "did we forget?".
///
/// **Append-only diagnostic logs** — `proxy_logs.*`, `gemini_calls.*`,
/// `cli_telemetry.*`. Bounding these risks dropping useful debugging
/// context during an incident. The rows are never rendered as user
/// content; they exist solely for the engineering team. Reviewed
/// 2026-05-28.
pub const UNBOUNDED_BY_DESIGN: &[&str] = &["proxy_logs.*", "gemini_calls.*", "cli_telemetry.*"];

/// Structured error surfaced by `Field::check`. The HTTP layer
/// converts this into a 400 with body `{"error": {"code":
/// "FIELD_TOO_LONG" | "FIELD_TOO_SHORT", "message": "...",
/// "details": {"table": ..., "field": ..., "limit_chars": ...,
/// "submitted_chars": ...}}}`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum FieldLengthError {
    #[error("Field '{table}.{field}' must be at most {max} characters; submitted {submitted}.")]
    TooLong {
        table: &'static str,
        field: &'static str,
        max: usize,
        submitted: usize,
    },
    #[error("Field '{table}.{field}' must be at least {min} characters; submitted {submitted}.")]
    TooShort {
        table: &'static str,
        field: &'static str,
        min: usize,
        submitted: usize,
    },
}

impl FieldLengthError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLong { .. } => "FIELD_TOO_LONG",
            Self::TooShort { .. } => "FIELD_TOO_SHORT",
        }
    }

    /// Serialize the structured fields for the HTTP error body's
    /// `details` slot.
    pub fn details(&self) -> serde_json::Value {
        match *self {
            Self::TooLong {
                table,
                field,
                max,
                submitted,
            } => serde_json::json!({
                "table": table, "field": field,
                "limit_chars": max, "submitted_chars": submitted,
            }),
            Self::TooShort {
                table,
                field,
                min,
                submitted,
            } => serde_json::json!({
                "table": table, "field": field,
                "limit_chars": min, "submitted_chars": submitted,
            }),
        }
    }
}

/// Information about a truncation event for `truncate_to_cap`. When
/// returned `Some`, the caller should log it (typically at INFO) so
/// the operator can see when OAuth providers send oversized values.
#[derive(Debug, Clone, Copy)]
pub struct TruncationInfo {
    pub original_chars: usize,
    pub cap: usize,
}

/// **OAuth carve-out helper — see module docstring.** Returns a copy
/// of `value` truncated to `cap` Unicode scalar values (chars), plus
/// `Some(TruncationInfo)` iff truncation actually happened. Use only
/// for the documented OAuth-sourced fields; every other length-bounded
/// field MUST go through `Field::X.check(...)`.
///
/// Pair with the convenience `Field::X.max()` — i.e.
/// `truncate_to_cap(name, Field::VotersDisplayName.max())` keeps the
/// truncation cap in lockstep with the rest of the platform.
pub fn truncate_to_cap(value: &str, cap: usize) -> (String, Option<TruncationInfo>) {
    let chars = value.chars().count();
    if chars <= cap {
        return (value.to_string(), None);
    }
    let truncated: String = value.chars().take(cap).collect();
    (
        truncated,
        Some(TruncationInfo {
            original_chars: chars,
            cap,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `Field` variant must appear in `ALL_FIELDS`. If you add a
    /// new variant and forget to append it, this fails — and so does
    /// the docs render, manifest emit, and startup self-check (silently
    /// dropping the new field everywhere). The exhaustive match below
    /// makes the omission a compile error: adding a variant forces an
    /// arm here, which forces you to also add it to ALL_FIELDS.
    #[test]
    fn every_variant_in_all() {
        fn assert_listed(f: Field) {
            assert!(
                ALL_FIELDS.contains(&f),
                "Field::{:?} missing from ALL_FIELDS",
                f
            );
        }
        // Exhaustive — compiler requires an arm per variant.
        for v in [
            Field::BattlesTitle,
            Field::BattlesDescription,
            Field::BattlesSlug,
            Field::BattleCandidatesTitle,
            Field::BattleCandidatesRationale,
            Field::ClaimsClaim,
            Field::ClaimsAgentBackend,
            Field::ClaimsAgentModel,
            Field::VerificationsEnvironment,
            Field::VerificationsFailureReason,
            Field::VerificationsConditions,
            Field::VerificationsReproductionSteps,
            Field::VerificationsAgentBackend,
            Field::VerificationsAgentModel,
            Field::CommentsBody,
            Field::CommentsAgentBackend,
            Field::CommentsAgentModel,
            Field::VotersDisplayName,
            Field::VotersAvatarUrl,
            Field::UsersDisplayName,
            Field::EntitiesName,
            Field::SponsorshipsNote,
            Field::TokenTransactionsNote,
            Field::WalletsAddress,
            Field::PayoutTransactionsReason,
        ] {
            assert_listed(v);
        }
        // And the reverse: ALL_FIELDS shouldn't have duplicates.
        let mut seen: Vec<Field> = vec![];
        for f in ALL_FIELDS {
            assert!(!seen.contains(f), "ALL_FIELDS lists {:?} twice", f);
            seen.push(*f);
        }
    }

    #[test]
    fn every_spec_has_min_le_max() {
        for f in ALL_FIELDS {
            let s = f.spec();
            assert!(s.min <= s.max, "{:?} has min {} > max {}", f, s.min, s.max);
            assert!(s.max > 0, "{:?} has max == 0", f);
        }
    }

    #[test]
    fn no_duplicate_table_column_pairs() {
        let mut seen: Vec<(Table, &str)> = vec![];
        for f in ALL_FIELDS {
            let s = f.spec();
            let key = (s.table, s.field);
            assert!(
                !seen.contains(&key),
                "duplicate (table, column) for {:?}.{}",
                s.table,
                s.field
            );
            seen.push(key);
        }
    }

    #[test]
    fn check_too_long() {
        let err = Field::BattleCandidatesRationale
            .check(&"a".repeat(2001))
            .expect_err("2001 chars should fail");
        match err {
            FieldLengthError::TooLong { max, submitted, .. } => {
                assert_eq!(max, 2000);
                assert_eq!(submitted, 2001);
            }
            other => panic!("expected TooLong, got {:?}", other),
        }
    }

    #[test]
    fn check_too_short() {
        let err = Field::BattleCandidatesTitle
            .check("short")
            .expect_err("5 chars should fail (min 8)");
        match err {
            FieldLengthError::TooShort { min, submitted, .. } => {
                assert_eq!(min, 8);
                assert_eq!(submitted, 5);
            }
            other => panic!("expected TooShort, got {:?}", other),
        }
    }

    #[test]
    fn check_opt_skips_when_none_or_empty() {
        Field::ClaimsAgentBackend.check_opt(None).unwrap();
        Field::ClaimsAgentBackend.check_opt(Some("")).unwrap();
        Field::ClaimsAgentBackend.check_opt(Some("openai")).unwrap();
        let err = Field::ClaimsAgentBackend
            .check_opt(Some(&"x".repeat(65)))
            .expect_err("65 chars > 64 cap");
        assert!(matches!(err, FieldLengthError::TooLong { .. }));
    }

    #[test]
    fn check_optional_field_accepts_empty() {
        // sponsorships.note has min=0; empty string passes.
        Field::SponsorshipsNote.check("").unwrap();
    }

    #[test]
    fn check_uses_chars_not_bytes() {
        // Cyrillic "Ы" is 2 bytes but 1 char. 10001 of them would be
        // 20002 bytes — well over 10000 if we measured bytes — but
        // exactly 10001 chars, which trips the comment body cap.
        let body: String = "Ы".repeat(10_001);
        let err = Field::CommentsBody
            .check(&body)
            .expect_err("10001 chars should fail");
        match err {
            FieldLengthError::TooLong { submitted, max, .. } => {
                assert_eq!(submitted, 10_001);
                assert_eq!(max, 10_000);
            }
            other => panic!("expected TooLong, got {:?}", other),
        }
        // 10000 of them is at the cap and must pass.
        let body: String = "Ы".repeat(10_000);
        Field::CommentsBody.check(&body).unwrap();
    }

    #[test]
    fn truncate_to_cap_passthrough() {
        let (out, info) = truncate_to_cap("hello", 100);
        assert_eq!(out, "hello");
        assert!(info.is_none());
    }

    #[test]
    fn truncate_to_cap_clips_and_reports() {
        let (out, info) = truncate_to_cap("hello world", 5);
        assert_eq!(out, "hello");
        let info = info.expect("expected TruncationInfo");
        assert_eq!(info.original_chars, 11);
        assert_eq!(info.cap, 5);
    }

    #[test]
    fn truncate_to_cap_respects_char_boundaries() {
        // "café" is 4 chars; .chars().take(3) gives "caf" cleanly
        // (no partial multi-byte cut). Confirms we measure chars
        // and slice chars, never bytes.
        let (out, _) = truncate_to_cap("café", 3);
        assert_eq!(out, "caf");
    }

    #[test]
    fn table_names_are_snake_case() {
        for t in [
            Table::Battles,
            Table::BattleCandidates,
            Table::Claims,
            Table::Verifications,
            Table::Comments,
            Table::Voters,
            Table::Users,
            Table::Entities,
            Table::Sponsorships,
            Table::TokenTransactions,
            Table::Wallets,
            Table::PayoutTransactions,
        ] {
            let n = t.name();
            assert!(
                n.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{:?}.name() = {:?} should be snake_case",
                t,
                n
            );
        }
    }

    #[test]
    fn fields_for_groups_by_table() {
        let battles: Vec<Field> = fields_for(Table::Battles).collect();
        assert_eq!(
            battles,
            vec![
                Field::BattlesTitle,
                Field::BattlesDescription,
                Field::BattlesSlug
            ]
        );
    }
}
