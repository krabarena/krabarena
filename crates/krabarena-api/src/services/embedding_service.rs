//! Pluggable embedding provider for the hybrid search feature.
//!
//! 0.19.3 swapped from OpenAI `text-embedding-3-small` (1536-dim) to
//! Gemini `gemini-embedding-001` (768-dim via matryoshka truncation)
//! to consolidate on a single provider — claim dedup already used
//! Gemini at 768 dims (migration 023), so the search column shape
//! now matches and there's only one upstream API to budget against.
//! Migration 051 dropped + readded the `search_embedding` columns at
//! `vector(768)` to match.
//!
//! Two impls:
//!
//!   · [`GeminiProvider`] — calls `gemini-embedding-001:embedContent`
//!     via the existing `GeminiClient::embed_text` path. Selected when
//!     `GEMINI_API_KEY` is set (the env var the rest of the codebase
//!     already reads).
//!   · [`HashFallbackProvider`] — deterministic SHA-256-derived
//!     768-dim vector. Selected when no API key is configured (CI,
//!     local dev without secrets) so the column shape stays stable
//!     and tests don't depend on network.
//!
//! Both providers return a unit-normalised vector so cosine similarity
//! (`1 - (a <=> b)` in pgvector) lives in `[-1, 1]` with `1.0` =
//! identical direction.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::Client;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::services::gemini_audit::{self, status};

/// Upstream model id — kept as a const so the URL and the audit-log
/// `model` column never drift apart.
const GEMINI_EMBED_MODEL: &str = "gemini-embedding-001";

/// `gemini_calls.purpose` tag for search embeddings, so the audit table
/// can tell them apart from review/judge calls.
const AUDIT_PURPOSE: &str = "search_embedding";

/// Dimensionality of every embedding produced by this module.
///
/// Pinned to match the `vector(768)` columns added in migration 051.
/// Matches Gemini `gemini-embedding-001`'s matryoshka truncation
/// dimension used by the existing dedup pre-check (migration 023). If
/// we ever swap to a bigger dim (e.g. 1536 or 3072), we'd add a fresh
/// column rather than mutate this constant — pgvector won't accept a
/// dim mismatch on insert.
pub const EMBEDDING_DIM: usize = 768;

/// Maximum characters of input text we hand to the embedding model.
///
/// `gemini-embedding-001` accepts up to 2048 tokens — ~8000 chars in
/// practice. We cap at 8000 chars before sending so a claim body plus
/// title plus a few section headers fits without paying tokenisation
/// cost client-side.
pub const MAX_INPUT_CHARS: usize = 8000;

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("gemini request failed: {0}")]
    Request(String),
    #[error("gemini response parse error: {0}")]
    Parse(String),
    #[error("gemini returned non-success status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("input text was empty after normalisation")]
    EmptyInput,
}

/// Gemini `embedContent` task type. Asymmetric retrieval — documents
/// embedded as `RETRIEVAL_DOCUMENT`, queries as `RETRIEVAL_QUERY` —
/// is the single biggest relevance lever (GEPA round-1: pairing the
/// task types with the kind-tagged docs + topic-suffixed query lifted
/// MRR@10 from 0.846 → 0.900 on the 20-query grounded eval). Plain
/// symmetric embedding compresses short queries like "web search"
/// into the same neighbourhood as every benchmark claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskType {
    Query,
    Document,
}

impl TaskType {
    fn as_gemini(self) -> &'static str {
        match self {
            Self::Query => "RETRIEVAL_QUERY",
            Self::Document => "RETRIEVAL_DOCUMENT",
        }
    }
}

/// Query-side prompt template — appended to the raw user query before
/// embedding. GEPA round-2 found a short suffix that *names the entity
/// kinds* beats both the bare query (0.838) and a long instruction
/// prefix (0.742); `topic`/`topic_short`/`topic_bench` all tied at
/// 0.900. Kept minimal so it doesn't dominate the user's actual terms.
pub fn render_query_prompt(raw: &str) -> String {
    format!(
        "{}\nFind benchmarks, battles, claims, or people about this topic.",
        raw.trim()
    )
}

pub trait EmbeddingProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Embed `text` under `task` and return a unit-normalised vector
    /// of [`EMBEDDING_DIM`] floats. Implementations must truncate to
    /// [`MAX_INPUT_CHARS`] characters before sending.
    fn embed<'a>(
        &'a self,
        text: &'a str,
        task: TaskType,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, EmbeddingError>> + Send + 'a>,
    >;
}

