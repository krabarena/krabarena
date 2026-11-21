//! User-Agent classifier for usage stats (`GET /v1/admin/usage`).
//!
//! One source of truth: every UA in `user_logs` is classified here, in
//! Rust, and the SQL side only ever receives the resulting
//! `ua → (family, category)` mapping as bound arrays (see
//! `usage_stats`). No pattern list is duplicated in SQL.
//!
//! The classification is **UA-claimed**: nothing here verifies the
//! claim (no reverse DNS). Googlebot in particular is spoofed a lot —
//! in a 30-day Cloud Run log sample ~95% of Googlebot-UA requests were
//! fake (vulnerability scanners on non-HTML paths). `user_logs` only
//! records HTML routes, so most Googlebot rows there are real, but a
//! digest should still say "UA-claimed".
//!
//! Keenable's crawler ran under older names: `SleepBot` (prod crawler
//! until 2026-09-11), `Aranet-SearchBot`, `Keenable-SearchBot`. All map
//! to the `KeenableBot` family so coverage over long windows is not
//! split across names.

use serde::Serialize;

/// Coarse bucket a UA falls in. `Browser` is the only human bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UaCategory {
    /// Classic web search engines (Googlebot, Bingbot, Applebot, …).
    SearchEngine,
    /// Search / fetch APIs sold to AI builders (Keenable, Exa, Perplexity, You.com, Parallel, …).
    AiSearchApi,
    /// Fetchers acting for an assistant / chat product (ChatGPT-User, Claude-User, OAI-SearchBot, …).
    AiAssistant,
    /// Corpus crawlers for model training (GPTBot, ClaudeBot, CCBot, Bytespider, …).
    AiTraining,
    /// SEO / backlink / marketing crawlers (Semrush, Ahrefs, MJ12, …).
    Seo,
    /// Link-preview unfurlers (Discord, X/Twitter, Facebook, Slack, LinkedIn, Telegram, …).
    SocialPreview,
    /// Uptime checks.
    Monitoring,
    /// Vulnerability / internet-wide scanners.
    Scanner,
    /// HTTP libraries, CLIs and headless browsers (curl, python-requests, HeadlessChrome, …).
    Script,
    /// KrabArena's own agents and the `krab` CLI.
    Internal,
    /// Self-declared bots we have no specific rule for.
    OtherBot,
    /// Empty / bare / malformed UA that is neither a browser nor a declared bot.
    Unknown,
    /// A real-looking browser UA — counted as a human.
    Browser,
}

impl UaCategory {
    pub const ALL: [UaCategory; 13] = [
        UaCategory::SearchEngine,
        UaCategory::AiSearchApi,
        UaCategory::AiAssistant,
        UaCategory::AiTraining,
        UaCategory::Seo,
        UaCategory::SocialPreview,
        UaCategory::Monitoring,
        UaCategory::Scanner,
        UaCategory::Script,
        UaCategory::Internal,
        UaCategory::OtherBot,
        UaCategory::Unknown,
        UaCategory::Browser,
    ];

    /// Stable machine key (same as the serde form).
    pub fn key(self) -> &'static str {
        match self {
            UaCategory::SearchEngine => "search_engine",
            UaCategory::AiSearchApi => "ai_search_api",
            UaCategory::AiAssistant => "ai_assistant",
            UaCategory::AiTraining => "ai_training",
            UaCategory::Seo => "seo",
            UaCategory::SocialPreview => "social_preview",
            UaCategory::Monitoring => "monitoring",
            UaCategory::Scanner => "scanner",
            UaCategory::Script => "script",
            UaCategory::Internal => "internal",
            UaCategory::OtherBot => "other_bot",
            UaCategory::Unknown => "unknown",
            UaCategory::Browser => "browser",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }

    /// Human label for digests.
    pub fn label(self) -> &'static str {
        match self {
            UaCategory::SearchEngine => "Search engine",
            UaCategory::AiSearchApi => "AI search API",
            UaCategory::AiAssistant => "AI assistant",
            UaCategory::AiTraining => "AI training",
            UaCategory::Seo => "SEO",
            UaCategory::SocialPreview => "Social preview",
            UaCategory::Monitoring => "Monitoring",
            UaCategory::Scanner => "Scanner",
            UaCategory::Script => "Script",
            UaCategory::Internal => "Internal",
            UaCategory::OtherBot => "Other bot",
            UaCategory::Unknown => "Unknown",
            UaCategory::Browser => "Browser",
        }
    }

    /// Everything except `Browser` is automated traffic.
    pub fn is_bot(self) -> bool {
        self != UaCategory::Browser
    }
}

