use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Proxy spec stored in battles.proxy_spec JSONB column.
/// Never exposed to claimers — secrets are injected server-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxySpec {
    pub services: HashMap<String, ServiceConfig>,
    #[serde(default)]
    pub budget_cents: i32,
    #[serde(default)]
    pub per_claim_cents: i32,
    #[serde(default = "default_rpm")]
    pub rate_limit_rpm: i32,
}

fn default_rpm() -> i32 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub upstream: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Cost per **1000 requests** in cents (for budget tracking). 0 = free.
    /// Sub-cent per-request pricing is the norm for modern search APIs
    /// (e.g. Exa at $7/1K = `cost_per_1k_cents: 700`), so the spec
    /// uses cents-per-thousand instead of cents-per-request and the
    /// runtime budget is computed by dividing by 1000 at read time.
    /// Old specs that used `cost_per_request_cents` deserialize via
    /// the serde alias and are interpreted as the same numeric value
    /// — `cost_per_request_cents: 1` (1¢/req) becomes
    /// `cost_per_1k_cents: 1` (1¢/1K), a 1000× under-bill that is
    /// strictly safer than the prior over-bill at sub-cent prices.
    #[serde(default, alias = "cost_per_request_cents")]
    pub cost_per_1k_cents: i32,
    /// Human-readable pricing note (e.g., "~$0.001 per search query")
    #[serde(default)]
    pub pricing_note: Option<String>,
    /// Link to the upstream provider's API docs — e.g.
    /// `https://docs.keenable.ai/search`. REQUIRED on write
    /// (enforced by `ProxySpec::validate`), `Option` on the struct so
    /// existing stored specs without it still deserialize on read.
    /// Exposed in the public `proxy-info` projection so Claimers can
    /// jump straight to the provider's docs for the service they're
    /// calling through KrabArena.
    #[serde(default)]
    pub docs_url: Option<String>,
    /// Optional actual-cost billing. When present, each request is
    /// charged what the upstream reports it cost (e.g. Monid
    /// `cost.value`, OpenRouter `usage.cost`, Exa `costDollars.total`)
    /// instead of the flat `cost_per_1k_cents / 1000`, which stays the
    /// fallback when the reported cost is missing or unparseable.
    /// Absent = today's flat billing, unchanged. Skipped on serialize
    /// when absent so stored specs without it round-trip byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing: Option<BillingConfig>,
    /// Optional AWS SigV4 request signing, for upstreams that take IAM
    /// credentials instead of a static API key (e.g. Amazon Bedrock
    /// AgentCore Web Search). When present, every forwarded request is
    /// signed with these credentials on top of any static `headers`.
    /// Secret, like `headers` — never in the public projection. Skipped
    /// on serialize when absent so existing specs round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_sigv4: Option<AwsSigV4Config>,
    /// Optional HTTP-method allowlist (e.g. `["GET", "POST"]`). Empty /
    /// absent = every method is forwarded (legacy behaviour). Without
    /// it a Claimer can call any method — `DELETE`, `PUT` on account
    /// APIs — with the sponsor's key. Skipped on serialize when empty
    /// so existing specs round-trip unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_methods: Vec<String>,
    /// Optional path allowlist. Each entry starts with `/` and is
    /// either a prefix matched on a segment boundary (`/v1/search`
    /// allows `/v1/search` and `/v1/search/x`, not `/v1/searchx`) or a
    /// glob where `*` matches any run of characters, including `/`
    /// (`/v3/*`). Matched against the path after `/proxy/<service>`,
    /// query string excluded. Empty / absent = every path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_paths: Vec<String>,
}

/// Methods accepted in `allowed_methods`.
const KNOWN_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// `*` matches any (possibly empty) run of characters, `/` included.
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// One `allowed_paths` entry against a request path (leading `/`).
fn path_pattern_matches(pattern: &str, path: &str) -> bool {
    if pattern.contains('*') {
        return glob_match(pattern, path);
    }
    let prefix = pattern.trim_end_matches('/');
    if prefix.is_empty() {
        return true; // "/" allows everything
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl ServiceConfig {
    /// Enforce `allowed_methods` / `allowed_paths` for one proxied
    /// request. `path` is the tail after `/proxy/<service>/` (no
    /// leading slash, as captured by the route). `Err` carries a
    /// Claimer-facing message listing what is allowed.
    pub fn check_endpoint_allowed(
        &self,
        service: &str,
        method: &str,
        path: &str,
    ) -> Result<(), String> {
        let full = format!("/{}", path.trim_start_matches('/'));
        let method_ok = self.allowed_methods.is_empty()
            || self
                .allowed_methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case(method));
        let path_ok = self.allowed_paths.is_empty()
            || self
                .allowed_paths
                .iter()
                .any(|p| path_pattern_matches(p, &full));
        if method_ok && path_ok {
            return Ok(());
        }
        let list = |v: &[String]| {
            if v.is_empty() {
                "any".to_string()
            } else {
                v.join(", ")
            }
        };
        Err(format!(
            "{method} {full} is not allowed on proxy service '{service}' — allowed methods: {}; \
             allowed paths: {}",
            list(&self.allowed_methods),
            list(&self.allowed_paths)
        ))
    }

    fn validate_allowlists(&self, service: &str) -> Result<(), String> {
        for m in &self.allowed_methods {
            if !KNOWN_METHODS.iter().any(|k| k.eq_ignore_ascii_case(m)) {
                return Err(format!(
                    "proxy_spec.services.{service}.allowed_methods has unknown method '{m}' \
                     (use {})",
                    KNOWN_METHODS.join(", ")
                ));
            }
        }
        for p in &self.allowed_paths {
            let bad = !p.starts_with('/')
                || p.chars()
                    .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '?' | '#' | '\\'))
                || p.split('/').any(|seg| seg == ".." || seg == ".");
            if bad {
                return Err(format!(
                    "proxy_spec.services.{service}.allowed_paths entry '{p}' is invalid — \
                     use an absolute path prefix or glob like '/v1/search' or '/v3/*' \
                     (no query string, no '..')"
                ));
            }
        }
        Ok(())
    }
}

