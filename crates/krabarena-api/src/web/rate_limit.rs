//! Redis-backed rate limiter (since 0.26.0).
//!
//! Two tiers:
//!   - **Strict** (default 10 req/min) for the abuse-sensitive auth /
//!     test surface (`/v1/auth/`, `/auth/`, `/v1/test/`) — the
//!     brute-force / token-mint endpoints.
//!   - **Global** (default 120 req/min) for everything else.
//!   - **Battle proxy** (since 0.43.3) — authenticated calls to
//!     `/v1/battles/{id}/proxy/...` skip the global bucket entirely and
//!     are throttled instead by the battle's own `proxy_spec.rate_limit_rpm`
//!     (per user, per battle, all services combined) inside the proxy
//!     handler via [`RateLimiter::check_proxy`]. Before 0.43.3 the
//!     global 120/min bucket also applied, so a battle advertising
//!     300 or 600 rpm silently capped at 120 and agents got 429s well
//!     below the advertised limit. Anonymous proxy calls (which 401
//!     downstream anyway) still count against the global IP bucket.
//!   - **Proxy miss** (since the 2026-11-19 proxy audit) — because the
//!     authenticated proxy path skips the global bucket *before* the
//!     battle is resolved, `/v1/battles/<garbage>/proxy/...` was an
//!     unthrottled DB-lookup primitive. Every proxy call whose battle /
//!     proxy spec / service doesn't resolve now counts against a per-user
//!     `rl:proxymiss:uid:<user_id>` bucket sized like the global tier
//!     ([`RateLimiter::record_proxy_miss`]); once it is full, further
//!     proxy calls from that user get 429 *before* any DB lookup
//!     ([`RateLimiter::proxy_miss_limited`]) until the window resets.
//!     Calls that resolve never touch it, so legitimate proxy throughput
//!     is still governed only by the battle's own `rate_limit_rpm`.
//!
//! **Keying (since 0.26.0):** the *subject* a request is throttled
//! against depends on the tier:
//!   - **Strict** is always keyed on the client **IP**. These endpoints
//!     run *before* authentication (login, device poll, token mint,
//!     voter magic link) — there is no trustworthy `user_id` yet, so
//!     brute-force protection has to be by address.
//!   - **Global** is keyed on the **`user_id`** of a *cryptographically
//!     valid* JWT when one is present, falling back to the **IP** for
//!     anonymous traffic. Per-user keying gives each authenticated agent
//!     a fair budget regardless of source address (no collateral
//!     throttling behind shared NAT/CGNAT, and an attacker can't dodge
//!     the limit by rotating cloud IPs). Only a signature-valid,
//!     unexpired token is trusted — an unverified `sub` could otherwise
//!     be forged to mint fresh per-user budgets on demand and to inflate
//!     Redis key cardinality (a memory-DoS on Memorystore).
//!
//! Key namespaces are kept disjoint — `rl:strict:ip:<ip>`,
//! `rl:global:uid:<user_id>`, `rl:global:ip:<ip>` — so a user_id can
//! never collide with an IP-shaped string.
//!
//! **Client IP (since the 2026-11 auth hardening):** the client IP is
//! the **rightmost `X-Forwarded-For` hop that is not a trusted proxy**.
//! Proxies *append* to XFF, so everything left of the hop our own
//! infrastructure appended is client-controlled — the old "leftmost hop"
//! rule let any caller pick its own rate-limit key by sending
//! `X-Forwarded-For: <random>` and so bypass every bucket. Topology:
//! external HTTPS LB (`34.49.91.89`, GFE ranges `35.191.0.0/16` and
//! `130.211.0.0/22`) → serverless NEG → Cloud Run, which sees
//! `<client>, <lb>`; Cloud Run ingress is also reachable directly on
//! `*.run.app`, where the Google front end appends `<client>`. Walking
//! from the right and skipping trusted hops yields the real client in
//! both paths. Extra trusted CIDRs can be added with the
//! comma-separated env var `TRUSTED_PROXY_CIDRS`. Falls back to the
//! socket peer address only when there is no usable XFF hop (local /
//! docker dev).
//!
//! Enforcement uses a fixed-window counter implemented as a single
//! atomic Lua `EVAL` (INCR + conditional EXPIRE + TTL read in one
//! round-trip) — see `LUA` below. Fixed-window is O(1) memory per IP
//! and the worst-case 2x burst at a window boundary is acceptable for
//! abuse throttling.
//!
//! **Redis failure**: the **Global** and **Proxy** tiers fail OPEN —
//! any Redis error lets the request through with a logged warning,
//! because a Redis outage must never take the API down. The **Strict**
//! (auth / brute-force) tier does NOT fail open: it falls back to a
//! per-instance in-memory fixed window with the same strict limit
//! ([`MemoryWindow`]), so a Redis outage can't be used to brute-force
//! login codes. If that in-memory table is full it fails closed (429).
//!
//! **Disabled in dev**: when `REDIS_URL` is unset the limiter is
//! `None` in `AppState` and the middleware is a zero-cost pass-through
//! (no Redis calls at all).
//!
//! **Static assets are exempt** (see `EXEMPT_PREFIXES`): a single page
//! navigation pulls the HTML plus CSS/JS/fonts, so counting every asset
//! made one budget worth only ~10 page views and tripped ordinary
//! refreshing. Static files are cheap and side-effect-free, so they skip
//! the limiter entirely.
//!
//! **429 representation is content-negotiated**: a browser *navigation*
//! (its `Accept` lists `text/html`) gets a friendly HTML page that
//! auto-refreshes after `Retry-After` — a raw JSON envelope rendered as
//! a page is useless to a human, and the `fetch` auto-retry wrapper
//! can't intercept a full-document navigation. Everything else
//! (`fetch`/XHR/API/CLI) still gets the standard JSON error envelope.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use deadpool_redis::Pool;
use uuid::Uuid;

use crate::AppState;
use crate::envelope::{ApiErrorBody, ApiErrorResponse, ResponseMeta};

/// Path prefixes exempt from rate limiting entirely. `/static/` covers
/// every CSS/JS/font/demo asset (the `ServeDir` mount); `/favicon`
/// covers the root-served `favicon.ico` browsers auto-request. These
/// load on every page view, are cheap to serve, and have no
/// abuse-sensitive side effects — limiting them just burned a visitor's
/// budget ~10x per navigation. Match is a plain prefix check.
const EXEMPT_PREFIXES: &[&str] = &["/static/", "/favicon"];

/// Path prefixes that get the strict (low) limit. Trailing slash is
/// significant: `/v1/auth/` matches `/v1/auth/email` but NOT
/// `/v1/authz` (which would be a different, non-auth route).
const STRICT_PREFIXES: &[&str] = &["/v1/auth/", "/auth/", "/v1/test/"];