/// Result of classifying one UA string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UaClass {
    /// Product family, e.g. `"KeenableBot"`, `"Googlebot Smartphone"`,
    /// `"curl"`. For `Browser` it's the browser engine family.
    pub family: String,
    pub category: UaCategory,
}

use UaCategory as C;

/// Ordered rule table: `(lowercase needle, family, category)`. First
/// substring match wins, so specific names sit above generic ones
/// (`keenable-user` before `keenablebot`, `claude-user` before
/// `claudebot`, scanners whose UA mentions krabarena before the
/// internal `krab` rules, internal `curl/… KrabAgent` before `curl`).
const RULES: &[(&str, &str, UaCategory)] = &[
    // --- scanners that embed our own hostname in the UA ---
    ("wp-admin", "WP install probe", C::Scanner),
    ("install.php", "WP install probe", C::Scanner),
    // --- internal ---
    ("krabagent", "KrabAgent", C::Internal),
    ("krab-agent", "KrabAgent", C::Internal),
    ("krab/", "krab CLI", C::Internal),
    ("krab-cli", "krab CLI", C::Internal),
    ("krabarena-", "KrabArena internal", C::Internal),
    // --- monitoring ---
    (
        "googlestackdrivermonitoring",
        "Google Uptime Checks",
        C::Monitoring,
    ),
    ("uptimerobot", "UptimeRobot", C::Monitoring),
    ("pingdom", "Pingdom", C::Monitoring),
    ("statuscake", "StatusCake", C::Monitoring),
    ("betteruptime", "Better Uptime", C::Monitoring),
    ("datadog", "Datadog", C::Monitoring),
    // --- AI search APIs (Keenable incl. its older crawler names) ---
    ("keenable-user", "Keenable-User", C::AiSearchApi),
    ("keenablebot", "KeenableBot", C::AiSearchApi),
    ("keenable-searchbot", "KeenableBot", C::AiSearchApi),
    ("sleepbot", "KeenableBot", C::AiSearchApi),
    ("aranet-searchbot", "KeenableBot", C::AiSearchApi),
    ("exauser", "ExaUser", C::AiSearchApi),
    ("exasearchbot", "ExaSearchBot", C::AiSearchApi),
    ("perplexity-user", "Perplexity-User", C::AiSearchApi),
    ("perplexitybot", "PerplexityBot", C::AiSearchApi),
    ("pplx-", "Perplexity (other)", C::AiSearchApi),
    ("youbot", "YouBot", C::AiSearchApi),
    ("shapbot", "ShapBot (Parallel)", C::AiSearchApi),
    ("firecrawl", "Firecrawl", C::AiSearchApi),
    ("tavily", "Tavily", C::AiSearchApi),
    ("linkupbot", "LinkupBot", C::AiSearchApi),
    (
        "google-cloudvertexbot",
        "Google-CloudVertexBot",
        C::AiSearchApi,
    ),
    // --- AI assistants / assistant search indexes ---
    ("oai-searchbot", "OAI-SearchBot", C::AiAssistant),
    ("chatgpt-user", "ChatGPT-User", C::AiAssistant),
    ("oai-adsbot", "OAI-AdsBot", C::AiAssistant),
    ("claude-searchbot", "Claude-SearchBot", C::AiAssistant),
    ("claude-user", "Claude-User", C::AiAssistant),
    ("claude-web", "Claude-Web", C::AiAssistant),
    ("duckassistbot", "DuckAssistBot", C::AiAssistant),
    ("mistralai-user", "MistralAI-User", C::AiAssistant),
    ("google-notebooklm", "Google-NotebookLM", C::AiAssistant),
    (
        "meta-externalfetcher",
        "Meta-ExternalFetcher",
        C::AiAssistant,
    ),
    ("meta-webindexer", "Meta-WebIndexer", C::AiAssistant),
    ("amzn-searchbot", "Amzn-SearchBot", C::AiAssistant),
    ("xai-searchbot", "GrokBot", C::AiAssistant),
    ("grokbot", "GrokBot", C::AiAssistant),
    ("kimi-searchbot", "Kimi-SearchBot", C::AiAssistant),
    ("opencode-user", "OpenCode-User", C::AiAssistant),
    ("grok-agent", "GrokBot", C::AiAssistant),
    // --- AI training ---
    ("gptbot", "GPTBot", C::AiTraining),
    ("claudebot", "ClaudeBot", C::AiTraining),
    ("anthropic-ai", "ClaudeBot", C::AiTraining),
    ("amazonbot", "Amazonbot", C::AiTraining),
    ("bytespider", "Bytespider", C::AiTraining),
    ("tiktokspider", "TikTokSpider", C::AiTraining),
    ("meta-externalagent", "meta-externalagent", C::AiTraining),
    ("ccbot", "CCBot", C::AiTraining),
    ("google-extended", "Google-Extended", C::AiTraining),
    ("applebot-extended", "Applebot-Extended", C::AiTraining),
    ("cohere-ai", "cohere-ai", C::AiTraining),
    ("deepseekbot", "DeepSeekBot", C::AiTraining),
    ("qwenbot", "Qwenbot", C::AiTraining),
    ("kimibot", "KimiBot", C::AiTraining),
    ("moonshotbot", "MoonshotBot", C::AiTraining),
    ("chatglm-spider", "ChatGLM-Spider", C::AiTraining),
    ("pangubot", "PanguBot", C::AiTraining),
    ("yibot", "YiBot", C::AiTraining),
    ("reflectionbot", "Reflectionbot", C::AiTraining),
    ("beansllm", "BeansLLM-CorpusBot", C::AiTraining),
    ("tokenfoundrybot", "TokenFoundryBot", C::AiTraining),
    ("aiwebindex", "AIWebIndex", C::AiTraining),
    ("diffbot", "Diffbot", C::AiTraining),
    ("omgili", "Omgili", C::AiTraining),
    ("ai2bot", "AI2Bot", C::AiTraining),
    ("hunyuan", "Hunyuan", C::AiTraining),
    // --- search engines ---
    ("googlebot-image", "Googlebot-Image", C::SearchEngine),
    (
        "google-inspectiontool",
        "Google-InspectionTool",
        C::SearchEngine,
    ),
    ("googleother", "GoogleOther", C::SearchEngine),
    // "googlebot" itself is handled in `classify_ua` (desktop vs smartphone).
    ("bingbot", "Bingbot", C::SearchEngine),
    ("applebot", "Applebot", C::SearchEngine),
    ("petalbot", "PetalBot", C::SearchEngine),
    ("yandex", "YandexBot", C::SearchEngine),
    ("baiduspider", "Baiduspider", C::SearchEngine),
    ("seznambot", "SeznamBot", C::SearchEngine),
    ("duckduckbot", "DuckDuckBot", C::SearchEngine),
    ("sogou", "Sogou", C::SearchEngine),
    ("360spider", "360Spider", C::SearchEngine),
    ("yisouspider", "YisouSpider", C::SearchEngine),
    ("qwantbot", "Qwantbot", C::SearchEngine),
    ("bravebot", "Bravebot", C::SearchEngine),
    ("mojeekbot", "MojeekBot", C::SearchEngine),
    ("ibou-search", "ibou", C::SearchEngine),
    // --- SEO / marketing ---
    ("semrush", "SemrushBot", C::Seo),
    ("ahrefs", "AhrefsBot", C::Seo),
    ("mj12bot", "MJ12bot", C::Seo),
    ("dataforseo", "DataForSeoBot", C::Seo),
    ("barkrowler", "Barkrowler", C::Seo),
    ("seranking", "SERankingBot", C::Seo),
    ("serpstatbot", "serpstatbot", C::Seo),
    ("dotbot", "DotBot (Moz)", C::Seo),
    ("rogerbot", "rogerbot (Moz)", C::Seo),
    ("seojuice", "SEOJuice", C::Seo),
    ("awariobot", "AwarioBot", C::Seo),
    ("qlyzebot", "QlyzeBot", C::Seo),
    ("trendiction", "trendictionbot", C::Seo),
    ("blexbot", "BLEXBot", C::Seo),
    ("screaming frog", "Screaming Frog", C::Seo),
    ("linkfluence", "linkfluence", C::Seo),
    ("hubspot", "HubSpot", C::Seo),
    ("keys-so-bot", "keys-so-bot", C::Seo),
    ("poweredbybot", "PoweredByBot", C::Seo),
    ("pandalytics", "Pandalytics", C::Seo),
    ("webxtrendbot", "webXtrendBot", C::Seo),
    ("checkmarknetwork", "CheckMarkNetwork", C::Seo),
    ("builtwith", "BuiltWith", C::Seo),
    // --- social previews ---
    ("discordbot", "Discordbot", C::SocialPreview),
    (
        "facebookexternalhit",
        "facebookexternalhit",
        C::SocialPreview,
    ),
    ("facebot", "facebookexternalhit", C::SocialPreview),
    ("twitterbot", "Twitterbot", C::SocialPreview),
    ("slackbot", "Slackbot", C::SocialPreview),
    ("linkedinbot", "LinkedInBot", C::SocialPreview),
    ("telegrambot", "TelegramBot", C::SocialPreview),
    ("whatsapp", "WhatsApp", C::SocialPreview),
    ("skypeuripreview", "Skype", C::SocialPreview),
    ("redditbot", "redditbot", C::SocialPreview),
    ("embedly", "Embedly", C::SocialPreview),
    ("iframely", "Iframely", C::SocialPreview),
    ("mastodon", "Mastodon", C::SocialPreview),
    ("bluesky", "Bluesky", C::SocialPreview),
    ("unfurl", "Unfurler (other)", C::SocialPreview),
    // iMessage / macOS link previews.
    (
        "networkingextension/",
        "Apple link preview",
        C::SocialPreview,
    ),
    (
        "com.apple.webkit.networking",
        "Apple link preview",
        C::SocialPreview,
    ),
    // --- scanners ---
    ("censysinspect", "CensysInspect", C::Scanner),
    ("zgrab", "zgrab", C::Scanner),
    ("l9explore", "l9explore", C::Scanner),
    ("paloaltonetworks", "Palo Alto Xpanse", C::Scanner),
    ("expanse", "Palo Alto Xpanse", C::Scanner),
    (
        "software security research",
        "Software Security Research",
        C::Scanner,
    ),
    ("research-scan", "research-scan", C::Scanner),
    ("geminiscanner", "GeminiScanner", C::Scanner),
    ("wp-safe-scanner", "WP-Safe-Scanner", C::Scanner),
    ("infrawatch", "Infrawatch", C::Scanner),
    ("libredtail", "libredtail", C::Scanner),
    ("nuclei", "nuclei", C::Scanner),
    ("masscan", "masscan", C::Scanner),
    ("nmap", "Nmap", C::Scanner),
    ("internetmeasurement", "InternetMeasurement", C::Scanner),
    ("modatscanner", "ModatScanner", C::Scanner),
    ("shodan", "Shodan", C::Scanner),
    ("scanner", "Scanner (other)", C::Scanner),
    ("scanning", "Scanner (other)", C::Scanner),
    ("/scan", "Scanner (other)", C::Scanner),
    ("proxycheck", "ProxyCheck", C::Scanner),
    // --- scripts / HTTP libraries / headless browsers ---
    ("headlesschrome", "HeadlessChrome", C::Script),
    ("phantomjs", "PhantomJS", C::Script),
    ("python-requests", "python-requests", C::Script),
    ("python-urllib", "Python-urllib", C::Script),
    ("python-httpx", "python-httpx", C::Script),
    ("aiohttp", "aiohttp", C::Script),
    ("urllib3", "urllib3", C::Script),
    ("go-http-client", "Go-http-client", C::Script),
    ("axios", "axios", C::Script),
    ("node-fetch", "node-fetch", C::Script),
    ("undici", "undici", C::Script),
    ("okhttp", "okhttp", C::Script),
    ("apache-httpclient", "Apache-HttpClient", C::Script),
    ("java/", "Java", C::Script),
    ("libwww-perl", "libwww-perl", C::Script),
    ("scrapy", "Scrapy", C::Script),
    ("curl/", "curl", C::Script),
    ("wget", "Wget", C::Script),
    ("httpie", "HTTPie", C::Script),
    ("websiphon", "WebSiphon", C::Script),
    ("markdownworker", "MarkdownWorker", C::Script),
    ("litellm", "litellm", C::Script),
    ("wordpress/", "WordPress", C::Script),
    ("dalvik/", "Dalvik", C::Script),
    ("fasthttp", "fasthttp", C::Script),
];