/// IAM credentials + signing scope for [`ServiceConfig::aws_sigv4`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AwsSigV4Config {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// AWS region of the upstream endpoint, e.g. `us-east-1`.
    pub region: String,
    /// SigV4 signing name, e.g. `bedrock-agentcore`.
    pub service: String,
    /// Only for temporary (STS) credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
}

impl AwsSigV4Config {
    fn validate(&self, service: &str) -> Result<(), String> {
        for (field, v) in [
            ("access_key_id", &self.access_key_id),
            ("secret_access_key", &self.secret_access_key),
            ("region", &self.region),
            ("service", &self.service),
        ] {
            if v.trim().is_empty() {
                return Err(format!(
                    "proxy_spec.services.{service}.aws_sigv4.{field} is required"
                ));
            }
        }
        Ok(())
    }
}

/// Where a usage-priced upstream reports the real price of a call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BillingSource {
    /// Cost is a number (or numeric string) inside the JSON response
    /// body — or inside the last SSE `data:` chunk for streamed bodies.
    ResponseJson,
    /// Cost is the value of a response header.
    ResponseHeader,
}

impl BillingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            BillingSource::ResponseJson => "response_json",
            BillingSource::ResponseHeader => "response_header",
        }
    }
}

/// Unit of the number found at `cost_path` / in `header`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CostUnit {
    /// Dollars (Monid `cost.value`, OpenRouter `usage.cost`, Exa
    /// `costDollars.total`). The default.
    #[default]
    Usd,
    Cents,
    MicroUsd,
}

/// One JSON path or a list of alternatives (first that resolves wins).
/// Paths are dot-separated; numeric segments index into arrays
/// (`choices.0.cost`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum JsonPaths {
    One(String),
    Many(Vec<String>),
}

impl JsonPaths {
    pub fn paths(&self) -> Vec<&str> {
        match self {
            JsonPaths::One(p) => vec![p.as_str()],
            JsonPaths::Many(ps) => ps.iter().map(String::as_str).collect(),
        }
    }

    fn validate(&self, field: &str) -> Result<(), String> {
        let paths = self.paths();
        if paths.is_empty() {
            return Err(format!("{field} must name at least one JSON path"));
        }
        for p in paths {
            if p.is_empty() || p.split('.').any(str::is_empty) {
                return Err(format!(
                    "{field} has an invalid JSON path '{p}' — use dot-separated keys, \
                     e.g. 'cost.value' or 'usage.cost'"
                ));
            }
        }
        Ok(())
    }
}

/// A `cost_path` entry with its own unit, for upstreams that report
/// the same cost in different units on different endpoints (Monid:
/// `POST /v1/run` → `billing.reportedCost.value` in micro-dollars,
/// `GET /v1/runs/{id}` → `cost.value` in dollars).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CostPathWithUnit {
    pub path: String,
    pub unit: CostUnit,
}

/// One `cost_path` entry: a bare path (uses the block's `unit`) or
/// `{"path": …, "unit": …}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum CostPath {
    Plain(String),
    WithUnit(CostPathWithUnit),
}

/// `cost_path`: one entry or a list of alternatives (first that
/// resolves wins).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum CostPaths {
    One(CostPath),
    Many(Vec<CostPath>),
}

impl CostPaths {
    /// `(path, unit override)` in priority order.
    pub fn entries(&self) -> Vec<(&str, Option<CostUnit>)> {
        fn one(c: &CostPath) -> (&str, Option<CostUnit>) {
            match c {
                CostPath::Plain(p) => (p.as_str(), None),
                CostPath::WithUnit(w) => (w.path.as_str(), Some(w.unit)),
            }
        }
        match self {
            CostPaths::One(c) => vec![one(c)],
            CostPaths::Many(cs) => cs.iter().map(one).collect(),
        }
    }

    fn validate(&self, field: &str) -> Result<(), String> {
        let paths = self
            .entries()
            .into_iter()
            .map(|(p, _)| p.to_string())
            .collect();
        JsonPaths::Many(paths).validate(field)
    }
}