/// Concrete provider selected at startup. An enum (rather than
/// `Arc<dyn EmbeddingProvider>`) keeps trait-object boxing out of the
/// hot read path and lets the type-checker enforce both arms exist.
#[derive(Clone)]
pub enum EmbeddingClient {
    Gemini(GeminiProvider),
    HashFallback(HashFallbackProvider),
}

impl EmbeddingClient {
    /// Build the right provider based on the `GEMINI_API_KEY` env var.
    /// Logs once on construction so an ops engineer can verify the
    /// choice from a single startup line.
    pub fn from_env() -> Self {
        match std::env::var("GEMINI_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            Some(key) => {
                tracing::info!(provider = "gemini", "embedding.provider.configured");
                Self::Gemini(GeminiProvider::new(key))
            }
            None => {
                tracing::warn!(
                    provider = "hash-fallback",
                    "embedding.provider.configured.fallback — GEMINI_API_KEY unset; \
                     hybrid search will run with deterministic hash vectors and \
                     produce noise-only similarity scores."
                );
                Self::HashFallback(HashFallbackProvider)
            }
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Gemini(p) => p.name(),
            Self::HashFallback(p) => p.name(),
        }
    }

    /// `true` when a real LLM-backed embedding is being produced. The
    /// search route uses this to surface a `provider` field in the
    /// JSON response so callers can tell live results from fallback.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Gemini(_))
    }

    /// Embed a stored document (battle/claim/user index text). Caller
    /// is responsible for the kind-tag prefix (see
    /// `services::search_index`); this just pins the task type.
    pub async fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        match self {
            Self::Gemini(p) => p.embed(text, TaskType::Document).await,
            Self::HashFallback(p) => p.embed(text, TaskType::Document).await,
        }
    }

    /// Embed a raw user query. Applies the GEPA-tuned query prompt
    /// template + `RETRIEVAL_QUERY` task type. Pass the user's raw
    /// query — do NOT pre-wrap it.
    ///
    /// Live (Gemini) query embeddings are cached by the raw query text
    /// so a repeated search — the common case for a busy box — doesn't
    /// re-hit the upstream API. The hash fallback is cheap enough that
    /// caching it would only waste memory, so it's computed directly.
    pub async fn embed_query(&self, raw_query: &str) -> Result<Vec<f32>, EmbeddingError> {
        let prompted = render_query_prompt(raw_query);
        match self {
            Self::Gemini(p) => {
                if let Some(hit) = query_cache().lock().ok().and_then(|c| c.get(raw_query)) {
                    return Ok(hit);
                }
                let v = p.embed(&prompted, TaskType::Query).await?;
                if let Ok(mut cache) = query_cache().lock() {
                    cache.put(raw_query.to_string(), v.clone());
                }
                Ok(v)
            }
            Self::HashFallback(p) => p.embed(&prompted, TaskType::Query).await,
        }
    }
}

/// Bounded FIFO cache of live query embeddings, keyed by raw query
/// text. FIFO (not strict LRU) keeps it dependency-free; for a query
/// box the hit pattern is dominated by a small set of popular terms, so
/// eviction order barely matters at this capacity.
const QUERY_CACHE_CAP: usize = 512;

struct QueryCache {
    map: HashMap<String, Vec<f32>>,
    order: VecDeque<String>,
}

impl QueryCache {
    fn get(&self, key: &str) -> Option<Vec<f32>> {
        self.map.get(key).cloned()
    }

