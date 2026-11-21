//! Geometry for the claim page's `frontier` hero scatter
//! (`templates/components/claim_hero.html`): "nice" axis ticks, axis
//! titles, and server-side label placement / collision handling.
//!
//! Everything here is pure and DB-free so it's unit-testable. The
//! template only positions elements from the percentages computed here
//! (via `data-x` / `data-y`, applied CSP-safely by
//! `frontend/src/dynamic-styles.js`) and picks a label side from the
//! `data-lbl` / `data-lbl-wide` attributes.
//!
//! Rules (mirrored in `docs/ui.md` → "Claim hero: frontier scatter"):
//!
//! - Each axis spans a "nice" envelope of the data (step = 1 / 2 / 2.5
//!   / 5 × 10^k, 3–5 ticks), never a silent min→max stretch. Tick
//!   labels show real values so a zoomed-in range is explicit.
//! - Percent axes whose data sits inside 0–100 are clamped to that
//!   range (no "105 %" tick).
//! - Labels are placed right / left / above / below the dot (inward
//!   side first for dots in the right half) and only where the label
//!   box stays inside the plot and overlaps no other label or marker —
//!   checked against both a phone-width and a desktop-width reference
//!   plot. A dot with no free side gets no in-plot label; the legend
//!   table under the plot always lists every dot.
//! - Dots with (near-)identical coordinates get a small horizontal
//!   jitter slot so each marker stays visible; the legend and the
//!   tooltip carry the exact values.

use crate::services::format_metric::format_metric;

/// One axis tick: position along the axis (0..=100 % of the plot
/// area) and its display label.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisTick {
    pub pct: f32,
    pub label: String,
}

/// A computed axis: nice bounds, ticks, and the unit that the tick
/// labels share (empty when the ticks carry their own units or are
/// dimensionless).
#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    pub lo: f64,
    pub hi: f64,
    pub ticks: Vec<AxisTick>,
    /// Unit shared by every non-zero tick (moved into the axis title so
    /// tick labels can render as bare numbers). Empty → no shared unit.
    pub shared_unit: String,
}

impl Axis {
    /// Map `v` onto 0..=100 % of the plot area.
    pub fn pct(&self, v: f64) -> f32 {
        if (self.hi - self.lo).abs() < f64::EPSILON {
            return 50.0;
        }
        (((v - self.lo) / (self.hi - self.lo)) * 100.0).clamp(0.0, 100.0) as f32
    }
}

/// Round `raw` up to the next "nice" step (1, 2, 2.5, 5, 10 × 10^k).
fn nice_step(raw: f64) -> f64 {
    if !(raw.is_finite() && raw > 0.0) {
        return 1.0;
    }
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    let m = if n <= 1.0 + 1e-9 {
        1.0
    } else if n <= 2.0 + 1e-9 {
        2.0
    } else if n <= 2.5 + 1e-9 {
        2.5
    } else if n <= 5.0 + 1e-9 {
        5.0
    } else {
        10.0
    };
    m * mag
}

/// Snap `v` to the nearest multiple of `step` with float noise
/// removed (so `0.1 * 3` prints as `0.3`).
fn snap(v: f64, step: f64) -> f64 {
    let q = (v / step).round() * step;
    // Round to 10 significant decimals relative to the step.
    let decimals = (-step.log10().floor() + 3.0).max(0.0) as i32;
    let f = 10f64.powi(decimals);
    (q * f).round() / f
}

/// Nice tick values covering `[min, max]` with 3–5 ticks. `percent`
/// clamps the envelope to 0..=100 when the data lies inside it.
pub fn nice_ticks(min: f64, max: f64, percent: bool) -> (f64, f64, Vec<f64>) {
    let (mut min, mut max) = (min.min(max), min.max(max));
    if (max - min).abs() < f64::EPSILON {
        // No spread: open a small window around the single value so it
        // sits mid-axis with real ticks either side.
        let pad = if min.abs() > 0.0 {
            min.abs() * 0.1
        } else {
            1.0
        };
        min -= pad;
        max += pad;
    }
    let in_pct_range = percent && min >= 0.0 && max <= 100.0;
    let mut step = nice_step((max - min) / 4.0);
    let (mut lo, mut hi);
    loop {
        lo = (min / step).floor() * step;
        hi = (max / step).ceil() * step;
        if in_pct_range {
            lo = lo.max(0.0);
            hi = hi.min(100.0);
        }
        if hi - lo < step * 0.5 {
            hi = lo + step;
        }
        let count = ((hi - lo) / step).round() as i64 + 1;
        if count <= 5 {
            break;
        }
        step = nice_step(step * 1.01);
    }
    let n = ((hi - lo) / step).round() as i64;
    let ticks = (0..=n).map(|i| snap(lo + i as f64 * step, step)).collect();
    (snap(lo, step), snap(hi, step), ticks)
}