/// Strict-tier exceptions — endpoints that sit under a strict prefix but
/// must NOT get the low cap. The device-flow login poll
/// (`/v1/auth/device/poll`) is read every 2s by `krab auth login` *by
/// design*; it's a UUID-gated status check, not a credential
/// submission, so the 10/min strict cap would throttle ordinary login
/// (and waste the budget). It falls back to the global tier instead.
const STRICT_EXCEPTIONS: &[&str] = &["/v1/auth/device/poll"];

/// Atomic fixed-window counter. One round-trip, no INCR/EXPIRE race
/// (a crash between a non-atomic INCR and EXPIRE would otherwise wedge
/// the key with no TTL forever).
///
/// `KEYS[1]` = the rate-limit key, `ARGV[1]` = limit, `ARGV[2]` =
/// window seconds. Returns `[count_after_incr, ttl_seconds]`.
const LUA: &str = r#"
local current = redis.call("INCR", KEYS[1])
if current == 1 then
    redis.call("EXPIRE", KEYS[1], ARGV[2])
end
local ttl = redis.call("TTL", KEYS[1])
if ttl < 0 then
    redis.call("EXPIRE", KEYS[1], ARGV[2])
    ttl = tonumber(ARGV[2])
end
return { current, ttl }
"#;

/// Per-tier request budgets. Window is shared across tiers (default
/// 60s) so both express "per minute". Overridable from env at startup.
#[derive(Clone, Copy, Debug)]
pub struct TierLimits {
    pub global_per_min: u32,
    pub strict_per_min: u32,
    pub window_secs: u32,
}

impl Default for TierLimits {
    fn default() -> Self {
        Self {
            global_per_min: 120,
            strict_per_min: 10,
            window_secs: 60,
        }
    }
}

/// The live limiter: a pooled Redis handle plus the tier config.
/// `Clone` is cheap — `deadpool_redis::Pool` is an `Arc` inside.
#[derive(Clone)]
pub struct RateLimiter {
    pool: Pool,
    limits: TierLimits,
    /// Strict-tier fallback used when Redis is unavailable.
    strict_fallback: Arc<MemoryWindow>,
}

impl RateLimiter {
    pub fn new(pool: Pool, limits: TierLimits) -> Self {
        Self {
            pool,
            limits,
            strict_fallback: Arc::new(MemoryWindow::new(MEMORY_WINDOW_MAX_KEYS)),
        }
    }
}

/// Upper bound on distinct keys the in-memory strict fallback tracks.
/// Beyond it (after pruning expired windows) new subjects are refused —
/// fail closed rather than grow without bound under a key-spray.
const MEMORY_WINDOW_MAX_KEYS: usize = 50_000;

/// Per-process fixed-window counter: the Strict tier's fallback when
/// Redis is down. Less accurate than Redis across many instances (each
/// instance counts separately) but never fail-open.
pub struct MemoryWindow {
    max_keys: usize,
    inner: Mutex<HashMap<String, (Instant, u32)>>,
}

impl MemoryWindow {
    pub fn new(max_keys: usize) -> Self {
        Self {
            max_keys,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Count one hit. Returns `(count_after_incr, ttl_seconds)`, the same
    /// shape as the Redis script. A full table yields `count = u32::MAX`
    /// so the caller rejects (fail closed).
    pub fn hit(&self, key: &str, window_secs: u32) -> (i64, i64) {
        self.hit_at(key, window_secs, Instant::now())
    }

    fn hit_at(&self, key: &str, window_secs: u32, now: Instant) -> (i64, i64) {
        let window = Duration::from_secs(window_secs.max(1) as u64);
        let mut map = match self.inner.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !map.contains_key(key) && map.len() >= self.max_keys {
            map.retain(|_, (start, _)| now.duration_since(*start) < window);
            if map.len() >= self.max_keys {
                return (u32::MAX as i64, window_secs as i64);
            }
        }
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= window {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        let elapsed = now.duration_since(entry.0).as_secs() as i64;
        (entry.1 as i64, (window_secs as i64 - elapsed).max(1))
    }
}

/// An IPv4/IPv6 CIDR block. Tiny local parser — no extra dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parse `a.b.c.d/nn`, `v6::/nn`, or a bare address (= /32 or /128).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a.parse::<IpAddr>().ok()?, Some(p.parse::<u8>().ok()?)),
            None => (s.parse::<IpAddr>().ok()?, None),
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        (prefix <= max).then_some(Self { net: addr, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, ip) {
            (IpAddr::V4(n), IpAddr::V4(i)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                (u32::from(n) & mask) == (u32::from(i) & mask)
            }
            (IpAddr::V6(n), IpAddr::V6(i)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                (u128::from(n) & mask) == (u128::from(i) & mask)
            }
            _ => false,
        }
    }
}

/// Our own proxy hops: the external HTTPS LB's IP plus Google's
/// front-end / health-check ranges that sit between the LB and Cloud Run.
const DEFAULT_TRUSTED_PROXIES: &[&str] = &["34.49.91.89/32", "35.191.0.0/16", "130.211.0.0/22"];

/// Build the trusted-proxy list from the defaults plus a
/// comma-separated `TRUSTED_PROXY_CIDRS` value. Unparseable entries are
/// logged and skipped.
pub fn trusted_proxies_from(extra: Option<&str>) -> Vec<Cidr> {
    let mut out: Vec<Cidr> = DEFAULT_TRUSTED_PROXIES
        .iter()
        .filter_map(|c| Cidr::parse(c))
        .collect();
    for raw in extra
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.trim().is_empty())
    {
        match Cidr::parse(raw) {
            Some(c) => out.push(c),
            None => {
                tracing::warn!(cidr = %raw.trim(), "TRUSTED_PROXY_CIDRS: ignoring invalid entry")
            }
        }
    }
    out
}

/// Process-wide trusted-proxy list (defaults + `TRUSTED_PROXY_CIDRS`),
/// read once on first use.
fn trusted_proxies() -> &'static [Cidr] {
    static TRUSTED: OnceLock<Vec<Cidr>> = OnceLock::new();
    TRUSTED
        .get_or_init(|| trusted_proxies_from(std::env::var("TRUSTED_PROXY_CIDRS").ok().as_deref()))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    Strict,
    Global,
    /// Battle proxy call — throttled by the battle's own
    /// `rate_limit_rpm` in the proxy handler, not by the global bucket.
    Proxy,
}

/// True for the battle proxy routes `/v1/battles/{id}/proxy` and
/// `/v1/battles/{id}/proxy/...` — but NOT the sibling metadata routes
/// `/v1/battles/{id}/proxy-info` / `/proxy-spec` (segment must be exactly
/// `proxy`).
fn is_proxy_call(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/v1/battles/") else {
        return false;
    };
    let mut segs = rest.splitn(3, '/');
    matches!(
        (segs.next(), segs.next()),
        (Some(id), Some("proxy")) if !id.is_empty()
    )
}

