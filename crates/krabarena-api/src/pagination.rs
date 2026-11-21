//! Unified keyset (cursor) pagination primitives.
//!
//! Every forward-scrolling list endpoint shares one opaque cursor
//! format and one page-finalisation helper so the `+1 fetch → has_more
//! → next_cursor` dance and the cursor encoding live in exactly one
//! place.
//!
//! ## Why opaque `(ts, id)` instead of a raw RFC3339 timestamp
//!
//! The historical cursors were bare RFC3339 strings compared with a
//! single-column predicate (`created_at < $cursor`). Two rows sharing
//! the *exact* same timestamp straddle a page boundary: one can be
//! emitted on both pages, or skipped entirely. Pairing the timestamp
//! with the row `id` as a tie-breaker (`(ts, id)` keyset) makes the
//! ordering total and the pagination stable.
//!
//! The wire cursor is `base64url("<rfc3339>|<uuid>")` — opaque to
//! clients so the encoding can evolve. [`Cursor::decode`] still accepts
//! a legacy bare RFC3339 string (mapping it to a `nil` id, which
//! reproduces the old single-column behaviour) so cursors minted by an
//! older deploy keep working across the rollout.

use base64::Engine;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::envelope::PaginationMeta;

fn b64_encode(raw: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

fn b64_decode(raw: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// A value that can be serialised into a `next_cursor` wire string.
/// Implemented by every keyset cursor variant so [`finalize_page`] is
/// agnostic to which one an endpoint uses.
pub trait EncodeCursor {
    fn encode(&self) -> String;
}

/// Keyset cursor for time-ordered lists: a `(timestamp, id)` pair
/// identifying the last row of the previous page. The `id` breaks ties
/// between rows with identical timestamps so the ordering is total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub ts: DateTime<Utc>,
    pub id: Uuid,
}

impl Cursor {
    pub fn new(ts: DateTime<Utc>, id: Uuid) -> Self {
        Self { ts, id }
    }

    /// Decode an incoming cursor. Accepts the opaque form first, then
    /// falls back to a legacy bare RFC3339 timestamp (with a `nil` id)
    /// so cursors issued before this change still resolve.
    pub fn decode(raw: &str) -> Option<Cursor> {
        if let Some(text) = b64_decode(raw) {
            if let Some((ts_str, id_str)) = text.split_once('|') {
                if let (Ok(ts), Ok(id)) = (ts_str.parse::<DateTime<Utc>>(), id_str.parse::<Uuid>())
                {
                    return Some(Cursor { ts, id });
                }
            }
        }
        // Legacy: a bare RFC3339 timestamp. `nil` id reproduces the old
        // single-column `created_at < $cursor` semantics exactly.
        raw.parse::<DateTime<Utc>>().ok().map(|ts| Cursor {
            ts,
            id: Uuid::nil(),
        })
    }
}

impl EncodeCursor for Cursor {
    /// `base64url("<rfc3339>|<uuid>")`.
    fn encode(&self) -> String {
        b64_encode(&format!("{}|{}", self.ts.to_rfc3339(), self.id))
    }
}

/// Keyset cursor for lists ranked by a mutable integer metric (e.g.
/// battle claims sorted by verify/refute count). The position is
/// `(rank, ts, id)`: the metric first, `created_at` then `id` as
/// deterministic tie-breakers. Because `rank` is mutable, pagination is
/// best-effort across concurrent writes — stable enough for browsing,
/// not a transactional snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankCursor {
    pub rank: i64,
    pub ts: DateTime<Utc>,
    pub id: Uuid,
}

impl RankCursor {
    pub fn new(rank: i64, ts: DateTime<Utc>, id: Uuid) -> Self {
        Self { rank, ts, id }
    }

    pub fn decode(raw: &str) -> Option<RankCursor> {
        let text = b64_decode(raw)?;
        let mut parts = text.splitn(3, '|');
        let rank = parts.next()?.parse::<i64>().ok()?;
        let ts = parts.next()?.parse::<DateTime<Utc>>().ok()?;
        let id = parts.next()?.parse::<Uuid>().ok()?;
        Some(RankCursor { rank, ts, id })
    }
}

