//! Deterministic grounding check for reviewer-emitted presentation.
//! Presentation is best-effort decoration: any failure returns None
//! and the claim publishes without it — never block on this.
//!
//! Invariant enforced here: `Presentation.refutation` survives only
//! when the structure's `hypothesis.verdict` is `"refutes"` — render
//! surfaces may trust `refutation.is_some()` alone. This check reads
//! ONLY `structure_raw` (never the claim markdown) — the hypothesis
//! lives exclusively in structure.json.
//!
//! Numeral grounding, by contrast, draws from a wider pool: every
//! number in `structure_raw` AND every number in the claim markdown
//! (concatenated, structure numbers first so a long markdown can't
//! truncate them out via `MAX_SOURCE_NUMBERS`), plus any ratio of two
//! numbers from that combined pool. This unlocks prose-only claims
//! (e.g. `myth_buster` layouts) that carry no structure.json at all —
//! a numeral restated in the claim markdown is grounded even with
//! `structure_raw: None`.

use crate::models::review::Presentation;
use krabarena_shared::claim_structure::ClaimStructure;

const LAYOUTS: [&str; 4] = ["h2h", "race", "frontier", "myth_buster"];
const MAX_TITLE: usize = 120;
const MAX_ANNOTATION: usize = 200;
const MAX_SOURCE_NUMBERS: usize = 400; // bounds the O(n²) ratio scan
const RATIO_TOLERANCE: f64 = 0.02;

pub fn sanitize_presentation(
    mut p: Presentation,
    structure_raw: Option<&str>,
    claim_markdown: Option<&str>,
) -> Option<Presentation> {
    if !LAYOUTS.contains(&p.layout.as_str()) {
        return None;
    }
    p.tweet_title = truncate_chars(&p.tweet_title, MAX_TITLE);
    p.tweet_annotation = truncate_chars(&p.tweet_annotation, MAX_ANNOTATION);

    // `refutation` only makes sense alongside a structure whose
    // hypothesis verdict is "refutes" — the reviewer LLM is instructed
    // to write the two together, but nothing upstream enforces it.
    // Strip it here (block survives; only the field is dropped) so
    // every render surface can trust `refutation.is_some()` alone.
    let verdict_refutes = structure_raw
        .and_then(|raw| serde_json::from_str::<ClaimStructure>(raw).ok())
        .and_then(|s| s.hypothesis)
        .is_some_and(|h| h.verdict == "refutes");
    if !verdict_refutes {
        p.refutation = None;
    }

    // Structure numbers first: `MAX_SOURCE_NUMBERS` is applied to the
    // combined pool, so a long claim markdown can't push structure.json's
    // (authoritative, machine-emitted) numbers out of the truncation
    // window.
    let mut source: Vec<f64> = structure_raw.map(extract_numbers).unwrap_or_default();
    if let Some(md) = claim_markdown {
        source.extend(extract_numbers(md));
    }
    source.truncate(MAX_SOURCE_NUMBERS);

    let mut checked: Vec<&str> = vec![&p.tweet_title];
    if let Some(h) = &p.hero_stat {
        checked.push(&h.value);
    }
    if let Some(r) = &p.refutation {
        checked.push(&r.claimed);
        checked.push(&r.measured);
    }

    for text in checked {
        for n in extract_numbers(text) {
            if !is_grounded(n, &source) {
                return None;
            }
        }
    }
    Some(p)
}

/// All decimal numbers in a string; "16,795" and "16 795" normalize to 16795.
fn extract_numbers(text: &str) -> Vec<f64> {
    let cleaned: String = {
        // drop thousands separators between digits
        let bytes: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        for (i, c) in bytes.iter().enumerate() {
            let sep = (*c == ',' || *c == ' ' || *c == '\u{202f}' || *c == '\u{00a0}')
                && i > 0
                && i + 1 < bytes.len()
                && bytes[i - 1].is_ascii_digit()
                && bytes[i + 1].is_ascii_digit();
            if !sep {
                out.push(*c);
            }
        }
        out
    };
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in cleaned.chars() {
        if c.is_ascii_digit() || (c == '.' && !cur.is_empty() && !cur.contains('.')) {
            cur.push(c);
        } else if !cur.is_empty() {
            if let Ok(v) = cur.trim_end_matches('.').parse::<f64>() {
                nums.push(v);
            }
            cur.clear();
        }
    }
    if let Ok(v) = cur.trim_end_matches('.').parse::<f64>() {
        nums.push(v);
    }
    nums
}

/// Powers of ten a reviewer's numeral may be scaled by relative to a
/// source number while still counting as "the same value" — unit
/// rescaling (`0.102` s written as `102` ms is `0.102 * 10^3`), not
/// just the percent-of-fraction case (`10^2`).
const RESCALE_POWERS: [i32; 6] = [-3, -2, -1, 1, 2, 3];

