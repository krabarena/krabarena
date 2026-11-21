//! Solution-name normalisation and the hand-curated vendor → logo
//! map used by the battle-owned solutions feature (0.13.0).
//!
//! The render path normalises every leaderboard row's `solution`
//! string and joins it against `battles.solutions[*].name_normalized`
//! to pick up the canonical logo. The backfill admin endpoint
//! collects every distinct `solution` string across a battle's
//! claims, normalises each one, derives a vendor (logo_url +
//! website_url) via the [`VENDOR_PATTERNS`] regex table, and writes
//! the resulting list to `battles.solutions`.
//!
//! Normalisation rules (mirrored in `038_battles_solutions.sql`):
//!
//!   1. trim
//!   2. lowercase
//!   3. strip everything inside `(...)` parens
//!      — `"Browserless v2 (Chromium-as-a-service)"` →
//!      `"browserless v2"`
//!   4. collapse internal whitespace runs to a single space
//!      — `"browserless\nv2 "` → `"browserless v2"`
//!   5. trim again (after paren-strip / whitespace collapse)
//!
//! Empty input → empty string; callers treat an empty key as "no
//! lookup, fall back to per-row logo_url".
//!
//! ## AGENT-BRAND-WINS logo rule (every logo-fill path must honor this)
//!
//! A contender is often an **agent / inference company / tool / harness**
//! that runs on top of a **base LLM model** — written `"<Contender>
//! (<base model>)"`, e.g. `"Junie (gpt-5.3-codex)"`, `"Cursor (claude
//! sonnet)"`, `"Some Harness (qwen2.5)"`. In that shape the parenthetical
//! is the *engine*, not the brand. The CONTENDER's own logo (JetBrains,
//! Cursor, the inference co, …) must be prioritized **higher** than the
//! base model provider's logo (OpenAI/Anthropic/Alibaba/…) — even when the
//! contender is NOT yet in the `vendor_logos` map. We must never paint an
//! agent with the logo of the LLM it merely runs on.
//!
//! [`lookup_vendor`] implements this centrally (via [`is_base_llm_model`]):
//! when the parens hold a base model it resolves from the OUTER name and
//! never falls back to the model provider — returning `None` (so callers
//! use the contender's own website/avatar) rather than the wrong logo.
//! Because `lookup_vendor` is the single chokepoint for every derivation
//! (battle-solutions backfill, `refresh_null_logos`, `apply_battle_logos`,
//! OG cards), fixing it here fixes every fill site. To give a specific
//! contender its exact logo, add a `vendor_logos` row for the CONTENDER
//! name (not the model) and re-run the battle's `backfill-solutions`.

use regex::Regex;

/// Normalise a free-text solution name into the canonical key used
/// both inside `battles.solutions[*].name_normalized` and on every
/// claim leaderboard row at render time. See the module docs for
/// the full rule list.
pub fn normalize_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_lowercase();
    let stripped = strip_parens(&lower);
    let collapsed = collapse_whitespace(&stripped);
    collapsed.trim().to_string()
}

fn strip_parens(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut depth: usize = 0;
    for c in input.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
            }
            other if depth == 0 => out.push(other),
            _ => {}
        }
    }
    out
}

fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_ws = false;
    for c in input.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out
}

/// A compiled vendor pattern loaded from the `vendor_logos` Neon table:
/// a case-insensitive regex matched against a solution name, the domain
/// its favicon / website URL are built from, plus the optionally-stored
/// self-hosted logo URL and extracted brand color (migration 057).
///
/// This map used to be the hardcoded `VENDOR_PATTERNS` const. It now
/// lives in Neon (`vendor_logos`, seeded in migration 055) so new
/// competitors get logos without a deploy — insert a row and re-run the
/// battle-solutions backfill. `stored_url`/`brand_color` are NULL until
/// the logo-asset backfill (`POST /v1/admin/vendor-logos/refresh-assets`)
/// runs; until then a vendor resolves to NO logo (initials avatar) —
/// resolution is GCP-only and never emits an external favicon.
pub struct CompiledVendor {
    pub regex: Regex,
    pub domain: String,
    pub stored_url: Option<String>,
    pub brand_color: Option<String>,
}

/// A resolved vendor logo: the self-hosted `stored_url` to render (GCP
/// bucket only) plus the optional brand color.
pub struct ResolvedLogo {
    pub url: String,
    pub brand_color: Option<String>,
}

