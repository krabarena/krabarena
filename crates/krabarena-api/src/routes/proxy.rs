use axum::body::Body;
use axum::extract::{OriginalUri, Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::ApiError;
use crate::models::proxy::ProxySpec;
use crate::routes::auth::extract_user_id;
use crate::services::{aws_sigv4, proxy_billing, proxy_service, slug_resolver};
use crate::web::rate_limit::{ProxyLimitDecision, apply_ratelimit_headers};

/// Resolve which Content-Type to set on the response we hand back.
///
/// The upstream value wins when present and ASCII-stringifiable;
/// otherwise we fall back to `application/octet-stream` rather than
/// letting axum guess (which historically coerced JSON bodies into
/// `text/html`-ish behaviour and broke strict clients).
fn pick_response_content_type(upstream: Option<&str>) -> &str {
    upstream.unwrap_or("application/octet-stream")
}

/// Upstream client for proxied calls. Separate from `AppState::proxy_http`
/// (which also fetches logos/favicons and needs redirects) because it
/// must NOT follow redirects: reqwest strips only `Authorization` /
/// `Cookie` on a cross-host hop, so a redirect would carry sponsor keys
/// in custom headers (`x-api-key`, `x-apikey`, `X-Subscription-Token`, …)
/// to whatever host the upstream points at, and would also bypass the
/// public-host check on `upstream`. 3xx responses are passed through to
/// the caller as-is. Process-wide so the TLS pool is shared.
fn upstream_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("failed to build proxy upstream client")
    })
}

/// Truncate `s` to at most `max` bytes without splitting a UTF-8
/// character. `&s[..max]` panics when byte `max` falls inside a
/// multibyte char (long Cyrillic/CJK responses dropped the connection
/// before the call was logged or billed — security review 2026-11-19).
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Compose the upstream URL the proxy should fetch.
///
/// Joins the spec's `upstream` host (trailing-slash-tolerant) with the
/// path captured by the route and forwards the inbound query string
/// verbatim. The querystring matters for GET-with-querystring upstreams
/// (you.com, etc.); POST-with-JSON-body upstreams send everything in
/// the body and don't notice. Pulled out as a free function so the
/// composition is unit-testable without spinning up the whole handler.
fn build_upstream_url(upstream: &str, path: &str, query: Option<&str>) -> String {
    let base = upstream.trim_end_matches('/');
    match query {
        Some(q) if !q.is_empty() => format!("{base}/{path}?{q}"),
        _ => format!("{base}/{path}"),
    }
}

/// Proxy route patterns, shared with `main.rs` so the routing test below
/// exercises exactly what the app registers.
pub const PROXY_PATH_ROUTE: &str = "/v1/battles/{battle_id}/proxy/{service}/{*path}";
/// Path-less forms for upstreams whose API lives at the bare root (TinyFish
/// Search: `GET https://api.search.tinyfish.ai/?query=…`). `{*path}` never
/// matches an empty tail, so without these every such call 404'd in our
/// router before reaching the upstream.
pub const PROXY_ROOT_ROUTE: &str = "/v1/battles/{battle_id}/proxy/{service}";
pub const PROXY_ROOT_SLASH_ROUTE: &str = "/v1/battles/{battle_id}/proxy/{service}/";

/// ANY /v1/battles/{battle_id}/proxy/{service}/{*path}
/// Proxies the request to the upstream service with injected secrets.
pub async fn handle_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    method: Method,
    Path((battle_id_raw, service, path)): Path<(String, String, String)>,
    OriginalUri(original_uri): OriginalUri,
    body: Body,
) -> Result<Response, ApiError> {
    proxy_request(
        state,
        headers,
        method,
        battle_id_raw,
        service,
        path,
        original_uri,
        body,
    )
    .await
}

/// ANY /v1/battles/{battle_id}/proxy/{service}[/]
/// Same as [`handle_proxy`], forwarding to the upstream's root path.
pub async fn handle_proxy_root(
    State(state): State<AppState>,
    headers: HeaderMap,
    method: Method,
    Path((battle_id_raw, service)): Path<(String, String)>,
    OriginalUri(original_uri): OriginalUri,
    body: Body,
) -> Result<Response, ApiError> {
    proxy_request(
        state,
        headers,
        method,
        battle_id_raw,
        service,
        String::new(),
        original_uri,
        body,
    )
    .await
}