fn classify(path: &str) -> Tier {
    if is_proxy_call(path) {
        return Tier::Proxy;
    }
    // Exceptions win over the strict prefixes (e.g. the high-frequency
    // device-login poll lives under `/v1/auth/` but isn't brute-forceable).
    if STRICT_EXCEPTIONS.iter().any(|p| path.starts_with(p)) {
        return Tier::Global;
    }
    if STRICT_PREFIXES.iter().any(|p| path.starts_with(p)) {
        Tier::Strict
    } else {
        Tier::Global
    }
}

/// Static assets / favicon — skipped by the limiter (see
/// `EXEMPT_PREFIXES`).
fn is_exempt(path: &str) -> bool {
    EXEMPT_PREFIXES.iter().any(|p| path.starts_with(p))
}

/// True when the client is a browser navigating to a page (its `Accept`
/// header lists `text/html`), as opposed to a `fetch`/XHR/API/CLI client
/// (`*/*`, `application/json`, …). Decides whether an over-limit request
/// gets the auto-refreshing HTML page or the JSON error envelope.
fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"))
}

/// Resolve the client IP using the process-wide trusted-proxy list.
/// Public so other pre-auth code (e.g. the reCAPTCHA `remote_ip` hint)
/// derives the client address the same, spoof-resistant way.
pub fn client_ip_from_request(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<IpAddr> {
    client_ip(headers, peer, trusted_proxies())
}

/// Resolve the client IP: the **rightmost** `X-Forwarded-For` hop that
/// is not in `trusted` (hops left of it are client-controlled and are
/// never consulted). If every parseable hop is trusted, the leftmost
/// trusted hop is used. An unparseable hop met while walking leftwards
/// stops the walk (it can only be client-supplied junk). The socket peer
/// address is used only when there is no usable XFF hop at all. `None`
/// when neither is present — callers treat that as "can't identify →
/// allow".
fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trusted: &[Cidr]) -> Option<IpAddr> {
    let mut last_trusted: Option<IpAddr> = None;
    // Multiple XFF header lines are concatenated in order (RFC 7230 3.2.2).
    let hops: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .collect();
    for hop in hops.iter().rev() {
        let Ok(ip) = hop.trim().parse::<IpAddr>() else {
            break;
        };
        if trusted.iter().any(|c| c.contains(ip)) {
            last_trusted = Some(ip);
            continue;
        }
        return Some(ip);
    }
    last_trusted.or_else(|| peer.map(|p| p.ip()))
}

/// Extract a **verified** `user_id` from the request's auth headers, or
/// `None` when there is no token, the token is malformed/expired, or the
/// signature doesn't check out. Mirrors `routes::auth`'s header
/// precedence: `x-krabarena-token` first, then `Authorization: Bearer`.
///
/// Only a cryptographically valid token is trusted. An attacker who
/// could supply an arbitrary unverified `sub` would otherwise (a) hand
/// themselves a fresh per-user budget on demand and (b) inflate Redis
/// key cardinality at will — a memory-DoS on Memorystore. An invalid or
/// absent token simply falls through to IP-based keying (and the request
/// itself will 401 downstream anyway).
fn verified_user_id(headers: &HeaderMap, jwt_secret: &str) -> Option<Uuid> {
    let token = headers
        .get("x-krabarena-token")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })?;
    crate::services::auth_service::validate_jwt(token, jwt_secret)
        .ok()
        .map(|c| c.sub)
}

/// Build the Redis key (and thus the throttle subject) for a request.
///
/// - **Global** prefers the verified `user_id`, falling back to IP.
/// - **Strict** is pre-auth: any `uid` is ignored and the key is always
///   IP-based.
///
/// Returns `None` when there is nothing to key on (global tier, no token
/// *and* no identifiable IP) — callers treat that as "can't identify →
/// allow". Namespaces are disjoint (`uid:` vs `ip:`) so the two subject
/// kinds can never collide.
fn rate_key(tier: Tier, uid: Option<Uuid>, ip: Option<IpAddr>) -> Option<String> {
    match tier {
        Tier::Global => match uid {
            Some(u) => Some(format!("rl:global:uid:{u}")),
            None => ip.map(|ip| format!("rl:global:ip:{ip}")),
        },
        Tier::Strict => ip.map(|ip| format!("rl:strict:ip:{ip}")),
        // Only reached for anonymous proxy calls (authenticated ones skip
        // the middleware bucket) — share the global IP bucket.
        Tier::Proxy => rate_key(Tier::Global, uid, ip),
    }
}

/// Redis key for the per-battle proxy bucket. Disjoint `rl:proxy:`
/// namespace so it can never collide with the global/strict buckets.
fn proxy_rate_key(battle_id: Uuid, user_id: Uuid) -> String {
    format!("rl:proxy:battle:{battle_id}:uid:{user_id}")
}

/// Redis key for the per-user unresolvable-proxy-call bucket. Disjoint
/// `rl:proxymiss:` namespace.
fn proxy_miss_key(user_id: Uuid) -> String {
    format!("rl:proxymiss:uid:{user_id}")
}

/// Read a fixed-window counter without incrementing it. Returns
/// `[count, ttl_seconds]` (`count` 0 when the key is absent).
const PEEK_LUA: &str = r#"
local current = tonumber(redis.call("GET", KEYS[1]) or "0")
local ttl = redis.call("TTL", KEYS[1])
return { current, ttl }
"#;

/// True when `count` recorded misses have used up the `limit`.
fn miss_bucket_full(count: i64, limit: u32) -> bool {
    count >= i64::from(limit)
}

/// Effective per-user requests/minute for a battle's proxy. A positive
/// `rate_limit_rpm` from the spec wins; `0` (or a negative value that
/// slipped past validation) falls back to the platform's global budget
/// so a proxy is never unthrottled.
pub fn effective_proxy_rpm(spec_rpm: i32, global_per_min: u32) -> u32 {
    if spec_rpm > 0 {
        spec_rpm as u32
    } else {
        global_per_min
    }
}

/// Outcome of [`RateLimiter::check_proxy`].
pub enum ProxyLimitDecision {
    /// Under the limit — stamp these `RateLimit-*` values on the response.
    Allowed {
        limit: u32,
        remaining: i64,
        reset: i64,
    },
    /// Over the limit — return this 429 as-is.
    Limited(Response),
    /// Redis unavailable — fail open, no headers.
    Unavailable,
}

impl RateLimiter {
    /// Configured global per-minute budget (the fallback when a battle's
    /// spec has `rate_limit_rpm: 0`).
    pub fn global_per_min(&self) -> u32 {
        self.limits.global_per_min
    }

