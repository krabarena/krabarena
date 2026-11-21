//! Background indexer for the hybrid search feature.
//!
//! Two surfaces:
//!
//!   - [`startup_backfill`] — invoked once from `main.rs` at server
//!     boot. Picks up to `MAX_PER_STARTUP` rows in each table where
//!     `search_embedding IS NULL` and fills them. Capped so a cold
//!     start against a freshly-migrated DB doesn't stall on first
//!     boot; subsequent boots whittle the backlog down further.
//!
//!   - [`spawn_index_battle`] / [`spawn_index_claim`] /
//!     [`spawn_index_user`] — fire-and-forget refresh hooks called
//!     from the write path. Spawned via `tokio::spawn` so the write
//!     handler isn't blocked on the Gemini round-trip; logs and
//!     drops on failure so a flaky upstream never wedges a POST.

use sqlx::PgPool;
use uuid::Uuid;

use crate::services::embedding_service::{EmbeddingClient, MAX_INPUT_CHARS, global};

/// Cap on per-startup work — Cloud Run rolls fresh revisions for
/// every deploy, so we'd rather a quick boot + gradual catchup than
/// a 60s startup waiting on Gemini.
const MAX_PER_STARTUP: i64 = 100;

/// Top-level entry point. Walks each table sequentially; one
/// straggling failure on one kind shouldn't gate the others. Logs
/// aggregate counts so an operator can verify the backfill is
/// making progress across deploys.
pub async fn startup_backfill(pool: PgPool) {
    let client = global();
    let battles = backfill_battles(&pool, client).await;
    let claims = backfill_claims(&pool, client).await;
    let users = backfill_users(&pool, client).await;
    tracing::info!(
        provider = %client.name(),
        battles_indexed = battles,
        claims_indexed = claims,
        users_indexed = users,
        "search.backfill.complete"
    );
}

async fn backfill_battles(pool: &PgPool, client: &EmbeddingClient) -> usize {
    let rows: Vec<(Uuid, String, String)> = match sqlx::query_as(
        "SELECT id, title, description FROM battles \
         WHERE search_embedding IS NULL \
         ORDER BY created_at DESC \
         LIMIT $1",
    )
    .bind(MAX_PER_STARTUP)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "search.backfill.battles.fetch_failed");
            return 0;
        }
    };
    let mut n = 0;
    for (id, title, description) in rows {
        let text = battle_index_text(&title, &description);
        if let Err(e) = index_battle(pool, client, id, &text).await {
            tracing::warn!(battle_id = %id, error = %e, "search.backfill.battle.failed");
            continue;
        }
        n += 1;
    }
    n
}

async fn backfill_claims(pool: &PgPool, client: &EmbeddingClient) -> usize {
    let rows: Vec<(Uuid, String)> = match sqlx::query_as(
        "SELECT id, claim FROM claims \
         WHERE search_embedding IS NULL \
         ORDER BY created_at DESC \
         LIMIT $1",
    )
    .bind(MAX_PER_STARTUP)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "search.backfill.claims.fetch_failed");
            return 0;
        }
    };
    let mut n = 0;
    for (id, body) in rows {
        let text = claim_index_text(&body);
        if let Err(e) = index_claim(pool, client, id, &text).await {
            tracing::warn!(claim_id = %id, error = %e, "search.backfill.claim.failed");
            continue;
        }
        n += 1;
    }
    n
}