/// Build an axis for `values`. `unit_hint` / `metric_name` feed the
/// shared [`format_metric`] so ticks scale/label like every other
/// presentation surface (e.g. `5000` with a `_ms` key → "5 s").
pub fn build_axis(values: &[f64], unit_hint: Option<&str>, metric_name: Option<&str>) -> Axis {
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let percent = is_percent(unit_hint, metric_name);
    let (lo, hi, raw_ticks) = if min.is_finite() && max.is_finite() {
        nice_ticks(min, max, percent)
    } else {
        (0.0, 1.0, vec![0.0, 1.0])
    };
    let formatted: Vec<(f64, String, String)> = raw_ticks
        .iter()
        .map(|&v| {
            if v == 0.0 {
                (v, "0".to_string(), String::new())
            } else {
                let m = format_metric(v, unit_hint, metric_name);
                (v, m.value, m.unit)
            }
        })
        .collect();
    let mut units = formatted
        .iter()
        .filter(|(v, _, _)| *v != 0.0)
        .map(|(_, _, u)| u.as_str());
    let first = units.next().unwrap_or("");
    let shared = units.all(|u| u == first);
    let shared_unit = if shared {
        first.to_string()
    } else {
        String::new()
    };
    let axis_tmp = Axis {
        lo,
        hi,
        ticks: Vec::new(),
        shared_unit: shared_unit.clone(),
    };
    let ticks = formatted
        .into_iter()
        .map(|(v, value, unit)| AxisTick {
            pct: axis_tmp.pct(v),
            label: if shared || unit.is_empty() {
                value
            } else if matches!(unit.as_str(), "%" | "pp" | "¢" | "x" | "×") {
                format!("{value}{unit}")
            } else {
                format!("{value} {unit}")
            },
        })
        .collect();
    Axis {
        lo,
        hi,
        ticks,
        shared_unit,
    }
}

fn is_percent(unit_hint: Option<&str>, metric_name: Option<&str>) -> bool {
    format_metric(50.0, unit_hint, metric_name).unit == "%"
}

/// Unit suffixes stripped from a metric key before humanizing (the
/// unit is re-added in parentheses from the formatter instead).
const UNIT_SUFFIXES: [&str; 8] = [
    "_percent", "_pct", "_msec", "_ms", "_us", "_sec", "_s", "_usd",
];

/// `google_url_overlap_pct` → "Google url overlap";
/// `raw_answer_presence_at_10_pct (non-error responses)` →
/// "Raw answer presence at 10". Drops a trailing parenthetical (the
/// axis title has no room for it — the full metric stays in the
/// tooltip / leaderboard) and the unit suffix.
pub fn humanize_metric_key(key: &str) -> String {
    let mut base = key.trim();
    if let Some(i) = base.find(" (")
        && i > 0
    {
        base = base[..i].trim();
    }
    let lower = base.to_lowercase();
    for suffix in UNIT_SUFFIXES {
        if lower.ends_with(suffix) && lower.len() > suffix.len() {
            base = &base[..base.len() - suffix.len()];
            break;
        }
    }
    let words: Vec<&str> = base
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect();
    let joined = words.join(" ");
    let mut chars = joined.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => key.trim().to_string(),
    }
}

/// "Raw answer presence at 10 (%)" — humanized name plus the axis'
/// shared tick unit, when there is one.
pub fn axis_title(metric_key: &str, axis: &Axis) -> String {
    let name = humanize_metric_key(metric_key);
    if axis.shared_unit.is_empty() {
        name
    } else {
        format!("{name} ({})", axis.shared_unit)
    }
}

// --- Label placement ----------------------------------------------------

/// Where a dot's in-plot label goes. `None` hides it (legend still
/// lists the dot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelSide {
    Right,
    Left,
    Above,
    Below,
    None,
}