/// Load `vendor_logos` ordered by `sort_order` (first-match-wins, same
/// precedence the const had) and compile each `pattern` into a
/// case-insensitive regex. A bad regex is skipped with a warning rather
/// than failing the whole load.
pub async fn load_vendor_patterns(pool: &sqlx::PgPool) -> Result<Vec<CompiledVendor>, sqlx::Error> {
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT pattern, domain, stored_url, brand_color FROM vendor_logos ORDER BY sort_order, id",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (pattern, domain, stored_url, brand_color) in rows {
        match Regex::new(&format!("(?i){pattern}")) {
            Ok(re) => out.push(CompiledVendor {
                regex: re,
                domain,
                stored_url,
                brand_color,
            }),
            Err(e) => {
                tracing::warn!(pattern = %pattern, error = %e, "vendor_logos.bad_regex_skipped")
            }
        }
    }
    Ok(out)
}

/// Look up a vendor domain for a solution name against the compiled
/// `vendor_logos` patterns. See [`lookup_vendor`] for the full precedence
/// (including the AGENT-BRAND-WINS rule for `"<Agent> (<base model>)"`).
pub fn lookup_vendor_domain(solution: &str, patterns: &[CompiledVendor]) -> Option<String> {
    lookup_vendor(solution, patterns).map(|v| v.domain.clone())
}

/// Resolve the full logo for a solution name: the matched row's
/// self-hosted `stored_url` (migration 057) plus its `brand_color`.
/// GCP-only — returns `None` when the matched vendor has no self-hosted
/// asset (never an external favicon). Matching semantics are identical to
/// [`lookup_vendor_domain`] (parens-target precedence, first-match-wins).
pub fn lookup_vendor_logo(solution: &str, patterns: &[CompiledVendor]) -> Option<ResolvedLogo> {
    // GCP-only: emit a logo ONLY when the vendor has a self-hosted
    // `stored_url` (populated by the logo-asset backfill / refresh-assets).
    // We never fall back to an external `google.com/s2/favicons` URL — a
    // missing asset renders as the initials avatar instead. Run
    // `POST /v1/admin/vendor-logos/refresh-assets` to self-host any domain
    // whose `stored_url` is still NULL.
    let v = lookup_vendor(solution, patterns)?;
    v.stored_url.clone().map(|url| ResolvedLogo {
        url,
        brand_color: v.brand_color.clone(),
    })
}

/// Whether a logo URL is one we self-host in our GCP bucket (as opposed
/// to an external favicon service or an author-supplied third-party URL).
/// Leaderboard logos must always be self-hosted so social-card / page
/// renders never block on — or leak to — an external host.
pub fn is_self_hosted_logo(url: &str) -> bool {
    url.contains("://storage.googleapis.com/")
}

/// Identity of a logo URL for grouping/comparison: the URL without its
/// query string or fragment. Stored logos carry a `?v=<content hash>`
/// cache-buster (0.43.3), so two rows pointing at the same vendor asset
/// can differ only in `?v=` while a backfill is mid-propagation — they
/// must still be treated as the same vendor.
pub fn logo_identity(url: &str) -> &str {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    &url[..end]
}