fn is_grounded(n: f64, source: &[f64]) -> bool {
    if n == 0.0 {
        return true;
    } // "0" / "100%"-style anchors are style, not data
    let close = |a: f64, b: f64| (a - b).abs() <= RATIO_TOLERANCE * b.abs().max(f64::EPSILON);
    if source.iter().any(|&s| close(n, s)) {
        return true;
    }
    // percent form of a source fraction (0.86 → 86) and, more
    // generally, any power-of-ten unit rescaling (0.102 → 102 via
    // s → ms, 10^3).
    if source
        .iter()
        .any(|&s| RESCALE_POWERS.iter().any(|&k| close(n, s * 10f64.powi(k))))
    {
        return true;
    }
    // ratio of any two source numbers, either direction
    for (i, &a) in source.iter().enumerate() {
        for &b in &source[i + 1..] {
            if b != 0.0 && close(n, a / b) {
                return true;
            }
            if a != 0.0 && close(n, b / a) {
                return true;
            }
        }
    }
    false
}

/// Truncate to `max` chars, appending `…` when truncated. Exported
/// `pub(crate)` so other viral-share-layer code (e.g. `og_summary`
/// capping in `review_service`) shares one implementation instead of
/// hand-rolling a copy.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::review::{HeroStat, Refutation};

    fn mk(layout: &str, hero: Option<(&str, &str)>, refu: Option<(&str, &str)>) -> Presentation {
        Presentation {
            layout: layout.into(),
            tweet_title: "Only one did it in 628 ms".into(),
            tweet_annotation: "n=50, seeded".into(),
            hero_stat: hero.map(|(v, l)| HeroStat {
                value: v.into(),
                label: l.into(),
            }),
            refutation: refu.map(|(c, m)| Refutation {
                claimed: c.into(),
                measured: m.into(),
            }),
        }
    }
    const STRUCT_RAW: &str = r#"{"leaderboard":[{"solution":"A","score":3.0,"metrics":{"latency_p50_ms":628}},{"solution":"B","score":3.0,"metrics":{"latency_p50_ms":2225}}]}"#;

    // Bare `serde_json::from_str::<ClaimStructure>` requires the full
    // required-field set (schema_version/benchmark/leaderboard/winners)
    // — unlike `STRUCT_RAW` above, which only ever needs to survive the
    // looser `extract_numbers` text scan used by the grounding check.
    const STRUCT_REFUTES: &str = r#"{"schema_version":2,
        "benchmark":{"name":"B","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
        "leaderboard":[{"rank":1,"solution":"A","score":3.0,"metrics":{"latency_p50_ms":628.0}},
                       {"rank":2,"solution":"B","score":3.0,"metrics":{"latency_p50_ms":2225.0}}],
        "winners":["A"],
        "hypothesis":{"statement":"A is faster","claimed_value":"6.4x","measured_value":"1.09x","verdict":"refutes"}}"#;
    const STRUCT_CONFIRMS: &str = r#"{"schema_version":2,
        "benchmark":{"name":"B","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
        "leaderboard":[{"rank":1,"solution":"A","score":3.0,"metrics":{"latency_p50_ms":628.0}},
                       {"rank":2,"solution":"B","score":3.0,"metrics":{"latency_p50_ms":2225.0}}],
        "winners":["A"],
        "hypothesis":{"statement":"A is faster","claimed_value":"6.4x","measured_value":"1.09x","verdict":"confirms"}}"#;
    const STRUCT_NO_HYPOTHESIS: &str = r#"{"schema_version":1,
        "benchmark":{"name":"B","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
        "leaderboard":[{"rank":1,"solution":"A","score":3.0}],
        "winners":["A"]}"#;

    #[test]
    fn grounded_verbatim_number_passes() {
        assert!(
            sanitize_presentation(
                mk("race", Some(("628 ms", "fastest")), None),
                Some(STRUCT_RAW),
                None,
            )
            .is_some()
        );
    }
    #[test]
    fn ratio_of_two_grounded_numbers_passes() {
        // 2225 / 628 = 3.54… → "3.5×" allowed within 2% tolerance
        assert!(
            sanitize_presentation(
                mk("race", Some(("3.5×", "faster")), None),
                Some(STRUCT_RAW),
                None,
            )
            .is_some()
        );
    }
    #[test]
    fn hallucinated_number_drops_block() {
        assert!(
            sanitize_presentation(
                mk("race", Some(("9.9×", "faster")), None),
                Some(STRUCT_RAW),
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn unit_rescaled_numeral_grounds_against_source() {
        // 0.102 s restated as 102 ms (10^3) must still ground.
        assert!(is_grounded(102.0, &[0.102]));
        // ...and the reverse direction (source in ms, numeral in s).
        assert!(is_grounded(0.102, &[102.0]));
    }

    #[test]
    fn unit_rescaling_does_not_launder_an_unrelated_number() {
        // 9.9 isn't a power-of-ten rescale of 628 or 2225 in either
        // direction, nor their ratio — must still fail.
        assert!(!is_grounded(9.9, &[628.0, 2225.0]));
    }

    #[test]
    fn hero_stat_with_unit_rescaled_numeral_passes() {
        // Hero stat restates 628 ms as "0.628 s" — a legitimate
        // rescale of a grounded structure number, not a hallucination.
        assert!(
            sanitize_presentation(
                mk("race", Some(("0.628 s", "fastest")), None),
                Some(STRUCT_RAW),
                None,
            )
            .is_some()
        );
    }

    #[test]
    fn bad_layout_drops_block() {
        assert!(
            sanitize_presentation(mk("sparkline", None, None), Some(STRUCT_RAW), None).is_none()
        );
    }
    #[test]
    fn no_structure_allows_only_numberless_copy() {
        let mut p = mk("myth_buster", None, None);
        p.tweet_title = "The loop is the problem, not the language".into();
        assert!(sanitize_presentation(p, None, None).is_some());
        assert!(
            sanitize_presentation(
                mk("myth_buster", Some(("107×", "slower")), None),
                None,
                None
            )
            .is_none()
        );
    }

    // --- numeral grounding: structure.json + claim markdown pool ---

    #[test]
    fn hero_numeral_grounded_only_in_claim_markdown_survives_when_markdown_passed() {
        // 42 doesn't appear anywhere in STRUCT_RAW's numbers (3.0, 628,
        // 2225) — only the claim markdown pool can ground it. Title is
        // overridden to a numberless copy so it isn't itself the thing
        // failing/passing the check.
        let mut p = mk("myth_buster", Some(("42%", "adoption")), None);
        p.tweet_title = "Adoption is a lot lower than teams claim".into();
        let markdown = "Only 42% of teams actually migrated within a year.";
        assert!(
            sanitize_presentation(p, Some(STRUCT_RAW), Some(markdown)).is_some(),
            "42 is grounded via the claim markdown pool"
        );
    }

    #[test]
    fn hero_numeral_in_markdown_only_dropped_when_markdown_none() {
        let mut p = mk("myth_buster", Some(("42%", "adoption")), None);
        p.tweet_title = "Adoption is a lot lower than teams claim".into();
        assert!(
            sanitize_presentation(p, Some(STRUCT_RAW), None).is_none(),
            "42 isn't in structure_raw and markdown wasn't passed — must drop"
        );
    }

    #[test]
    fn structure_less_claim_grounds_numeral_in_markdown_alone() {
        // The whole point: a myth_buster prose claim with no
        // structure.json at all can still ground a numeral as long as
        // it's restated in the claim markdown.
        let mut p = mk("myth_buster", Some(("42%", "adoption")), None);
        p.tweet_title = "Adoption is a lot lower than teams claim".into();
        let markdown = "Only 42% of teams actually migrated within a year.";
        assert!(
            sanitize_presentation(p, None, Some(markdown)).is_some(),
            "structure-less claim must still ground against the markdown"
        );
    }

    // --- refutation ⇔ refutes-verdict invariant ---

    #[test]
    fn refutation_survives_when_structure_verdict_refutes() {
        let out = sanitize_presentation(
            mk("h2h", None, Some(("6.4x", "1.09x"))),
            Some(STRUCT_REFUTES),
            None,
        )
        .expect("valid layout + grounded numbers must survive");
        assert!(
            out.refutation.is_some(),
            "refutes verdict must keep refutation"
        );
    }

    #[test]
    fn refutation_stripped_when_structure_verdict_confirms() {
        let out = sanitize_presentation(
            mk("h2h", None, Some(("6.4x", "1.09x"))),
            Some(STRUCT_CONFIRMS),
            None,
        )
        .expect("block survives — only the refutation field is dropped");
        assert!(
            out.refutation.is_none(),
            "confirms verdict must strip refutation"
        );
    }

    #[test]
    fn refutation_stripped_when_structure_has_no_hypothesis() {
        // Numberless tweet_title: STRUCT_NO_HYPOTHESIS carries no 628
        // to ground the `mk()` default title against, and grounding
        // is orthogonal to what this test is checking.
        let mut p = mk("h2h", None, Some(("6.4x", "1.09x")));
        p.tweet_title = "The loop is the problem, not the language".into();
        let out = sanitize_presentation(p, Some(STRUCT_NO_HYPOTHESIS), None)
            .expect("block survives — only the refutation field is dropped");
        assert!(
            out.refutation.is_none(),
            "missing hypothesis must strip refutation"
        );
    }

    #[test]
    fn refutation_stripped_when_structure_raw_absent() {
        let mut p = mk("h2h", None, Some(("6.4x", "1.09x")));
        p.tweet_title = "The loop is the problem, not the language".into();
        let out = sanitize_presentation(p, None, None)
            .expect("block survives — only the refutation field is dropped");
        assert!(
            out.refutation.is_none(),
            "no structure_raw must strip refutation"
        );
    }

    #[test]
    fn overlong_tweet_fields_truncated_not_dropped() {
        let mut p = mk("race", None, None);
        p.tweet_title = "x".repeat(300);
        let out = sanitize_presentation(p, Some(STRUCT_RAW), None).unwrap();
        assert!(out.tweet_title.chars().count() <= 120);
    }
}
