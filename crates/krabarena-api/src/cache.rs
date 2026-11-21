//! Tiny in-process TTL cache for hot, expensive read-only aggregates.
//!
//! The leaderboards (`/agents`, the home top-10, `/arena`) each run a
//! full-table aggregate over `claims` / `verifications` / `claim_reviews`
//! on every page load. The output is bounded (top-N) but the *scan* cost
//! grows with the corpus, so under traffic the same heavy query runs
//! again and again for identical results. Memoising the result for a
//! short TTL collapses that to at most one recompute per key per window.
//!
//! Deliberately minimal: a `Mutex<HashMap>`, no background eviction
//! (stale entries are simply recomputed on next access and overwritten),
//! no size cap (the key space is tiny — a handful of `limit` values).
//! Not a substitute for denormalisation if the aggregate ever becomes
//! the dominant cost; it just removes the redundant repeats.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Cloneable values memoised under string keys with a fixed TTL.
pub struct TtlCache<V> {
    ttl: Duration,
    inner: Mutex<HashMap<String, (Instant, V)>>,
}

impl<V: Clone> TtlCache<V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Return the cached value for `key` if present and within TTL.
    pub fn get(&self, key: &str) -> Option<V> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(key)
            .and_then(|(stored_at, value)| (stored_at.elapsed() < self.ttl).then(|| value.clone()))
    }

    /// Insert / overwrite the cached value for `key`, stamping it now.
    pub fn put(&self, key: impl Into<String>, value: V) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(key.into(), (Instant::now(), value));
    }

    /// Return the cached value, or run `fill` to compute, store and
    /// return it. The fill runs outside the lock, so concurrent misses
    /// may each compute once (a thundering-herd we tolerate — the work
    /// is read-only and the window is short); the last writer wins.
    pub async fn get_or_fill<F, Fut>(&self, key: &str, fill: F) -> V
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = V>,
    {
        if let Some(value) = self.get(key) {
            return value;
        }
        let value = fill().await;
        self.put(key, value.clone());
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fills_on_miss_then_serves_from_cache() {
        let cache: TtlCache<i32> = TtlCache::new(Duration::from_secs(60));
        let a = cache.get_or_fill("k", || async { 1 }).await;
        // Second call must NOT recompute (would return 2) — it's cached.
        let b = cache.get_or_fill("k", || async { 2 }).await;
        assert_eq!(a, 1);
        assert_eq!(b, 1);
    }

    #[tokio::test]
    async fn expired_entry_recomputes() {
        let cache: TtlCache<i32> = TtlCache::new(Duration::from_millis(0));
        cache.put("k", 1);
        // TTL of 0 means any elapsed time is already expired.
        assert!(cache.get("k").is_none());
    }
}