impl LabelSide {
    /// Value of the `data-lbl` / `data-lbl-wide` attribute the CSS keys
    /// off.
    pub fn as_attr(self) -> &'static str {
        match self {
            LabelSide::Right => "r",
            LabelSide::Left => "l",
            LabelSide::Above => "a",
            LabelSide::Below => "b",
            LabelSide::None => "none",
        }
    }
}

/// Reference plot-area sizes (px) the placement is checked against.
/// Narrow ≈ an iPhone (390 px viewport minus page gutters, card padding
/// and the y-tick gutter); wide ≈ the desktop claim column. Kept in
/// sync with `.frontier-area` in `claim-hero.css`.
pub const NARROW_W: f64 = 250.0;
pub const WIDE_W: f64 = 540.0;
pub const AREA_H: f64 = 180.0;
/// Jitter slot width in px (`.fdot[data-jit]` in `claim-hero.css`).
pub const JITTER_PX: f64 = 9.0;
/// Marker radius (logo / numbered dot are both 18 px across).
const MARK_R: f64 = 9.0;
const LABEL_GAP: f64 = 3.0;
const LABEL_H: f64 = 13.0;
/// CSS `max-width` of `.fdot-label`.
const LABEL_MAX_W: f64 = 96.0;
/// Conservative average glyph width of the 10 px bold label font.
const CHAR_W: f64 = 6.5;
/// How far (px) a label may hang outside the plot area into
/// `.frontier-area`'s insets before it would clip: top inset (24 px)
/// leaves room for labels above the highest dots; the left inset borders
/// the y tick gutter, so only a little slack there.
const PAD_TOP: f64 = 22.0;
const PAD_RIGHT: f64 = 14.0;
const PAD_BOTTOM: f64 = 8.0;
const PAD_LEFT: f64 = 10.0;

/// Estimated rendered width of a label (ellipsized at
/// [`LABEL_MAX_W`]).
fn label_width(label: &str) -> f64 {
    (label.chars().count() as f64 * CHAR_W + 2.0).min(LABEL_MAX_W)
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Rect {
    fn overlaps(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }
    fn inside(&self, w: f64, h: f64) -> bool {
        self.x0 >= -PAD_LEFT
            && self.y0 >= -PAD_TOP
            && self.x1 <= w + PAD_RIGHT
            && self.y1 <= h + PAD_BOTTOM
    }
}

/// A point to place: percentages on each axis, jitter slot, label text.
#[derive(Debug, Clone)]
pub struct PlacementInput<'a> {
    pub x_pct: f32,
    pub y_pct: f32,
    pub jitter: i32,
    pub label: &'a str,
}

fn screen_point(p: &PlacementInput, w: f64, h: f64) -> (f64, f64) {
    let x = p.x_pct as f64 / 100.0 * w + p.jitter as f64 * JITTER_PX;
    let y = (1.0 - p.y_pct as f64 / 100.0) * h;
    (x, y)
}

fn label_rect(side: LabelSide, x: f64, y: f64, lw: f64) -> Rect {
    match side {
        LabelSide::Right => Rect {
            x0: x + MARK_R + LABEL_GAP,
            y0: y - LABEL_H / 2.0,
            x1: x + MARK_R + LABEL_GAP + lw,
            y1: y + LABEL_H / 2.0,
        },
        LabelSide::Left => Rect {
            x0: x - MARK_R - LABEL_GAP - lw,
            y0: y - LABEL_H / 2.0,
            x1: x - MARK_R - LABEL_GAP,
            y1: y + LABEL_H / 2.0,
        },
        LabelSide::Above => Rect {
            x0: x - lw / 2.0,
            y0: y - MARK_R - LABEL_GAP - LABEL_H,
            x1: x + lw / 2.0,
            y1: y - MARK_R - LABEL_GAP,
        },
        LabelSide::Below | LabelSide::None => Rect {
            x0: x - lw / 2.0,
            y0: y + MARK_R + LABEL_GAP,
            x1: x + lw / 2.0,
            y1: y + MARK_R + LABEL_GAP + LABEL_H,
        },
    }
}

