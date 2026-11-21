//! Startup self-check: enforce that every `Field` in
//! `krabarena_shared::field_limits::ALL_FIELDS` is backed by a DB
//! CHECK constraint whose min/max match the source-of-truth.
//!
//! Called once from `main.rs` after the pool is ready. Behaviour:
//!
//!   * **DB unreachable** → log and skip (the server already tolerates
//!     boot-without-DB for local dev).
//!   * **All match** → log `schema_check.ok` at INFO.
//!   * **Drift** → log every mismatch at ERROR, then `panic!`. The
//!     server refuses to serve traffic against a schema that doesn't
//!     enforce the same limits the API advertises.
//!
//! Opt-out: set `SKIP_SCHEMA_CHECK=true`. Useful while bootstrapping a
//! brand-new database where migration 030 hasn't been applied yet.
//! Production deployments should leave it unset.
//!
//! Parsing strategy: `pg_get_constraintdef(oid)` returns strings like
//! `CHECK ((char_length(title) BETWEEN 1 AND 500))` (BETWEEN form) or
//! `CHECK ((char_length(rationale) <= 2000))` (max-only form). We
//! match both with one regex and infer min=0 for the max-only form,
//! which is exactly how field_limits.rs treats OAuth carve-out fields.

use krabarena_shared::field_limits::{ALL_FIELDS, Table};
use regex::Regex;
use sqlx::PgPool;
use std::sync::OnceLock;

/// One parsed CHECK constraint on a bounded text column.
#[derive(Debug, Clone)]
struct ParsedCheck {
    table: String,
    column: String,
    min: usize,
    max: usize,
}

fn re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Matches a single `char_length(col) {op} N` clause. The
        // constraint definition we get back from `pg_get_constraintdef`
        // is the canonical form Postgres stores, which differs from
        // what the ADD CONSTRAINT text said:
        //   ADD CONSTRAINT … CHECK (char_length(col) BETWEEN 1 AND 500)
        //     → stored as `(char_length(col) >= 1) AND (char_length(col) <= 500)`
        // So we never see a literal `BETWEEN`. Match each clause
        // separately and recombine per (table, column) in `audit`.
        // The four clause shapes that appear in our constraints:
        //   char_length(col) >= N
        //   char_length(col) <= N
        //   char_length(col) BETWEEN N AND M   (kept for raw-DDL paths)
        //   char_length(col) = N               (currently unused)
        Regex::new(
            r"char_length\(\s*([a-z_][a-z0-9_]*)\s*\)\s+(?:BETWEEN\s+(\d+)\s+AND\s+(\d+)|(>=|<=|=)\s*(\d+))",
        )
        .expect("schema_check regex must compile")
    })
}

