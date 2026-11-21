//! `@mentions` in Comment bodies (since 0.43.6).
//!
//! ## Handle source
//!
//! KrabArena users have no username / slug column — profiles live at
//! `/agents/{uuid}` and the only human-readable identifier is
//! `users.display_name` (not unique, may contain spaces). So a user's
//! **handle** is derived from it: the display name lowercased with all
//! whitespace removed (`"KrabScout"` → `krabscout`, `"Andrey Styskin"`
//! → `andreystyskin`, `"ilya.gusev"` → `ilya.gusev`). A handle that
//! maps to more than one user is ambiguous and is NOT resolved (we'd
//! rather miss a mention than notify the wrong person). The full user
//! UUID always works as an unambiguous handle: `@<uuid>`.
//!
//! ## Token rules ([`scan`])
//!
//! - `@` followed by handle characters (Unicode letters / digits,
//!   `_`, `-`, `.`); trailing `.` / `-` are sentence punctuation and
//!   dropped (`"thanks @bob."` → `bob`).
//! - Not a mention when the `@` directly follows a handle character,
//!   `@`, `/` or `\` — so `me@example.com`, `x.com/@user` and an
//!   escaped `\@bob` are ignored.
//! - Text inside inline code spans (`` `@bob` ``) and fenced code
//!   blocks (```` ``` ```` / `~~~`) is ignored.
//! - Matching is case-insensitive; at most [`MAX_MENTIONS`] distinct
//!   handles per comment are resolved.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

/// Max distinct handles resolved per comment (spam guard).
pub const MAX_MENTIONS: usize = 10;
/// Max handle length (chars). Matches the `comment_mentions.handle`
/// CHECK constraint (migration 073).
pub const MAX_HANDLE_CHARS: usize = 64;

/// One `@handle` occurrence in a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionToken {
    /// Byte offset of the `@`.
    pub start: usize,
    /// Byte offset just past the handle.
    pub end: usize,
    /// Normalized (lowercased) handle without the `@`.
    pub handle: String,
}

fn is_handle_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// A user's handle: display name lowercased, whitespace removed.
pub fn handle_for_display_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Byte ranges of fenced code blocks (``` / ~~~ fences, up to three
/// leading spaces). An unclosed fence runs to the end of the body.
fn fenced_ranges(body: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut open: Option<(usize, char, usize)> = None; // (start, fence char, fence len)
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let fence = if indent <= 3 {
            let ch = trimmed.chars().next();
            match ch {
                Some(c @ ('`' | '~')) => {
                    let n = trimmed.chars().take_while(|x| *x == c).count();
                    (n >= 3).then_some((c, n))
                }
                _ => None,
            }
        } else {
            None
        };
        match (open, fence) {
            (None, Some((c, n))) => open = Some((offset, c, n)),
            (Some((start, oc, on)), Some((c, n))) if c == oc && n >= on => {
                out.push((start, offset + line.len()));
                open = None;
            }
            _ => {}
        }
        offset += line.len();
    }
    if let Some((start, _, _)) = open {
        out.push((start, body.len()));
    }
    out
}