/// Exact (whole-string, case-insensitive) script UAs too short to match
/// safely as substrings.
const EXACT_SCRIPTS: &[(&str, &str)] = &[
    ("node", "node"),
    ("python", "python"),
    ("ruby", "ruby"),
    ("php", "php"),
];

/// Generic markers of a self-declared bot (checked after all rules).
const GENERIC_BOT_MARKERS: &[&str] = &["bot", "crawl", "spider", "scrape", "fetcher", "archiver"];

/// Classify a raw User-Agent header value.
pub fn classify_ua(ua: &str) -> UaClass {
    let raw = ua.trim();
    let lower = raw.to_ascii_lowercase();
    let mk = |family: &str, category: UaCategory| UaClass {
        family: family.to_string(),
        category,
    };

    if lower.is_empty() {
        return mk("(empty)", C::Unknown);
    }
    if lower == "krab" {
        return mk("krab CLI", C::Internal);
    }
    for (needle, family) in EXACT_SCRIPTS {
        if lower == *needle {
            return mk(family, C::Script);
        }
    }
    // Googlebot: split desktop vs smartphone (the smartphone UA embeds a
    // Nexus 5X Android browser string). Checked before the table so the
    // `googlebot-image` style variants can still win via the table when
    // they're more specific.
    if lower.contains("googlebot") && !lower.contains("googlebot-") {
        return if lower.contains("android") || lower.contains("mobile") {
            mk("Googlebot Smartphone", C::SearchEngine)
        } else {
            mk("Googlebot Desktop", C::SearchEngine)
        };
    }
    for (needle, family, category) in RULES {
        if lower.contains(needle) {
            return mk(family, *category);
        }
    }
    if let Some(browser) = browser_family(raw, &lower) {
        return mk(browser, C::Browser);
    }
    if GENERIC_BOT_MARKERS.iter().any(|m| lower.contains(m)) {
        return mk(&generic_bot_name(raw), C::OtherBot);
    }
    // Something with a contact URL but no browser shape is a bot too.
    if lower.contains("http://") || lower.contains("https://") || lower.contains('@') {
        return mk(&generic_bot_name(raw), C::OtherBot);
    }
    mk("(unrecognised)", C::Unknown)
}