/// `services.<name>.billing` — see docs/proxy-spec.md "Actual-cost billing".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BillingConfig {
    #[serde(rename = "type")]
    pub source: BillingSource,
    /// JSON path(s) to the cost — required for `response_json`. Entries
    /// may carry their own unit: `{"path": "...", "unit": "micro_usd"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_path: Option<CostPaths>,
    /// Header carrying the cost — required for `response_header`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default)]
    pub unit: CostUnit,
    /// Currency the cost is denominated in. Only `USD` is supported
    /// (budgets are USD cents).
    #[serde(default = "default_currency")]
    pub currency: String,
    /// Optional JSON path to the response's own currency code (Monid
    /// `cost.currency`). When it resolves to something other than
    /// `currency`, the reported cost is ignored and the flat rate is
    /// charged instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency_path: Option<String>,
    /// JSON path(s) to an id that identifies the billable unit (Monid
    /// `runId`). Each key is charged once per battle+service, so a poll
    /// that repeats a run's cost is not billed again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key_path: Option<JsonPaths>,
    /// Hard ceiling for a single request's charge, in cents. Reported
    /// costs above it are clamped (and logged). Also the amount held
    /// against the budget while the upstream call is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_request_cents: Option<f64>,
}

/// Upper bound for `max_per_request_cents`: $100. Matches the sanity
/// ceiling `proxy_billing` applies to upstream-reported costs.
pub const MAX_PER_REQUEST_CENTS_CEILING: f64 = 10_000.0;

fn default_currency() -> String {
    "USD".into()
}

impl BillingConfig {
    fn validate(&self, service: &str) -> Result<(), String> {
        let at = format!("proxy_spec.services.{service}.billing");
        match self.source {
            BillingSource::ResponseJson => {
                let paths = self.cost_path.as_ref().ok_or_else(|| {
                    format!(
                        "{at}.cost_path is required for type 'response_json' \
                         (e.g. 'cost.value', 'usage.cost', 'costDollars.total')"
                    )
                })?;
                paths.validate(&format!("{at}.cost_path"))?;
                if self.header.is_some() {
                    return Err(format!(
                        "{at}.header only applies to type 'response_header'"
                    ));
                }
            }
            BillingSource::ResponseHeader => {
                match self.header.as_deref().map(str::trim) {
                    None | Some("") => {
                        return Err(format!(
                            "{at}.header is required for type 'response_header'"
                        ));
                    }
                    Some(h) if axum::http::HeaderName::from_bytes(h.as_bytes()).is_err() => {
                        return Err(format!("{at}.header '{h}' is not a valid header name"));
                    }
                    _ => {}
                }
                if self.cost_path.is_some() {
                    return Err(format!(
                        "{at}.cost_path only applies to type 'response_json'"
                    ));
                }
            }
        }
        if !self.currency.eq_ignore_ascii_case("USD") {
            return Err(format!(
                "{at}.currency '{}' is not supported — only 'USD' (budgets are USD cents)",
                self.currency
            ));
        }
        if let Some(p) = &self.currency_path {
            JsonPaths::One(p.clone()).validate(&format!("{at}.currency_path"))?;
        }
        if let Some(p) = &self.dedupe_key_path {
            p.validate(&format!("{at}.dedupe_key_path"))?;
        }
        if let Some(cap) = self.max_per_request_cents
            && !(cap.is_finite() && cap > 0.0)
        {
            return Err(format!(
                "{at}.max_per_request_cents must be a positive number"
            ));
        }
        if let Some(cap) = self.max_per_request_cents
            && cap > MAX_PER_REQUEST_CENTS_CEILING
        {
            return Err(format!(
                "{at}.max_per_request_cents must be at most {MAX_PER_REQUEST_CENTS_CEILING} \
                 ($100 per request)"
            ));
        }
        Ok(())
    }
}

/// Reject upstreams that would turn the proxy into an SSRF primitive:
/// the server injects the sponsor's secret headers and returns the
/// upstream body to the caller, so an upstream of e.g.
/// `http://metadata.google.internal` + `Metadata-Flavor: Google` would
/// hand out the Cloud Run service-account token. Requires https and a
/// public DNS name or public IP literal. (DNS names that *resolve* to
/// private addresses are not caught here; the proxy client also refuses
/// to follow redirects, so a public upstream can't bounce inward.)
pub fn check_upstream_is_public(upstream: &str) -> Result<(), String> {
    check_url_is_public(upstream, true)
}

/// Shared SSRF guard for any server-side fetch of a user-supplied URL
/// (proxy upstreams, webhook targets, avatar URLs). `require_https`
/// false still allows only http/https.
pub fn check_url_is_public(upstream: &str, require_https: bool) -> Result<(), String> {
    let url = reqwest::Url::parse(upstream).map_err(|e| format!("is not a valid URL: {e}"))?;
    let scheme_ok = match url.scheme() {
        "https" => true,
        "http" => !require_https,
        _ => false,
    };
    if !scheme_ok {
        return Err(format!(
            "must use {} (got '{upstream}')",
            if require_https { "https" } else { "http(s)" }
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not embed credentials; use `headers`".into());
    }
    let host = url.host_str().unwrap_or("");
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let blocked = match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            let o = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT 100.64/10
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.to_ipv4_mapped().is_some()
        }
        Err(_) => {
            let d = bare.trim_end_matches('.').to_ascii_lowercase();
            d.is_empty()
                || d == "localhost"
                || d.ends_with(".localhost")
                || d.ends_with(".internal")
                || d.ends_with(".local")
                || !d.contains('.')
        }
    };
    if blocked {
        return Err(format!(
            "must be a public host (got '{upstream}'); private, loopback, link-local and \
             metadata addresses are not allowed"
        ));
    }
    Ok(())
}