/// Upstream responses larger than this are refused with a 502 instead
/// of being buffered: `bytes()` used to read the whole body into memory
/// with no bound, so one huge upstream response could OOM the instance.
pub const MAX_UPSTREAM_RESPONSE_BYTES: usize = 20 * 1024 * 1024;

/// Request / response bodies are stored in `proxy_logs` up to this
/// many bytes (UTF-8-safe). Request bodies used to be stored whole
/// (up to the 10 MB inbound cap) — a cheap storage DoS.
const LOG_BODY_MAX: usize = 100_000;

/// Body text as stored in `proxy_logs`: truncated at [`LOG_BODY_MAX`]
/// bytes on a char boundary, with a marker.
fn log_snippet(s: &str) -> String {
    if s.len() > LOG_BODY_MAX {
        format!("{}... [truncated at 100KB]", truncate_utf8(s, LOG_BODY_MAX))
    } else {
        s.to_string()
    }
}

/// Legacy integer `proxy_logs.cost_cents` for a charge in µ$: rounded
/// up so a sub-cent charge logs as 1¢, not 0¢. Informational only —
/// accounting reads `proxy_charges`.
fn micro_to_log_cents(micro: i64) -> i32 {
    let cents = micro
        .max(0)
        .saturating_add(proxy_billing::MICRO_PER_CENT - 1)
        / proxy_billing::MICRO_PER_CENT;
    cents.min(i64::from(i32::MAX)) as i32
}

#[derive(Debug)]
enum BodyReadError {
    TooLarge,
    Upstream(reqwest::Error),
}

