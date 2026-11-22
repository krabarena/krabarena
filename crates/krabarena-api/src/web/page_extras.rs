use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::models::comment::{COMMENT_COLUMNS_WITH_PARENT, COMMENTS_FROM_WITH_PARENT, CommentRow};
use crate::web::comment_threads::{CommentView, build_comment_views};

const BATTLE_PAGE_COMMENT_LIMIT: i64 = 200;
const SUCCESSOR_MAX_DEPTH: i32 = 50;

#[derive(Debug, Clone)]
pub struct LinkedBattle {
    pub href: String,
    pub title: String,
    pub status: String,
}

#[derive(Debug, Clone, Default)]
pub struct BattleExtras {
    pub parent: Option<LinkedBattle>,
    pub sub_battles: Vec<LinkedBattle>,
    pub comment_threads: Vec<CommentView>,
}

fn battle_href(id: Uuid, slug: Option<&str>) -> String {
    let ident = slug
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string());
    format!("/battles/{ident}")
}

fn linked(row: (Uuid, Option<String>, String, String)) -> LinkedBattle {
    let (id, slug, title, status) = row;
    LinkedBattle {
        href: battle_href(id, slug.as_deref()),
        title,
        status,
    }
}

pub async fn load_battle_extras(
    pool: &PgPool,
    battle_id: Uuid,
    parent_id: Option<Uuid>,
) -> BattleExtras {
    let parent_fut = async {
        match parent_id {
            Some(pid) => sqlx::query_as::<_, (Uuid, Option<String>, String, String)>(
                "SELECT id, slug, title, status FROM battles WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(pid)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten(),
            None => None,
        }
    };
    let subs_fut = sqlx::query_as::<_, (Uuid, Option<String>, String, String)>(
        "SELECT id, slug, title, status FROM battles \
         WHERE parent_id = $1 AND deleted_at IS NULL \
         ORDER BY updated_at DESC, id DESC",
    )
    .bind(battle_id)
    .fetch_all(pool);
    let comments_sql = format!(
        "SELECT {COMMENT_COLUMNS_WITH_PARENT} FROM {COMMENTS_FROM_WITH_PARENT} \
         WHERE battle_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2"
    );
    let comments_fut = sqlx::query_as::<_, CommentRow>(&comments_sql)
        .bind(battle_id)
        .bind(BATTLE_PAGE_COMMENT_LIMIT)
        .fetch_all(pool);

    let (parent, subs, comments) = tokio::join!(parent_fut, subs_fut, comments_fut);
    let comments = comments.unwrap_or_default();

    BattleExtras {
        parent: parent.map(linked),
        sub_battles: subs.unwrap_or_default().into_iter().map(linked).collect(),
        comment_threads: comment_views(pool, &comments).await,
    }
}

async fn comment_views(pool: &PgPool, comments: &[CommentRow]) -> Vec<CommentView> {
    if comments.is_empty() {
        return Vec::new();
    }
    let mut user_ids: Vec<Uuid> = comments.iter().map(|c| c.author_id).collect();
    user_ids.sort();
    user_ids.dedup();
    let comment_ids: Vec<Uuid> = comments.iter().map(|c| c.id).collect();

    let users_fut = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
        "SELECT id, display_name, avatar_url FROM users WHERE id = ANY($1)",
    )
    .bind(&user_ids)
    .fetch_all(pool);
    let mentions_fut = crate::services::comment_service::load_mentions(pool, &comment_ids);
    let (users, mentions) = tokio::join!(users_fut, mentions_fut);

    let mut names: HashMap<String, String> = HashMap::new();
    let mut avatars: HashMap<String, String> = HashMap::new();
    for (id, name, avatar) in users.unwrap_or_default() {
        let key = id.to_string();
        names.insert(key.clone(), name);
        if let Some(url) = avatar.filter(|u| !u.trim().is_empty()) {
            avatars.insert(key, url);
        }
    }
    build_comment_views(comments, &mentions.unwrap_or_default(), &names, &avatars)
}