impl ProxySpec {
    /// Validate the parts of an already-deserialized spec that serde
    /// can't (every service's `billing`, `aws_sigv4`, `allowed_methods`
    /// and `allowed_paths`). Shared by
    /// [`ProxySpec::validate`] and the `PUT /proxy-spec` route.
    pub fn validate_billing(&self) -> Result<(), String> {
        for (name, svc) in &self.services {
            if let Some(b) = &svc.billing {
                b.validate(name)?;
            }
            if let Some(a) = &svc.aws_sigv4 {
                a.validate(name)?;
            }
            svc.validate_allowlists(name)?;
        }
        Ok(())
    }

    /// Round-trip parse an untyped JSON value into `ProxySpec` and
    /// enforce the invariants that the runtime consumer
    /// (`routes/proxy.rs`) assumes. Called from the create/edit paths
    /// so a bad shape fails at write time with a clean 400 instead of
    /// getting stored as-is and exploding on `GET /proxy-info` or on
    /// the first proxied request.
    ///
    /// Canonical shape: https://krabarena.com/docs/proxy-spec
    pub fn validate(raw: &serde_json::Value) -> Result<Self, String> {
        let spec: ProxySpec = serde_json::from_value(raw.clone()).map_err(|e| {
            format!(
                "invalid proxy_spec shape: {e} \
                 (schema: https://krabarena.com/docs/proxy-spec)"
            )
        })?;
        if spec.services.is_empty() {
            return Err("proxy_spec.services must have at least one entry".into());
        }
        for (name, svc) in &spec.services {
            if name.is_empty() {
                return Err("proxy_spec.services has an empty service name".into());
            }
            if svc.upstream.is_empty() {
                return Err(format!(
                    "proxy_spec.services.{name}.upstream is required (base URL the proxy forwards to)"
                ));
            }
            if !svc.upstream.starts_with("https://") && !svc.upstream.starts_with("http://") {
                return Err(format!(
                    "proxy_spec.services.{name}.upstream must be an absolute URL with scheme \
                     (got '{}'); e.g. 'https://api.keenable.ai'",
                    svc.upstream
                ));
            }
            check_upstream_is_public(&svc.upstream)
                .map_err(|e| format!("proxy_spec.services.{name}.upstream {e}"))?;
            // `docs_url` required on write so Claimers always have a
            // pointer to the provider's API docs. If you're asking
            // someone to make API calls via this proxy, you should
            // hand them the docs — low cost, high clarity.
            match svc.docs_url.as_deref() {
                None | Some("") => {
                    return Err(format!(
                        "proxy_spec.services.{name}.docs_url is required — set it to the \
                         provider's public API docs URL (e.g. 'https://docs.keenable.ai/search')"
                    ));
                }
                Some(url) if !url.starts_with("https://") && !url.starts_with("http://") => {
                    return Err(format!(
                        "proxy_spec.services.{name}.docs_url must be an absolute URL with \
                         scheme (got '{url}')"
                    ));
                }
                _ => {}
            }
        }
        if spec.budget_cents < 0 || spec.per_claim_cents < 0 || spec.rate_limit_rpm < 0 {
            return Err("proxy_spec numeric fields must be non-negative".into());
        }
        spec.validate_billing()?;
        Ok(spec)
    }
}

/// Public info about a battle's proxy (no secrets).
///
/// `services` is kept as `Vec<String>` for wire compatibility with
/// the existing web template and any CLI consumers that iterate
/// names. `service_details` is the richer per-service projection
/// added in 0.4.3 so Claimers can get to `docs_url` and
/// `pricing_note` without guessing.
#[derive(Debug, Serialize)]
pub struct ProxyInfo {
    pub services: Vec<String>,
    pub service_details: Vec<ServicePublicInfo>,
    pub budget_cents: i32,
    pub per_claim_cents: i32,
    /// Effective proxy rate limit in requests/minute: the spec's
    /// `rate_limit_rpm`, or the platform default (120) when the spec
    /// sets `0`. Enforced per user per battle — see `rate_limit_scope`.
    pub rate_limit_rpm: i32,
    /// Human-readable semantics of `rate_limit_rpm` (since 0.43.3).
    pub rate_limit_scope: &'static str,
    pub spent_cents: i32,
    pub remaining_cents: i32,
    pub total_requests: i32,
    /// Precise total spend in dollars (sub-cent precision). Includes
    /// actual upstream-reported costs for services with a `billing`
    /// block and in-flight reservations. `spent_cents` is this value
    /// floored to whole cents.
    pub spent_usd: f64,
}