/// A UA "looks like a real browser" when it has the `Mozilla/5.0 (…)`
/// platform prefix AND a browser product token. Bare `Mozilla/5.0`,
/// or the truncated `Mozilla/5.0 (Windows NT 10.0; Win64; x64)
/// AppleWebKit/537.36` scrapers use, fail the second check.
fn browser_family(raw: &str, lower: &str) -> Option<&'static str> {
    if !raw.starts_with("Mozilla/5.0 (") {
        return None;
    }
    if lower.contains("compatible;") && !lower.contains("msie") && !lower.contains("trident") {
        // `(compatible; Foo/1.0)` is how bots self-declare.
        return None;
    }
    if lower.contains("edg/") {
        Some("Edge")
    } else if lower.contains("opr/") {
        Some("Opera")
    } else if lower.contains("samsungbrowser/") {
        Some("Samsung Internet")
    } else if lower.contains("firefox/") || lower.contains("fxios/") {
        Some("Firefox")
    } else if lower.contains("chrome/") || lower.contains("crios/") {
        Some("Chrome")
    } else if lower.contains("safari/") && lower.contains("version/") {
        Some("Safari")
    } else if lower.contains("mobile/") && lower.contains("applewebkit/") {
        // iOS in-app webviews (no Safari/ token).
        Some("Safari")
    } else if lower.contains("trident/") || lower.contains("msie") {
        Some("Internet Explorer")
    } else {
        None
    }
}