    fn put(&mut self, key: String, value: Vec<f32>) {
        if self.map.contains_key(&key) {
            return;
        }
        if self.order.len() >= QUERY_CACHE_CAP {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }
}

fn query_cache() -> &'static Mutex<QueryCache> {
    static CACHE: OnceLock<Mutex<QueryCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(QueryCache {
            map: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

/// Default for tests + AppState defaults. Always picks the hash
/// fallback so no test inadvertently hits Gemini.
impl Default for EmbeddingClient {
    fn default() -> Self {
        Self::HashFallback(HashFallbackProvider)
    }
}

// ---------------------------------------------------------------------------
// Gemini provider
// ---------------------------------------------------------------------------

/// Calls `gemini-embedding-001:embedContent` with
/// `outputDimensionality: 768`. Reuses a single `reqwest::Client`
/// across calls so the TLS pool stays warm — embedding throughput on
/// startup backfill can spike to ~30 req/s otherwise (pool warmup
/// dominates).
///
/// Independent of `GeminiClient` in `gemini_service.rs` because that
/// client is constructed once and held in `AppState`, but the search
/// embedding path uses a process-singleton (`global()` below) to avoid
/// threading state through the write-path indexer and the startup
/// backfill task. Both paths hit the same upstream endpoint; the
/// duplication is a small `reqwest::Client` + API key string, not
/// worth the lifetime gymnastics.
#[derive(Clone)]
pub struct GeminiProvider {
    http: Client,
    api_key: String,
}

impl GeminiProvider {
    pub fn new(api_key: String) -> Self {
        // 5s timeout — search sits in the request hot path, so a slow
        // upstream must fail fast and let the caller degrade to
        // lexical-only rather than hang the whole search.
        let http = Client::builder()
            .timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { http, api_key }
    }

    async fn embed_inner(&self, text: &str, task: TaskType) -> Result<Vec<f32>, EmbeddingError> {
        let trimmed = truncate_for_embedding(text);
        if trimmed.is_empty() {
            return Err(EmbeddingError::EmptyInput);
        }
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{GEMINI_EMBED_MODEL}:embedContent?key={}",
            self.api_key
        );
        let body = serde_json::json!({
            "model": format!("models/{GEMINI_EMBED_MODEL}"),
            "content": { "parts": [{ "text": trimmed }] },
            "taskType": task.as_gemini(),
            "outputDimensionality": EMBEDDING_DIM,
        });
        // Audit every outbound call to `gemini_calls` so search
        // embedding cost + error rate are observable (the search path
        // was previously invisible there — only review calls logged).
        // Single attempt: this client doesn't retry; the caller
        // degrades to lexical-only on failure.
        let request_id = Uuid::new_v4();
        let started = Instant::now();
        let resp = match self.http.post(&url).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                gemini_audit::record(
                    request_id,
                    1,
                    AUDIT_PURPOSE,
                    GEMINI_EMBED_MODEL.to_string(),
                    status::TRANSPORT_ERROR,
                    None,
                    Some("transport"),
                    Some(e.to_string()),
                    started.elapsed().as_millis() as i32,
                );
                return Err(EmbeddingError::Request(e.to_string()));
            }
        };
        let http_status = resp.status();
        let latency_ms = started.elapsed().as_millis() as i32;
        if !http_status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            gemini_audit::record(
                request_id,
                1,
                AUDIT_PURPOSE,
                GEMINI_EMBED_MODEL.to_string(),
                status::NON_2XX_TERMINAL,
                Some(http_status.as_u16() as i32),
                Some("non_2xx"),
                Some(body.clone()),
                latency_ms,
            );
            return Err(EmbeddingError::Status {
                status: http_status.as_u16(),
                body,
            });
        }
        gemini_audit::record(
            request_id,
            1,
            AUDIT_PURPOSE,
            GEMINI_EMBED_MODEL.to_string(),
            status::SUCCESS,
            Some(http_status.as_u16() as i32),
            None,
            None,
            latency_ms,
        );
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| EmbeddingError::Parse(e.to_string()))?;
        let values = json
            .get("embedding")
            .and_then(|e| e.get("values"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| EmbeddingError::Parse("missing embedding.values".into()))?;
        let mut out = Vec::with_capacity(values.len());
        for v in values {
            let f = v
                .as_f64()
                .ok_or_else(|| EmbeddingError::Parse("non-float in embedding values".into()))?;
            out.push(f as f32);
        }
        if out.len() != EMBEDDING_DIM {
            return Err(EmbeddingError::Parse(format!(
                "expected {} dims, got {}",
                EMBEDDING_DIM,
                out.len()
            )));
        }
        Ok(unit_normalise(out))
    }
}

impl EmbeddingProvider for GeminiProvider {
    fn name(&self) -> &'static str {
        "gemini"
    }
    fn embed<'a>(
        &'a self,
        text: &'a str,
        task: TaskType,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, EmbeddingError>> + Send + 'a>,
    > {
        Box::pin(self.embed_inner(text, task))
    }
}

// ---------------------------------------------------------------------------
// Hash fallback provider
// ---------------------------------------------------------------------------

