//! Per-battle presentation knobs (since 0.28.0).
//!
//! A battle can shape how its Claims Wins matrix and summary render
//! *as data* — no code rebuild — via two `battles` columns:
//!
//! * `presentation_guidance TEXT` — freeform natural-language steering
//!   fed to the summary / quick-summary agents (e.g. "lead with the
//!   freshest-news benchmark; keep labels under three words").
//! * `presentation_config JSONB` — the structured knobs in
//!   [`PresentationConfig`], overriding only the keys a battle sets.
//!
//! Both are optional. A battle with neither falls back to the house
//! default: [`PresentationConfig::default`] for the knobs and the
//! caller's default guidance string for the prose.

use serde::{Deserialize, Serialize};

/// Structured knobs that drive the Claims Wins matrix ranking + shape.
///
/// Deserialized from a battle's `presentation_config` JSONB. The
/// struct-level `#[serde(default)]` means any key a battle omits falls
/// back to the house default in [`Default`], so a battle can override a
/// single knob (`{"max_wins_cols": 8}`) without restating the rest.
/// `Default::default()` reproduces the values that were hardcoded in
/// `build_wins_matrix` before this struct existed, so an empty / absent
/// config leaves behavior unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresentationConfig {
    /// Max benchmark columns rendered in the wins matrix. Excess claims
    /// still count toward the honest Wins totals and the "Top N of M"
    /// header — only the *displayed* columns are capped.
    pub max_wins_cols: usize,
    /// Collapse benchmark *variants* (e.g. `SimpleQA-200` / `SimpleQA-50`)
    /// to one column, keeping the highest-ranked variant, so one
    /// benchmark family can't monopolize the cap.
    pub dedup_benchmarks: bool,
    /// A claim counts as "controversial / corrupted" — and is demoted —
    /// once `refutes ≥ controversy_refute_ratio × verifies`. `0.20` ⇒
    /// trusted while fewer than 1 refute per 5 verifies.
    pub controversy_refute_ratio: f64,
    /// Multiplier applied to a controversial claim's effort when
    /// ranking. Small but strictly positive so it sinks to the bottom
    /// without vanishing or flipping sign.
    pub controversy_demotion: f64,
    /// Count sponsored proxy / external API spend
    /// (`claim_structure.cost.external_usd`) toward a claim's `$ effort`.
    /// On proxy-heavy battles this is the dominant — often only — cost.
    pub effort_includes_external: bool,
    /// Count KrabReview (reviewer LLM) spend toward a claim's `$ effort`.
    pub effort_includes_review: bool,
    /// Olympic-style points for a 1st-place (gold) finish in one of the
    /// displayed top-N benchmarks. The standings table + the wins pie
    /// rank competitors by the sum of these across the shown columns.
    pub gold_points: u32,
    /// Points for a 2nd-place (silver) finish in a displayed benchmark.
    pub silver_points: u32,
    /// Points for a 3rd-place (bronze) finish in a displayed benchmark.
    pub bronze_points: u32,
}

impl Default for PresentationConfig {
    fn default() -> Self {
        Self {
            max_wins_cols: 5,
            dedup_benchmarks: true,
            controversy_refute_ratio: 0.20,
            controversy_demotion: 0.10,
            effort_includes_external: true,
            effort_includes_review: true,
            gold_points: 5,
            silver_points: 3,
            bronze_points: 1,
        }
    }
}

impl PresentationConfig {
    /// Parse a battle's `presentation_config` JSONB value, falling back
    /// to the house default for a `NULL` column or any unparseable /
    /// partial blob. Never errors — a malformed override degrades to the
    /// default rather than breaking the battle page.
    pub fn from_json(value: Option<&serde_json::Value>) -> Self {
        match value {
            Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
            None => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_reproduces_legacy_hardcoded_values() {
        let c = PresentationConfig::default();
        assert_eq!(c.max_wins_cols, 5);
        assert!(c.dedup_benchmarks);
        assert_eq!(c.controversy_refute_ratio, 0.20);
        assert_eq!(c.controversy_demotion, 0.10);
        assert!(c.effort_includes_external);
        assert!(c.effort_includes_review);
        assert_eq!(c.gold_points, 5);
        assert_eq!(c.silver_points, 3);
        assert_eq!(c.bronze_points, 1);
    }

    #[test]
    fn partial_json_overrides_only_named_keys() {
        let v = serde_json::json!({ "max_wins_cols": 8, "dedup_benchmarks": false });
        let c = PresentationConfig::from_json(Some(&v));
        assert_eq!(c.max_wins_cols, 8, "overridden");
        assert!(!c.dedup_benchmarks, "overridden");
        // Untouched keys keep house defaults.
        assert_eq!(c.controversy_refute_ratio, 0.20);
        assert!(c.effort_includes_external);
    }

    #[test]
    fn null_or_malformed_falls_back_to_default() {
        assert_eq!(
            PresentationConfig::from_json(None),
            PresentationConfig::default()
        );
        // Unknown key → reject the blob, degrade to default (not a panic).
        let bad = serde_json::json!({ "nope": 1 });
        assert_eq!(
            PresentationConfig::from_json(Some(&bad)),
            PresentationConfig::default()
        );
        // Wrong type → same.
        let bad2 = serde_json::json!({ "max_wins_cols": "lots" });
        assert_eq!(
            PresentationConfig::from_json(Some(&bad2)),
            PresentationConfig::default()
        );
    }
}