/// Verify that every `Field` is covered by a matching DB CHECK.
/// Returns a list of human-readable drift messages; an empty Vec means
/// everything is in sync.
pub async fn audit(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    let tables: Vec<&'static str> = [
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
    ]
    .iter()
    .map(|t| t.name())
    .collect();

    // pg_get_constraintdef gives the canonical CHECK string regardless
    // of how the operator wrote the original ADD CONSTRAINT. Filter by
    // contype = 'c' (CHECK constraints only) and the tables we care
    // about; everything else (FKs, NOT NULL implicit, etc.) is irrelevant.
    let rows: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT t.relname::text AS table_name,
               pg_get_constraintdef(c.oid)::text AS def
          FROM pg_constraint c
          JOIN pg_class t ON t.oid = c.conrelid
         WHERE c.contype = 'c'
           AND t.relname = ANY($1)
        "#,
    )
    .bind(&tables)
    .fetch_all(pool)
    .await?;

    // Walk every CHECK definition row, collect ALL `char_length(col) {op} N`
    // clauses, group by (table, column, constraint-id) and synthesise a
    // (min, max) per CHECK. Postgres normalises `BETWEEN N AND M` to
    // `(char_length(col) >= N) AND (char_length(col) <= M)`, so each
    // bounded constraint produces two clauses — we have to combine
    // them rather than treat each match as a standalone ParsedCheck
    // (the prior implementation did the latter, which meant a
    // standard `BETWEEN 1 AND 500` constraint stored normalised
    // showed up as two half-checks `[1,0]` and `[0,500]`, neither
    // matching `field_limits` and the verifier panicking on every
    // boot — see schema_check regression test below).
    use std::collections::HashMap;
    type Key = (String, String, usize); // (table, column, def-hash) — one bucket per source constraint
    let mut buckets: HashMap<Key, (Option<usize>, Option<usize>)> = HashMap::new();
    let re = re();
    for (table, def) in rows {
        // Hash the def string so two distinct CHECKs on the same column
        // (legacy + new) stay in separate buckets.
        let def_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            def.hash(&mut h);
            h.finish() as usize
        };
        for cap in re.captures_iter(&def) {
            let column = cap.get(1).unwrap().as_str().to_string();
            let key = (table.clone(), column, def_hash);
            let entry = buckets.entry(key).or_insert((None, None));
            if let (Some(lo), Some(hi)) = (cap.get(2), cap.get(3)) {
                // Literal BETWEEN N AND M form — populates both bounds.
                entry.0 = Some(lo.as_str().parse().unwrap_or(0));
                entry.1 = Some(hi.as_str().parse().unwrap_or(0));
            } else if let (Some(op), Some(n)) = (cap.get(4), cap.get(5)) {
                let n: usize = n.as_str().parse().unwrap_or(0);
                match op.as_str() {
                    ">=" => entry.0 = Some(n),
                    "<=" => entry.1 = Some(n),
                    "=" => {
                        entry.0 = Some(n);
                        entry.1 = Some(n);
                    }
                    _ => {}
                }
            }
        }
    }
    let mut found: Vec<ParsedCheck> = Vec::with_capacity(buckets.len());
    for ((table, column, _), (min, max)) in buckets {
        // Default missing min to 0 (matches the OAuth carve-out fields
        // and the prior implementation's treatment of max-only checks).
        // Skip the rare case where we got only a `>=` clause with no
        // upper bound — that's not a length cap shape we care about.
        let Some(max) = max else { continue };
        let min = min.unwrap_or(0);
        found.push(ParsedCheck {
            table,
            column,
            min,
            max,
        });
    }

    let mut drift = Vec::new();
    for f in ALL_FIELDS {
        let spec = f.spec();
        let table_name = spec.table.name();
        let matches: Vec<&ParsedCheck> = found
            .iter()
            .filter(|p| p.table == table_name && p.column == spec.field)
            .collect();
        if matches.is_empty() {
            drift.push(format!(
                "missing CHECK for {table_name}.{} (field_limits says min={}, max={})",
                spec.field, spec.min, spec.max
            ));
            continue;
        }
        // A column may carry multiple CHECKs (e.g. legacy + new); the
        // *tightest* pair wins for our purposes. If any single CHECK
        // matches the expected (min, max), we're satisfied.
        let exact = matches
            .iter()
            .any(|p| p.min == spec.min && p.max == spec.max);
        if !exact {
            let found_str = matches
                .iter()
                .map(|p| format!("[{},{}]", p.min, p.max))
                .collect::<Vec<_>>()
                .join(", ");
            drift.push(format!(
                "drift on {table_name}.{}: field_limits=[{},{}] but DB has {}",
                spec.field, spec.min, spec.max, found_str
            ));
        }
    }
    Ok(drift)
}