pub async fn load_claim_successor_href(pool: &PgPool, claim_id: Uuid) -> Option<String> {
    let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "WITH RECURSIVE chain AS ( \
             SELECT c.id, c.slug, c.created_at, 1 AS depth FROM claims c \
              WHERE c.predecessor_id = $1 AND c.deleted_at IS NULL \
                AND c.visibility IS DISTINCT FROM 'private' \
             UNION ALL \
             SELECT c.id, c.slug, c.created_at, chain.depth + 1 FROM claims c \
               JOIN chain ON c.predecessor_id = chain.id \
              WHERE c.deleted_at IS NULL AND c.visibility IS DISTINCT FROM 'private' \
                AND chain.depth < $2 \
         ) \
         SELECT id, slug FROM chain ORDER BY depth DESC, created_at DESC, id DESC LIMIT 1",
    )
    .bind(claim_id)
    .bind(SUCCESSOR_MAX_DEPTH)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    row.map(|(id, slug)| {
        let ident = slug
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| id.to_string());
        format!("/claims/{ident}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn test_pool() -> Option<PgPool> {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("SKIP: set TEST_DATABASE_URL for page extras DB tests");
            return None;
        };
        crate::db::run_migrations(&url)
            .await
            .expect("migrations failed against TEST_DATABASE_URL");
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .ok()
    }

    async fn seed_user(pool: &PgPool) -> Uuid {
        let suffix = Uuid::new_v4();
        let entity_id: Uuid = sqlx::query_scalar(
            "INSERT INTO entities (name, domain, verification_method)
             VALUES ($1, $2, 'email') RETURNING id",
        )
        .bind(format!("Extras {suffix}"))
        .bind(format!("extras-{suffix}.test"))
        .fetch_one(pool)
        .await
        .expect("insert entity");
        sqlx::query_scalar(
            "INSERT INTO users (entity_id, display_name, email, auth_method, role)
             VALUES ($1, $2, $3, 'email', 'user') RETURNING id",
        )
        .bind(entity_id)
        .bind(format!("Extras{}", &suffix.simple().to_string()[..8]))
        .bind(format!("extras-{suffix}@example.test"))
        .fetch_one(pool)
        .await
        .expect("insert user")
    }

    async fn seed_battle(pool: &PgPool, author: Uuid, parent: Option<Uuid>, title: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO battles (author_id, parent_id, title, description, domain_tags, status, slug)
             VALUES ($1, $2, $3, 'Disposable', ARRAY['extras'], 'open', $4) RETURNING id",
        )
        .bind(author)
        .bind(parent)
        .bind(title)
        .bind(format!("extras-battle-{}", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("insert battle")
    }

    async fn seed_claim(
        pool: &PgPool,
        author: Uuid,
        battle: Uuid,
        predecessor: Option<Uuid>,
        visibility: &str,
    ) -> (Uuid, String) {
        let suffix = Uuid::new_v4();
        let slug = format!("extras-claim-{suffix}");
        let id = sqlx::query_scalar(
            "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url,
                                 artifact_size, visibility, slug, topic_tags, predecessor_id)
             VALUES ($1, $2, '# t', $3, $4, 1, $5, $6, ARRAY[]::text[], $7) RETURNING id",
        )
        .bind(battle)
        .bind(author)
        .bind(format!("sha256:{suffix}"))
        .bind(format!("https://example.test/{suffix}.tar.gz"))
        .bind(visibility)
        .bind(&slug)
        .bind(predecessor)
        .fetch_one(pool)
        .await
        .expect("insert claim");
        (id, slug)
    }

    #[tokio::test]
    async fn successor_href_points_at_latest_public_version() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let author = seed_user(&pool).await;
        let battle = seed_battle(&pool, author, None, "Extras").await;
        let (v1, _) = seed_claim(&pool, author, battle, None, "public").await;
        assert_eq!(load_claim_successor_href(&pool, v1).await, None);
        let (v2, _) = seed_claim(&pool, author, battle, Some(v1), "public").await;
        let (v3, v3_slug) = seed_claim(&pool, author, battle, Some(v2), "public").await;
        assert_eq!(
            load_claim_successor_href(&pool, v1).await,
            Some(format!("/claims/{v3_slug}"))
        );
        assert_eq!(
            load_claim_successor_href(&pool, v2).await,
            Some(format!("/claims/{v3_slug}"))
        );
        assert_eq!(load_claim_successor_href(&pool, v3).await, None);
        seed_claim(&pool, author, battle, Some(v3), "private").await;
        assert_eq!(load_claim_successor_href(&pool, v3).await, None);
    }

    #[tokio::test]
    async fn battle_extras_include_parent_children_and_threaded_comments() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let author = seed_user(&pool).await;
        let parent = seed_battle(&pool, author, None, "Parent battle").await;
        let child = seed_battle(&pool, author, Some(parent), "Child battle").await;
        let root: Uuid = sqlx::query_scalar(
            "INSERT INTO comments (battle_id, author_id, body) VALUES ($1, $2, 'root') RETURNING id",
        )
        .bind(parent)
        .bind(author)
        .fetch_one(&pool)
        .await
        .expect("insert root comment");
        let reply: Uuid = sqlx::query_scalar(
            "INSERT INTO comments (battle_id, author_id, body) VALUES ($1, $2, 'reply') RETURNING id",
        )
        .bind(parent)
        .bind(author)
        .fetch_one(&pool)
        .await
        .expect("insert reply");
        sqlx::query("INSERT INTO comment_replies (comment_id, parent_id) VALUES ($1, $2)")
            .bind(reply)
            .bind(root)
            .execute(&pool)
            .await
            .expect("link reply");

        let extras = load_battle_extras(&pool, parent, None).await;
        assert!(extras.parent.is_none());
        assert_eq!(extras.sub_battles.len(), 1);
        assert_eq!(extras.sub_battles[0].title, "Child battle");
        let ids: Vec<Uuid> = extras.comment_threads.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![root, reply]);
        assert!(extras.comment_threads[1].is_reply);

        let child_extras = load_battle_extras(&pool, child, Some(parent)).await;
        let p = child_extras.parent.expect("parent link");
        assert_eq!(p.title, "Parent battle");
        assert!(p.href.starts_with("/battles/extras-battle-"));
        assert!(child_extras.comment_threads.is_empty());
    }
}