/// Per-service public info exposed to Claimers via `proxy-info`.
/// Secrets (`headers`, `upstream`) are NOT included — only what a
/// Claimer needs to decide whether to use the service and how to
/// call it.
#[derive(Debug, Clone, Serialize)]
pub struct ServicePublicInfo {
    pub name: String,
    pub docs_url: Option<String>,
    pub pricing_note: Option<String>,
    /// Cost per 1000 requests, in cents. Mirrors
    /// `ServiceConfig.cost_per_1k_cents` so the public projection
    /// uses the same unit as the spec — no conversions on either side.
    pub cost_per_1k_cents: i32,
    /// Number of requests proxied for this service on this battle so
    /// far. Aggregated from `proxy_logs` at read time.
    pub request_count: i64,
    /// Spend in cents for this service so far. Flat-billed services:
    /// `request_count * cost_per_1k_cents / 1000`. Services with a
    /// `billing` block: the sum of actual upstream-reported costs (plus
    /// the flat rate for any older requests made before billing was
    /// enabled). Float-derived to keep sub-cent precision; the template
    /// formats with two/four decimal places.
    pub spent_cents: f64,
    /// Same as `spent_cents`, in dollars.
    pub spent_usd: f64,
    /// `"response_json"` / `"response_header"` when this service is
    /// billed at the upstream-reported actual cost; absent for flat
    /// per-request billing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing: Option<&'static str>,
    /// Requests charged at an upstream-reported cost (actual or capped).
    pub actual_cost_requests: i64,
}

/// Database row for proxy_logs.
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct ProxyLogRow {
    pub id: Uuid,
    pub battle_id: Uuid,
    pub user_id: Uuid,
    pub claim_id: Option<Uuid>,
    pub service_name: String,
    pub method: String,
    pub path: String,
    pub request_body_hash: Option<String>,
    pub response_status: i32,
    pub response_body_size: Option<i32>,
    pub latency_ms: Option<i32>,
    pub cost_cents: Option<i32>,
    pub created_at: DateTime<Utc>,
}