/// Deterministic SHA-256-derived 768-dim vector. Produces the same
/// vector for the same input every time, lets the rest of the
/// pipeline (column writes, cosine math, response shape) exercise
/// without secrets. Cosine similarity between two unrelated texts is
/// noise-distributed around 0 — not a useful ranking signal, but the
/// column round-trips and the math doesn't blow up.
#[derive(Clone, Default)]
pub struct HashFallbackProvider;

impl HashFallbackProvider {
    fn embed_inner(&self, text: &str) -> Vec<f32> {
        let trimmed = truncate_for_embedding(text);
        let mut out = Vec::with_capacity(EMBEDDING_DIM);
        let mut counter: u32 = 0;
        let mut buffer: Vec<u8> = Vec::new();
        while out.len() < EMBEDDING_DIM {
            if buffer.is_empty() {
                let mut hasher = Sha256::new();
                hasher.update(trimmed.as_bytes());
                hasher.update(counter.to_le_bytes());
                buffer = hasher.finalize().to_vec();
                counter = counter.wrapping_add(1);
            }
            while buffer.len() >= 2 && out.len() < EMBEDDING_DIM {
                let hi = buffer.remove(0) as u16;
                let lo = buffer.remove(0) as u16;
                let raw = (hi << 8) | lo;
                let f = (raw as f32 / 32_768.0) - 1.0;
                out.push(f);
            }
            buffer.clear();
        }
        unit_normalise(out)
    }
}

impl EmbeddingProvider for HashFallbackProvider {
    fn name(&self) -> &'static str {
        "hash-fallback"
    }
    fn embed<'a>(
        &'a self,
        text: &'a str,
        _task: TaskType,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<f32>, EmbeddingError>> + Send + 'a>,
    > {
        // Task type is irrelevant to a deterministic hash — same text
        // always yields the same vector regardless of query/document.
        let v = self.embed_inner(text);
        Box::pin(async move { Ok(v) })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// L2-normalise a vector to unit length. Cosine similarity over
/// unit-length vectors equals their dot product, which keeps the
/// `[-1, 1]` interpretation stable regardless of provider.
fn unit_normalise(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

/// Trim + cap input text to [`MAX_INPUT_CHARS`]. Operates on chars
/// (not bytes) so a multi-byte boundary doesn't slice mid-codepoint.
pub fn truncate_for_embedding(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_INPUT_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(MAX_INPUT_CHARS).collect()
}

/// Lazily-built process-singleton. The startup backfill + the
/// write-path fire-and-forget both call into this; constructing one
/// `EmbeddingClient` per call would re-build the reqwest pool every
/// time. `OnceLock` keeps the construction lock-free after the first
/// call.
pub fn global() -> &'static EmbeddingClient {
    static CLIENT: OnceLock<EmbeddingClient> = OnceLock::new();
    CLIENT.get_or_init(EmbeddingClient::from_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_fallback_produces_stable_unit_vector() {
        let p = HashFallbackProvider;
        let a = p.embed("hello world", TaskType::Document).await.unwrap();
        let b = p.embed("hello world", TaskType::Document).await.unwrap();
        assert_eq!(a.len(), EMBEDDING_DIM);
        assert_eq!(a, b, "hash fallback must be deterministic");
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "expected unit norm, got {norm}");
    }

    #[tokio::test]
    async fn hash_fallback_distinguishes_different_inputs() {
        let p = HashFallbackProvider;
        let a = p
            .embed("battle: rust vs go", TaskType::Document)
            .await
            .unwrap();
        let b = p
            .embed("battle: typescript vs python", TaskType::Document)
            .await
            .unwrap();
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        assert!(dot.abs() < 0.99, "expected near-orthogonal, got dot={dot}");
    }

    #[test]
    fn truncate_handles_multibyte_chars() {
        let s = "🦀".repeat(MAX_INPUT_CHARS + 10);
        let out = truncate_for_embedding(&s);
        assert_eq!(out.chars().count(), MAX_INPUT_CHARS);
    }

    #[test]
    fn truncate_passes_through_short_input() {
        let out = truncate_for_embedding("  hello  ");
        assert_eq!(out, "hello");
    }

    #[test]
    fn embedding_dim_matches_dedup_column() {
        // Migration 023's claim_embedding column + migration 051's
        // search_embedding columns are both `vector(768)`. If the
        // constant ever drifts from 768, both columns reject the
        // insert with a pgvector dim-mismatch error.
        assert_eq!(EMBEDDING_DIM, 768);
    }
}