/// Stream the upstream body into memory, refusing anything over `max`
/// bytes (checked against `Content-Length` up front and while reading).
async fn read_capped_body(
    mut resp: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, BodyReadError> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Err(BodyReadError::TooLarge);
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(BodyReadError::Upstream)? {
        if buf.len().saturating_add(chunk.len()) > max {
            return Err(BodyReadError::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// What became of a reserved charge that did not complete normally.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GuardOutcome {
    /// The upstream never received the request — drop the reservation.
    Release,
    /// Sent upstream but never completed normally — settle at the
    /// per-request cap (else flat) and write a `proxy_logs` row.
    Abandon {
        status: i32,
        latency_ms: i32,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardStage {
    /// Charge reserved, request not yet sent.
    Reserved,
    /// Request handed to the upstream client.
    Sent,
    /// Outcome decided (settled by the caller, released or abandoned).
    Done,
}

type GuardSink = Box<dyn FnOnce(GuardOutcome) + Send>;

/// Drop guard over a reserved `proxy_charges` row. Every exit from the
/// handler after the reservation — early `?` return, upstream timeout,
/// oversize body, or the whole future being dropped because the client
/// hung up — reaches exactly one outcome: released (never sent),
/// abandoned (sent, settled at the cap / flat rate and logged), or
/// completed (the caller settles from the parsed response). Before
/// this, a flat-rate call that was dropped mid-flight left no record
/// at all while the sponsor was billed upstream.
struct ChargeGuard {
    stage: GuardStage,
    sent_at: Option<std::time::Instant>,
    sink: Option<GuardSink>,
}

impl ChargeGuard {
    fn new(sink: GuardSink) -> Self {
        Self {
            stage: GuardStage::Reserved,
            sent_at: None,
            sink: Some(sink),
        }
    }

    fn mark_sent(&mut self) {
        self.stage = GuardStage::Sent;
        self.sent_at = Some(std::time::Instant::now());
    }

    fn finish(&mut self, outcome: Option<GuardOutcome>) {
        self.stage = GuardStage::Done;
        if let (Some(sink), Some(outcome)) = (self.sink.take(), outcome) {
            sink(outcome);
        }
    }

    fn release(&mut self) {
        self.finish(Some(GuardOutcome::Release));
    }

    fn abandon(&mut self, status: i32, reason: &str) {
        let latency_ms = self
            .sent_at
            .map(|t| t.elapsed().as_millis().min(i32::MAX as u128) as i32)
            .unwrap_or(0);
        self.finish(Some(GuardOutcome::Abandon {
            status,
            latency_ms,
            reason: reason.to_string(),
        }));
    }

    /// The response was read; the caller settles the charge itself.
    fn complete(&mut self) {
        self.finish(None);
    }
}

impl Drop for ChargeGuard {
    fn drop(&mut self) {
        match self.stage {
            GuardStage::Reserved => self.release(),
            GuardStage::Sent => self.abandon(
                499,
                "client disconnected or request cancelled before the upstream response was read",
            ),
            GuardStage::Done => {}
        }
    }
}

/// Everything needed to write the `proxy_logs` row for one call,
/// captured before the upstream request so an abandoned call can still
/// be logged from a detached task.
#[derive(Clone)]
struct ProxyLogCtx {
    pool: sqlx::PgPool,
    log_id: uuid::Uuid,
    battle_id: uuid::Uuid,
    user_id: uuid::Uuid,
    service: String,
    method: String,
    path: String,
    body_hash: Option<String>,
    request_body: Option<String>,
    request_headers: serde_json::Value,
}

impl ProxyLogCtx {
    #[allow(clippy::too_many_arguments)]
    async fn write(
        &self,
        status: i32,
        resp_size: Option<i32>,
        latency_ms: i32,
        charged_micro: i64,
        response_body: Option<&str>,
        response_headers: &serde_json::Value,
    ) {
        proxy_service::log_request(
            &self.pool,
            self.log_id,
            self.battle_id,
            self.user_id,
            &self.service,
            &self.method,
            &self.path,
            self.body_hash.as_deref(),
            self.request_body.as_deref(),
            &self.request_headers,
            status,
            resp_size,
            latency_ms,
            micro_to_log_cents(charged_micro),
            response_body,
            response_headers,
        )
        .await;
    }
}

/// The production guard sink: performs the outcome in a detached task
/// (a dropped handler can't `.await`).
fn charge_guard_sink(ctx: ProxyLogCtx, charge_id: uuid::Uuid, abandon_micro: i64) -> GuardSink {
    Box::new(move |outcome| {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return; // no runtime (shutdown): the stale sweep settles it
        };
        rt.spawn(async move {
            match outcome {
                GuardOutcome::Release => {
                    proxy_service::release_charge(&ctx.pool, charge_id).await;
                }
                GuardOutcome::Abandon {
                    status,
                    latency_ms,
                    reason,
                } => {
                    tracing::warn!(
                        battle_id = %ctx.battle_id,
                        service = %ctx.service,
                        status,
                        charged_micro_usd = abandon_micro,
                        reason = %reason,
                        "proxy billing: call abandoned after send, charging cap/flat"
                    );
                    let micro = proxy_service::settle_charge(
                        &ctx.pool,
                        charge_id,
                        &proxy_billing::abandoned_decision(abandon_micro),
                    )
                    .await;
                    ctx.write(
                        status,
                        None,
                        latency_ms,
                        micro,
                        Some(&format!("[krabarena proxy: {reason}]")),
                        &serde_json::json!({}),
                    )
                    .await;
                }
            }
        });
    })
}

/// Resolve battle → proxy spec → service. Any failure here is a "miss"
/// for the proxy-miss rate-limit bucket.
async fn resolve_proxy_target(
    pool: &sqlx::PgPool,
    battle_id_raw: &str,
    service: &str,
) -> Result<(uuid::Uuid, ProxySpec), ApiError> {
    let battle_id = slug_resolver::resolve_battle(pool, battle_id_raw).await?;
    let spec = proxy_service::get_proxy_spec(pool, battle_id).await?;
    if !spec.services.contains_key(service) {
        let mut names: Vec<_> = spec.services.keys().cloned().collect();
        names.sort();
        return Err(ApiError::ValidationError(format!(
            "service '{}' not configured on this battle. Available: {}",
            service,
            names.join(", ")
        )));
    }
    Ok((battle_id, spec))
}

#[allow(clippy::too_many_arguments)]
async fn proxy_request(
    state: AppState,
    headers: HeaderMap,
    method: Method,
    battle_id_raw: String,
    service: String,
    path: String,
    original_uri: axum::http::Uri,
    body: Body,
) -> Result<Response, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    // `..` segments would let a caller escape the sponsor's base path on
    // the upstream host (still carrying the injected key).
    if path.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(ApiError::ValidationError(
            "proxy path must not contain '..' segments".into(),
        ));
    }

    // Authenticated proxy calls skip the global bucket (see
    // `web::rate_limit`), so lookups of nonexistent battles / services
    // are throttled by a per-user miss bucket instead: refused here,
    // before any DB work, once the user has burned through it.
    if let Some(limiter) = state.rate_limiter.as_ref()
        && let Some(resp) = limiter.proxy_miss_limited(user_id).await
    {
        return Ok(resp);
    }
    let (battle_id, spec) =
        match resolve_proxy_target(&state.proxy_pool, &battle_id_raw, &service).await {
            Ok(v) => v,
            Err(e) => {
                if !matches!(e, ApiError::Internal(_))
                    && let Some(limiter) = state.rate_limiter.as_ref()
                {
                    limiter.record_proxy_miss(user_id).await;
                }
                return Err(e);
            }
        };
    let svc = &spec.services[&service];

    // Sponsor-defined endpoint allowlist: without it a Claimer can call
    // any method/path (account APIs, DELETE, …) with the sponsor's key.
    svc.check_endpoint_allowed(&service, method.as_str(), &path)
        .map_err(ApiError::ProxyEndpointNotAllowed)?;

    // Enforce the battle's own `rate_limit_rpm` per user (all services of
    // this battle share one budget). The platform's global 120/min bucket
    // deliberately does NOT apply to authenticated proxy calls — see
    // `web::rate_limit` — so this is the only throttle on this path.
    // No-op when Redis is unset (dev); fails open on Redis errors.
    let mut ratelimit_headers: Option<(u32, i64, i64)> = None;
    if let Some(limiter) = state.rate_limiter.as_ref() {
        match limiter
            .check_proxy(battle_id, user_id, spec.rate_limit_rpm)
            .await
        {
            ProxyLimitDecision::Limited(resp) => return Ok(resp),
            ProxyLimitDecision::Allowed {
                limit,
                remaining,
                reset,
            } => ratelimit_headers = Some((limit, remaining, reset)),
            ProxyLimitDecision::Unavailable => {}
        }
    }

    // Read body for hashing and logging
    let body_bytes = axum::body::to_bytes(body, 10 * 1024 * 1024)
        .await
        .map_err(|e| ApiError::ValidationError(format!("request body too large: {e}")))?;

    let body_hash = if body_bytes.is_empty() {
        None
    } else {
        let mut hasher = Sha256::new();
        hasher.update(&body_bytes);
        Some(format!("sha256:{}", hex::encode(hasher.finalize())))
    };

    // Request body as stored in `proxy_logs` (truncated at 100 KB; the
    // full body is still forwarded and hashed).
    let request_body_str = std::str::from_utf8(&body_bytes).ok().map(log_snippet);

    // Capture request headers (strip auth, keep content-type etc.)
    let mut req_headers_map = serde_json::Map::new();
    for (key, val) in headers.iter() {
        let k = key.as_str().to_lowercase();
        if k == "authorization" || k == "x-krabarena-token" || k == "cookie" {
            continue; // Don't log claimer's auth
        }
        if let Ok(v) = val.to_str() {
            req_headers_map.insert(k, serde_json::Value::String(v.to_string()));
        }
    }
    let request_headers_json = serde_json::Value::Object(req_headers_map);

    // Record intent BEFORE the upstream call: every proxied request —
    // flat-rate or usage-billed — reserves a `proxy_charges` row holding
    // the most it can cost (the per-request cap, else the flat rate),
    // atomically with the budget check under the per-battle lock, so
    // concurrent calls can't overshoot and a call that dies mid-flight is
    // still charged.
    let log_id = uuid::Uuid::new_v4();
    let flat_micro = proxy_billing::flat_micro_per_request(svc.cost_per_1k_cents);
    let abandon_micro = proxy_billing::abandoned_micro(svc.billing.as_ref(), flat_micro);
    let charge_id = proxy_service::reserve_charge(
        &state.proxy_pool,
        &spec,
        battle_id,
        user_id,
        &service,
        log_id,
        abandon_micro,
        flat_micro,
    )
    .await?;
    let log_ctx = ProxyLogCtx {
        pool: state.proxy_pool.clone(),
        log_id,
        battle_id,
        user_id,
        service: service.clone(),
        method: method.as_str().to_string(),
        path: path.clone(),
        body_hash,
        request_body: request_body_str,
        request_headers: request_headers_json,
    };
    let mut guard = ChargeGuard::new(charge_guard_sink(log_ctx.clone(), charge_id, abandon_micro));

    let upstream_url = build_upstream_url(&svc.upstream, &path, original_uri.query());

    // Shared, redirect-less upstream client (see `upstream_client`):
    // reuses the TCP+TLS pool across requests.
    let mut req = upstream_client().request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
        &upstream_url,
    );

    // Inject sponsor's secret headers
    for (key, value) in &svc.headers {
        req = req.header(key, value);
    }

    // Forward content-type from original request
    if let Some(ct) = headers.get("content-type") {
        req = req.header("content-type", ct.to_str().unwrap_or("application/json"));
    }

    // AWS SigV4 upstreams (Bedrock AgentCore Web Search, …): sign the
    // exact URL + body being sent with the sponsor's IAM credentials.
    // A signing failure returns early; the guard releases the charge.
    if let Some(aws) = &svc.aws_sigv4 {
        let signed = reqwest::Url::parse(&upstream_url)
            .map_err(|e| format!("invalid upstream URL: {e}"))
            .and_then(|url| {
                aws_sigv4::sign(aws, method.as_str(), &url, &body_bytes, chrono::Utc::now())
            })
            .map_err(|e| ApiError::ValidationError(format!("aws_sigv4 signing failed: {e}")))?;
        for (key, value) in signed {
            req = req.header(key, value);
        }
    }

    // Forward body
    if !body_bytes.is_empty() {
        req = req.body(body_bytes.to_vec());
    }

    req = req.timeout(std::time::Duration::from_secs(120));

    // Send upstream request. From here on the sponsor may be billed
    // upstream, so any exit other than a parsed response settles the
    // reservation at the cap / flat rate (see `ChargeGuard`).
    guard.mark_sent();
    let start = std::time::Instant::now();
    let upstream_resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            // Don't echo reqwest's error text: it embeds the full
            // upstream URL (hidden host, and any key in its query).
            tracing::warn!(service = %service, error = %e, "proxy upstream request failed");
            let kind = if e.is_timeout() {
                "timed out"
            } else if e.is_connect() {
                "connection failed"
            } else {
                "request failed"
            };
            if e.is_connect() {
                // Never reached the upstream: nothing was billed there.
                guard.release();
            } else {
                let status = if e.is_timeout() { 504 } else { 502 };
                guard.abandon(status, &format!("upstream {kind}"));
            }
            return Err(ApiError::ValidationError(format!(
                "upstream service error: {kind}"
            )));
        }
    };
    let latency_ms = start.elapsed().as_millis().min(i32::MAX as u128) as i32;

    let status = upstream_resp.status().as_u16() as i32;

    // Capture response headers (strip secret-bearing ones before they
    // land in `proxy_logs`). Upstreams can echo credentials back —
    // `set-cookie` session tokens, `authorization`, vendor API-key
    // headers — and the full response-header set is persisted, so
    // redact the same way the request side strips the claimer's auth.
    let mut resp_headers_map = serde_json::Map::new();
    for (key, val) in upstream_resp.headers().iter() {
        let k = key.as_str().to_lowercase();
        if matches!(
            k.as_str(),
            "set-cookie"
                | "authorization"
                | "proxy-authorization"
                | "x-api-key"
                | "x-apikey"
                | "api-key"
                | "x-amz-security-token"
        ) {
            continue; // Don't persist secrets the upstream echoed back.
        }
        if let Ok(v) = val.to_str() {
            resp_headers_map.insert(k, serde_json::Value::String(v.to_string()));
        }
    }
    let response_headers_json = serde_json::Value::Object(resp_headers_map);

    // Pull the upstream Content-Type before consuming `upstream_resp`
    // so we can forward it on the response (strict JSON clients like
    // aiohttp refuse to decode without it) and so billing only parses
    // SSE for `text/event-stream`.
    let upstream_content_type = upstream_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Header-sourced cost must be captured before the body consumes
    // the response.
    let billing_header_value = svc
        .billing
        .as_ref()
        .and_then(|b| b.header.as_deref())
        .and_then(|h| upstream_resp.headers().get(h.trim()))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let resp_bytes = match read_capped_body(upstream_resp, MAX_UPSTREAM_RESPONSE_BYTES).await {
        Ok(b) => b,
        Err(BodyReadError::TooLarge) => {
            guard.abandon(status, "upstream response exceeded the 20 MB proxy limit");
            return Err(ApiError::UpstreamError(format!(
                "upstream response exceeded the proxy's {} MB limit — request less data \
                 (smaller page size / fewer results)",
                MAX_UPSTREAM_RESPONSE_BYTES / (1024 * 1024)
            )));
        }
        Err(BodyReadError::Upstream(e)) => {
            tracing::warn!(service = %service, error = %e, "proxy upstream body read failed");
            let kind = if e.is_timeout() {
                "timed out"
            } else {
                "failed"
            };
            guard.abandon(status, &format!("reading the upstream response {kind}"));
            return Err(ApiError::UpstreamError(format!(
                "reading the upstream response {kind}"
            )));
        }
    };
    // Response fully read: settled below from the parsed body. No
    // `.await` between here and the spawn, so cancellation can't strand
    // the reservation.
    guard.complete();
    let resp_size = resp_bytes.len().min(i32::MAX as usize) as i32;

    // Decide the charge. Usage-billed services parse the FULL buffered
    // body (before the 100 KB log truncation); flat services pay the
    // flat rate. The bytes handed back to the claimer are untouched.
    let decision = match &svc.billing {
        None => proxy_billing::flat_decision(flat_micro),
        Some(billing) => {
            let ex = proxy_billing::extract(
                billing,
                billing_header_value.as_deref(),
                upstream_content_type.as_deref(),
                &resp_bytes,
            );
            let decision = proxy_billing::decide(billing, flat_micro, &ex);
            if decision.source == proxy_billing::ChargeSource::Capped {
                tracing::warn!(
                    %battle_id,
                    service = %service,
                    reported_micro_usd = decision.reported_micro.unwrap_or_default(),
                    charged_micro_usd = decision.amount_micro,
                    "proxy billing: reported cost above max_per_request_cents, capped"
                );
            }
            if let Some(reason) = &ex.miss_reason {
                tracing::info!(
                    %battle_id,
                    service = %service,
                    status,
                    reason = %reason,
                    "proxy billing: no usable reported cost, charging flat rate"
                );
            }
            decision
        }
    };

    // Response body as stored (truncated at 100 KB).
    let response_body_str = std::str::from_utf8(&resp_bytes).ok().map(log_snippet);

    // Settle the reservation, then log (async, non-blocking). The legacy
    // per-row `cost_cents` reflects what was actually charged (rounded
    // up to a whole cent, 0 for a deduped poll).
    let pool = state.proxy_pool.clone();
    tokio::spawn(async move {
        let micro = proxy_service::settle_charge(&pool, charge_id, &decision).await;
        log_ctx
            .write(
                status,
                Some(resp_size),
                latency_ms,
                micro,
                response_body_str.as_deref(),
                &response_headers_json,
            )
            .await;
    });

    // Build response
    let mut response = Response::builder()
        .status(StatusCode::from_u16(status as u16).unwrap_or(StatusCode::BAD_GATEWAY));

    response = response
        .header("x-krabarena-proxy", "true")
        .header("x-krabarena-battle", battle_id.to_string())
        .header("x-krabarena-service", &service)
        .header("x-krabarena-latency-ms", latency_ms.to_string());

    // Forward the upstream Content-Type so strict JSON clients (aiohttp,
    // some browser fetch handlers) decode the body as the upstream
    // intended.
    response = response.header(
        "content-type",
        pick_response_content_type(upstream_content_type.as_deref()),
    );

    let mut response = response
        .body(Body::from(resp_bytes))
        .map_err(|e| ApiError::Internal(e.into()))?;
    if let Some((limit, remaining, reset)) = ratelimit_headers {
        apply_ratelimit_headers(response.headers_mut(), limit, remaining, reset);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::build_upstream_url;

    use super::{
        BodyReadError, ChargeGuard, GuardOutcome, log_snippet, micro_to_log_cents, read_capped_body,
    };
    use std::sync::{Arc, Mutex};

    fn recording_guard() -> (ChargeGuard, Arc<Mutex<Vec<GuardOutcome>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let guard = ChargeGuard::new(Box::new(move |o| sink_seen.lock().unwrap().push(o)));
        (guard, seen)
    }

    // Regression (proxy audit 2026-11-19): a flat-rate call whose handler
    // was dropped after the request went upstream (client hung up) left
    // no `proxy_logs` row and no charge — the sponsor paid upstream, the
    // battle budget never did.
    #[test]
    fn dropped_after_send_abandons_at_cap() {
        let (mut g, seen) = recording_guard();
        g.mark_sent();
        drop(g);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(
            matches!(&seen[0], GuardOutcome::Abandon { status: 499, .. }),
            "{seen:?}"
        );
    }

    #[test]
    fn dropped_before_send_releases() {
        let (g, seen) = recording_guard();
        drop(g);
        assert_eq!(*seen.lock().unwrap(), vec![GuardOutcome::Release]);
    }

    #[test]
    fn completed_or_explicit_outcomes_fire_once() {
        let (mut g, seen) = recording_guard();
        g.mark_sent();
        g.complete();
        drop(g);
        assert!(
            seen.lock().unwrap().is_empty(),
            "caller settles completed calls"
        );

        let (mut g, seen) = recording_guard();
        g.mark_sent();
        g.abandon(504, "upstream timed out");
        drop(g);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(matches!(
            &seen[0],
            GuardOutcome::Abandon { status: 504, .. }
        ));
    }

    /// The real cancellation path: the handler future is dropped while
    /// awaiting the upstream.
    #[tokio::test]
    async fn cancelled_future_abandons() {
        let (mut g, seen) = recording_guard();
        let fut = async move {
            g.mark_sent();
            std::future::pending::<()>().await;
            g.complete();
        };
        let _ = tokio::time::timeout(std::time::Duration::from_millis(10), fut).await;
        assert!(matches!(
            seen.lock().unwrap().as_slice(),
            [GuardOutcome::Abandon { status: 499, .. }]
        ));
    }

    fn upstream(body: Vec<u8>) -> reqwest::Response {
        reqwest::Response::from(axum::http::Response::new(body))
    }

    // Regression (proxy audit 2026-11-19): `upstream_resp.bytes()`
    // buffered unbounded upstream bodies.
    #[tokio::test]
    async fn upstream_body_is_capped() {
        let ok = read_capped_body(upstream(vec![b'a'; 1024]), 1024).await;
        assert_eq!(ok.unwrap().len(), 1024);
        let big = read_capped_body(upstream(vec![b'a'; 1025]), 1024).await;
        assert!(matches!(big, Err(BodyReadError::TooLarge)), "{big:?}");
    }

    // Regression (proxy audit 2026-11-19): request bodies up to 10 MB were
    // stored untruncated in `proxy_logs.request_body`.
    #[test]
    fn logged_bodies_are_truncated_utf8_safely() {
        let s = "я".repeat(100_000); // 200 KB
        let logged = log_snippet(&s);
        assert!(logged.ends_with("... [truncated at 100KB]"));
        assert!(logged.len() <= 100_000 + "... [truncated at 100KB]".len());
        assert_eq!(log_snippet("small"), "small");
    }

    #[test]
    fn log_cents_round_up_and_saturate() {
        assert_eq!(micro_to_log_cents(0), 0);
        assert_eq!(micro_to_log_cents(1_000), 1); // 0.1¢ logs as 1¢
        assert_eq!(micro_to_log_cents(10_000), 1);
        assert_eq!(micro_to_log_cents(10_001), 2);
        assert_eq!(micro_to_log_cents(i64::MAX), i32::MAX);
        assert_eq!(micro_to_log_cents(-5), 0);
    }

    // Regression guard: `&s[..100_000]` panicked when byte 100_000 fell
    // inside a multibyte char, dropping the request unlogged/unbilled.
    #[test]
    fn truncate_utf8_never_splits_a_char() {
        let s = "я".repeat(60_000); // 2 bytes each → 120_000 bytes
        let t = super::truncate_utf8(&s, 100_001);
        assert_eq!(t.len(), 100_000);
        assert!(t.chars().all(|c| c == 'я'));
        assert_eq!(super::truncate_utf8("abc", 10), "abc");
    }

    #[test]
    fn appends_query_string_when_present() {
        // Regression: GET-with-querystring upstreams (you.com) used to
        // 422 because the proxy dropped the inbound query string.
        let url = build_upstream_url(
            "https://ydc-index.io",
            "v1/search",
            Some("query=celtics&count=5"),
        );
        assert_eq!(url, "https://ydc-index.io/v1/search?query=celtics&count=5");
    }

    #[test]
    fn omits_query_string_when_absent() {
        let url = build_upstream_url("https://api.exa.ai", "search", None);
        assert_eq!(url, "https://api.exa.ai/search");
    }

    #[test]
    fn omits_question_mark_for_empty_query() {
        // axum hands back `Some("")` for `/path?` — don't emit a bare `?`.
        let url = build_upstream_url("https://api.exa.ai", "search", Some(""));
        assert_eq!(url, "https://api.exa.ai/search");
    }

    #[test]
    fn empty_path_targets_upstream_root() {
        // Root-path upstreams (TinyFish Search) are reached via the
        // path-less proxy routes, which pass an empty path.
        let url = build_upstream_url("https://api.search.tinyfish.ai", "", Some("query=k2"));
        assert_eq!(url, "https://api.search.tinyfish.ai/?query=k2");
    }

    /// Regression: `/proxy/tinyfish?query=…` and `/proxy/tinyfish/?query=…`
    /// 404'd in our router (the only proxy route needed a non-empty
    /// `{*path}`), so TinyFish — whose API lives at `/` — was unreachable
    /// through the Battle proxy. Uses the same patterns `main.rs` registers.
    #[tokio::test]
    async fn proxy_routes_match_root_and_subpaths() {
        use super::{PROXY_PATH_ROUTE, PROXY_ROOT_ROUTE, PROXY_ROOT_SLASH_ROUTE};
        use axum::{Router, body::Body, http::Request, routing::any};
        use tower::ServiceExt;

        let app = Router::new()
            .route(PROXY_PATH_ROUTE, any(|| async { "path" }))
            .route(PROXY_ROOT_ROUTE, any(|| async { "root" }))
            .route(PROXY_ROOT_SLASH_ROUTE, any(|| async { "root" }));
        for (uri, want) in [
            ("/v1/battles/b/proxy/tinyfish?query=k2", "root"),
            ("/v1/battles/b/proxy/tinyfish/?query=k2", "root"),
            ("/v1/battles/b/proxy/exa/search", "path"),
            ("/v1/battles/b/proxy/you_com/v1/search?query=x", "path"),
        ] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "{uri}");
            let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
            assert_eq!(&body[..], want.as_bytes(), "{uri}");
        }
    }

    #[test]
    fn tolerates_trailing_slash_on_upstream() {
        let url = build_upstream_url("https://api.exa.ai/", "search", None);
        assert_eq!(url, "https://api.exa.ai/search");
    }

    use super::pick_response_content_type;

    #[test]
    fn forwards_upstream_content_type() {
        assert_eq!(
            pick_response_content_type(Some("application/json; charset=utf-8")),
            "application/json; charset=utf-8"
        );
    }

    #[test]
    fn falls_back_to_octet_stream_when_upstream_omits_content_type() {
        // axum's default would otherwise leave the response unmarked;
        // strict JSON clients (aiohttp.ClientResponse.json()) then
        // refuse to decode. `application/octet-stream` is the neutral
        // RFC-7231 default for binary data.
        assert_eq!(pick_response_content_type(None), "application/octet-stream");
    }

    #[test]
    fn preserves_query_verbatim_including_encoded_chars() {
        // The handler does not re-encode — whatever axum captured from
        // the inbound request (already %-encoded by the client) flows
        // through unchanged.
        let url = build_upstream_url(
            "https://ydc-index.io",
            "v1/search",
            Some("query=Who%20won%20the%202024%20NBA%20Finals%3F&count=5"),
        );
        assert_eq!(
            url,
            "https://ydc-index.io/v1/search?query=Who%20won%20the%202024%20NBA%20Finals%3F&count=5"
        );
    }
}