/// Database row for proxy_budgets. The table is still written by
/// `log_request` for back-compat (consumers reading the column
/// directly), but accounting authoritative reads now go through
/// `proxy_logs` aggregation in `proxy_service::get_proxy_info`.
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)]
pub struct ProxyBudgetRow {
    pub battle_id: Uuid,
    pub total_spent_cents: i32,
    pub total_requests: i32,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Regression guard (pre-open-source security review, 2026-11-19):
    // an upstream of http://metadata.google.internal + Metadata-Flavor
    // header turned the proxy into a service-account-token oracle.
    #[test]
    fn upstream_must_be_public_https() {
        for ok in [
            "https://api.exa.ai",
            "https://bedrock-agentcore.us-east-1.amazonaws.com",
            "https://ydc-index.io/v1",
            "https://8.8.8.8",
        ] {
            assert!(check_upstream_is_public(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://api.exa.ai",
            "https://metadata.google.internal/computeMetadata/v1",
            "https://169.254.169.254",
            "https://127.0.0.1:8080",
            "https://10.0.0.5",
            "https://192.168.1.1",
            "https://100.64.0.1",
            "https://localhost",
            "https://[::1]",
            "https://[fd00::1]",
            "https://[::ffff:169.254.169.254]",
            "https://redis",
            "https://user:pw@api.exa.ai",
        ] {
            assert!(check_upstream_is_public(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validate_accepts_minimal_valid_spec() {
        let raw = json!({
            "services": {
                "keenable": {
                    "upstream": "https://api.keenable.ai",
                    "docs_url": "https://docs.keenable.ai/search"
                }
            }
        });
        let spec = ProxySpec::validate(&raw).expect("minimal spec should be valid");
        assert_eq!(spec.services.len(), 1);
        assert_eq!(spec.budget_cents, 0);
        assert_eq!(spec.rate_limit_rpm, 60); // default from default_rpm()
        assert_eq!(
            spec.services["keenable"].docs_url.as_deref(),
            Some("https://docs.keenable.ai/search")
        );
    }

    #[test]
    fn validate_accepts_full_spec() {
        let raw = json!({
            "services": {
                "keenable": {
                    "upstream": "https://api.keenable.ai",
                    "headers": { "x-api-key": "k" },
                    "cost_per_1k_cents": 100,
                    "pricing_note": "$1 per 1000 search calls",
                    "docs_url": "https://docs.keenable.ai/search"
                }
            },
            "budget_cents": 5000,
            "per_claim_cents": 100,
            "rate_limit_rpm": 30
        });
        let spec = ProxySpec::validate(&raw).unwrap();
        assert_eq!(spec.budget_cents, 5000);
        assert_eq!(spec.services["keenable"].cost_per_1k_cents, 100);
    }

    #[test]
    fn validate_accepts_legacy_cost_per_request_cents_alias() {
        // Old specs in the wild (and in tests) used the per-request
        // unit. The serde alias re-maps the same numeric value into
        // the new field, deliberately leaving the rate at 1000× lower
        // than the original intent so a bad migration under-bills
        // rather than over-bills.
        let raw = json!({
            "services": {
                "keenable": {
                    "upstream": "https://api.keenable.ai",
                    "cost_per_request_cents": 1,
                    "docs_url": "https://docs.keenable.ai/search"
                }
            }
        });
        let spec = ProxySpec::validate(&raw).expect("alias should still parse");
        assert_eq!(spec.services["keenable"].cost_per_1k_cents, 1);
    }

    #[test]
    fn validate_rejects_missing_docs_url() {
        let raw = json!({
            "services": { "keenable": { "upstream": "https://api.keenable.ai" } }
        });
        let err = ProxySpec::validate(&raw).unwrap_err();
        assert!(err.contains("docs_url is required"), "got {err}");
        // The error should point to a concrete example so sponsors
        // don't have to fish through the docs to know what shape to
        // provide.
        assert!(
            err.contains("docs.keenable.ai"),
            "expected an example, got {err}"
        );
    }

    #[test]
    fn validate_rejects_relative_docs_url() {
        let raw = json!({
            "services": { "keenable": {
                "upstream": "https://api.keenable.ai",
                "docs_url": "docs.keenable.ai"
            } }
        });
        let err = ProxySpec::validate(&raw).unwrap_err();
        assert!(
            err.contains("docs_url must be an absolute URL"),
            "got {err}"
        );
    }

    #[test]
    fn validate_rejects_services_as_array() {
        // The original regression the user hit: `services: [...]`.
        let raw = json!({
            "services": [ { "name": "keenable", "upstream": "https://api.keenable.ai" } ]
        });
        let err = ProxySpec::validate(&raw).unwrap_err();
        assert!(
            err.contains("invalid proxy_spec shape"),
            "expected shape error, got {err}"
        );
    }

    #[test]
    fn validate_rejects_empty_services() {
        let raw = json!({ "services": {} });
        let err = ProxySpec::validate(&raw).unwrap_err();
        assert!(err.contains("at least one entry"), "got {err}");
    }

    #[test]
    fn validate_rejects_missing_upstream() {
        let raw = json!({
            "services": { "keenable": { "headers": { "x-api-key": "k" } } }
        });
        let err = ProxySpec::validate(&raw).unwrap_err();
        // Serde catches the missing field first.
        assert!(
            err.contains("missing field") || err.contains("upstream is required"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_rejects_relative_upstream() {
        let raw = json!({
            "services": { "keenable": { "upstream": "api.keenable.ai" } }
        });
        let err = ProxySpec::validate(&raw).unwrap_err();
        assert!(
            err.contains("absolute URL"),
            "expected scheme hint, got {err}"
        );
    }

    #[test]
    fn validate_rejects_negative_budget() {
        let raw = json!({
            "services": {
                "keenable": {
                    "upstream": "https://api.keenable.ai",
                    "docs_url": "https://docs.keenable.ai"
                }
            },
            "budget_cents": -1
        });
        let err = ProxySpec::validate(&raw).unwrap_err();
        assert!(err.contains("non-negative"), "got {err}");
    }

    /// Shaped like the live web-access / scrape battle specs (secrets
    /// replaced): mixed-case header names, free (0) services, long
    /// pricing notes, no `billing` anywhere. Must keep parsing, and
    /// re-serializing must not add a `billing` key.
    #[test]
    fn live_shaped_spec_without_billing_still_parses_and_round_trips() {
        let raw = json!({
            "budget_cents": 56200,
            "per_claim_cents": 25000,
            "rate_limit_rpm": 600,
            "services": {
                "brightdata_serpapi": {
                    "upstream": "https://api.brightdata.com",
                    "docs_url": "https://docs.brightdata.com/scraping-automation/serp-api",
                    "headers": { "Authorization": "Bearer REDACTED" },
                    "cost_per_1k_cents": 150,
                    "pricing_note": "$1.50 per 1000 SERP results"
                },
                "jina": {
                    "upstream": "https://r.jina.ai",
                    "docs_url": "https://jina.ai/reader",
                    "headers": {},
                    "cost_per_1k_cents": 0,
                    "pricing_note": "Jina Reader, keyless free tier"
                },
                "monid": {
                    "upstream": "https://api.monid.ai",
                    "docs_url": "https://monid.ai/SKILL.md",
                    "headers": { "Authorization": "Bearer REDACTED" },
                    "cost_per_1k_cents": 0,
                    "pricing_note": "POST /v1/run {provider,endpoint,input} -> runId"
                },
                "openrouter": {
                    "upstream": "https://openrouter.ai",
                    "docs_url": "https://openrouter.ai/docs/api-reference/overview",
                    "headers": { "Authorization": "Bearer REDACTED" },
                    "cost_per_1k_cents": 100,
                    "pricing_note": "Per-token per-model; 100c/K approximates a small chat call."
                },
                "perplexity": {
                    "upstream": "https://api.perplexity.ai",
                    "docs_url": "https://docs.perplexity.ai/docs/agent-api/wide-research",
                    "headers": { "Authorization": "Bearer REDACTED" },
                    "cost_per_1k_cents": 100000,
                    "pricing_note": "conservative cap"
                }
            }
        });
        let spec = ProxySpec::validate(&raw).expect("live-shaped spec must stay valid");
        assert_eq!(spec.services.len(), 5);
        assert!(spec.services.values().all(|s| s.billing.is_none()));
        let back = serde_json::to_value(&spec).unwrap();
        for svc in back["services"].as_object().unwrap().values() {
            assert!(
                svc.get("billing").is_none(),
                "no billing key on re-serialize"
            );
        }
        assert_eq!(back["services"]["jina"]["headers"], json!({}));
    }

    fn with_billing(billing: serde_json::Value) -> serde_json::Value {
        json!({
            "services": {
                "monid": {
                    "upstream": "https://api.monid.ai",
                    "docs_url": "https://monid.ai/SKILL.md",
                    "billing": billing
                }
            }
        })
    }

    #[test]
    fn validate_accepts_billing_blocks() {
        for b in [
            json!({"type": "response_json", "cost_path": "cost.value", "currency": "USD",
                   "currency_path": "cost.currency", "dedupe_key_path": "runId",
                   "max_per_request_cents": 50}),
            json!({"type": "response_json", "cost_path": ["usage.cost", "usage.total_cost"]}),
            json!({"type": "response_json", "cost_path": "costDollars.total",
                   "dedupe_key_path": ["runId", "id"], "max_per_request_cents": 0.5}),
            json!({"type": "response_header", "header": "x-cost-usd"}),
            json!({"type": "response_header", "header": "x-cost", "unit": "cents"}),
            json!({"type": "response_json", "cost_path": [
                "cost.value",
                {"path": "billing.reportedCost.value", "unit": "micro_usd"}
            ]}),
            json!({"type": "response_json",
                   "cost_path": {"path": "usage.cost_cents", "unit": "cents"}}),
        ] {
            let spec = ProxySpec::validate(&with_billing(b.clone()))
                .unwrap_or_else(|e| panic!("{b} should be valid: {e}"));
            // Round-trips to the same JSON (no defaults injected except
            // `unit` / `currency`).
            let back =
                serde_json::to_value(spec.services["monid"].billing.as_ref().unwrap()).unwrap();
            assert_eq!(back["type"], b["type"]);
        }
    }

    #[test]
    fn validate_rejects_bad_billing_blocks() {
        for (b, want) in [
            (json!({"type": "response_json"}), "cost_path is required"),
            (
                json!({"type": "response_json", "cost_path": ""}),
                "invalid JSON path",
            ),
            (
                json!({"type": "response_json", "cost_path": "cost..value"}),
                "invalid JSON path",
            ),
            (
                json!({"type": "response_json", "cost_path": []}),
                "at least one JSON path",
            ),
            (
                json!({"type": "response_json",
                       "cost_path": [{"path": "a..b", "unit": "usd"}]}),
                "invalid JSON path",
            ),
            (
                json!({"type": "response_json",
                       "cost_path": [{"path": "a.b", "unit": "yen"}]}),
                "invalid proxy_spec shape",
            ),
            (
                json!({"type": "response_json",
                       "cost_path": [{"path": "a.b", "units": "usd"}]}),
                "invalid proxy_spec shape",
            ),
            (json!({"type": "response_header"}), "header is required"),
            (
                json!({"type": "response_header", "header": "bad header"}),
                "not a valid header",
            ),
            (
                json!({"type": "response_json", "cost_path": "c", "currency": "EUR"}),
                "only 'USD'",
            ),
            (
                json!({"type": "response_json", "cost_path": "c", "max_per_request_cents": 0}),
                "positive",
            ),
            (
                json!({"type": "response_json", "cost_path": "c", "max_per_request_cents": -3}),
                "positive",
            ),
            (
                json!({"type": "per_token", "cost_path": "c"}),
                "invalid proxy_spec shape",
            ),
            (
                json!({"type": "response_json", "cost_path": "c", "costpath": "typo"}),
                "unknown field",
            ),
        ] {
            let err = ProxySpec::validate(&with_billing(b.clone())).unwrap_err();
            assert!(err.contains(want), "{b}: expected '{want}', got {err}");
        }
    }

    #[test]
    fn validate_accepts_and_round_trips_aws_sigv4() {
        let raw = json!({
            "services": {
                "amazon_web_search": {
                    "upstream": "https://bedrock-agentcore.us-east-1.amazonaws.com",
                    "docs_url": "https://docs.aws.amazon.com/bedrock-agentcore/",
                    "aws_sigv4": {
                        "access_key_id": "AKIA",
                        "secret_access_key": "s",
                        "region": "us-east-1",
                        "service": "bedrock-agentcore"
                    },
                    "cost_per_1k_cents": 700
                }
            }
        });
        let spec = ProxySpec::validate(&raw).unwrap();
        let back = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            back["services"]["amazon_web_search"]["aws_sigv4"],
            raw["services"]["amazon_web_search"]["aws_sigv4"]
        );
    }

    #[test]
    fn validate_rejects_incomplete_aws_sigv4() {
        let raw = json!({
            "services": {
                "aws": {
                    "upstream": "https://bedrock-agentcore.us-east-1.amazonaws.com",
                    "docs_url": "https://docs.aws.amazon.com/",
                    "aws_sigv4": {
                        "access_key_id": "AKIA",
                        "secret_access_key": "",
                        "region": "us-east-1",
                        "service": "bedrock-agentcore"
                    }
                }
            }
        });
        let err = ProxySpec::validate(&raw).unwrap_err();
        assert!(err.contains("aws_sigv4.secret_access_key"), "{err}");
    }
    // Regression (proxy audit 2026-11-19): no method/path allowlist, so
    // a Claimer could call any endpoint (account/team APIs, DELETE) with
    // the sponsor's key.
    fn with_allowlist(methods: serde_json::Value, paths: serde_json::Value) -> serde_json::Value {
        json!({
            "services": {
                "webiq": {
                    "upstream": "https://api.microsoft.ai",
                    "docs_url": "https://learn.microsoft.com/",
                    "allowed_methods": methods,
                    "allowed_paths": paths
                }
            }
        })
    }

    #[test]
    fn allowlist_enforces_methods_and_paths() {
        let spec = ProxySpec::validate(&with_allowlist(json!(["POST"]), json!(["/v3/*"]))).unwrap();
        let svc = &spec.services["webiq"];
        assert!(
            svc.check_endpoint_allowed("webiq", "POST", "v3/search/web")
                .is_ok()
        );
        assert!(
            svc.check_endpoint_allowed("webiq", "post", "v3/news")
                .is_ok()
        );
        let err = svc
            .check_endpoint_allowed("webiq", "DELETE", "v3/search/web")
            .unwrap_err();
        assert!(err.contains("allowed methods: POST"), "{err}");
        assert!(err.contains("allowed paths: /v3/*"), "{err}");
        assert!(
            svc.check_endpoint_allowed("webiq", "POST", "v1/account")
                .is_err()
        );
        assert!(svc.check_endpoint_allowed("webiq", "POST", "v3").is_err());
        assert!(svc.check_endpoint_allowed("webiq", "POST", "").is_err());

        // Prefix entries match on a segment boundary.
        let spec = ProxySpec::validate(&with_allowlist(
            json!([]),
            json!(["/web-search/web-search.v1/invoke"]),
        ))
        .unwrap();
        let svc = &spec.services["webiq"];
        for ok in [
            "web-search/web-search.v1/invoke",
            "/web-search/web-search.v1/invoke",
            "web-search/web-search.v1/invoke/x",
        ] {
            assert!(svc.check_endpoint_allowed("s", "GET", ok).is_ok(), "{ok}");
        }
        for bad in ["web-search/web-search.v1/invokeX", "web-search", "other"] {
            assert!(
                svc.check_endpoint_allowed("s", "GET", bad).is_err(),
                "{bad}"
            );
        }

        // Glob with a middle wildcard.
        let spec =
            ProxySpec::validate(&with_allowlist(json!(["GET"]), json!(["/v1/*/search"]))).unwrap();
        let svc = &spec.services["webiq"];
        assert!(
            svc.check_endpoint_allowed("s", "GET", "v1/web/search")
                .is_ok()
        );
        assert!(
            svc.check_endpoint_allowed("s", "GET", "v1/web/searchx")
                .is_err()
        );

        // Empty lists = legacy allow-all.
        let spec = ProxySpec::validate(&with_allowlist(json!([]), json!([]))).unwrap();
        assert!(
            spec.services["webiq"]
                .check_endpoint_allowed("s", "DELETE", "anything")
                .is_ok()
        );
    }

    #[test]
    fn allowlist_validation_rejects_garbage() {
        for (m, p, want) in [
            (json!(["FETCH"]), json!([]), "unknown method"),
            (json!([]), json!(["v3/*"]), "allowed_paths entry"),
            (json!([]), json!(["/v3/../admin"]), "allowed_paths entry"),
            (json!([]), json!(["/v3?x=1"]), "allowed_paths entry"),
            (json!([]), json!(["/v3 x"]), "allowed_paths entry"),
        ] {
            let err = ProxySpec::validate(&with_allowlist(m.clone(), p.clone())).unwrap_err();
            assert!(err.contains(want), "{m} {p}: {err}");
        }
    }

    #[test]
    fn allowlists_round_trip_and_are_omitted_when_empty() {
        let raw = with_allowlist(json!(["POST"]), json!(["/v3/*"]));
        let back = serde_json::to_value(ProxySpec::validate(&raw).unwrap()).unwrap();
        assert_eq!(
            back["services"]["webiq"]["allowed_methods"],
            json!(["POST"])
        );
        assert_eq!(back["services"]["webiq"]["allowed_paths"], json!(["/v3/*"]));
        let back = serde_json::to_value(
            ProxySpec::validate(&with_allowlist(json!([]), json!([]))).unwrap(),
        )
        .unwrap();
        assert!(back["services"]["webiq"].get("allowed_methods").is_none());
        assert!(back["services"]["webiq"].get("allowed_paths").is_none());
    }

    // Regression (proxy audit 2026-11-19): an unbounded cap let a
    // garbage reported cost saturate the spend sums.
    #[test]
    fn max_per_request_cents_has_a_ceiling() {
        let err = ProxySpec::validate(&with_billing(json!({
            "type": "response_json", "cost_path": "c", "max_per_request_cents": 10_001
        })))
        .unwrap_err();
        assert!(err.contains("at most"), "{err}");
        assert!(
            ProxySpec::validate(&with_billing(json!({
                "type": "response_json", "cost_path": "c", "max_per_request_cents": 10_000
            })))
            .is_ok()
        );
    }
}