/// Best-effort product name for an unlisted bot: the product declared
/// after `compatible;`, else the first token that contains a bot
/// marker, else the first product token — without its `/version`.
fn generic_bot_name(raw: &str) -> String {
    if let Some(i) = raw.to_ascii_lowercase().find("compatible;") {
        let after = raw[i + "compatible;".len()..].trim_start();
        let product = after
            .split(|c: char| matches!(c, ';' | ')' | '(' | ',' | '+') || c.is_whitespace())
            .next()
            .unwrap_or("");
        let name = product.split('/').next().unwrap_or("");
        if name.len() >= 2 {
            return name.chars().take(40).collect();
        }
    }
    let tokens: Vec<&str> = raw
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '(' | ')' | ',' | '+'))
        .filter(|t| !t.is_empty())
        .collect();
    let pick = tokens
        .iter()
        .find(|t| {
            let l = t.to_ascii_lowercase();
            !l.starts_with("http") && GENERIC_BOT_MARKERS.iter().any(|m| l.contains(m))
        })
        .or_else(|| {
            tokens.iter().find(|t| {
                let l = t.to_ascii_lowercase();
                !l.starts_with("mozilla/")
                    && !l.starts_with("applewebkit/")
                    && !matches!(l.as_str(), "compatible" | "khtml" | "like" | "gecko")
                    && !t.contains("://")
            })
        })
        .copied()
        .unwrap_or("(bot)");
    let name = pick.split('/').next().unwrap_or(pick);
    let name: String = name.chars().take(40).collect();
    if name.is_empty() {
        "(bot)".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fam(ua: &str) -> (String, UaCategory) {
        let c = classify_ua(ua);
        (c.family, c.category)
    }

    fn assert_class(ua: &str, family: &str, cat: UaCategory) {
        let (f, c) = fam(ua);
        assert_eq!((f.as_str(), c), (family, cat), "UA: {ua}");
    }

    #[test]
    fn keenable_and_its_old_names_are_one_family() {
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; KeenableBot/1.0; +https://keenable.ai/bot)",
            "KeenableBot",
            C::AiSearchApi,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; SleepBot/1.0; +http://sleepbot.com/) Chrome/131.0.0.0 Safari/537.36",
            "KeenableBot",
            C::AiSearchApi,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Aranet-SearchBot/1.0; +https://aranet.ai/bot) Chrome/131.0.0.0 Safari/537.36",
            "KeenableBot",
            C::AiSearchApi,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Keenable-User/1.0; +https://keenable.ai/)",
            "Keenable-User",
            C::AiSearchApi,
        );
    }

    #[test]
    fn exa_user_vs_exa_searchbot() {
        assert_class(
            "Mozilla/5.0 (compatible; ExaUser/1.0; +https://crawler.exa.ai/exauser)",
            "ExaUser",
            C::AiSearchApi,
        );
        assert_class(
            "Mozilla/5.0 (compatible; ExaSearchBot/1.0; +https://crawler.exa.ai/)",
            "ExaSearchBot",
            C::AiSearchApi,
        );
        assert_class("ExaSearchBot", "ExaSearchBot", C::AiSearchApi);
    }

    #[test]
    fn googlebot_smartphone_vs_desktop() {
        assert_class(
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            "Googlebot Desktop",
            C::SearchEngine,
        );
        assert_class(
            "Mozilla/5.0 (Linux; Android 6.0.1; Nexus 5X Build/MMB29P) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.7871.186 Mobile Safari/537.36 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            "Googlebot Smartphone",
            C::SearchEngine,
        );
        assert_class("Googlebot-Image/1.0", "Googlebot-Image", C::SearchEngine);
        assert_class(
            "Mozilla/5.0 (compatible; Google-Extended/1.0; +http://www.google.com/bot.html)",
            "Google-Extended",
            C::AiTraining,
        );
    }

    #[test]
    fn ai_assistants_vs_training_crawlers() {
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; ChatGPT-User/1.0; +https://openai.com/bot",
            "ChatGPT-User",
            C::AiAssistant,
        );
        assert_class(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36; compatible; OAI-SearchBot/1.4; +https://openai.com/searchbot",
            "OAI-SearchBot",
            C::AiAssistant,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Claude-User/1.0; +claudebot@anthropic.com)",
            "Claude-User",
            C::AiAssistant,
        );
        assert_class(
            "Claude-User (claude-code/2.1.296; +https://support.anthropic.com/)",
            "Claude-User",
            C::AiAssistant,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Claude-SearchBot/1.0; +https://www.anthropic.com/claude-searchbot)",
            "Claude-SearchBot",
            C::AiAssistant,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; ClaudeBot/1.0; +claudebot@anthropic.com)",
            "ClaudeBot",
            C::AiTraining,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.4; +https://openai.com/gptbot)",
            "GPTBot",
            C::AiTraining,
        );
        assert_class(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 (compatible; meta-externalagent/1.1 (+https://developers.facebook.com/docs/sharing/webmasters/crawler))",
            "meta-externalagent",
            C::AiTraining,
        );
        assert_class(
            "CCBot/2.0 (https://commoncrawl.org/faq/)",
            "CCBot",
            C::AiTraining,
        );
        assert_class(
            "Mozilla/5.0 (Linux; Android 5.0) AppleWebKit/537.36 (KHTML, like Gecko) Mobile Safari/537.36 (compatible; Bytespider; spider-feedback@bytedance.com)",
            "Bytespider",
            C::AiTraining,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; ShapBot/0.1.0",
            "ShapBot (Parallel)",
            C::AiSearchApi,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; Perplexity-User/1.0; +https://perplexity.ai/perplexity-user)",
            "Perplexity-User",
            C::AiSearchApi,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; PerplexityBot/1.0; +https://perplexity.ai/perplexitybot)",
            "PerplexityBot",
            C::AiSearchApi,
        );
    }

    #[test]
    fn search_seo_social() {
        assert_class(
            "Mozilla/5.0 (Linux; Android 7.0;) AppleWebKit/537.36 (HTML, like Gecko) Mobile Safari/537.36 (compatible; PetalBot;+https://webmaster.petalsearch.com/site/petalbot)",
            "PetalBot",
            C::SearchEngine,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm) Chrome/116.0.1938.76 Safari/537.36",
            "Bingbot",
            C::SearchEngine,
        );
        assert_class(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15 (Applebot/0.1; +http://www.apple.com/go/applebot)",
            "Applebot",
            C::SearchEngine,
        );
        assert_class(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0; 360Spider",
            "360Spider",
            C::SearchEngine,
        );
        assert_class(
            "Mozilla/5.0 (compatible; SemrushBot/7~bl; +http://www.semrush.com/bot.html)",
            "SemrushBot",
            C::Seo,
        );
        assert_class(
            "Mozilla/5.0 (compatible; SERankingBacklinksBot/1.0; +https://seranking.com/backlinks-crawler)",
            "SERankingBot",
            C::Seo,
        );
        assert_class(
            "Mozilla/5.0 (compatible; Discordbot/2.0; +https://discordapp.com)",
            "Discordbot",
            C::SocialPreview,
        );
        assert_class("Twitterbot/1.0", "Twitterbot", C::SocialPreview);
        assert_class(
            "facebookexternalhit/1.1 (+http://www.facebook.com/externalhit_uatext.php)",
            "facebookexternalhit",
            C::SocialPreview,
        );
        assert_class(
            "LinkedInBot/1.0 (compatible; Mozilla/5.0; Apache-HttpClient +http://www.linkedin.com)",
            "LinkedInBot",
            C::SocialPreview,
        );
    }

    #[test]
    fn scripts_scanners_internal_monitoring() {
        assert_class("curl/7.88.1", "curl", C::Script);
        assert_class("python-requests/2.32.5", "python-requests", C::Script);
        assert_class("Python-urllib/3.11", "Python-urllib", C::Script);
        assert_class("Go-http-client/2.0", "Go-http-client", C::Script);
        assert_class("axios/1.11.0", "axios", C::Script);
        assert_class(
            "node-fetch/1.0 (+https://github.com/bitinn/node-fetch)",
            "node-fetch",
            C::Script,
        );
        assert_class("node", "node", C::Script);
        assert_class(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/131.0.6778.33 Safari/537.36",
            "HeadlessChrome",
            C::Script,
        );
        // Internal wins over the generic curl rule.
        assert_class("curl/8.0 KrabAgent", "KrabAgent", C::Internal);
        assert_class("krabagent-planner", "KrabAgent", C::Internal);
        assert_class(
            "KrabAgent daily under-served planner",
            "KrabAgent",
            C::Internal,
        );
        assert_class("krab-agent", "KrabAgent", C::Internal);
        assert_class("krab/0.43.6", "krab CLI", C::Internal);
        assert_class("krab", "krab CLI", C::Internal);
        // A WP install probe whose UA is our own URL is a scanner, not internal.
        assert_class(
            "http://krabarena.org/wp-admin/install.php?step=1",
            "WP install probe",
            C::Scanner,
        );
        assert_class(
            "Mozilla/5.0 (compatible; CensysInspect/1.1; +https://about.censys.io/)",
            "CensysInspect",
            C::Scanner,
        );
        assert_class("Mozilla/5.0 zgrab/0.x", "zgrab", C::Scanner);
        assert_class(
            "GoogleStackdriverMonitoring-UptimeChecks(https://cloud.google.com/monitoring)",
            "Google Uptime Checks",
            C::Monitoring,
        );
    }

    #[test]
    fn real_browsers_are_not_bots() {
        for ua in [
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36 Edg/144.0.0.0",
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:151.0) Gecko/20100101 Firefox/151.0",
            "Mozilla/5.0 (Android 12; Mobile; rv:155.0) Gecko/155.0 Firefox/155.0",
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
            "Mozilla/5.0 (Linux; Android 7.0; SAMSUNG SM-G930T Build/NRD90M) AppleWebKit/537.36 (KHTML, like Gecko) SamsungBrowser/6.2 Chrome/56.0.2924.87 Mobile Safari/537.36",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7_2) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
        ] {
            let c = classify_ua(ua);
            assert_eq!(c.category, C::Browser, "UA misclassified as {c:?}: {ua}");
        }
        assert_eq!(
            classify_ua("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36 Edg/144.0.0.0").family,
            "Edge"
        );
    }

    #[test]
    fn spoofy_and_malformed_uas_are_not_humans() {
        // Truncated "browser" UA with no product token.
        assert_eq!(
            classify_ua("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36").category,
            C::Unknown
        );
        assert_eq!(classify_ua("Mozilla/5.0").category, C::Unknown);
        assert_eq!(classify_ua("").category, C::Unknown);
        assert_eq!(classify_ua("   ").family, "(empty)");
        // Self-declared bots without a rule land in other_bot with a name.
        assert_class(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 (compatible; AionBot/1.0)",
            "AionBot",
            C::OtherBot,
        );
        assert_class(
            "KingfisherBot/1.0 (+https://github.com/tldrmedia/kingfisher)",
            "KingfisherBot",
            C::OtherBot,
        );
        assert_class("jobs-crawler/0.2", "jobs-crawler", C::OtherBot);
        // The product after `compatible;` names the bot.
        assert_class(
            "Mozilla/5.0 (compatible; CyberConvoyScout/1.0; +https://scout.cyberconvoy.co)",
            "CyberConvoyScout",
            C::OtherBot,
        );
        assert_class(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; OpenCode-User/1.0; +https://opencode.ai",
            "OpenCode-User",
            C::AiAssistant,
        );
        assert_class(
            "NetworkingExtension/8624.5.1.10.3 Network/5812.160.9 iOS/26.6.2",
            "Apple link preview",
            C::SocialPreview,
        );
        // A browser-shaped UA carrying a bot marker is a bot.
        assert_eq!(
            classify_ua("Mozilla/5.0 (compatible; YandexBot/3.0; +http://yandex.com/bots) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/108.0.0.0").category,
            C::SearchEngine
        );
    }

    #[test]
    fn category_keys_round_trip() {
        for c in UaCategory::ALL {
            assert_eq!(UaCategory::from_key(c.key()), Some(c));
            assert_eq!(serde_json::to_value(c).unwrap(), c.key());
            assert_eq!(c.is_bot(), c != C::Browser);
        }
    }
}