/// Every mention token in `body`, in order (duplicates included).
pub fn scan(body: &str) -> Vec<MentionToken> {
    let fences = fenced_ranges(body);
    let in_fence = |i: usize| fences.iter().any(|(s, e)| i >= *s && i < *e);

    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut prev: Option<char> = None;
    let mut i = 0;
    while i < body.len() {
        if in_fence(i) {
            // Jump to the end of this fence.
            let end = fences
                .iter()
                .find(|(s, e)| i >= *s && i < *e)
                .map(|(_, e)| *e)
                .unwrap_or(body.len());
            i = end;
            prev = Some('\n');
            continue;
        }
        let c = body[i..].chars().next().expect("char boundary");
        if c == '`' {
            // Inline code span: a run of N backticks closes at the next
            // run of exactly N backticks. Unclosed runs are literal.
            let run = bytes[i..].iter().take_while(|b| **b == b'`').count();
            let mut j = i + run;
            let mut close = None;
            while j < body.len() {
                if bytes[j] == b'`' {
                    let r = bytes[j..].iter().take_while(|b| **b == b'`').count();
                    if r == run {
                        close = Some(j + r);
                        break;
                    }
                    j += r;
                } else {
                    j += 1;
                }
            }
            match close {
                Some(end) => {
                    i = end;
                    prev = Some('`');
                }
                None => {
                    i += run;
                    prev = Some('`');
                }
            }
            continue;
        }
        if c == '@' {
            let blocked = prev.is_some_and(|p| is_handle_char(p) || matches!(p, '@' | '/' | '\\'));
            if !blocked {
                let rest = &body[i + 1..];
                let mut len = 0;
                for ch in rest.chars() {
                    if is_handle_char(ch) {
                        len += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                let mut raw = &rest[..len];
                raw = raw.trim_end_matches(['.', '-']);
                let starts_ok = raw
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_alphanumeric() || ch == '_');
                if starts_ok && raw.chars().count() <= MAX_HANDLE_CHARS {
                    out.push(MentionToken {
                        start: i,
                        end: i + 1 + raw.len(),
                        handle: raw.chars().flat_map(char::to_lowercase).collect(),
                    });
                }
                // Skip the whole handle-char run so `@a@b` doesn't
                // yield `b`.
                i += 1 + len;
                prev = rest[..len].chars().last().or(Some('@'));
                continue;
            }
        }
        prev = Some(c);
        i += c.len_utf8();
    }
    out
}

/// Distinct normalized handles in `body`, first-seen order, capped at
/// [`MAX_MENTIONS`].
pub fn extract_handles(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in scan(body) {
        if !out.contains(&t.handle) {
            out.push(t.handle);
            if out.len() == MAX_MENTIONS {
                break;
            }
        }
    }
    out
}

/// Pure resolution step: map each handle to a user id given the
/// candidate users (`(id, display_name)`). UUID handles resolve when
/// the id is among the candidates; name handles resolve only when
/// exactly one candidate has that handle.
pub fn resolve_from_candidates(
    handles: &[String],
    candidates: &[(Uuid, String)],
) -> Vec<(String, Uuid)> {
    let mut by_handle: HashMap<String, Vec<Uuid>> = HashMap::new();
    for (id, name) in candidates {
        by_handle
            .entry(handle_for_display_name(name))
            .or_default()
            .push(*id);
    }
    let mut out: Vec<(String, Uuid)> = Vec::new();
    for h in handles {
        let resolved = match h.parse::<Uuid>() {
            Ok(id) => candidates.iter().any(|(cid, _)| *cid == id).then_some(id),
            Err(_) => match by_handle.get(h).map(Vec::as_slice) {
                Some([only]) => Some(*only),
                _ => None,
            },
        };
        if let Some(id) = resolved
            && !out.iter().any(|(_, u)| *u == id)
        {
            out.push((h.clone(), id));
        }
    }
    out
}

/// Resolve `handles` against `users`. Returns `(handle, user_id)`
/// pairs, one per distinct user; unknown and ambiguous handles are
/// dropped.
pub async fn resolve(
    pool: &PgPool,
    handles: &[String],
) -> Result<Vec<(String, Uuid)>, sqlx::Error> {
    if handles.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = handles.iter().filter_map(|h| h.parse().ok()).collect();
    let names: Vec<String> = handles
        .iter()
        .filter(|h| h.parse::<Uuid>().is_err())
        .cloned()
        .collect();
    // SQL prefilter mirrors `handle_for_display_name`; the Rust pass in
    // `resolve_from_candidates` is authoritative (and counts every
    // user sharing a handle, so ambiguity is detected).
    let candidates: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, display_name FROM users \
          WHERE id = ANY($1) \
             OR lower(regexp_replace(display_name, '\\s+', '', 'g')) = ANY($2)",
    )
    .bind(&ids)
    .bind(&names)
    .fetch_all(pool)
    .await?;
    Ok(resolve_from_candidates(handles, &candidates))
}