/// Shared matcher for [`lookup_vendor_domain`] / [`lookup_vendor_logo`]
/// and the single chokepoint every logo-fill path funnels through. Public
/// so the battle-solutions backfill can read `stored_url` + `domain` off
/// the same matched row in one pass.
///
/// Precedence:
///   1. **AGENT-BRAND-WINS** (see module docs): if the raw name is
///      `"<Contender> (<base model>)"` — i.e. the LAST `(...)` group is a
///      base LLM model per [`is_base_llm_model`] — resolve from the OUTER
///      contender name (the agent / inference co / tool / harness) and
///      NEVER fall back to a pattern that would match the model text. When
///      the contender isn't mapped, return `None` so the caller uses the
///      contender's own site/avatar, not the LLM provider's logo. This is
///      what stops e.g. `"Junie (gpt-5.3-codex)"` painting OpenAI's logo.
///   2. **TARGET-VENDOR-IN-PARENS**: otherwise, if the parens name a
///      target vendor (e.g. `"BrightData (Google)"` → `google.com`), match
///      that first — the proxy/access method sits outside the parens.
///   3. Else match the FULL string, first-match-wins by `sort_order`.
pub fn lookup_vendor<'a>(
    solution: &str,
    patterns: &'a [CompiledVendor],
) -> Option<&'a CompiledVendor> {
    let s = solution.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(inner) = last_parens_content(s) {
        let inner = inner.trim();
        if !inner.is_empty() {
            if is_base_llm_model(inner) {
                // AGENT-BRAND-WINS: the parens hold the base LLM model the
                // contender runs on, NOT a target vendor. Resolve from the
                // OUTER contender identity and never fall through to a
                // model-provider match — even on a miss (→ None), so the
                // agent keeps its own brand rather than the LLM's logo.
                let outer = strip_trailing_parens(s).trim();
                if !outer.is_empty() {
                    return patterns.iter().find(|v| v.regex.is_match(outer));
                }
                // Degenerate `"(model)"` with no outer identity: nothing to
                // prioritize, so treat the whole thing as a bare model name.
                return patterns.iter().find(|v| v.regex.is_match(s));
            }
            // TARGET-VENDOR-IN-PARENS (e.g. "BrightData (Google)"): the
            // parens name the vendor whose logo we want — match it first.
            if let Some(v) = patterns.iter().find(|v| v.regex.is_match(inner)) {
                return Some(v);
            }
        }
    }
    patterns.iter().find(|v| v.regex.is_match(s))
}

/// True when `s` looks like a bare **base LLM model** name (the engine a
/// contender runs on) rather than a product / company / target vendor.
///
/// Drives the AGENT-BRAND-WINS rule in [`lookup_vendor`] (see module
/// docs): a solution written `"<Agent> (<base model>)"` must take its logo
/// from the OUTER agent/tool/company, not the model provider. The check is
/// anchored at the START of the (already parens-inner) string and covers
/// mainstream model FAMILIES (gpt/claude/qwen/gemini/llama/mistral/…). It
/// is intentionally broad on families and conservative on generic words:
/// a false positive only demotes a genuine `"Product (Vendor)"` parens to
/// its outer name, while a miss would silently repaint an agent with its
/// LLM's logo. Extend the family list here as new base models ship.
pub fn is_base_llm_model(s: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?ix)
            ^\s*(?:
                gpt | chat\s?gpt                          # OpenAI GPT / ChatGPT
              | o[1-4](?:[-\s](?:mini|preview|pro|high|low))?  # o1/o3/o4 reasoning
              | (?:text-)?davinci | curie | babbage       # legacy OpenAI
              | codex                                     # gpt-*-codex family
              | claude | opus | sonnet | haiku            # Anthropic
              | gemini | gemma | palm | bard              # Google
              | qwen | qwq | qvq                          # Alibaba Qwen
              | llama | code\s?llama | alpaca | vicuna    # Meta Llama & derivs
              | mi(?:s|x)tral | codestral | ministral | magistral | devstral | pixtral  # Mistral
              | deepseek | janus                          # DeepSeek
              | kimi | moonshot                           # Moonshot
              | grok                                      # xAI
              | phi[-\s]?\d                                # Microsoft Phi-N
              | command[-\s]?r                            # Cohere Command-R
              | glm | chatglm                             # Zhipu
              | ernie | hunyuan | doubao | minimax | internlm  # other CN labs
              | granite | dbrx | jamba | olmo | starcoder | stablelm | wizardlm | mpt
            )
            (?:$|[^a-z])  # family token ends at a non-letter (qwen2, gpt-4) or EOS
            ",
        )
        .expect("base-llm-model regex compiles")
    });
    re.is_match(s.trim())
}

/// Return `input` with its LAST balanced `(...)` group removed and any
/// trailing whitespace trimmed — recovering the OUTER agent/product
/// identity from a `"<Agent> (<model>)"` string (e.g. `"Junie (gpt-5.3-
/// codex)"` → `"Junie"`). Falls back to the whole string when there's no
/// balanced group. Mirrors [`last_parens_content`]'s scan.
fn strip_trailing_parens(input: &str) -> &str {
    let bytes = input.as_bytes();
    let mut last_close: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate().rev() {
        if b == b')' {
            last_close = Some(i);
            break;
        }
    }
    let Some(close) = last_close else {
        return input;
    };
    let mut depth: usize = 1;
    let mut idx = close;
    while idx > 0 {
        idx -= 1;
        match bytes[idx] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return input[..idx].trim_end();
                }
            }
            _ => {}
        }
    }
    input
}

