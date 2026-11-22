pub mod schema_check;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

tokio::task_local! {
    /// Per-request DB-query counter, scoped by the
    /// `count_db_queries_layer` tower middleware. Each
    /// `pool.acquire()` (which precedes every sqlx query) bumps it
    /// in the `before_acquire` hook below. Reads at request end
    /// give us the exact query count for that request.
    ///
    /// `Arc<AtomicU64>` (not just `AtomicU64`) because
    /// `task_local!::scope` takes ownership of the value but we
    /// need the middleware to read it after the inner future
    /// completes. The Arc lets both the scope and the middleware
    /// hold a handle to the same counter.
    pub static DB_QUERY_COUNT: Arc<AtomicU64>;
}

pub const PROXY_POOL_MAX_CONNECTIONS: u32 = 6;

pub async fn create_proxy_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(PROXY_POOL_MAX_CONNECTIONS)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(database_url)
        .await
}

pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(20)
        // Counts every connection acquire. sqlx fires one `acquire`
        // per query (the `Executor` impl on `&PgPool` always
        // checks out a connection before executing), so the count
        // equals the per-request query count. `try_with` is a
        // no-op when the task-local isn't set — happens for any
        // sqlx call outside an HTTP request (startup, background
        // tasks). Hot path; we avoid the closure boxing tax by
        // keeping the body trivial.
        .before_acquire(|_conn, _meta| {
            Box::pin(async move {
                let _ = DB_QUERY_COUNT.try_with(|c| {
                    c.fetch_add(1, Ordering::Relaxed);
                });
                Ok(true)
            })
        })
        // Widen the HNSW search beam once per connection. The default
        // `hnsw.ef_search` (40) is the candidate budget pgvector
        // explores per query; hybrid search post-filters those results
        // (`deleted_at IS NULL`, superseded claims), so a tight beam can
        // starve the result set below the requested limit. 100 restores
        // recall headroom at a negligible latency cost; harmless for
        // every non-vector query on the connection.
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute("SET hnsw.ef_search = 100").await?;
                Ok(())
            })
        })
        .connect(database_url)
        .await
}

/// Rewrite a Neon connection string to target the *direct*
/// (non-pooled) compute endpoint by stripping the `-pooler` suffix
/// from the host. Neon's pooler host is `ep-<id>-pooler.<region>...`;
/// the direct host is the same without `-pooler`. For any URL that
/// doesn't contain `-pooler` (local Postgres, the docker-compose db,
/// a direct Neon URL) this is a no-op and the string is returned
/// unchanged.
fn direct_endpoint_url(database_url: &str) -> String {
    // `-pooler` only ever appears in the Neon host segment; the
    // username (`neondb_owner`) and password (`npg_…`) never contain
    // it, so a plain replace is safe.
    database_url.replace("-pooler", "")
}

/// Apply every `src/db/migrations/NNN_*.sql` that hasn't already
/// been applied. Off by default; opt-in via the `SQLX_AUTO_MIGRATE`
/// env var in `main.rs`.
///
/// Prerequisite: `_sqlx_migrations` must already record all
/// previously-applied migrations. `bootstrap_sqlx_migrations` does
/// that one-time backfill on legacy DBs that were originally
/// provisioned with `psql -f` and never had the tracking table.
///
/// Runs over a dedicated, single-use connection to Neon's **direct**
/// (non-pooler) endpoint. sqlx's migrator takes a session-level
/// `pg_advisory_lock`; over Neon's transaction-mode pooler that lock
/// is orphaned onto a backend the pooler later reuses for ordinary
/// queries, so it is never released and every subsequent migrate
/// (even a no-op) hangs forever waiting on it — which manifests as a
/// Cloud Run startup-probe timeout. A real direct session releases
/// the advisory lock on disconnect, so the lock cannot leak. Locking
/// stays enabled so concurrent instances still serialize correctly.
pub async fn run_migrations(database_url: &str) -> anyhow::Result<()> {
    let direct_url = direct_endpoint_url(database_url);
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&direct_url)
        .await?;
    let result = sqlx::migrate!("src/db/migrations").run(&pool).await;
    // Close explicitly so the migrator's connection (and its advisory
    // lock) is released immediately rather than at pool drop.
    pool.close().await;
    result.map_err(Into::into)
}

/// One-time backfill of `_sqlx_migrations` for databases that were
/// provisioned manually (via `psql -f`) before sqlx integration.
/// Idempotent — `ON CONFLICT DO NOTHING` on `version` makes it safe
/// to run repeatedly. Returns the number of rows actually inserted
/// (zero on subsequent runs).
///
/// After this returns `Ok`, set `SQLX_AUTO_MIGRATE=true` permanently
/// and `BOOTSTRAP_SQLX_MIGRATIONS` can be unset; `run_migrations`
/// will become a no-op until new files appear in the migrations dir.
///
/// **Warning.** sqlx verifies file checksums against the table on
/// every `run_migrations` call. If a migration file is edited after
/// bootstrap (even whitespace), startup will fail with `VersionMismatch`.
/// Treat applied migrations as immutable.
pub async fn bootstrap_sqlx_migrations(pool: &PgPool) -> Result<usize, sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS _sqlx_migrations (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL,
            execution_time BIGINT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    let migrator = sqlx::migrate!("src/db/migrations");
    let mut inserted = 0usize;
    for m in migrator.iter() {
        let result = sqlx::query(
            r#"
            INSERT INTO _sqlx_migrations
                (version, description, installed_on, success, checksum, execution_time)
            VALUES ($1, $2, NOW(), TRUE, $3, 0)
            ON CONFLICT (version) DO NOTHING
            "#,
        )
        .bind(m.version)
        .bind(m.description.as_ref())
        .bind(m.checksum.as_ref())
        .execute(pool)
        .await?;
        inserted += result.rows_affected() as usize;
    }
    Ok(inserted)
}
