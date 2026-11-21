//! Shared, unit-aware number formatter for every claim-presentation
//! surface (leaderboard table, race bars, h2h faceoff, frontier dots,
//! OG cards). Centralizing this fixes two viral-share bugs at once:
//! numbers rendered with no dimension at all ("0.102" instead of
//! "102 ms") and numbers rendered unscaled (a raw seconds value shown
//! where milliseconds would read naturally).
//!
//! [`format_metric`] never *guesses* a unit it can't derive from either
//! `unit_hint` or `metric_name` — an ungrounded guess (e.g. assuming a
//! bare "latency" field is seconds) would make the reviewer's grounding
//! check unreliable. When the dimension can't be determined the value
//! renders bare (unit `""`), which is honest and matches the previous
//! behaviour for genuinely dimensionless scores.

/// A formatted metric split into its numeral and unit, so a template
/// that renders them in separate elements (e.g. a muted `.unit` span)
/// can do so; [`format_metric_compact`] joins them back into one string
/// for surfaces that only have room for a single value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormattedMetric {
    pub value: String,
    /// Empty when the metric has no determinable dimension.
    pub unit: String,
}

/// `"102 ms"` — [`format_metric`] with the value and unit joined into
/// one string for surfaces that render a single text node (race bars,
/// h2h faces, leaderboard table cells, OG card rows). Symbol-style
/// units (`%`, `pp`, `¢`, `x`, `×`) attach directly with no space;
/// everything else gets a single space.
pub fn format_metric_compact(
    score: f64,
    unit_hint: Option<&str>,
    metric_name: Option<&str>,
) -> String {
    let m = format_metric(score, unit_hint, metric_name);
    if m.unit.is_empty() {
        m.value
    } else if matches!(m.unit.as_str(), "%" | "pp" | "¢" | "x" | "×") {
        format!("{}{}", m.value, m.unit)
    } else {
        format!("{} {}", m.value, m.unit)
    }
}

/// Format `score` with its detected dimension. See the module docs for
/// the "never guess" principle and [`detect_dimension`] for the
/// detection order.
pub fn format_metric(
    score: f64,
    unit_hint: Option<&str>,
    metric_name: Option<&str>,
) -> FormattedMetric {
    if !score.is_finite() {
        return FormattedMetric {
            value: "–".to_string(),
            unit: String::new(),
        };
    }
    match detect_dimension(unit_hint, metric_name) {
        Dimension::Time(unit) => {
            let base_seconds = to_base_seconds(score, unit);
            let (value, display_unit) = scale_time(base_seconds);
            FormattedMetric {
                value: format_number(value, 3),
                unit: display_unit.to_string(),
            }
        }
        Dimension::Percent(symbol) => FormattedMetric {
            value: format_number(score, 4),
            unit: symbol,
        },
        Dimension::MoneyDollar => FormattedMetric {
            value: format_money(score),
            unit: String::new(),
        },
        Dimension::MoneyCents => {
            if score.abs() >= 100.0 {
                FormattedMetric {
                    value: format_money(score / 100.0),
                    unit: String::new(),
                }
            } else {
                FormattedMetric {
                    value: format_number(score, 4),
                    unit: "¢".to_string(),
                }
            }
        }
        Dimension::Verbatim(unit) => FormattedMetric {
            value: format_number(score, 4),
            unit,
        },
        Dimension::Dimensionless => FormattedMetric {
            value: format_number(score, 4),
            unit: String::new(),
        },
    }
}

// --- Dimension detection -----------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeUnit {
    Micros,
    Millis,
    Seconds,
    Minutes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Dimension {
    Time(TimeUnit),
    /// Display symbol carried along — `"%"` or `"pp"` render
    /// differently even though both are "already-scaled" percentages.
    Percent(String),
    MoneyDollar,
    MoneyCents,
    /// Multiplier / rate / points / any short opaque unit: the value
    /// is formatted plain and the unit passed through verbatim.
    Verbatim(String),
    Dimensionless,
}

const LATENCY_KEYWORDS: [&str; 4] = ["latency", "ttft", "duration", "wall_clock"];