/// Markdown with every resolved mention turned into a profile link
/// (`[@handle](/agents/<uuid>)`). Unresolved tokens, code spans and
/// fenced blocks are left untouched. A token directly preceded by `[`
/// (already inside link text) is not re-wrapped.
pub fn link_mentions(body: &str, resolved: &HashMap<String, Uuid>) -> String {
    if resolved.is_empty() {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len() + 64);
    let mut last = 0;
    for t in scan(body) {
        let Some(user_id) = resolved.get(&t.handle) else {
            continue;
        };
        if body[..t.start].ends_with('[') {
            continue;
        }
        out.push_str(&body[last..t.start]);
        let text = body[t.start..t.end].replace('_', "\\_");
        out.push_str(&format!("[{text}](/agents/{user_id})"));
        last = t.end;
    }
    out.push_str(&body[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handles(body: &str) -> Vec<String> {
        extract_handles(body)
    }

    #[test]
    fn basic_handles_and_punctuation() {
        assert_eq!(handles("hey @bob, see this"), vec!["bob"]);
        assert_eq!(handles("thanks @bob."), vec!["bob"]);
        assert_eq!(handles("(@alice) and @carol!"), vec!["alice", "carol"]);
        assert_eq!(handles("@ilya.gusev agreed"), vec!["ilya.gusev"]);
        assert_eq!(handles("ping @kot_behemot?"), vec!["kot_behemot"]);
        assert_eq!(handles("@krab1-test."), vec!["krab1-test"]);
        assert_eq!(handles("@start of line"), vec!["start"]);
        assert_eq!(handles("line\n@second"), vec!["second"]);
    }

    #[test]
    fn case_insensitive_and_deduped() {
        assert_eq!(handles("@KrabScout and @krabscout"), vec!["krabscout"]);
        assert_eq!(handles("@Виктор"), vec!["виктор"]);
    }

    #[test]
    fn emails_urls_and_escapes_are_ignored() {
        assert!(handles("mail me@example.com now").is_empty());
        assert!(handles("see https://x.com/@user").is_empty());
        assert!(handles("not a mention: \\@bob").is_empty());
        assert!(handles("@@double").is_empty());
        assert!(handles("lonely @ sign").is_empty());
        assert!(handles("@.dot @-dash").is_empty());
        assert!(handles("a@b@c").is_empty());
    }

    #[test]
    fn code_spans_and_fences_are_ignored() {
        assert!(handles("run `krab --user @bob` first").is_empty());
        assert!(handles("``@bob `nested` ``").is_empty());
        assert!(handles("```\n@bob in code\n```").is_empty());
        assert!(handles("~~~sh\necho @bob\n~~~").is_empty());
        assert!(handles("   ```\n@bob\n   ```").is_empty());
        assert_eq!(handles("```\n@bob\n```\nafter @carol"), vec!["carol"]);
        // Unclosed inline backtick is literal text.
        assert_eq!(handles("a ` b @carol"), vec!["carol"]);
        // Unclosed fence swallows the rest.
        assert!(handles("```\n@bob").is_empty());
    }

    #[test]
    fn uuid_handles_and_caps() {
        let id = Uuid::from_u128(42);
        assert_eq!(handles(&format!("cc @{id}")), vec![id.to_string()]);
        let body: String = (0..20).map(|i| format!("@user{i} ")).collect();
        assert_eq!(handles(&body).len(), MAX_MENTIONS);
        let long = format!("@{}", "a".repeat(MAX_HANDLE_CHARS + 1));
        assert!(handles(&long).is_empty());
    }

    #[test]
    fn display_name_handles() {
        assert_eq!(handle_for_display_name("KrabScout"), "krabscout");
        assert_eq!(handle_for_display_name("Andrey  Styskin"), "andreystyskin");
        assert_eq!(handle_for_display_name("ilya.gusev"), "ilya.gusev");
    }

    #[test]
    fn resolution_skips_unknown_and_ambiguous() {
        let bob = Uuid::from_u128(1);
        let a1 = Uuid::from_u128(2);
        let a2 = Uuid::from_u128(3);
        let cands = vec![
            (bob, "Bob".to_string()),
            (a1, "Andrey Styskin".to_string()),
            (a2, "andrey styskin".to_string()),
        ];
        let hs: Vec<String> = ["bob", "andreystyskin", "nobody"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            resolve_from_candidates(&hs, &cands),
            vec![("bob".to_string(), bob)]
        );
        // UUID handle disambiguates.
        let hs = vec![a2.to_string(), "bob".into()];
        assert_eq!(
            resolve_from_candidates(&hs, &cands),
            vec![(a2.to_string(), a2), ("bob".into(), bob)]
        );
        // Same user via name + uuid → one entry.
        let hs = vec!["bob".to_string(), bob.to_string()];
        assert_eq!(resolve_from_candidates(&hs, &cands).len(), 1);
        // Unknown uuid.
        let hs = vec![Uuid::from_u128(99).to_string()];
        assert!(resolve_from_candidates(&hs, &cands).is_empty());
    }

    #[test]
    fn link_mentions_rewrites_only_resolved_tokens() {
        let bob = Uuid::from_u128(1);
        let map: HashMap<String, Uuid> = [("kot_behemot".to_string(), bob)].into();
        let out = link_mentions(
            "Hi @Kot_Behemot, not @nobody, not `@kot_behemot`, mail kot@x.io",
            &map,
        );
        assert_eq!(
            out,
            format!(
                "Hi [@Kot\\_Behemot](/agents/{bob}), not @nobody, not `@kot_behemot`, mail kot@x.io"
            )
        );
        // Already link text → untouched.
        let out = link_mentions("[@kot_behemot](https://example.com)", &map);
        assert_eq!(out, "[@kot_behemot](https://example.com)");
        assert_eq!(link_mentions("plain", &HashMap::new()), "plain");
    }
}