/// Greedy label placement for a `w × h` px plot area. `order` gives
/// the priority (indices into `points`; earlier = placed first, e.g.
/// winners). Returns one [`LabelSide`] per point, in `points` order.
pub fn place_labels(points: &[PlacementInput], order: &[usize], w: f64, h: f64) -> Vec<LabelSide> {
    let centers: Vec<(f64, f64)> = points.iter().map(|p| screen_point(p, w, h)).collect();
    let marks: Vec<Rect> = centers
        .iter()
        .map(|&(x, y)| Rect {
            x0: x - MARK_R,
            y0: y - MARK_R,
            x1: x + MARK_R,
            y1: y + MARK_R,
        })
        .collect();
    let mut placed: Vec<Rect> = Vec::new();
    let mut out = vec![LabelSide::None; points.len()];
    for &i in order {
        let (x, y) = centers[i];
        let lw = label_width(points[i].label);
        let candidates = if x > w / 2.0 {
            [
                LabelSide::Left,
                LabelSide::Right,
                LabelSide::Above,
                LabelSide::Below,
            ]
        } else {
            [
                LabelSide::Right,
                LabelSide::Left,
                LabelSide::Above,
                LabelSide::Below,
            ]
        };
        for side in candidates {
            let r = label_rect(side, x, y, lw);
            if !r.inside(w, h) {
                continue;
            }
            let hits_label = placed.iter().any(|p| p.overlaps(&r));
            let hits_mark = marks
                .iter()
                .enumerate()
                .any(|(j, m)| j != i && m.overlaps(&r));
            if !hits_label && !hits_mark {
                placed.push(r);
                out[i] = side;
                break;
            }
        }
    }
    out
}