/// Detect what `score` measures, in order:
///
/// 1. `unit_hint`, normalized (trimmed + lowercased), matches one of
///    the known unit vocabularies exactly.
/// 2. Else, suffix-sniff `unit_hint` and `metric_name` for
///    `_ms`/`_msec`, `_us`, `_s`/`_sec`, `_pct`/`_percent`, `_usd`.
/// 3. Else, if either string contains a latency-shaped keyword
///    (`latency`, `ttft`, `duration`, `wall_clock`) with no unit found
///    above, the base unit is genuinely unknown — never assume
///    seconds, fall back to dimensionless.
/// 4. Else, `unit_hint` alone (not `metric_name` — a metric *name*
///    isn't a unit to print) is either a short, whitespace-free token
///    passed through verbatim, or dropped as garbage (>12 chars or
///    contains whitespace).
fn detect_dimension(unit_hint: Option<&str>, metric_name: Option<&str>) -> Dimension {
    if let Some(u) = unit_hint {
        match norm(u).as_str() {
            "µs" | "us" | "μs" => return Dimension::Time(TimeUnit::Micros),
            "ms" => return Dimension::Time(TimeUnit::Millis),
            "s" | "sec" | "secs" | "seconds" | "second" => {
                return Dimension::Time(TimeUnit::Seconds);
            }
            "min" | "minutes" | "minute" => return Dimension::Time(TimeUnit::Minutes),
            "%" | "pct" | "percent" => return Dimension::Percent("%".to_string()),
            "pp" => return Dimension::Percent("pp".to_string()),
            "$" | "usd" | "dollars" => return Dimension::MoneyDollar,
            "cents" | "¢" => return Dimension::MoneyCents,
            "x" | "×" => return Dimension::Verbatim(u.trim().to_string()),
            "pts" | "points" => return Dimension::Verbatim(u.trim().to_string()),
            _ => {}
        }
    }

    for candidate in [unit_hint, metric_name].into_iter().flatten() {
        let n = norm(candidate);
        if n.ends_with("_msec") || n.ends_with("_ms") {
            return Dimension::Time(TimeUnit::Millis);
        }
        if n.ends_with("_us") {
            return Dimension::Time(TimeUnit::Micros);
        }
        if n.ends_with("_sec") || n.ends_with("_s") {
            return Dimension::Time(TimeUnit::Seconds);
        }
        if n.ends_with("_pct") || n.ends_with("_percent") {
            return Dimension::Percent("%".to_string());
        }
        if n.ends_with("_usd") {
            return Dimension::MoneyDollar;
        }
    }

    for candidate in [unit_hint, metric_name].into_iter().flatten() {
        let n = norm(candidate);
        if LATENCY_KEYWORDS.iter().any(|k| n.contains(*k)) {
            return Dimension::Dimensionless;
        }
    }

    match unit_hint {
        Some(u) => {
            let trimmed = u.trim();
            let is_garbage = trimmed.is_empty()
                || trimmed.chars().count() > 12
                || trimmed.chars().any(char::is_whitespace);
            if is_garbage {
                Dimension::Dimensionless
            } else {
                Dimension::Verbatim(trimmed.to_string())
            }
        }
        None => Dimension::Dimensionless,
    }
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

// --- Time scaling ---------------------------------------------------------

fn to_base_seconds(score: f64, unit: TimeUnit) -> f64 {
    match unit {
        TimeUnit::Micros => score / 1_000_000.0,
        TimeUnit::Millis => score / 1_000.0,
        TimeUnit::Seconds => score,
        TimeUnit::Minutes => score * 60.0,
    }
}

/// Pick µs/ms/s so the displayed value lands in `[1, 1000)`, preferring
/// the smallest unit that achieves it (matches how people write
/// latencies: "102 ms", not "0.102 s"). Values ≥ 1000 s stay in
/// seconds rather than growing a compound "Nm Ns" — see module docs.
/// Sub-microsecond values (no unit gets the value ≥ 1) fall back to µs
/// so the number doesn't render unitless.
fn scale_time(base_seconds: f64) -> (f64, &'static str) {
    let neg = base_seconds.is_sign_negative();
    let mag = base_seconds.abs();
    let us = mag * 1_000_000.0;
    let ms = mag * 1_000.0;
    let (val, unit) = if (1.0..1000.0).contains(&us) {
        (us, "µs")
    } else if (1.0..1000.0).contains(&ms) {
        (ms, "ms")
    } else if mag >= 1.0 {
        (mag, "s")
    } else {
        (us, "µs")
    };
    (if neg { -val } else { val }, unit)
}

// --- Numeric formatting -----------------------------------------------

/// `"$0.0043"` / `"-$5"` — sign carried as a prefix outside the `$` so
/// negative money never reads as `"$-5"`.
fn format_money(v: f64) -> String {
    let neg = v.is_sign_negative() && v != 0.0;
    let body = format_number(v.abs(), 4);
    if neg {
        format!("-${body}")
    } else {
        format!("${body}")
    }
}

/// Format `v` to at most `sig` significant figures, stripping trailing
/// zeros and never using scientific notation. Values that are already
/// whole numbers skip sig-fig rounding entirely and print exactly,
/// with thin thousands separators once the magnitude reaches 1000 —
/// this is what keeps a dimensionless `16795` as `"16,795"` while a
/// rescaled time value like `16.795` (seconds) still rounds to `"16.8"`.
fn format_number(v: f64, sig: i32) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let neg = v.is_sign_negative();
    let av = v.abs();
    if let Some(s) = whole_number_string(av) {
        return if neg { format!("-{s}") } else { s };
    }
    let rounded = round_to_sig_figs(av, sig);
    if rounded == 0.0 {
        return "0".to_string();
    }
    if let Some(s) = whole_number_string(rounded) {
        return if neg { format!("-{s}") } else { s };
    }
    let magnitude = rounded.log10().floor() as i32;
    let decimals = (sig - 1 - magnitude).max(0) as usize;
    let mut s = format!("{rounded:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if neg { format!("-{s}") } else { s }
}

/// `Some("16,795")` when `av` is a whole number safely representable
/// as `i64`; `None` otherwise (leaves the caller to sig-fig-round it).
fn whole_number_string(av: f64) -> Option<String> {
    if av.fract() == 0.0 && av < 1.0e15 {
        Some(group_thousands(av as i64))
    } else {
        None
    }
}

fn round_to_sig_figs(v: f64, sig: i32) -> f64 {
    if v == 0.0 {
        return 0.0;
    }
    let magnitude = v.log10().floor();
    let factor = 10f64.powf((sig as f64 - 1.0) - magnitude);
    (v * factor).round() / factor
}

/// `1234 -> "1,234"`, `16795 -> "16,795"`. `n` must be non-negative —
/// callers apply the sign prefix themselves.
fn group_thousands(n: i64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(score: f64, unit_hint: Option<&str>) -> FormattedMetric {
        format_metric(score, unit_hint, None)
    }

    // --- Time scaling: the exact examples from the spec ---

    #[test]
    fn time_seconds_to_ms() {
        assert_eq!(
            fm(0.102, Some("s")),
            FormattedMetric {
                value: "102".into(),
                unit: "ms".into()
            }
        );
    }

    #[test]
    fn time_ms_to_s_2324() {
        assert_eq!(
            fm(2324.0, Some("ms")),
            FormattedMetric {
                value: "2.32".into(),
                unit: "s".into()
            }
        );
    }

    #[test]
    fn time_ms_to_s_16795() {
        assert_eq!(
            fm(16795.0, Some("ms")),
            FormattedMetric {
                value: "16.8".into(),
                unit: "s".into()
            }
        );
    }

    #[test]
    fn time_ms_stays_ms() {
        assert_eq!(
            fm(628.0, Some("ms")),
            FormattedMetric {
                value: "628".into(),
                unit: "ms".into()
            }
        );
    }

    #[test]
    fn time_seconds_to_us() {
        assert_eq!(
            fm(0.000045, Some("s")),
            FormattedMetric {
                value: "45".into(),
                unit: "µs".into()
            }
        );
    }

    #[test]
    fn time_large_seconds_stay_in_seconds_no_compound() {
        let m = fm(1350.0, Some("s"));
        assert_eq!(m.unit, "s");
        assert!(!m.value.contains('m'), "must not emit compound m/s: {m:?}");
    }

    #[test]
    fn time_minutes_convert_to_base_then_rescale() {
        // 2 min -> 120 s base -> stays in seconds (never displays "min").
        assert_eq!(
            fm(2.0, Some("min")),
            FormattedMetric {
                value: "120".into(),
                unit: "s".into()
            }
        );
    }

    #[test]
    fn time_unit_aliases_all_recognized() {
        for alias in ["µs", "us", "ms", "s", "sec", "secs", "seconds", "second"] {
            let d = detect_dimension(Some(alias), None);
            assert!(
                matches!(d, Dimension::Time(_)),
                "{alias} should be a time unit"
            );
        }
    }

    // --- Percent ---

    #[test]
    fn percent_already_scaled_never_multiplied() {
        assert_eq!(
            fm(82.0, Some("%")),
            FormattedMetric {
                value: "82".into(),
                unit: "%".into()
            }
        );
    }

    #[test]
    fn percent_negative_pp_preserved() {
        assert_eq!(
            fm(-62.0, Some("pp")),
            FormattedMetric {
                value: "-62".into(),
                unit: "pp".into()
            }
        );
    }

    #[test]
    fn percent_spelled_variants_normalize_to_symbol() {
        assert_eq!(fm(50.0, Some("pct")).unit, "%");
        assert_eq!(fm(50.0, Some("percent")).unit, "%");
    }

    // --- Money ---

    #[test]
    fn money_dollar_fraction() {
        assert_eq!(
            fm(0.0043, Some("$")),
            FormattedMetric {
                value: "$0.0043".into(),
                unit: "".into()
            }
        );
    }

    #[test]
    fn money_cents_under_100_stays_cents() {
        assert_eq!(
            fm(43.0, Some("cents")),
            FormattedMetric {
                value: "43".into(),
                unit: "¢".into()
            }
        );
    }

    #[test]
    fn money_cents_over_100_converts_to_dollars() {
        assert_eq!(
            fm(250.0, Some("cents")),
            FormattedMetric {
                value: "$2.5".into(),
                unit: "".into()
            }
        );
    }

    #[test]
    fn money_negative_prefix_outside_dollar_sign() {
        assert_eq!(fm(-5.0, Some("usd")).value, "-$5");
    }

    // --- Rate / multiplier / points / unknown-short passthrough ---

    #[test]
    fn rate_passthrough_verbatim() {
        assert_eq!(
            fm(142.5, Some("tok/s")),
            FormattedMetric {
                value: "142.5".into(),
                unit: "tok/s".into()
            }
        );
    }

    #[test]
    fn multiplier_passthrough() {
        assert_eq!(
            fm(2.5, Some("x")),
            FormattedMetric {
                value: "2.5".into(),
                unit: "x".into()
            }
        );
    }

    #[test]
    fn points_passthrough() {
        assert_eq!(
            fm(3.0, Some("pts")),
            FormattedMetric {
                value: "3".into(),
                unit: "pts".into()
            }
        );
    }

    #[test]
    fn unknown_short_unit_passthrough() {
        assert_eq!(
            fm(5.0, Some("qps")),
            FormattedMetric {
                value: "5".into(),
                unit: "qps".into()
            }
        );
    }

    #[test]
    fn unknown_long_unit_is_dimensionless() {
        let long_unit = "a rambling sentence describing the metric";
        assert_eq!(
            fm(3.0, Some(long_unit)),
            FormattedMetric {
                value: "3".into(),
                unit: "".into()
            }
        );
    }

    #[test]
    fn unit_with_whitespace_is_dimensionless() {
        assert_eq!(
            fm(3.0, Some("req sec")),
            FormattedMetric {
                value: "3".into(),
                unit: "".into()
            }
        );
    }

    #[test]
    fn boundary_twelve_chars_kept_thirteen_dropped() {
        assert_eq!(
            detect_dimension(Some("123456789012"), None),
            Dimension::Verbatim("123456789012".into())
        );
        assert_eq!(
            detect_dimension(Some("1234567890123"), None),
            Dimension::Dimensionless
        );
    }

    // --- Suffix sniffing on metric_name ---

    #[test]
    fn suffix_sniffing_from_metric_name() {
        assert_eq!(
            format_metric(628.0, None, Some("latency_p50_ms")),
            FormattedMetric {
                value: "628".into(),
                unit: "ms".into()
            }
        );
        assert_eq!(
            format_metric(45.0, None, Some("ttft_us")),
            FormattedMetric {
                value: "45".into(),
                unit: "µs".into()
            }
        );
        assert_eq!(
            format_metric(2.0, None, Some("wall_clock_sec")),
            FormattedMetric {
                value: "2".into(),
                unit: "s".into()
            }
        );
        assert_eq!(
            format_metric(90.0, None, Some("accuracy_pct")),
            FormattedMetric {
                value: "90".into(),
                unit: "%".into()
            }
        );
        assert_eq!(
            format_metric(4.5, None, Some("cost_usd")),
            FormattedMetric {
                value: "$4.5".into(),
                unit: "".into()
            }
        );
    }

    #[test]
    fn latency_shaped_name_with_no_unit_never_guesses() {
        // No unit_hint, metric_name is bare "latency" — must NOT assume
        // seconds; the base unit is genuinely unknown.
        assert_eq!(
            format_metric(628.0, None, Some("latency")),
            FormattedMetric {
                value: "628".into(),
                unit: "".into()
            }
        );
        assert_eq!(
            format_metric(1.0, None, Some("ttft")),
            FormattedMetric {
                value: "1".into(),
                unit: "".into()
            }
        );
    }

    // --- Value formatting: sig figs, commas, no scientific notation ---

    #[test]
    fn dimensionless_integer_over_1000_gets_commas() {
        assert_eq!(
            fm(16795.0, None),
            FormattedMetric {
                value: "16,795".into(),
                unit: "".into()
            }
        );
    }

    #[test]
    fn dimensionless_strips_trailing_zeros() {
        assert_eq!(fm(2.40, None).value, "2.4");
        assert_eq!(fm(88.70, None).value, "88.7");
        assert_eq!(fm(3.0, None).value, "3");
    }

    #[test]
    fn dimensionless_caps_at_four_significant_figures() {
        assert_eq!(fm(142.5, Some("tok/s")).value, "142.5");
        // 5+ sig figs in, rounds to 4 ("1.235e5") then regroups with commas.
        assert_eq!(fm(123456.789, None).value, "123,500");
        assert_eq!(fm(0.123456, None).value, "0.1235");
    }

    #[test]
    fn never_scientific_notation() {
        let m = fm(0.00000123, None);
        assert!(!m.value.contains('e') && !m.value.contains('E'), "{m:?}");
    }

    #[test]
    fn negative_dimensionless_preserved() {
        assert_eq!(fm(-62.0, None).value, "-62");
        assert_eq!(fm(-0.5, None).value, "-0.5");
    }

    // --- Edge cases: 0, negative, NaN/inf never panic ---

    #[test]
    fn zero_formats_plainly() {
        assert_eq!(
            fm(0.0, None),
            FormattedMetric {
                value: "0".into(),
                unit: "".into()
            }
        );
        assert_eq!(fm(0.0, Some("ms")).value, "0");
    }

    #[test]
    fn nan_and_infinite_fall_back_without_panicking() {
        assert_eq!(
            fm(f64::NAN, Some("ms")),
            FormattedMetric {
                value: "–".into(),
                unit: "".into()
            }
        );
        assert_eq!(fm(f64::INFINITY, Some("s")).value, "–");
        assert_eq!(fm(f64::NEG_INFINITY, None).value, "–");
    }

    // --- Compact form ---

    #[test]
    fn compact_joins_with_space_for_word_units() {
        assert_eq!(format_metric_compact(0.102, Some("s"), None), "102 ms");
        assert_eq!(
            format_metric_compact(142.5, Some("tok/s"), None),
            "142.5 tok/s"
        );
        assert_eq!(format_metric_compact(3.0, Some("pts"), None), "3 pts");
    }

    #[test]
    fn compact_attaches_symbol_units_with_no_space() {
        assert_eq!(format_metric_compact(82.0, Some("%"), None), "82%");
        assert_eq!(format_metric_compact(-62.0, Some("pp"), None), "-62pp");
        assert_eq!(format_metric_compact(2.5, Some("x"), None), "2.5x");
        assert_eq!(format_metric_compact(43.0, Some("cents"), None), "43¢");
    }

    #[test]
    fn compact_dimensionless_has_no_trailing_space() {
        assert_eq!(format_metric_compact(3.0, None, None), "3");
    }

    #[test]
    fn compact_nan_falls_back() {
        assert_eq!(format_metric_compact(f64::NAN, Some("ms"), None), "–");
    }
}