/// Run the audit and panic on drift (unless `SKIP_SCHEMA_CHECK=true`).
/// Logs every problem at ERROR before bailing so the operator sees
/// the full picture, not just the first failure.
pub async fn verify_or_die(pool: &PgPool) {
    if std::env::var("SKIP_SCHEMA_CHECK").ok().as_deref() == Some("true") {
        tracing::warn!("schema_check.skipped (SKIP_SCHEMA_CHECK=true)");
        return;
    }
    match audit(pool).await {
        Ok(drift) if drift.is_empty() => {
            tracing::info!(field_count = ALL_FIELDS.len(), "schema_check.ok");
        }
        Ok(drift) => {
            for line in &drift {
                tracing::error!("schema_check.drift: {line}");
            }
            panic!(
                "schema_check failed: {} field(s) drifted between field_limits.rs and the DB. \
                 Either apply migration 030 (or matching) or set SKIP_SCHEMA_CHECK=true \
                 if intentional.",
                drift.len()
            );
        }
        Err(e) => {
            // DB-down case: server is already tolerant of no-DB startup.
            // Don't kill the process here — the pool was created lazy
            // and other code paths will surface DB outage when they
            // actually try to query.
            tracing::warn!(error = %e, "schema_check.skipped (could not reach DB)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_between_form() {
        let caps = re()
            .captures("CHECK ((char_length(title) BETWEEN 1 AND 500))")
            .unwrap();
        assert_eq!(caps.get(1).unwrap().as_str(), "title");
        assert_eq!(caps.get(2).unwrap().as_str(), "1");
        assert_eq!(caps.get(3).unwrap().as_str(), "500");
    }

    #[test]
    fn parses_max_only_form() {
        let caps = re()
            .captures("CHECK ((char_length(rationale) <= 2000))")
            .unwrap();
        assert_eq!(caps.get(1).unwrap().as_str(), "rationale");
        assert!(caps.get(2).is_none());
        assert_eq!(caps.get(4).unwrap().as_str(), "<=");
        assert_eq!(caps.get(5).unwrap().as_str(), "2000");
    }

    #[test]
    fn ignores_unrelated_check() {
        // Other CHECK constraints — e.g. enum constraints — don't
        // mention char_length and so don't match the regex at all.
        assert!(
            re().captures("CHECK ((status = ANY (ARRAY['open'::text, 'settled'::text])))")
                .is_none()
        );
    }

    /// Regression for the v0.11.0 boot-panic: Postgres normalises
    /// `BETWEEN N AND M` to `(>= N) AND (<= M)` in `pg_get_constraintdef`,
    /// so the parser never sees the literal `BETWEEN` keyword in practice.
    /// The old regex only matched `BETWEEN`, treating every bounded
    /// constraint as missing and panicking on boot.
    #[test]
    fn parses_normalized_between_as_two_clauses() {
        let def = "CHECK (((char_length(title) >= 1) AND (char_length(title) <= 500)))";
        let caps: Vec<_> = re().captures_iter(def).collect();
        assert_eq!(caps.len(), 2, "two clauses expected — `>=` and `<=`");
        // First match: char_length(title) >= 1
        assert_eq!(caps[0].get(1).unwrap().as_str(), "title");
        assert_eq!(caps[0].get(4).unwrap().as_str(), ">=");
        assert_eq!(caps[0].get(5).unwrap().as_str(), "1");
        // Second match: char_length(title) <= 500
        assert_eq!(caps[1].get(1).unwrap().as_str(), "title");
        assert_eq!(caps[1].get(4).unwrap().as_str(), "<=");
        assert_eq!(caps[1].get(5).unwrap().as_str(), "500");
    }

    /// Same shape as the previous test but with the `IS NULL OR …` outer
    /// wrap that Postgres adds when the column is nullable + the
    /// CHECK includes a `BETWEEN` clause. This is by far the most
    /// common form in `pg_get_constraintdef` for our nullable text
    /// fields — the original boot panic on prod was driven entirely
    /// by these going unparsed.
    #[test]
    fn parses_nullable_between_form() {
        let def = "CHECK (((agent_backend IS NULL) OR \
                          ((char_length(agent_backend) >= 1) AND \
                           (char_length(agent_backend) <= 64))))";
        let caps: Vec<_> = re().captures_iter(def).collect();
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].get(4).unwrap().as_str(), ">=");
        assert_eq!(caps[0].get(5).unwrap().as_str(), "1");
        assert_eq!(caps[1].get(4).unwrap().as_str(), "<=");
        assert_eq!(caps[1].get(5).unwrap().as_str(), "64");
    }
}