    /// Enforce a battle's `proxy_spec.rate_limit_rpm` for one user.
    /// Fixed 60s window keyed by battle + user, so every service of the
    /// battle shares one budget and different battles don't interfere.
    /// Fails open (like the middleware) on any Redis error.
    pub async fn check_proxy(
        &self,
        battle_id: Uuid,
        user_id: Uuid,
        spec_rpm: i32,
    ) -> ProxyLimitDecision {
        let limit = effective_proxy_rpm(spec_rpm, self.limits.global_per_min);
        let key = proxy_rate_key(battle_id, user_id);
        match run_window(&self.pool, &key, limit, self.limits.window_secs).await {
            Ok((count, ttl)) if count > limit as i64 => {
                ProxyLimitDecision::Limited(rate_limited_response_with(
                    limit,
                    ttl,
                    false,
                    &format!(
                        "Battle proxy rate limit exceeded: this battle allows {limit} \
                         requests/min per user (all services combined) — retry after the \
                         Retry-After interval"
                    ),
                ))
            }
            Ok((count, ttl)) => ProxyLimitDecision::Allowed {
                limit,
                remaining: (limit as i64 - count).max(0),
                reset: ttl,
            },
            Err(e) => {
                tracing::warn!(error = %e, key = %key, "proxy rate_limit: redis unavailable — failing open");
                ProxyLimitDecision::Unavailable
            }
        }
    }
}

impl RateLimiter {
    /// 429 when this user has already made `global_per_min`
    /// unresolvable proxy calls (unknown battle, no proxy spec, unknown
    /// service) in the current window. Checked before the handler
    /// touches the DB; read-only. Fails open on Redis errors.
    pub async fn proxy_miss_limited(&self, user_id: Uuid) -> Option<Response> {
        let limit = self.limits.global_per_min;
        let key = proxy_miss_key(user_id);
        let peek = async {
            let mut conn = self.pool.get().await?;
            let res: (i64, i64) = redis::Script::new(PEEK_LUA)
                .key(&key)
                .invoke_async(&mut conn)
                .await?;
            anyhow::Ok(res)
        };
        match peek.await {
            Ok((count, ttl)) if miss_bucket_full(count, limit) => Some(rate_limited_response_with(
                limit,
                ttl.max(1),
                false,
                &format!(
                    "Too many proxy calls to unknown battles or services ({limit}/min) — \
                         check the battle id and service name, then retry after the \
                         Retry-After interval"
                ),
            )),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(error = %e, key = %key, "proxy miss rate_limit: redis unavailable — failing open");
                None
            }
        }
    }

    /// Count one unresolvable proxy call against the user's miss bucket.
    pub async fn record_proxy_miss(&self, user_id: Uuid) {
        let key = proxy_miss_key(user_id);
        if let Err(e) = run_window(
            &self.pool,
            &key,
            self.limits.global_per_min,
            self.limits.window_secs,
        )
        .await
        {
            tracing::warn!(error = %e, key = %key, "proxy miss rate_limit: redis unavailable");
        }
    }
}

/// Stamp `RateLimit-*` headers (public wrapper for the proxy handler).
pub fn apply_ratelimit_headers(h: &mut HeaderMap, limit: u32, remaining: i64, reset: i64) {
    set_ratelimit_headers(h, limit, remaining, reset);
}

/// axum middleware. Registered via `from_fn_with_state` so it can read
/// the (optional) limiter out of `AppState`.
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    // Disabled in dev (REDIS_URL unset) → transparent pass-through.
    let Some(limiter) = state.rate_limiter.clone() else {
        return next.run(req).await;
    };

    // Static assets / favicon are never rate limited (they load on every
    // page view and have no abuse-sensitive side effects).
    if is_exempt(req.uri().path()) {
        return next.run(req).await;
    }

    // Socket peer addr, set by `into_make_service_with_connect_info`.
    // Read straight from extensions (rather than the `ConnectInfo`
    // extractor) so the middleware also works when the router is driven
    // without connect-info wiring — e.g. in-process `oneshot` tests.
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);

    let tier = classify(req.uri().path());

    // Strict tier runs pre-auth, so never trust a `user_id` there; only
    // the global/proxy tiers resolve one (and pay the per-request JWT verify).
    let uid = match tier {
        Tier::Global | Tier::Proxy => verified_user_id(req.headers(), &state.jwt_secret),
        Tier::Strict => None,
    };

    // Authenticated battle-proxy calls are NOT counted against the
    // global bucket — the proxy handler enforces the battle's own
    // `rate_limit_rpm` per user instead (see `RateLimiter::check_proxy`).
    if skips_global_bucket(tier, uid) {
        return next.run(req).await;
    }

    let limit = match tier {
        Tier::Strict => limiter.limits.strict_per_min,
        Tier::Global | Tier::Proxy => limiter.limits.global_per_min,
    };
    let ip = client_ip_from_request(req.headers(), peer);
    let Some(key) = rate_key(tier, uid, ip) else {
        // No identifiable subject (no token, no IP) → don't penalize; allow.
        return next.run(req).await;
    };
    let window = limiter.limits.window_secs;

    // Capture the desired 429 representation before the request is moved
    // into `next.run` — a browser navigation gets HTML, everything else
    // JSON.
    let wants_html = accepts_html(req.headers());

    // Redis error: Global/Proxy fail OPEN (availability); Strict falls
    // back to the in-memory window so auth brute force stays throttled.
    let outcome = match run_window(&limiter.pool, &key, limit, window).await {
        Ok(v) => v,
        Err(e) if tier == Tier::Strict => {
            tracing::warn!(error = %e, key = %key, "rate_limit: redis unavailable — strict tier using in-memory fallback");
            limiter.strict_fallback.hit(&key, window)
        }
        Err(e) => {
            tracing::warn!(error = %e, key = %key, "rate_limit: redis unavailable — failing open");
            return next.run(req).await;
        }
    };

    let (count, ttl) = outcome;
    if count > limit as i64 {
        return rate_limited_response(limit, ttl, wants_html);
    }

    let mut resp = next.run(req).await;
    let remaining = (limit as i64 - count).max(0);
    set_ratelimit_headers(resp.headers_mut(), limit, remaining, ttl);
    resp
}

/// True when the middleware must let the request through without
/// touching any bucket: an authenticated battle-proxy call, which is
/// throttled by the battle's own limit in the handler.
fn skips_global_bucket(tier: Tier, uid: Option<Uuid>) -> bool {
    tier == Tier::Proxy && uid.is_some()
}

/// Run the atomic window script. Returns `(count_after_incr,
/// ttl_seconds)`. `redis::Script` transparently uses `EVALSHA` with an
/// `EVAL` fallback, so the script body is cached server-side.
async fn run_window(pool: &Pool, key: &str, limit: u32, window: u32) -> anyhow::Result<(i64, i64)> {
    let mut conn = pool.get().await?;
    let res: (i64, i64) = redis::Script::new(LUA)
        .key(key)
        .arg(limit)
        .arg(window)
        .invoke_async(&mut conn)
        .await?;
    Ok(res)
}

/// 429 response carrying `Retry-After` + `RateLimit-*` headers. The body
/// is content-negotiated: a browser navigation (`wants_html`) gets a
/// friendly auto-refreshing HTML page; everything else gets the standard
/// `ApiErrorResponse` JSON envelope used across the API. Built inline
/// (rather than via `ApiError::RateLimited`) so it can carry the
/// rate-limit headers.
fn rate_limited_response(limit: u32, ttl: i64, wants_html: bool) -> Response {
    rate_limited_response_with(
        limit,
        ttl,
        wants_html,
        "Too many requests — slow down and retry shortly",
    )
}

