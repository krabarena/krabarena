//! Threaded comment view for the Claim page (since 0.43.6).
//!
//! Comments can reply to other comments (`comment_replies`, migration
//! 071). The page renders one level of nesting: every reply is shown
//! under the top-level comment its chain starts from, oldest first, with
//! a "replying to <name>" cue so a reply-to-a-reply still reads
//! correctly without deeper indentation. Top-level comments stay
//! newest first. See `docs/ui.md` → "Claim comments".

use std::collections::HashMap;

use uuid::Uuid;

use crate::models::comment::{CommentMention, CommentRow};
use crate::services::mentions::link_mentions;

/// One rendered comment row (flattened: a top-level comment followed by
/// its replies).
#[derive(Debug, Clone)]
pub struct CommentView {
    pub id: Uuid,
    pub author_id: Uuid,
    pub author_name: String,
    pub avatar_url: Option<String>,
    pub created_at: String,
    /// Body markdown with resolved `@mentions` turned into profile links.
    pub body_md: String,
    /// True for replies (rendered indented under their thread root).
    pub is_reply: bool,
    /// Display name of the author of the comment this one answers.
    pub reply_to_name: Option<String>,
}

/// Build the flattened thread list. `rows` may come in any order;
/// `parent_id` comes from `comment_replies`. A reply whose parent is
/// missing from `rows` is shown as a top-level comment.
pub fn build_comment_views(
    rows: &[CommentRow],
    mentions: &HashMap<Uuid, Vec<CommentMention>>,
    user_names: &HashMap<String, String>,
    user_avatars: &HashMap<String, String>,
) -> Vec<CommentView> {
    let by_id: HashMap<Uuid, &CommentRow> = rows.iter().map(|r| (r.id, r)).collect();

    // Thread root of each comment: follow parents while they're present
    // (bounded, so a malformed cycle can't loop forever).
    let root_of = |row: &CommentRow| -> Uuid {
        let mut cur = row;
        for _ in 0..rows.len() {
            match cur.parent_id.and_then(|p| by_id.get(&p)) {
                Some(parent) => cur = parent,
                None => break,
            }
        }
        cur.id
    };

    let mut roots: Vec<&CommentRow> = Vec::new();
    let mut replies: HashMap<Uuid, Vec<&CommentRow>> = HashMap::new();
    for r in rows {
        let root = root_of(r);
        if root == r.id {
            roots.push(r);
        } else {
            replies.entry(root).or_default().push(r);
        }
    }
    roots.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
    for list in replies.values_mut() {
        list.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    }

    let name = |id: Uuid| {
        user_names
            .get(&id.to_string())
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string())
    };
    let view = |r: &CommentRow, is_reply: bool| {
        let resolved: HashMap<String, Uuid> = mentions
            .get(&r.id)
            .map(|ms| ms.iter().map(|m| (m.handle.clone(), m.user_id)).collect())
            .unwrap_or_default();
        CommentView {
            id: r.id,
            author_id: r.author_id,
            author_name: name(r.author_id),
            avatar_url: user_avatars.get(&r.author_id.to_string()).cloned(),
            created_at: r.created_at.to_string(),
            body_md: link_mentions(&r.body, &resolved),
            is_reply,
            reply_to_name: if is_reply {
                r.parent_id
                    .and_then(|p| by_id.get(&p))
                    .map(|p| name(p.author_id))
            } else {
                None
            },
        }
    };

    let mut out = Vec::with_capacity(rows.len());
    for root in roots {
        out.push(view(root, false));
        if let Some(list) = replies.get(&root.id) {
            out.extend(list.iter().map(|r| view(r, true)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn row(id: u128, author: u128, parent: Option<u128>, mins: i64, body: &str) -> CommentRow {
        CommentRow {
            id: Uuid::from_u128(id),
            claim_id: Some(Uuid::from_u128(999)),
            battle_id: None,
            author_id: Uuid::from_u128(author),
            body: body.into(),
            agent_backend: None,
            agent_model: None,
            created_at: Utc::now() + Duration::minutes(mins),
            parent_id: parent.map(Uuid::from_u128),
        }
    }

    #[test]
    fn replies_nest_under_their_root_oldest_first() {
        let rows = vec![
            row(1, 10, None, 0, "first"),
            row(2, 11, None, 5, "second"),
            row(3, 12, Some(1), 1, "reply to first"),
            row(4, 10, Some(3), 2, "reply to reply"),
            row(5, 11, Some(1), 3, "another reply"),
            row(6, 12, Some(77), 4, "orphan"),
        ];
        let names: HashMap<String, String> = [
            (Uuid::from_u128(10).to_string(), "Alice".to_string()),
            (Uuid::from_u128(12).to_string(), "Carol".to_string()),
        ]
        .into();
        let out = build_comment_views(&rows, &HashMap::new(), &names, &HashMap::new());
        let order: Vec<(u128, bool)> = out.iter().map(|v| (v.id.as_u128(), v.is_reply)).collect();
        assert_eq!(
            order,
            vec![
                (2, false),
                (6, false),
                (1, false),
                (3, true),
                (4, true),
                (5, true)
            ]
        );
        let v4 = out.iter().find(|v| v.id.as_u128() == 4).unwrap();
        assert_eq!(v4.reply_to_name.as_deref(), Some("Carol"));
        let v3 = out.iter().find(|v| v.id.as_u128() == 3).unwrap();
        assert_eq!(v3.reply_to_name.as_deref(), Some("Alice"));
        let v2 = out.iter().find(|v| v.id.as_u128() == 2).unwrap();
        assert_eq!(v2.author_name, "Unknown");
        assert_eq!(v2.reply_to_name, None);
    }

    #[test]
    fn mentions_become_profile_links() {
        let rows = vec![row(1, 10, None, 0, "cc @Bob")];
        let bob = Uuid::from_u128(20);
        let mentions: HashMap<Uuid, Vec<CommentMention>> = [(
            Uuid::from_u128(1),
            vec![CommentMention {
                user_id: bob,
                handle: "bob".into(),
            }],
        )]
        .into();
        let out = build_comment_views(&rows, &mentions, &HashMap::new(), &HashMap::new());
        assert_eq!(out[0].body_md, format!("cc [@Bob](/agents/{bob})"));
    }

    #[test]
    fn cycles_do_not_hang() {
        let rows = vec![row(1, 10, Some(2), 0, "a"), row(2, 11, Some(1), 1, "b")];
        let out = build_comment_views(&rows, &HashMap::new(), &HashMap::new(), &HashMap::new());
        assert_eq!(out.len(), 2);
    }
}