/// Return the content of the LAST balanced `(...)` group in `input`,
/// or `None` when no parens are present. Inner parens are kept verbatim
/// — we only split at the outermost matching pair. Unbalanced parens
/// (e.g. `"foo (bar"`) yield `None` so we don't accidentally feed a
/// half-string into the vendor matcher.
fn last_parens_content(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    let mut last_close: Option<usize> = None;
    // Find the last `)` — that's the close of the outermost-rightmost group.
    for (i, &b) in bytes.iter().enumerate().rev() {
        if b == b')' {
            last_close = Some(i);
            break;
        }
    }
    let close = last_close?;
    // Walk back from `close - 1`, counting nested parens so we land on
    // the matching `(`.
    let mut depth: usize = 1;
    let mut idx = close;
    while idx > 0 {
        idx -= 1;
        match bytes[idx] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&input[idx + 1..close]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Build the Google S2 favicon URL for a domain.
pub fn favicon_url_for(domain: &str) -> String {
    format!("https://www.google.com/s2/favicons?domain={domain}&sz=64")
}

/// Build the website URL for a domain.
pub fn website_url_for(domain: &str) -> String {
    format!("https://{domain}")
}

/// Extract the bare host from an author-supplied URL so we can derive a
/// favicon when no curated vendor pattern matches a solution (the
/// `website_url` in a claim's leaderboard entry). Strips the scheme,
/// userinfo, port and path, lowercases, and drops a leading `www.`.
/// Returns `None` when there's no usable dotted host (empty, `localhost`,
/// an IP-less token, etc.) — callers then fall back to initials.
pub fn host_from_url(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // Drop scheme, then take the authority up to the first path/query/frag.
    let after_scheme = s.split_once("://").map_or(s, |(_, rest)| rest);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    // Strip userinfo (`user:pass@host`) and any `:port`.
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let mut host = host_port
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if let Some(stripped) = host.strip_prefix("www.") {
        host = stripped.to_string();
    }
    // Require a dotted name — guards against `localhost`, bare tokens, junk.
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    Some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_parens_and_collapses_whitespace() {
        assert_eq!(
            normalize_name("Browserless v2 (Chromium-as-a-service)"),
            "browserless v2"
        );
        assert_eq!(normalize_name("browserless\nv2 "), "browserless v2");
    }

    #[test]
    fn normalize_basic() {
        assert_eq!(normalize_name("Browserless"), "browserless");
        assert_eq!(normalize_name("  Lightpanda  "), "lightpanda");
        assert_eq!(normalize_name("Claude Code"), "claude code");
    }

    #[test]
    fn host_from_url_extracts_bare_host() {
        // Real author-supplied URLs from the Skills-vs-MCP battle.
        assert_eq!(
            host_from_url("https://modelcontextprotocol.io/").as_deref(),
            Some("modelcontextprotocol.io")
        );
        assert_eq!(
            host_from_url("https://agentskills.io/home").as_deref(),
            Some("agentskills.io")
        );
        // Scheme-less, www, port, userinfo, query, and casing all handled.
        assert_eq!(
            host_from_url("foo.example.com/x").as_deref(),
            Some("foo.example.com")
        );
        assert_eq!(
            host_from_url("https://www.Example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            host_from_url("http://Host.io:8080/p?q=1").as_deref(),
            Some("host.io")
        );
        assert_eq!(
            host_from_url("https://u:p@api.host.org/v1").as_deref(),
            Some("api.host.org")
        );
        // No usable dotted host → None (caller falls back to initials).
        assert_eq!(host_from_url(""), None);
        assert_eq!(host_from_url("   "), None);
        assert_eq!(host_from_url("localhost"), None);
        assert_eq!(host_from_url("not a url"), None);
    }

    #[test]
    fn normalize_empty_input() {
        assert_eq!(normalize_name(""), "");
        assert_eq!(normalize_name("   "), "");
        assert_eq!(normalize_name("(only parens)"), "");
    }

    #[test]
    fn normalize_keeps_punctuation_outside_parens() {
        assert_eq!(normalize_name("Z.AI · GLM-4.6"), "z.ai · glm-4.6");
    }

    // A representative fixture of `vendor_logos` rows. The real map
    // lives in Neon (migration 055); these unit tests cover the matching
    // LOGIC (parens-target precedence, $-anchors, native_ underscores,
    // first-match-wins) against a fixed seed rather than the full table.
    fn seed() -> Vec<CompiledVendor> {
        [
            (r"^claude code", "anthropic.com"),
            (r"^claude\s", "anthropic.com"),
            (r"^claude$", "anthropic.com"),
            (r"^vertex", "cloud.google.com"),
            (r"^google$", "google.com"),
            (r"^browserless\b", "browserless.io"),
            (r"^lightpanda\b", "lightpanda.io"),
            (r"^tavily\b", "tavily.com"),
            (r"^exa\b", "exa.ai"),
            (r"^keenable\b", "keenable.ai"),
            (r"^duckdb\b", "duckdb.org"),
            (r"^polars\b", "pola.rs"),
            (r"^clickhouse", "clickhouse.com"),
            (r"^snowflake", "snowflake.com"),
            (r"^sambanova", "sambanova.ai"),
            (r"^brightdata\b|^bright data\b", "brightdata.com"),
            (r"^native_rust", "rust-lang.org"),
            (r"^native_python", "python.org"),
            (r"^native_c", "isocpp.org"),
            (r"^python$", "python.org"),
            (r"^rust$", "rust-lang.org"),
            (r"^typescript$", "typescriptlang.org"),
            (r"^javascript$", "javascript.com"),
        ]
        .into_iter()
        .map(|(pat, dom)| CompiledVendor {
            regex: Regex::new(&format!("(?i){pat}")).unwrap(),
            domain: dom.to_string(),
            stored_url: None,
            brand_color: None,
        })
        .collect()
    }

    fn dom(name: &str) -> Option<String> {
        lookup_vendor_domain(name, &seed())
    }

    #[test]
    fn vendor_lookup_first_match_wins() {
        assert_eq!(dom("Claude Code").as_deref(), Some("anthropic.com"));
        assert_eq!(dom("Claude 3.7 Sonnet").as_deref(), Some("anthropic.com"));
        assert_eq!(dom("Vertex Search").as_deref(), Some("cloud.google.com"));
    }

    #[test]
    fn vendor_lookup_misc_vendors() {
        assert_eq!(dom("Browserless").as_deref(), Some("browserless.io"));
        assert_eq!(dom("Lightpanda").as_deref(), Some("lightpanda.io"));
        assert_eq!(dom("Tavily").as_deref(), Some("tavily.com"));
        assert_eq!(dom("Exa").as_deref(), Some("exa.ai"));
        assert_eq!(dom("Keenable").as_deref(), Some("keenable.ai"));
        assert_eq!(dom("DuckDB").as_deref(), Some("duckdb.org"));
        assert_eq!(dom("Polars").as_deref(), Some("pola.rs"));
        assert_eq!(dom("ClickHouse").as_deref(), Some("clickhouse.com"));
        assert_eq!(dom("native_rust").as_deref(), Some("rust-lang.org"));
    }

    #[test]
    fn vendor_lookup_returns_none_for_unknown() {
        assert_eq!(dom("SomeUnknownVendor"), None);
        assert_eq!(dom(""), None);
        assert_eq!(dom("   "), None);
    }

    #[test]
    fn favicon_url_format() {
        assert_eq!(
            favicon_url_for("browserless.io"),
            "https://www.google.com/s2/favicons?domain=browserless.io&sz=64"
        );
    }

    #[test]
    fn vendor_lookup_snowflake() {
        assert_eq!(dom("Snowflake Medium").as_deref(), Some("snowflake.com"));
        assert_eq!(
            dom("Snowflake XS Interactive ($2.00/hr Standard)").as_deref(),
            Some("snowflake.com")
        );
    }

    #[test]
    fn vendor_lookup_sambanova() {
        assert_eq!(
            dom("SambaNova · Llama 3.3 70B Instruct").as_deref(),
            Some("sambanova.ai")
        );
    }

    #[test]
    fn vendor_lookup_bare_language_baselines() {
        assert_eq!(dom("Python").as_deref(), Some("python.org"));
        assert_eq!(dom("Rust").as_deref(), Some("rust-lang.org"));
        assert_eq!(dom("TypeScript").as_deref(), Some("typescriptlang.org"));
        assert_eq!(dom("JavaScript").as_deref(), Some("javascript.com"));
    }

    #[test]
    fn vendor_lookup_bare_language_is_anchored() {
        // `$` anchor — "Rust 2024 edition" must NOT match the bare row.
        assert_eq!(dom("Rust 2024 edition"), None);
    }

    #[test]
    fn vendor_lookup_target_in_parens_wins_over_proxy() {
        assert_eq!(dom("BrightData (Google)").as_deref(), Some("google.com"));
        assert_eq!(
            dom("Brightdata SERP (Google)").as_deref(),
            Some("google.com")
        );
        assert_eq!(
            dom("Brightdata SerpAPI (Google)").as_deref(),
            Some("google.com")
        );
        assert_eq!(dom("Some thing (Tavily)").as_deref(), Some("tavily.com"));
    }

    #[test]
    fn vendor_lookup_no_parens_falls_back_to_prefix() {
        assert_eq!(dom("BrightData").as_deref(), Some("brightdata.com"));
        assert_eq!(dom("Claude").as_deref(), Some("anthropic.com"));
    }

    #[test]
    fn vendor_lookup_parens_miss_falls_back_to_prefix() {
        assert_eq!(
            dom("Claude Code (KrabArena bench)").as_deref(),
            Some("anthropic.com")
        );
    }

    #[test]
    fn vendor_logo_prefers_stored_url_with_brand_color() {
        let patterns = vec![
            CompiledVendor {
                regex: Regex::new("(?i)^tavily").unwrap(),
                domain: "tavily.com".to_string(),
                stored_url: Some("https://storage.googleapis.com/b/logos/tavily.com.png".into()),
                brand_color: Some("#3572A5".into()),
            },
            CompiledVendor {
                regex: Regex::new("(?i)^exa").unwrap(),
                domain: "exa.ai".to_string(),
                stored_url: None,
                brand_color: None,
            },
        ];
        // stored_url present → prefer it, carry the brand color.
        let tavily = lookup_vendor_logo("Tavily", &patterns).unwrap();
        assert_eq!(
            tavily.url,
            "https://storage.googleapis.com/b/logos/tavily.com.png"
        );
        assert_eq!(tavily.brand_color.as_deref(), Some("#3572A5"));
        // stored_url absent → GCP-only means NO logo (never an external
        // favicon); the caller renders the initials avatar instead.
        assert!(lookup_vendor_logo("Exa", &patterns).is_none());
        // No match → None.
        assert!(lookup_vendor_logo("MysteryTool", &patterns).is_none());
    }

    #[test]
    fn logo_identity_ignores_version_query() {
        let base = "https://storage.googleapis.com/krabarena-artifacts/logos/keenable.ai.png";
        assert_eq!(logo_identity(base), base);
        assert_eq!(logo_identity(&format!("{base}?v=0123456789")), base);
        assert_eq!(logo_identity(&format!("{base}?v=abc#frag")), base);
        assert_eq!(logo_identity(&format!("{base}#frag")), base);
        assert!(is_self_hosted_logo(&format!("{base}?v=0123456789")));
    }

    #[test]
    fn is_self_hosted_logo_gates_external_hosts() {
        assert!(is_self_hosted_logo(
            "https://storage.googleapis.com/krabarena-artifacts/logos/openai.com.png"
        ));
        // Every non-GCP host is rejected — favicon services, vendor sites,
        // author-supplied third parties, empty.
        assert!(!is_self_hosted_logo(
            "https://www.google.com/s2/favicons?domain=x.ai&sz=64"
        ));
        assert!(!is_self_hosted_logo("https://openai.com/favicon.ico"));
        assert!(!is_self_hosted_logo(
            "http://storage.googleapis.com.evil.com/x"
        ));
        assert!(!is_self_hosted_logo(""));
    }

    /// Invariant guard: the vendor-logo resolver NEVER yields a
    /// non-self-hosted URL. A missing self-hosted asset resolves to
    /// `None` (initials avatar), never to an external favicon. If this
    /// fails, an external-favicon fallback has crept back in.
    #[test]
    fn lookup_vendor_logo_is_gcp_only() {
        // Pattern matches but no self-hosted asset → no logo.
        let no_asset = vec![CompiledVendor {
            regex: Regex::new("(?i)^acme").unwrap(),
            domain: "acme.com".into(),
            stored_url: None,
            brand_color: None,
        }];
        assert!(lookup_vendor_logo("Acme Corp", &no_asset).is_none());

        // Self-hosted asset present → that URL, and it IS self-hosted.
        let with_asset = vec![CompiledVendor {
            regex: Regex::new("(?i)^acme").unwrap(),
            domain: "acme.com".into(),
            stored_url: Some(
                "https://storage.googleapis.com/krabarena-artifacts/logos/acme.com.png".into(),
            ),
            brand_color: None,
        }];
        let resolved = lookup_vendor_logo("Acme Corp", &with_asset).expect("has asset");
        assert!(
            is_self_hosted_logo(&resolved.url),
            "resolver must only ever emit self-hosted GCP logos, got {}",
            resolved.url
        );
    }

    #[test]
    fn vendor_lookup_native_underscore_regression() {
        assert_eq!(
            dom("native_python_inverted_index").as_deref(),
            Some("python.org")
        );
        assert_eq!(
            dom("native_rust_inverted_index").as_deref(),
            Some("rust-lang.org")
        );
        assert_eq!(
            dom("native_c_inverted_index").as_deref(),
            Some("isocpp.org")
        );
    }

    // --- AGENT-BRAND-WINS: agent/tool/company logo beats the base LLM
    // model it runs on (the parens content). ---

    fn vendors(rows: &[(&str, &str)]) -> Vec<CompiledVendor> {
        rows.iter()
            .map(|(pat, dom)| CompiledVendor {
                regex: Regex::new(&format!("(?i){pat}")).unwrap(),
                domain: dom.to_string(),
                stored_url: None,
                brand_color: None,
            })
            .collect()
    }

    #[test]
    fn is_base_llm_model_recognizes_families() {
        for m in [
            "gpt-5.3-codex",
            "GPT-4o",
            "chatgpt",
            "o3-mini",
            "Opus 4.8",
            "claude-3.7-sonnet",
            "Sonnet 4",
            "Gemini 1.5 Pro",
            "gemma-2",
            "Qwen2.5-72B",
            "qwq-32b",
            "llama-3.3-70b",
            "Mixtral 8x7B",
            "codestral",
            "DeepSeek-V3",
            "Kimi K2",
            "grok-2",
            "phi-4",
            "command-r-plus",
            "GLM-4.6",
        ] {
            assert!(is_base_llm_model(m), "should be a base model: {m:?}");
        }
        // Products / companies / target vendors are NOT base models.
        // (These are only ever tested against the *parens* content, so a
        // product like "Claude Code" — which starts with a family token —
        // is never passed here; it has no model in its own parens.)
        for not in [
            "Junie",
            "Cursor",
            "Google",
            "Tavily",
            "BrightData",
            "KrabArena bench",
            "SomeUnknownTool",
        ] {
            assert!(
                !is_base_llm_model(not),
                "should NOT be a base model: {not:?}"
            );
        }
    }

    #[test]
    fn agent_brand_wins_over_base_model_in_parens() {
        // Junie (a JetBrains agent) runs on an OpenAI model; its OWN brand
        // must win over the model's provider.
        let v = vendors(&[
            ("^junie", "jetbrains.com"),
            ("codex cli", "openai.com"),
            ("^openai", "openai.com"),
            ("^gpt", "openai.com"),
        ]);
        assert_eq!(
            lookup_vendor_domain("Junie (gpt-5.3-codex)", &v).as_deref(),
            Some("jetbrains.com"),
            "agent brand must beat the base model in parens"
        );
        // Codex CLI genuinely IS OpenAI: outer name resolves to openai.
        assert_eq!(
            lookup_vendor_domain("OpenAI Codex CLI (gpt-5.5)", &v).as_deref(),
            Some("openai.com")
        );
    }

    #[test]
    fn base_model_in_parens_never_leaks_model_vendor() {
        // No mapping for the agent, only for the base model. We must return
        // None (caller uses the agent's own site/avatar) — NOT the model's
        // OpenAI logo.
        let v = vendors(&[("^gpt", "openai.com"), ("codex cli", "openai.com")]);
        assert_eq!(lookup_vendor_domain("Junie (gpt-5.3-codex)", &v), None);
        assert_eq!(lookup_vendor_domain("Some Harness (qwen2.5-72b)", &v), None);
    }

    #[test]
    fn target_vendor_in_parens_still_wins_when_not_a_model() {
        // Regression: the proxy/target-vendor heuristic is unchanged for
        // non-model parens like "(Google)".
        assert_eq!(dom("BrightData (Google)").as_deref(), Some("google.com"));
        assert_eq!(dom("Some thing (Tavily)").as_deref(), Some("tavily.com"));
    }
}