impl EncodeCursor for RankCursor {
    /// `base64url("<rank>|<rfc3339>|<uuid>")`.
    fn encode(&self) -> String {
        b64_encode(&format!(
            "{}|{}|{}",
            self.rank,
            self.ts.to_rfc3339(),
            self.id
        ))
    }
}

/// Decode an optional raw cursor query param into a [`Cursor`].
///
/// Returns `None` for both "no cursor supplied" and "cursor failed to
/// parse" — an unparseable cursor degrades to "first page" rather than
/// erroring, matching the previous `cursor.and_then(parse)` behaviour.
pub fn decode_cursor(raw: Option<&str>) -> Option<Cursor> {
    raw.and_then(Cursor::decode)
}

/// [`decode_cursor`] for the integer-ranked variant.
pub fn decode_rank_cursor(raw: Option<&str>) -> Option<RankCursor> {
    raw.and_then(RankCursor::decode)
}

/// Finalise a keyset page.
///
/// Callers fetch `limit + 1` rows; the extra row is the "is there a
/// next page?" probe. This trims the probe, computes `has_more`, and
/// derives `next_cursor` from the last *returned* row via `cursor_of`.
///
/// ```ignore
/// let rows = sqlx::query_as(...).bind(limit + 1).fetch_all(pool).await?;
/// let (items, pagination) =
///     finalize_page(rows, limit, |r| Cursor::new(r.created_at, r.id));
/// ```
pub fn finalize_page<T, C: EncodeCursor>(
    mut rows: Vec<T>,
    limit: i64,
    cursor_of: impl Fn(&T) -> C,
) -> (Vec<T>, PaginationMeta) {
    let limit = limit.max(0) as usize;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        rows.last().map(|r| cursor_of(r).encode())
    } else {
        None
    };
    (
        rows,
        PaginationMeta {
            next_cursor,
            has_more,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse::<DateTime<Utc>>().unwrap()
    }

    #[test]
    fn opaque_round_trip() {
        let c = Cursor::new(ts("2024-01-02T03:04:05.123456Z"), Uuid::from_u128(42));
        let decoded = Cursor::decode(&c.encode()).expect("decodes");
        assert_eq!(decoded, c);
    }

    #[test]
    fn legacy_rfc3339_decodes_with_nil_id() {
        let decoded = Cursor::decode("2024-01-02T03:04:05Z").expect("legacy decodes");
        assert_eq!(decoded.ts, ts("2024-01-02T03:04:05Z"));
        assert_eq!(decoded.id, Uuid::nil());
    }

    #[test]
    fn garbage_cursor_is_none() {
        assert!(Cursor::decode("not-a-cursor!!").is_none());
        assert!(decode_cursor(None).is_none());
    }

    #[test]
    fn finalize_trims_probe_and_sets_cursor() {
        // limit 2, fetched 3 (probe present) → has_more, cursor from row 2.
        let rows = vec![
            (ts("2024-01-03T00:00:00Z"), Uuid::from_u128(3)),
            (ts("2024-01-02T00:00:00Z"), Uuid::from_u128(2)),
            (ts("2024-01-01T00:00:00Z"), Uuid::from_u128(1)),
        ];
        let (items, page) = finalize_page(rows, 2, |(t, i)| Cursor::new(*t, *i));
        assert_eq!(items.len(), 2);
        assert!(page.has_more);
        let cursor = Cursor::decode(&page.next_cursor.unwrap()).unwrap();
        assert_eq!(cursor.id, Uuid::from_u128(2));
    }

    #[test]
    fn finalize_no_probe_means_last_page() {
        let rows = vec![(ts("2024-01-01T00:00:00Z"), Uuid::from_u128(1))];
        let (items, page) = finalize_page(rows, 2, |(t, i)| Cursor::new(*t, *i));
        assert_eq!(items.len(), 1);
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }
}