async fn backfill_users(pool: &PgPool, client: &EmbeddingClient) -> usize {
    let rows: Vec<(Uuid, String, Option<String>)> = match sqlx::query_as(
        "SELECT id, display_name, email FROM users \
         WHERE search_embedding IS NULL \
         ORDER BY created_at DESC \
         LIMIT $1",
    )
    .bind(MAX_PER_STARTUP)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "search.backfill.users.fetch_failed");
            return 0;
        }
    };
    let mut n = 0;
    for (id, display_name, email) in rows {
        let text = user_index_text(&display_name, email.as_deref());
        if let Err(e) = index_user(pool, client, id, &text).await {
            tracing::warn!(user_id = %id, error = %e, "search.backfill.user.failed");
            continue;
        }
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// Per-row indexers
// ---------------------------------------------------------------------------

pub async fn index_battle(
    pool: &PgPool,
    client: &EmbeddingClient,
    id: Uuid,
    text: &str,
) -> Result<(), anyhow::Error> {
    let v = client.embed_document(text).await?;
    sqlx::query("UPDATE battles SET search_embedding = $1 WHERE id = $2")
        .bind(pgvector::Vector::from(v))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn index_claim(
    pool: &PgPool,
    client: &EmbeddingClient,
    id: Uuid,
    text: &str,
) -> Result<(), anyhow::Error> {
    let v = client.embed_document(text).await?;
    sqlx::query("UPDATE claims SET search_embedding = $1 WHERE id = $2")
        .bind(pgvector::Vector::from(v))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn index_user(
    pool: &PgPool,
    client: &EmbeddingClient,
    id: Uuid,
    text: &str,
) -> Result<(), anyhow::Error> {
    let v = client.embed_document(text).await?;
    sqlx::query("UPDATE users SET search_embedding = $1 WHERE id = $2")
        .bind(pgvector::Vector::from(v))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Fire-and-forget spawners for the write path. The call-site stays
// fast; reindex happens in the background.
// ---------------------------------------------------------------------------

pub fn spawn_index_battle(pool: PgPool, id: Uuid, title: String, description: String) {
    let text = battle_index_text(&title, &description);
    tokio::spawn(async move {
        if let Err(e) = index_battle(&pool, global(), id, &text).await {
            tracing::warn!(battle_id = %id, error = %e, "search.index.battle.failed");
        }
    });
}

pub fn spawn_index_claim(pool: PgPool, id: Uuid, body: String) {
    let text = claim_index_text(&body);
    tokio::spawn(async move {
        if let Err(e) = index_claim(&pool, global(), id, &text).await {
            tracing::warn!(claim_id = %id, error = %e, "search.index.claim.failed");
        }
    });
}

pub fn spawn_index_user(pool: PgPool, id: Uuid, display_name: String, email: Option<String>) {
    let text = user_index_text(&display_name, email.as_deref());
    tokio::spawn(async move {
        if let Err(e) = index_user(&pool, global(), id, &text).await {
            tracing::warn!(user_id = %id, error = %e, "search.index.user.failed");
        }
    });
}

// ---------------------------------------------------------------------------
// Indexing text rules — kept central so the backfill + write-path
// hooks agree on what text the column represents. If the rules
// change, every existing row needs to be re-embedded — the simplest
// rollout is to drop the column to NULL and let `startup_backfill`
// repopulate over the next few deploys.
// ---------------------------------------------------------------------------

// Each index text is prefixed with a `Kind:` tag (Battle / Claim /
// Person). GEPA found this kind-tag, paired with the query prompt that
// names the same kinds + asymmetric task types, was the winning config
// (MRR@10 0.846 → 0.900). The tag gives the document embedding an
// explicit kind signal the short query can latch onto. If the tag
// wording changes, every row must be re-embedded — the simplest
// rollout is to NULL the column and let `startup_backfill` repopulate
// (that's exactly what migration 052 does for the GEPA cutover).

pub fn battle_index_text(title: &str, description: &str) -> String {
    format!("Battle: {}\n{}", title.trim(), description.trim())
}

pub fn claim_index_text(body: &str) -> String {
    // Claims include `# Title / ## Summary / ## Methodology /
    // ## Conclusion` already structured in the markdown body, so the
    // leading 4000 chars covers the high-signal sections without
    // dragging the embedding into low-signal logs at the tail.
    let trimmed = body.trim();
    let head: String = if trimmed.chars().count() <= 4000 {
        trimmed.to_string()
    } else {
        trimmed.chars().take(4000).collect()
    };
    format!("Claim: {head}")
}

pub fn user_index_text(display_name: &str, email: Option<&str>) -> String {
    // The `users` table has no `bio` column today (see migration
    // 001 / 042 / 043 — only display_name + email + role). We index
    // the display_name + email domain so a query like "openai
    // engineer" can still surface a user whose email is
    // `eng@openai.com`. Truncate to embedding window just in case.
    let domain = email
        .and_then(|e| e.rsplit_once('@').map(|(_, d)| d))
        .unwrap_or("");
    let mut out = format!("Person: {}", display_name.trim());
    if !domain.is_empty() {
        out.push('\n');
        out.push_str(domain);
    }
    if out.chars().count() > MAX_INPUT_CHARS {
        out = out.chars().take(MAX_INPUT_CHARS).collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battle_index_text_tags_kind_and_concats() {
        assert_eq!(battle_index_text("T", "D"), "Battle: T\nD");
        assert_eq!(battle_index_text("  T  ", "  D  "), "Battle: T\nD");
    }

    #[test]
    fn claim_index_text_tags_kind_and_truncates_body_at_4000() {
        let body = "x".repeat(5000);
        let out = claim_index_text(&body);
        // 4000 body chars + the "Claim: " tag.
        assert!(out.starts_with("Claim: "));
        assert_eq!(out.chars().count(), 4000 + "Claim: ".chars().count());
    }

    #[test]
    fn user_index_text_tags_kind_and_appends_email_domain() {
        let out = user_index_text("Alice", Some("alice@example.com"));
        assert_eq!(out, "Person: Alice\nexample.com");
        let out = user_index_text("Bob", None);
        assert_eq!(out, "Person: Bob");
    }
}