/// Jitter slots for (near-)coincident dots: dots whose centers are
/// closer than a marker radius on the narrow reference plot fan out
/// horizontally in slots 0, +1, -1, +2, -2, … (× [`JITTER_PX`]).
pub fn jitter_slots(points: &[(f32, f32)]) -> Vec<i32> {
    const SLOTS: [i32; 7] = [0, 1, -1, 2, -2, 3, -3];
    let mut out = vec![0; points.len()];
    let mut grouped = vec![false; points.len()];
    let to_px = |(x, y): (f32, f32)| {
        (
            x as f64 / 100.0 * NARROW_W,
            (1.0 - y as f64 / 100.0) * AREA_H,
        )
    };
    for i in 0..points.len() {
        if grouped[i] {
            continue;
        }
        grouped[i] = true;
        let (xi, yi) = to_px(points[i]);
        let mut slot = 1;
        for j in (i + 1)..points.len() {
            if grouped[j] {
                continue;
            }
            let (xj, yj) = to_px(points[j]);
            if ((xi - xj).powi(2) + (yi - yj).powi(2)).sqrt() < MARK_R {
                grouped[j] = true;
                out[j] = SLOTS[slot.min(SLOTS.len() - 1)];
                slot += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ticks_percent_zooms_but_shows_range() {
        let (lo, hi, ticks) = nice_ticks(84.4, 99.0, true);
        assert_eq!((lo, hi), (80.0, 100.0));
        assert_eq!(ticks, vec![80.0, 85.0, 90.0, 95.0, 100.0]);
    }

    #[test]
    fn nice_ticks_percent_never_exceeds_100() {
        let (_, hi, ticks) = nice_ticks(91.0, 99.5, true);
        assert!(hi <= 100.0);
        assert!(ticks.iter().all(|t| *t <= 100.0));
    }

    #[test]
    fn nice_ticks_latency_outlier_keeps_three_to_five_ticks() {
        let (lo, hi, ticks) = nice_ticks(789.0, 11308.0, false);
        assert_eq!(lo, 0.0);
        assert!(hi >= 11308.0);
        assert!((3..=5).contains(&ticks.len()), "{ticks:?}");
    }

    #[test]
    fn nice_ticks_single_value_opens_window() {
        let (lo, hi, ticks) = nice_ticks(3.0, 3.0, false);
        assert!(lo < 3.0 && hi > 3.0);
        assert!(ticks.len() >= 2);
    }

    #[test]
    fn nice_ticks_small_fractions_have_no_float_noise() {
        let (_, _, ticks) = nice_ticks(0.11, 0.38, false);
        for t in &ticks {
            let s = format!("{t}");
            assert!(s.len() <= 5, "noisy tick {s}");
        }
    }

    #[test]
    fn axis_moves_shared_time_unit_into_title() {
        let axis = build_axis(&[789.0, 11308.0], None, Some("latency_p50_ms"));
        assert_eq!(axis.shared_unit, "s");
        assert_eq!(axis.ticks.first().unwrap().label, "0");
        assert_eq!(axis.ticks.first().unwrap().pct, 0.0);
        assert_eq!(axis.ticks.last().unwrap().pct, 100.0);
        assert_eq!(axis_title("latency_p50_ms", &axis), "Latency p50 (s)");
    }

    #[test]
    fn axis_keeps_per_tick_units_when_mixed() {
        // 0, 500 ms, 1 s, 1.5 s → no single unit; ticks carry theirs.
        let axis = build_axis(&[100.0, 1400.0], None, Some("latency_ms"));
        assert_eq!(axis.shared_unit, "");
        assert!(axis.ticks.iter().any(|t| t.label.ends_with("ms")));
        assert!(axis.ticks.iter().any(|t| t.label.ends_with(" s")));
        assert_eq!(axis_title("latency_ms", &axis), "Latency");
    }

    #[test]
    fn axis_percent_title() {
        let axis = build_axis(
            &[84.4, 99.0],
            Some("%"),
            Some("raw_answer_presence_at_10_pct (non-error responses)"),
        );
        assert_eq!(axis.shared_unit, "%");
        assert_eq!(
            axis_title("raw_answer_presence_at_10_pct (non-error responses)", &axis),
            "Raw answer presence at 10 (%)"
        );
        let labels: Vec<&str> = axis.ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, vec!["80", "85", "90", "95", "100"]);
    }

    #[test]
    fn humanize_metric_keys() {
        assert_eq!(
            humanize_metric_key("google_url_overlap_pct"),
            "Google url overlap"
        );
        assert_eq!(humanize_metric_key("latency_p50_ms"), "Latency p50");
        assert_eq!(humanize_metric_key("usd_per_1k_list"), "Usd per 1k list");
        assert_eq!(
            humanize_metric_key("Mean reciprocal rank"),
            "Mean reciprocal rank"
        );
        assert_eq!(humanize_metric_key("_pct"), "Pct");
    }

    #[test]
    fn labels_avoid_each_other_and_edges() {
        // Two dots side by side at the top-left: the second can't take
        // the right side (first dot's label would collide) and the
        // left/above sides leave the plot → hidden or below.
        let pts = vec![
            PlacementInput {
                x_pct: 2.0,
                y_pct: 100.0,
                jitter: 0,
                label: "Keenable",
            },
            PlacementInput {
                x_pct: 6.0,
                y_pct: 100.0,
                jitter: 0,
                label: "Exa (highlights)",
            },
        ];
        let sides = place_labels(&pts, &[0, 1], NARROW_W, AREA_H);
        assert_ne!(sides[0], LabelSide::Left, "would clip the left edge");
        assert_ne!(sides[0], LabelSide::Above, "would clip the top edge");
        assert_ne!(sides[1], LabelSide::Left);
        assert_ne!(sides[1], LabelSide::Above);
        // Whatever was chosen must not overlap.
        let rects: Vec<Rect> = sides
            .iter()
            .zip(&pts)
            .filter(|(s, _)| **s != LabelSide::None)
            .map(|(s, p)| {
                let (x, y) = screen_point(p, NARROW_W, AREA_H);
                label_rect(*s, x, y, label_width(p.label))
            })
            .collect();
        for (i, a) in rects.iter().enumerate() {
            for b in rects.iter().skip(i + 1) {
                assert!(!a.overlaps(b));
            }
        }
    }

    #[test]
    fn right_edge_label_anchors_inward() {
        let pts = vec![PlacementInput {
            x_pct: 98.0,
            y_pct: 20.0,
            jitter: 0,
            label: "Google via Bright Data SERP (control)",
        }];
        let sides = place_labels(&pts, &[0], NARROW_W, AREA_H);
        assert_eq!(sides[0], LabelSide::Left);
    }

    #[test]
    fn coincident_dots_get_distinct_jitter() {
        let slots = jitter_slots(&[(50.0, 50.0), (50.0, 50.0), (50.5, 50.0), (90.0, 10.0)]);
        assert_eq!(slots[0], 0);
        assert_ne!(slots[1], 0);
        assert_ne!(slots[2], 0);
        assert_ne!(slots[1], slots[2]);
        assert_eq!(slots[3], 0, "far-away dot is untouched");
    }
}