fn rate_limited_response_with(limit: u32, ttl: i64, wants_html: bool, message: &str) -> Response {
    let mut resp = if wants_html {
        rate_limited_html(ttl)
    } else {
        let body = ApiErrorResponse {
            error: ApiErrorBody {
                code: "RATE_LIMITED".to_string(),
                message: message.to_string(),
                details: None,
            },
            meta: ResponseMeta::generate(),
        };
        (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response()
    };
    set_ratelimit_headers(resp.headers_mut(), limit, 0, ttl);
    if let Ok(hv) = HeaderValue::from_str(&ttl.max(1).to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, hv);
    }
    resp
}

/// A minimal, self-contained 429 page for browser navigations. A live
/// JS countdown ticks the displayed seconds down and reloads at zero; a
/// `<meta http-equiv="refresh">` is the no-JS fallback (whichever fires
/// first wins — both reload once the limiter's window resets). Inline
/// styles + one inline script only, no sidecar assets (asset routes are
/// exempt anyway). `script-src` has no `'unsafe-inline'`, so the script
/// carries the per-response CSP nonce from `web::security_headers`
/// (the limiter runs inside that layer); without a nonce the countdown
/// is simply blocked and the meta refresh still reloads the page.
///
/// Built as two halves so the inline script (full of literal `{}`)
/// doesn't fight `format!`'s brace-escaping — only the head interpolates.
fn rate_limited_html(ttl: i64) -> Response {
    let secs = ttl.max(1);
    let plural = if secs == 1 { "" } else { "s" };
    let head = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="{secs}">
<title>Too many requests — KrabArena</title>
<style>
  html,body{{height:100%;margin:0}}
  body{{display:flex;align-items:center;justify-content:center;
    background:#0b0b0c;color:#fafafa;text-align:center;padding:24px;
    font:500 16px/1.5 ui-sans-serif,system-ui,-apple-system,"Inter",sans-serif}}
  .card{{max-width:30rem}}
  h1{{font-size:1.4rem;margin:0 0 .5rem}}
  p{{margin:.35rem 0;color:#a1a1aa}}
  .secs{{color:#fafafa;font-variant-numeric:tabular-nums}}
  a{{color:#fafafa}}
</style>
</head>
<body>
  <main class="card">
    <h1>Slow down a moment</h1>
    <p>You've hit KrabArena's request limit.</p>
    <p>This page retries automatically in <span class="secs" id="rl-secs">{secs}</span> <span id="rl-unit">second{plural}</span>.</p>
    <p><a href="/">Return home</a></p>
  </main>
"#
    );
    // Live countdown — separate raw literal so its `{}` need no escaping.
    let nonce = crate::web::security_headers::current_nonce().unwrap_or_default();
    let tail = r#"
  (function () {
    var el = document.getElementById('rl-secs');
    var unit = document.getElementById('rl-unit');
    if (!el) return;
    var n = parseInt(el.textContent, 10);
    if (!isFinite(n)) return;
    var timer = setInterval(function () {
      n -= 1;
      if (n <= 0) { clearInterval(timer); location.reload(); return; }
      el.textContent = String(n);
      if (unit) unit.textContent = (n === 1 ? 'second' : 'seconds');
    }, 1000);
  })();
</script>
</body>
</html>"#;
    (
        StatusCode::TOO_MANY_REQUESTS,
        Html(format!("{head}<script nonce=\"{nonce}\">{tail}")),
    )
        .into_response()
}

fn set_ratelimit_headers(h: &mut HeaderMap, limit: u32, remaining: i64, ttl: i64) {
    let mut set = |name: &'static str, v: String| {
        if let Ok(hv) = HeaderValue::from_str(&v) {
            h.insert(HeaderName::from_static(name), hv);
        }
    };
    set("ratelimit-limit", limit.to_string());
    set("ratelimit-remaining", remaining.max(0).to_string());
    set("ratelimit-reset", ttl.max(0).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 429 page's countdown is an inline script; with no
    /// `'unsafe-inline'` in script-src it must carry the response nonce.
    #[tokio::test]
    async fn rate_limited_html_script_carries_csp_nonce() {
        let resp = crate::web::security_headers::CSP_NONCE
            .scope("test-nonce".to_string(), async { rate_limited_html(5) })
            .await;
        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains(r#"<script nonce="test-nonce">"#), "{html}");
        assert_eq!(html.matches("<script").count(), 1);
    }

    #[test]
    fn classify_strict_prefixes() {
        assert_eq!(classify("/v1/auth/email"), Tier::Strict);
        // NB: `/v1/auth/device/poll` is a deliberate exception (global) —
        // see `classify_device_poll_is_global_not_strict`.
        assert_eq!(classify("/v1/auth/device"), Tier::Strict);
        assert_eq!(classify("/auth/verify"), Tier::Strict);
        assert_eq!(classify("/auth/voter/google/callback"), Tier::Strict);
        assert_eq!(classify("/v1/test/mint-token"), Tier::Strict);
    }

    #[test]
    fn classify_global_default() {
        assert_eq!(classify("/v1/battles"), Tier::Global);
        assert_eq!(classify("/health"), Tier::Global);
        assert_eq!(classify("/"), Tier::Global);
        // Boundary: `/v1/authz` must NOT match the `/v1/auth/` prefix.
        assert_eq!(classify("/v1/authz"), Tier::Global);
        // Boundary: bare `/v1/auth` (no trailing slash) is not a real
        // route and must not be classified strict either.
        assert_eq!(classify("/v1/auth"), Tier::Global);
    }

    #[test]
    fn classify_device_poll_is_global_not_strict() {
        // The device-login poll lives under `/v1/auth/` but is polled
        // every 2s by `krab auth login` — it must escape the strict cap.
        assert_eq!(classify("/v1/auth/device/poll"), Tier::Global);
        assert_eq!(
            classify("/v1/auth/device/poll?session=abc"),
            Tier::Global,
            "query string must not change classification"
        );
        // Sibling auth endpoints stay strict.
        assert_eq!(classify("/v1/auth/email/poll"), Tier::Strict);
        assert_eq!(classify("/v1/auth/device"), Tier::Strict);
    }

    #[test]
    fn exempts_static_and_favicon() {
        assert!(is_exempt("/static/style.css"));
        assert!(is_exempt("/static/fetch-retry.js"));
        assert!(is_exempt("/static/demo/index.html"));
        assert!(is_exempt("/favicon.ico"));
        // Non-assets stay limited.
        assert!(!is_exempt("/"));
        assert!(!is_exempt("/v1/battles"));
        assert!(!is_exempt("/battles/some-slug"));
        // `/staticX` (no slash) must NOT be treated as the static mount.
        assert!(!is_exempt("/staticfoo"));
    }

    #[test]
    fn accepts_html_only_for_browser_navigations() {
        let mk = |a: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::ACCEPT, HeaderValue::from_str(a).unwrap());
            h
        };
        assert!(accepts_html(&mk(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        )));
        assert!(!accepts_html(&mk("*/*"))); // default fetch()
        assert!(!accepts_html(&mk("application/json")));
        assert!(!accepts_html(&HeaderMap::new())); // no Accept at all
    }

    fn headers_with_xff(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_str(v).unwrap());
        h
    }

    fn trusted() -> Vec<Cidr> {
        trusted_proxies_from(None)
    }

    /// Regression: the limiter used to key on the LEFTMOST XFF hop, which
    /// the client controls — sending `X-Forwarded-For: <random>` minted a
    /// fresh bucket per request and bypassed every limit. Through the LB
    /// path Cloud Run sees `<spoofed>, <client>, <lb>`; the real client is
    /// the rightmost non-trusted hop.
    #[test]
    fn client_ip_ignores_spoofed_leftmost_hop_via_lb() {
        let h = headers_with_xff("6.6.6.6, 1.2.3.4, 34.49.91.89");
        assert_eq!(client_ip(&h, None, &trusted()), Some(ip("1.2.3.4")));
        // Google front-end hop appended after the LB is skipped too.
        let h = headers_with_xff("6.6.6.6, 1.2.3.4, 34.49.91.89, 35.191.7.9");
        assert_eq!(client_ip(&h, None, &trusted()), Some(ip("1.2.3.4")));
        let h = headers_with_xff("1.2.3.4, 130.211.3.1");
        assert_eq!(client_ip(&h, None, &trusted()), Some(ip("1.2.3.4")));
    }

    /// Direct `*.run.app` access: Google's front end appends the real
    /// client, so the rightmost hop is it — anything the client put to
    /// its left is ignored.
    #[test]
    fn client_ip_direct_run_app_takes_rightmost_hop() {
        let h = headers_with_xff("9.9.9.9, 8.8.4.4, 5.6.7.8");
        assert_eq!(client_ip(&h, None, &trusted()), Some(ip("5.6.7.8")));
    }

    #[test]
    fn client_ip_spoofed_value_cannot_rotate_key() {
        let a = headers_with_xff("10.0.0.1, 1.2.3.4, 34.49.91.89");
        let b = headers_with_xff("10.0.0.2, 1.2.3.4, 34.49.91.89");
        assert_eq!(
            client_ip(&a, None, &trusted()),
            client_ip(&b, None, &trusted())
        );
    }

    #[test]
    fn client_ip_trims_whitespace() {
        let h = headers_with_xff("   5.6.7.8   , 34.49.91.89 ");
        assert_eq!(client_ip(&h, None, &trusted()), Some(ip("5.6.7.8")));
    }

    #[test]
    fn client_ip_all_trusted_uses_leftmost_trusted() {
        let h = headers_with_xff("35.191.1.1, 34.49.91.89");
        assert_eq!(client_ip(&h, None, &trusted()), Some(ip("35.191.1.1")));
    }

    #[test]
    fn client_ip_honours_extra_trusted_cidrs() {
        let t = trusted_proxies_from(Some("10.0.0.0/8, bogus, 2001:db8::/32"));
        let h = headers_with_xff("1.2.3.4, 10.20.30.40");
        assert_eq!(client_ip(&h, None, &t), Some(ip("1.2.3.4")));
        let h = headers_with_xff("1.2.3.4, 2001:db8::1");
        assert_eq!(client_ip(&h, None, &t), Some(ip("1.2.3.4")));
        // Without the extra CIDR, 10.20.30.40 is itself the client.
        let h = headers_with_xff("1.2.3.4, 10.20.30.40");
        assert_eq!(client_ip(&h, None, &trusted()), Some(ip("10.20.30.40")));
    }

    #[test]
    fn cidr_parse_and_contains() {
        let c = Cidr::parse("130.211.0.0/22").unwrap();
        assert!(c.contains(ip("130.211.3.255")));
        assert!(!c.contains(ip("130.211.4.0")));
        assert!(Cidr::parse("1.2.3.4/33").is_none());
        assert!(Cidr::parse("nope").is_none());
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        assert!(!c.contains(ip("::1")));
    }

    #[test]
    fn client_ip_falls_back_to_peer() {
        let h = HeaderMap::new();
        let peer: SocketAddr = "203.0.113.5:54321".parse().unwrap();
        assert_eq!(
            client_ip(&h, Some(peer), &trusted()),
            Some(ip("203.0.113.5"))
        );
    }

    #[test]
    fn client_ip_malformed_xff_falls_back_to_peer() {
        let h = headers_with_xff("not-an-ip");
        let peer: SocketAddr = "203.0.113.9:1234".parse().unwrap();
        assert_eq!(
            client_ip(&h, Some(peer), &trusted()),
            Some(ip("203.0.113.9"))
        );
    }

    #[test]
    fn client_ip_none_when_nothing_present() {
        assert_eq!(client_ip(&HeaderMap::new(), None, &trusted()), None);
    }

    /// Strict-tier fallback window counts and resets.
    #[test]
    fn memory_window_counts_and_resets() {
        let w = MemoryWindow::new(10);
        let t0 = Instant::now();
        for i in 1..=3 {
            assert_eq!(w.hit_at("k", 60, t0).0, i);
        }
        assert_eq!(w.hit_at("other", 60, t0).0, 1);
        assert_eq!(w.hit_at("k", 60, t0 + Duration::from_secs(61)).0, 1);
    }

    /// A full fallback table fails closed instead of growing unbounded.
    #[test]
    fn memory_window_full_table_fails_closed() {
        let w = MemoryWindow::new(2);
        let t0 = Instant::now();
        w.hit_at("a", 60, t0);
        w.hit_at("b", 60, t0);
        assert!(w.hit_at("c", 60, t0).0 > 1_000);
        // After the windows expire, room frees up again.
        assert_eq!(w.hit_at("c", 60, t0 + Duration::from_secs(61)).0, 1);
    }

    /// Regression: with Redis down the strict (auth) tier used to fail
    /// open, letting a brute-forcer hammer login codes during an outage.
    /// It must now throttle via the in-memory fallback, while the global
    /// tier still fails open.
    #[tokio::test]
    async fn strict_tier_does_not_fail_open_when_redis_down() {
        use axum::{Router, body::Body, routing::any};
        use tower::ServiceExt;

        let pool = deadpool_redis::Config::from_url("redis://127.0.0.1:1")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap();
        let limiter = RateLimiter::new(
            pool,
            TierLimits {
                global_per_min: 2,
                strict_per_min: 3,
                window_secs: 60,
            },
        );
        let state = test_state(limiter, "s");
        let app = Router::new()
            .route("/auth/voter/email/verify", any(|| async { "ok" }))
            .route("/v1/battles", any(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                rate_limit,
            ))
            .with_state(state);
        let req = |uri: &str| {
            Request::builder()
                .uri(uri)
                .header("x-forwarded-for", "198.51.100.7")
                .body(Body::empty())
                .unwrap()
        };
        for _ in 0..3 {
            let r = app
                .clone()
                .oneshot(req("/auth/voter/email/verify"))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
        }
        let r = app
            .clone()
            .oneshot(req("/auth/voter/email/verify"))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        // Global tier: still fail-open past its limit of 2.
        for _ in 0..5 {
            let r = app.clone().oneshot(req("/v1/battles")).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK);
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn rate_key_global_prefers_user_id() {
        let uid = Uuid::nil();
        // user_id present → keyed on uid, IP ignored even when present.
        assert_eq!(
            rate_key(Tier::Global, Some(uid), Some(ip("1.2.3.4"))),
            Some(format!("rl:global:uid:{uid}"))
        );
    }

    #[test]
    fn rate_key_global_falls_back_to_ip_when_anonymous() {
        assert_eq!(
            rate_key(Tier::Global, None, Some(ip("1.2.3.4"))),
            Some("rl:global:ip:1.2.3.4".to_string())
        );
    }

    #[test]
    fn rate_key_global_none_when_no_subject() {
        // Anonymous and no identifiable IP → nothing to key on → allow.
        assert_eq!(rate_key(Tier::Global, None, None), None);
    }

    #[test]
    fn rate_key_strict_is_always_ip_even_with_user_id() {
        let uid = Uuid::nil();
        // Strict tier is pre-auth: a user_id (if somehow present) is
        // ignored and the key is IP-based.
        assert_eq!(
            rate_key(Tier::Strict, Some(uid), Some(ip("9.9.9.9"))),
            Some("rl:strict:ip:9.9.9.9".to_string())
        );
    }

    #[test]
    fn rate_key_strict_none_without_ip() {
        assert_eq!(rate_key(Tier::Strict, Some(Uuid::nil()), None), None);
        assert_eq!(rate_key(Tier::Strict, None, None), None);
    }

    fn headers_with_bearer(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        h
    }

    #[test]
    fn verified_user_id_accepts_valid_token() {
        use crate::services::auth_service;
        let secret = "test-secret";
        let sub = Uuid::new_v4();
        let entity = Uuid::new_v4();
        let token = auth_service::create_jwt(sub, entity, "user", secret).unwrap();

        // Bearer header.
        assert_eq!(
            verified_user_id(&headers_with_bearer(&token), secret),
            Some(sub)
        );
        // And via the x-krabarena-token fallback header.
        let mut h = HeaderMap::new();
        h.insert("x-krabarena-token", HeaderValue::from_str(&token).unwrap());
        assert_eq!(verified_user_id(&h, secret), Some(sub));
    }

    #[test]
    fn verified_user_id_rejects_forged_or_wrong_secret() {
        use crate::services::auth_service;
        let sub = Uuid::new_v4();
        let entity = Uuid::new_v4();
        let token = auth_service::create_jwt(sub, entity, "user", "real-secret").unwrap();

        // Token signed with a different secret must NOT be trusted —
        // otherwise an attacker could forge `sub` and mint budgets.
        assert_eq!(
            verified_user_id(&headers_with_bearer(&token), "attacker-secret"),
            None
        );
        // Garbage token → None.
        assert_eq!(
            verified_user_id(&headers_with_bearer("not.a.jwt"), "real-secret"),
            None
        );
    }

    #[test]
    fn verified_user_id_none_when_no_auth_header() {
        assert_eq!(verified_user_id(&HeaderMap::new(), "secret"), None);
    }

    // ---- Battle proxy tier (0.43.3) -------------------------------------

    /// Regression (2026-10-25): battle proxy calls were classified into the
    /// global tier, so the 120/min global bucket capped real throughput on
    /// battles advertising `rate_limit_rpm: 300` / `600` and agents got
    /// 429s below the advertised limit.
    #[test]
    fn classify_proxy_calls_are_not_global() {
        for p in [
            "/v1/battles/what-is-the-best-web-fetch-api/proxy/keenable/v1/fetch",
            "/v1/battles/0b6c7f2e-1111-2222-3333-444455556666/proxy/exa/search",
            "/v1/battles/b/proxy/tinyfish",
            "/v1/battles/b/proxy/tinyfish/",
        ] {
            assert_eq!(classify(p), Tier::Proxy, "{p}");
        }
        // Sibling metadata routes stay in the global tier.
        assert_eq!(classify("/v1/battles/b/proxy-info"), Tier::Global);
        assert_eq!(classify("/v1/battles/b/proxy-spec"), Tier::Global);
        assert_eq!(classify("/v1/battles/b"), Tier::Global);
        assert_eq!(classify("/v1/battles//proxy/x"), Tier::Global);
        assert_eq!(classify("/battles/b/proxy/x"), Tier::Global);
    }

    #[test]
    fn authenticated_proxy_calls_skip_global_bucket_anonymous_do_not() {
        assert!(skips_global_bucket(Tier::Proxy, Some(Uuid::nil())));
        // Anonymous proxy calls (401 downstream) keep the IP bucket.
        assert!(!skips_global_bucket(Tier::Proxy, None));
        assert_eq!(
            rate_key(Tier::Proxy, None, Some(ip("1.2.3.4"))),
            Some("rl:global:ip:1.2.3.4".to_string())
        );
        assert!(!skips_global_bucket(Tier::Global, Some(Uuid::nil())));
        assert!(!skips_global_bucket(Tier::Strict, Some(Uuid::nil())));
    }

    // Regression (proxy audit 2026-11-19): authenticated proxy calls to
    // nonexistent battles skipped every bucket → unthrottled DB lookups.
    #[test]
    fn proxy_miss_bucket_is_disjoint_and_trips_at_the_global_limit() {
        let u = Uuid::new_v4();
        assert_eq!(proxy_miss_key(u), format!("rl:proxymiss:uid:{u}"));
        assert_ne!(
            Some(proxy_miss_key(u)),
            rate_key(Tier::Global, Some(u), None)
        );
        assert!(!miss_bucket_full(0, 120));
        assert!(!miss_bucket_full(119, 120));
        assert!(miss_bucket_full(120, 120));
    }

    /// Unresolvable proxy calls fill the miss bucket; once full, the
    /// next call is refused before any DB lookup.
    #[tokio::test]
    #[ignore]
    async fn proxy_miss_bucket_limits_after_global_per_min_misses() {
        let limiter = redis_limiter(TierLimits {
            global_per_min: 3,
            strict_per_min: 10,
            window_secs: 60,
        });
        let user = Uuid::new_v4();
        for _ in 0..3 {
            assert!(limiter.proxy_miss_limited(user).await.is_none());
            limiter.record_proxy_miss(user).await;
        }
        let resp = limiter.proxy_miss_limited(user).await.expect("limited");
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(resp.headers().contains_key(header::RETRY_AFTER));
        assert!(limiter.proxy_miss_limited(Uuid::new_v4()).await.is_none());
    }

    /// Fail-open: an unreachable Redis never blocks proxy calls.
    #[tokio::test]
    async fn proxy_miss_bucket_fails_open_without_redis() {
        let pool = deadpool_redis::Config::from_url("redis://127.0.0.1:1")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap();
        let limiter = RateLimiter::new(pool, TierLimits::default());
        let u = Uuid::new_v4();
        limiter.record_proxy_miss(u).await;
        assert!(limiter.proxy_miss_limited(u).await.is_none());
    }

    #[test]
    fn effective_proxy_rpm_prefers_spec_and_falls_back_to_global() {
        assert_eq!(effective_proxy_rpm(600, 120), 600);
        assert_eq!(effective_proxy_rpm(30, 120), 30);
        assert_eq!(effective_proxy_rpm(0, 120), 120);
        assert_eq!(effective_proxy_rpm(-5, 120), 120);
    }

    #[test]
    fn proxy_rate_key_is_per_battle_and_user() {
        let (b1, b2, u) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            proxy_rate_key(b1, u),
            format!("rl:proxy:battle:{b1}:uid:{u}")
        );
        assert_ne!(proxy_rate_key(b1, u), proxy_rate_key(b2, u));
    }

    // ---- Redis-backed (opt-in) ------------------------------------------
    //
    // Need a reachable Redis: `redis-server --port 6390 &` then
    //   KRAB_TEST_REDIS_URL=redis://127.0.0.1:6390 \
    //     cargo test -p krabarena-api --lib rate_limit -- --ignored

    fn redis_limiter(limits: TierLimits) -> RateLimiter {
        let url = std::env::var("KRAB_TEST_REDIS_URL")
            .expect("KRAB_TEST_REDIS_URL must point at a test Redis");
        let pool = deadpool_redis::Config::from_url(url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap();
        RateLimiter::new(pool, limits)
    }

    fn test_state(limiter: RateLimiter, secret: &str) -> AppState {
        AppState {
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgresql://localhost/krabarena_test")
                .expect("lazy pool"),
            jwt_secret: secret.into(),
            blob_store_url: "http://localhost:3001/blob".into(),
            private_blob_store_url: None,
            public_base_url: "http://localhost:3000".into(),
            discord_monitor: crate::services::discord_monitor::DiscordMonitorConfig::from_lookup(
                |_| None,
            ),
            email_service: None,
            gemini: crate::services::gemini_service::GeminiClient::new(None),
            proxy_http: reqwest::Client::new(),
            rate_limiter: Some(limiter),
            leaderboards: std::sync::Arc::new(crate::LeaderboardCaches::new()),
            recaptcha: crate::RecaptchaConfig::default(),
        }
    }

    /// Regression (2026-10-25): with the global bucket at N/min, the
    /// (N+1)th authenticated proxy call used to 429 in the middleware even
    /// though the battle advertised a higher `rate_limit_rpm`. Now the
    /// middleware lets every authenticated proxy call through, while a
    /// non-proxy route from the same user still trips the global cap.
    #[tokio::test]
    #[ignore]
    async fn middleware_does_not_count_proxy_calls_against_global_bucket() {
        use crate::services::auth_service;
        use axum::{Router, body::Body, routing::any};
        use tower::ServiceExt;

        let secret = "rl-test-secret";
        let limiter = redis_limiter(TierLimits {
            global_per_min: 3,
            strict_per_min: 10,
            window_secs: 60,
        });
        let state = test_state(limiter, secret);
        let token =
            auth_service::create_jwt(Uuid::new_v4(), Uuid::new_v4(), "user", secret).unwrap();
        let app = Router::new()
            .route(
                crate::routes::proxy::PROXY_PATH_ROUTE,
                any(|| async { "proxied" }),
            )
            .route("/v1/battles", any(|| async { "list" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                rate_limit,
            ))
            .with_state(state);
        let req = |uri: &str| {
            Request::builder()
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap()
        };

        for i in 0..10 {
            let resp = app
                .clone()
                .oneshot(req("/v1/battles/b/proxy/keenable/v1/fetch"))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "proxy call #{i} throttled");
        }
        // Proxy calls didn't drain the global bucket: 3 non-proxy calls
        // still fit, the 4th trips.
        for _ in 0..3 {
            let resp = app.clone().oneshot(req("/v1/battles")).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        let resp = app.clone().oneshot(req("/v1/battles")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    /// The battle's own `rate_limit_rpm` is enforced per user per battle.
    #[tokio::test]
    #[ignore]
    async fn check_proxy_enforces_battle_rpm_per_user_and_battle() {
        let limiter = redis_limiter(TierLimits {
            global_per_min: 2,
            strict_per_min: 10,
            window_secs: 60,
        });
        let (battle, other_battle, user, other_user) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );

        // rpm 5 > global 2: five calls allowed, the sixth limited.
        for i in 0..5 {
            match limiter.check_proxy(battle, user, 5).await {
                ProxyLimitDecision::Allowed {
                    limit, remaining, ..
                } => {
                    assert_eq!(limit, 5);
                    assert_eq!(remaining, 4 - i);
                }
                _ => panic!("call #{i} should be allowed"),
            }
        }
        match limiter.check_proxy(battle, user, 5).await {
            ProxyLimitDecision::Limited(resp) => {
                assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
                assert!(resp.headers().contains_key(header::RETRY_AFTER));
                assert_eq!(resp.headers()["ratelimit-limit"], "5");
            }
            _ => panic!("sixth call must be limited"),
        }
        // Other user / other battle have their own buckets.
        assert!(matches!(
            limiter.check_proxy(battle, other_user, 5).await,
            ProxyLimitDecision::Allowed { .. }
        ));
        assert!(matches!(
            limiter.check_proxy(other_battle, user, 5).await,
            ProxyLimitDecision::Allowed { .. }
        ));
        // rpm 0 falls back to the global budget (2).
        let u = Uuid::new_v4();
        for _ in 0..2 {
            assert!(matches!(
                limiter.check_proxy(battle, u, 0).await,
                ProxyLimitDecision::Allowed { limit: 2, .. }
            ));
        }
        assert!(matches!(
            limiter.check_proxy(battle, u, 0).await,
            ProxyLimitDecision::Limited(_)
        ));
    }

    /// Fail-open: an unreachable Redis lets proxy calls through.
    #[tokio::test]
    async fn check_proxy_fails_open_without_redis() {
        let pool = deadpool_redis::Config::from_url("redis://127.0.0.1:1")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .unwrap();
        let limiter = RateLimiter::new(pool, TierLimits::default());
        assert!(matches!(
            limiter.check_proxy(Uuid::new_v4(), Uuid::new_v4(), 5).await,
            ProxyLimitDecision::Unavailable
        ));
    }
}
