//! The Crawler Zoo (`/zoo`) — a live menagerie of the bots that visit
//! krabarena.com, inspired by crawlerzoo.com.
//!
//! **Data source.** Everything here is derived from `user_logs` (every
//! HTML `GET` is logged by `web::auth_context`), with User-Agents
//! classified by [`ua_classifier::classify_ua`] — the same single source
//! of truth `GET /v1/admin/usage` uses. Classification happens in Rust
//! over the *distinct* UAs of a window; SQL only ever aggregates per UA.
//! No IP addresses are stored or shown, so identity is **UA-claimed**:
//! nothing here verifies that a "Googlebot" really is Google.
//!
//! **Model.**
//! - A *species* is a classifier family (`GPTBot`, `Bingbot`, …).
//! - Each species lives in an [`Enclosure`] picked from its category.
//!   KrabArena's own agents (`Internal`), uptime monitors and real
//!   browsers are not exhibits.
//! - Its [`Conservation`] status comes from robots.txt behaviour over the
//!   last 30 days: did it read `/robots.txt`, and did it fetch a path
//!   our robots.txt disallows ([`DISALLOWED_PREFIXES`] — the same list
//!   `web::seo::robots_txt` serves, so the two can't drift).
//! - The **Trap Room** is [`TRAP_PREFIX`]: disallowed in robots.txt and
//!   linked only from a hidden anchor on zoo pages, so only a crawler
//!   that ignores robots.txt ever walks in.
//!
//! **Cost.** The 30-day per-UA aggregate is memoised for
//! [`SNAPSHOT_TTL`]; the all-time first-seen / total scan for
//! [`ALL_TIME_TTL`]; per-species detail for [`DETAIL_TTL`]. All caches are
//! process-local statics (one Cloud Run instance recomputes at most once
//! per window).

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use sqlx::PgPool;

use crate::cache::TtlCache;
use crate::services::ua_classifier::{UaCategory, classify_ua};

// ---------------------------------------------------------------------------
// robots.txt rules (shared with web::seo)
// ---------------------------------------------------------------------------

/// Path prefixes our robots.txt disallows for `User-agent: *`. The
/// robots.txt handler renders exactly this list; the zoo counts a
/// request as "disallowed" when its path starts with one of them.
/// Note `/v1/` and `/auth/` are never in `user_logs` (not HTML routes),
/// so in practice the observable ones are the rest.
pub const DISALLOWED_PREFIXES: &[&str] = &[
    "/v1/",
    "/auth/",
    "/internal/",
    "/profile/",
    "/notifications",
    TRAP_PREFIX,
];

/// The Trap Room. Disallowed for everyone; reachable only via a hidden
/// link on zoo pages.
pub const TRAP_PREFIX: &str = "/zoo/trap/";

/// Is `path` disallowed by our robots.txt (prefix match, RFC 9309)?
pub fn is_disallowed(path: &str) -> bool {
    DISALLOWED_PREFIXES.iter().any(|p| path.starts_with(p))
}

/// `LIKE` patterns for [`DISALLOWED_PREFIXES`] (none contain `%`/`_`).
fn disallowed_like_patterns() -> Vec<String> {
    DISALLOWED_PREFIXES
        .iter()
        .map(|p| format!("{p}%"))
        .collect()
}

/// First day `user_logs` has data — the day the zoo "opened". Plate
/// numbers count from here.
pub const ZOO_OPENED: (i32, u32, u32) = (2026, 7, 13);

pub const SNAPSHOT_TTL: StdDuration = StdDuration::from_secs(60);
pub const ALL_TIME_TTL: StdDuration = StdDuration::from_secs(60 * 60);
pub const DETAIL_TTL: StdDuration = StdDuration::from_secs(5 * 60);

/// Accession-register length on the home page.
pub const REGISTER_LEN: usize = 24;

// ---------------------------------------------------------------------------
// Enclosures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Enclosure {
    Savanna,
    Aviary,
    Swamp,
    Petting,
    Feral,
}

impl Enclosure {
    pub const ALL: [Enclosure; 5] = [
        Enclosure::Aviary,
        Enclosure::Savanna,
        Enclosure::Swamp,
        Enclosure::Petting,
        Enclosure::Feral,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Enclosure::Savanna => "savanna",
            Enclosure::Aviary => "aviary",
            Enclosure::Swamp => "swamp",
            Enclosure::Petting => "petting",
            Enclosure::Feral => "feral",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Enclosure::Savanna => "The Savanna",
            Enclosure::Aviary => "The AI Aviary",
            Enclosure::Swamp => "The SEO Swamp",
            Enclosure::Petting => "The Petting Zoo",
            Enclosure::Feral => "The Feral Pit",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Enclosure::Savanna => {
                "Classic search-engine crawlers. Large, slow-moving, mostly harmless grazers."
            }
            Enclosure::Aviary => {
                "AI crawlers: agent search APIs, assistant fetchers, training scrapers."
            }
            Enclosure::Swamp => "SEO and marketing crawlers. They map every link and sell the map.",
            Enclosure::Petting => "Link unfurlers. They only come when someone shares a link.",
            Enclosure::Feral => {
                "Scanners, scripts and visitors with no name. Do not feed through the bars."
            }
        }
    }

    pub fn emoji(self) -> &'static str {
        match self {
            Enclosure::Savanna => "🦒",
            Enclosure::Aviary => "🦜",
            Enclosure::Swamp => "🐊",
            Enclosure::Petting => "🐐",
            Enclosure::Feral => "🐀",
        }
    }
}

/// Which enclosure a classifier category lives in. `None` = not an
/// exhibit (real browsers, KrabArena's own agents).
pub fn enclosure_for(category: UaCategory) -> Option<Enclosure> {
    use UaCategory as C;
    match category {
        C::SearchEngine => Some(Enclosure::Savanna),
        C::AiSearchApi | C::AiAssistant | C::AiTraining => Some(Enclosure::Aviary),
        C::Seo => Some(Enclosure::Swamp),
        C::SocialPreview => Some(Enclosure::Petting),
        C::Scanner | C::Script | C::OtherBot | C::Unknown => Some(Enclosure::Feral),
        // Uptime checks are the keepers' own equipment (Google Uptime
        // Checks alone is ~70% of raw bot hits, all on /health); KrabArena's
        // agents are the keepers. Neither is an exhibit.
        C::Monitoring | C::Internal | C::Browser => None,
    }
}

// ---------------------------------------------------------------------------
// Conservation status (robots.txt behaviour)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Conservation {
    /// Read robots.txt, never fetched a disallowed path.
    Respects,
    /// Read robots.txt, fetched a disallowed path anyway.
    Selective,
    /// Never read robots.txt, fetched a disallowed path.
    Feral,
    /// Never read robots.txt, never fetched a disallowed path (yet).
    Unassessed,
}

impl Conservation {
    pub const ALL: [Conservation; 4] = [
        Conservation::Respects,
        Conservation::Selective,
        Conservation::Feral,
        Conservation::Unassessed,
    ];

    pub fn classify(robots_reads: i64, disallowed: i64) -> Self {
        match (robots_reads > 0, disallowed > 0) {
            (true, false) => Conservation::Respects,
            (true, true) => Conservation::Selective,
            (false, true) => Conservation::Feral,
            (false, false) => Conservation::Unassessed,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Conservation::Respects => "RR",
            Conservation::Selective => "SO",
            Conservation::Feral => "FE",
            Conservation::Unassessed => "NR",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Conservation::Respects => "respects",
            Conservation::Selective => "selective",
            Conservation::Feral => "feral",
            Conservation::Unassessed => "unassessed",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Conservation::Respects => "Respects robots.txt",
            Conservation::Selective => "Selectively obedient",
            Conservation::Feral => "Feral",
            Conservation::Unassessed => "No robots.txt record yet",
        }
    }

    /// Gate-count line for the home page.
    pub fn gate_label(self) -> &'static str {
        match self {
            Conservation::Respects => "From bots that respect robots.txt",
            Conservation::Selective => "From bots that read it, broke it before",
            Conservation::Feral => "From bots that broke it unread",
            Conservation::Unassessed => "From bots with no robots.txt record yet",
        }
    }
}

// ---------------------------------------------------------------------------
// Species catalogue (the keepers' field guide)
// ---------------------------------------------------------------------------

/// Curated facts about a well-known family. Anything not listed falls
/// back to [`generic_species`] text for its category.
struct Curated {
    family: &'static str,
    /// Display name when it differs from the classifier family.
    name: Option<&'static str>,
    latin: &'static str,
    operator: &'static str,
    about: &'static str,
    temperament: &'static str,
    /// The robots.txt `User-agent` token(s) that address it. Empty =
    /// no meaningful token (block by rate limit instead).
    robots_tokens: &'static [&'static str],
    docs_url: Option<&'static str>,
}

macro_rules! sp {
    ($family:expr, $latin:expr, $op:expr, $about:expr, $temper:expr, [$($tok:expr),*], $docs:expr) => {
        Curated {
            family: $family,
            name: None,
            latin: $latin,
            operator: $op,
            about: $about,
            temperament: $temper,
            robots_tokens: &[$($tok),*],
            docs_url: $docs,
        }
    };
}

const CATALOGUE: &[Curated] = &[
    // --- AI search APIs ---
    sp!(
        "KeenableBot",
        "Quaesitor agentium",
        "Keenable.ai",
        "The crawler for Keenable, which sells search and page-reading APIs to AI agents. It fetches public pages to build Keenable's search index; answers built on that index link back to the source.",
        "helpful. Reads ahead so AI agents don't have to. Leaves a link behind.",
        ["KeenableBot", "Keenable-User"],
        Some("https://keenable.ai/bot")
    ),
    sp!(
        "Keenable-User",
        "Quaesitor obsequens",
        "Keenable.ai",
        "Keenable's on-demand fetcher: it reads a single page when an AI agent asks Keenable for it, and does not crawl.",
        "punctual. Comes when called, takes one page, leaves.",
        ["Keenable-User"],
        Some("https://keenable.ai/bot")
    ),
    sp!(
        "ExaSearchBot",
        "Exa indagatrix",
        "Exa",
        "The crawler behind Exa's neural search API, which AI developers use to find and read web pages.",
        "studious. Reads for meaning, not for keywords.",
        ["ExaSearchBot"],
        Some("https://exa.ai")
    ),
    sp!(
        "ExaUser",
        "Exa obsequens",
        "Exa",
        "Exa's on-demand fetcher, used when an Exa customer asks for the contents of a specific page.",
        "prompt. Fetches exactly what it was asked for.",
        ["ExaUser"],
        Some("https://exa.ai")
    ),
    sp!(
        "PerplexityBot",
        "Perplexa curiosa",
        "Perplexity",
        "Perplexity's crawler. It indexes pages so the Perplexity answer engine can cite them.",
        "inquisitive. Every answer needs a footnote.",
        ["PerplexityBot"],
        Some("https://docs.perplexity.ai/guides/bots")
    ),
    sp!(
        "Perplexity-User",
        "Perplexa obsequens",
        "Perplexity",
        "Fetches a page when a Perplexity user's question needs it. It acts for a person, so it may not consult robots.txt.",
        "eager. Runs errands for whoever is asking.",
        ["Perplexity-User"],
        Some("https://docs.perplexity.ai/guides/bots")
    ),
    sp!(
        "YouBot",
        "Tu quaerens",
        "You.com",
        "The crawler for You.com's search engine and search API.",
        "chatty. Wants to know about you, specifically.",
        ["YouBot"],
        Some("https://about.you.com/youbot/")
    ),
    sp!(
        "ShapBot (Parallel)",
        "Parallela formans",
        "Parallel Web Systems",
        "The crawler for Parallel, a web search API built for AI agents.",
        "efficient. Does several things at once.",
        ["ShapBot"],
        Some("https://parallel.ai")
    ),
    sp!(
        "Firecrawl",
        "Ignis repens",
        "Firecrawl (Mendable)",
        "A scraping API that turns websites into LLM-ready markdown for whoever is paying.",
        "fiery. Leaves nothing unread.",
        ["FirecrawlAgent"],
        Some("https://firecrawl.dev")
    ),
    sp!(
        "Tavily",
        "Tavilia festinans",
        "Tavily",
        "A search and extraction API for AI agents. Fetches pages on behalf of its customers' agents.",
        "hurried. Somebody's agent is waiting.",
        [],
        Some("https://tavily.com")
    ),
    // --- AI assistants ---
    sp!(
        "ChatGPT-User",
        "Chatgptus obsequens",
        "OpenAI",
        "Fetches a page when a ChatGPT user (or a custom GPT) asks for it. Not used for training or automatic crawling.",
        "obliging. Only visits when somebody asked it to.",
        ["ChatGPT-User"],
        Some("https://platform.openai.com/docs/bots")
    ),
    sp!(
        "OAI-SearchBot",
        "Openaia exploratrix",
        "OpenAI",
        "OpenAI's search crawler. It indexes pages so they can appear, with links, in ChatGPT search results. Not used for training.",
        "diligent. Wants to be able to cite you later.",
        ["OAI-SearchBot"],
        Some("https://platform.openai.com/docs/bots")
    ),
    sp!(
        "Claude-User",
        "Claudius obsequens",
        "Anthropic",
        "Fetches a page when a Claude user asks Claude to read it.",
        "courteous. Asks before it reads, then reads carefully.",
        ["Claude-User"],
        Some("https://support.anthropic.com/en/articles/8896518")
    ),
    sp!(
        "Claude-SearchBot",
        "Claudius explorator",
        "Anthropic",
        "Anthropic's search crawler. It indexes pages to improve the quality of Claude's search results.",
        "thorough. Takes notes in the margins.",
        ["Claude-SearchBot"],
        Some("https://support.anthropic.com/en/articles/8896518")
    ),
    sp!(
        "Meta-WebIndexer",
        "Meta indexatrix",
        "Meta",
        "Meta's crawler for improving search inside Meta AI.",
        "sociable. Knows a friend of a friend of every page.",
        ["Meta-WebIndexer"],
        Some("https://developers.facebook.com/docs/sharing/webmasters/web-crawlers/")
    ),
    sp!(
        "Meta-ExternalFetcher",
        "Meta obsequens",
        "Meta",
        "Fetches individual links on behalf of a Meta AI user. It may not consult robots.txt.",
        "impulsive. Someone in the chat said 'look at this'.",
        ["Meta-ExternalFetcher"],
        Some("https://developers.facebook.com/docs/sharing/webmasters/web-crawlers/")
    ),
    sp!(
        "GrokBot",
        "Grokkus intellegens",
        "xAI",
        "Fetches and indexes pages for xAI's Grok assistant.",
        "irreverent. Groks it, allegedly.",
        ["GrokBot", "xAI-SearchBot"],
        None
    ),
    sp!(
        "DuckAssistBot",
        "Anas auxilians",
        "DuckDuckGo",
        "Fetches pages in real time for DuckDuckGo's AI-assisted answers. Not used for training.",
        "discreet. Doesn't tell anyone where it's been.",
        ["DuckAssistBot"],
        Some("https://duckduckgo.com/duckassistbot")
    ),
    sp!(
        "MistralAI-User",
        "Mistralis obsequens",
        "Mistral AI",
        "Fetches pages when a Le Chat user asks Mistral's assistant to read them.",
        "breezy. Blows in from the south of France.",
        ["MistralAI-User"],
        Some("https://docs.mistral.ai/robots")
    ),
    // --- AI training ---
    sp!(
        "GPTBot",
        "Gptus vorax",
        "OpenAI",
        "OpenAI's training crawler. Pages it collects may be used to train OpenAI's foundation models.",
        "ravenous. Has read most of the internet and is still hungry.",
        ["GPTBot"],
        Some("https://platform.openai.com/docs/bots")
    ),
    sp!(
        "ClaudeBot",
        "Claudius colligens",
        "Anthropic",
        "Anthropic's training crawler. It collects public pages that may be used to train Claude models.",
        "polite. Reads robots.txt first, then reads everything else.",
        ["ClaudeBot"],
        Some("https://support.anthropic.com/en/articles/8896518")
    ),
    sp!(
        "Amazonbot",
        "Amazonia praedatrix",
        "Amazon",
        "Amazon's crawler. Content may be used to improve Amazon services, including training Amazon AI models.",
        "acquisitive. Adds every page to cart.",
        ["Amazonbot"],
        Some("https://developer.amazon.com/amazonbot")
    ),
    sp!(
        "Bytespider",
        "Byta aranea",
        "ByteDance",
        "ByteDance's crawler, widely reported to collect training data for its large language models.",
        "voracious. Notorious among keepers for ignoring the sign on the door.",
        ["Bytespider"],
        None
    ),
    sp!(
        "CCBot",
        "Communis colligens",
        "Common Crawl",
        "Common Crawl's crawler. It builds the free, open web archive that most AI training datasets start from.",
        "generous. Takes everything, gives it all away.",
        ["CCBot"],
        Some("https://commoncrawl.org/ccbot")
    ),
    sp!(
        "meta-externalagent",
        "Meta vorax",
        "Meta",
        "Meta's crawler for training AI models and improving products.",
        "gregarious. Brings the whole family to every page.",
        ["meta-externalagent"],
        Some("https://developers.facebook.com/docs/sharing/webmasters/web-crawlers/")
    ),
    sp!(
        "Google-Extended",
        "Googlus extensus",
        "Google",
        "Not a separate crawler but a robots.txt token: it controls whether pages Googlebot fetches may be used for Gemini training and grounding.",
        "ghostly. Mostly seen in robots.txt files, rarely in the flesh.",
        ["Google-Extended"],
        Some("https://developers.google.com/search/docs/crawling-indexing/google-common-crawlers")
    ),
    sp!(
        "Applebot-Extended",
        "Malus extensus",
        "Apple",
        "A robots.txt token that controls whether Applebot's pages may be used to train Apple's foundation models.",
        "elusive. A control knob dressed as an animal.",
        ["Applebot-Extended"],
        Some("https://support.apple.com/en-us/119829")
    ),
    sp!(
        "Reflectionbot",
        "Speculum doctum",
        "Reflection AI",
        "Reflection AI's crawler, collecting pages for training its models.",
        "thoughtful. Looks at every page twice: once to read it, and once to think about what it read.",
        ["Reflectionbot"],
        None
    ),
    sp!(
        "TokenFoundryBot",
        "Tokena fabricatrix",
        "Unknown",
        "A self-declared corpus crawler that collects text for AI training.",
        "industrious. Melts pages down into tokens.",
        ["TokenFoundryBot"],
        None
    ),
    sp!(
        "Diffbot",
        "Differentia extrahens",
        "Diffbot",
        "Diffbot extracts structured data from web pages to build its Knowledge Graph.",
        "analytical. Sees a page as a table waiting to happen.",
        ["Diffbot"],
        Some("https://www.diffbot.com")
    ),
    // --- search engines ---
    sp!(
        "Googlebot Desktop",
        "Googlus vulgaris",
        "Google",
        "Google Search's main crawler, in its desktop form. Fetches pages so they can appear in Google results.",
        "dutiful. The oldest animal in the zoo, and still the one everybody watches.",
        ["Googlebot"],
        Some("https://developers.google.com/search/docs/crawling-indexing/googlebot")
    ),
    sp!(
        "Googlebot Smartphone",
        "Googlus mobilis",
        "Google",
        "Google Search's main crawler, in its smartphone form. Google indexes the mobile version of pages first.",
        "restless. Always checking how you look on a small screen.",
        ["Googlebot"],
        Some("https://developers.google.com/search/docs/crawling-indexing/googlebot")
    ),
    sp!(
        "GoogleOther",
        "Googlus alius",
        "Google",
        "A generic Google crawler used by product teams for one-off fetches and research outside Search.",
        "mysterious. Even Google won't say exactly what it's for.",
        ["GoogleOther"],
        Some("https://developers.google.com/search/docs/crawling-indexing/google-common-crawlers")
    ),
    sp!(
        "Bingbot",
        "Bingus diligens",
        "Microsoft",
        "Microsoft Bing's crawler. Its index also feeds Copilot, DuckDuckGo and many AI search products.",
        "dependable. Quietly feeds half the other animals.",
        ["bingbot"],
        Some("https://www.bing.com/webmasters/help/which-crawlers-does-bing-use-8c184ec0")
    ),
    sp!(
        "Applebot",
        "Malus explorator",
        "Apple",
        "Apple's crawler for Siri and Spotlight suggestions.",
        "tasteful. Only visits well-designed pages, it claims.",
        ["Applebot"],
        Some("https://support.apple.com/en-us/119829")
    ),
    sp!(
        "YandexBot",
        "Yandexus borealis",
        "Yandex",
        "The crawler for Yandex search.",
        "hardy. Thrives in cold climates.",
        ["Yandex"],
        Some("https://yandex.com/support/webmaster/robot-workings/check-yandex-robots.html")
    ),
    sp!(
        "Baiduspider",
        "Baidua aranea",
        "Baidu",
        "The crawler for Baidu search.",
        "persistent. Comes from far away and stays a while.",
        ["Baiduspider"],
        None
    ),
    sp!(
        "PetalBot",
        "Petala florens",
        "Huawei",
        "The crawler for Petal Search, Huawei's search engine.",
        "delicate. Picks pages like flowers.",
        ["PetalBot"],
        Some("https://webmaster.petalsearch.com/site/petalbot")
    ),
    sp!(
        "DuckDuckBot",
        "Anas indagatrix",
        "DuckDuckGo",
        "DuckDuckGo's own crawler, used alongside the results it licenses from Bing.",
        "private. Leaves no footprints.",
        ["DuckDuckBot"],
        Some("https://duckduckgo.com/duckduckbot")
    ),
    sp!(
        "SeznamBot",
        "Seznamus bohemicus",
        "Seznam.cz",
        "The crawler for Seznam, the Czech search engine.",
        "local. Knows every street in Prague.",
        ["SeznamBot"],
        None
    ),
    sp!(
        "Sogou",
        "Sogua sinica",
        "Sogou (Tencent)",
        "The crawler for Sogou search.",
        "nocturnal. Mostly seen while the keepers sleep.",
        ["Sogou web spider"],
        None
    ),
    // --- SEO ---
    sp!(
        "SemrushBot",
        "Semrushia mensurans",
        "Semrush",
        "Semrush's crawler. It maps backlinks and site health for Semrush's SEO customers.",
        "acquisitive. Counts every link and sells the count.",
        ["SemrushBot"],
        Some("https://www.semrush.com/bot/")
    ),
    sp!(
        "AhrefsBot",
        "Ahrefsia vinculorum",
        "Ahrefs",
        "Ahrefs' crawler, one of the busiest on the web. It builds Ahrefs' backlink index.",
        "tireless. Has followed more links than anyone alive.",
        ["AhrefsBot"],
        Some("https://ahrefs.com/robot")
    ),
    sp!(
        "MJ12bot",
        "Majestica distributa",
        "Majestic",
        "Majestic's distributed crawler, run on volunteers' machines to map the web's links.",
        "communal. Lives in a thousand small burrows.",
        ["MJ12bot"],
        Some("https://mj12bot.com")
    ),
    sp!(
        "DotBot (Moz)",
        "Moza punctata",
        "Moz",
        "Moz's link-index crawler.",
        "punctilious. Dots every i.",
        ["DotBot"],
        Some("https://moz.com/help/moz-procedures/crawlers/dotbot")
    ),
    sp!(
        "DataForSeoBot",
        "Datafora venalis",
        "DataForSEO",
        "Collects link and SERP data that DataForSEO resells through its API.",
        "mercantile. Everything is for sale.",
        ["DataForSeoBot"],
        Some("https://dataforseo.com/dataforseo-bot")
    ),
    // --- social previews / monitoring ---
    sp!(
        "Discordbot",
        "Discordia nuntia",
        "Discord",
        "Fetches a page when someone pastes its link into Discord, to draw the preview card.",
        "excitable. Arrives seconds after anyone mentions you.",
        ["Discordbot"],
        None
    ),
    sp!(
        "Slackbot",
        "Slackia nuntia",
        "Slack",
        "Unfurls links shared in Slack into preview cards.",
        "sociable. Shows up at every meeting.",
        ["Slackbot"],
        Some("https://api.slack.com/robots")
    ),
    sp!(
        "Twitterbot",
        "Avis caerulea",
        "X (Twitter)",
        "Fetches pages shared on X to draw their preview cards.",
        "loud. Announces itself to everyone.",
        ["Twitterbot"],
        None
    ),
    sp!(
        "facebookexternalhit",
        "Facies externa",
        "Meta",
        "Fetches shared links for Facebook, Instagram and Messenger previews.",
        "nosy. Wants to see the picture first.",
        ["facebookexternalhit"],
        Some("https://developers.facebook.com/docs/sharing/webmasters/web-crawlers/")
    ),
    sp!(
        "LinkedInBot",
        "Nexus professionalis",
        "LinkedIn",
        "Fetches shared links to draw LinkedIn post previews.",
        "networking. Would like to add you to its professional network.",
        ["LinkedInBot"],
        None
    ),
    sp!(
        "TelegramBot",
        "Telegramma celer",
        "Telegram",
        "Fetches shared links to draw Telegram message previews.",
        "brisk. In and out before the message is read.",
        ["TelegramBot"],
        None
    ),
    // --- scanners / scripts / the nameless ---
    sp!(
        "CensysInspect",
        "Census inspectans",
        "Censys",
        "Censys scans the whole internet and catalogues every exposed service it finds.",
        "watchful. Takes an inventory of every door in the city.",
        [],
        Some("https://about.censys.io")
    ),
    sp!(
        "curl",
        "Crispus vulgaris",
        "Whoever typed the command",
        "The command-line HTTP tool. Behind it is a person or a script, but the user agent won't say which.",
        "blunt. Asks for exactly one thing.",
        [],
        None
    ),
    sp!(
        "python-requests",
        "Python petens",
        "Whoever wrote the script",
        "Python's most popular HTTP library, running someone's script.",
        "straightforward. Doesn't bother with a disguise.",
        [],
        None
    ),
    sp!(
        "HeadlessChrome",
        "Chromius acephalus",
        "Whoever runs the automation",
        "A real Chrome browser with no window, driven by automation software (Puppeteer, Playwright, Selenium).",
        "uncanny. Looks like a browser, has no head.",
        [],
        None
    ),
    Curated {
        family: "(empty)",
        name: Some("Nameless visitor"),
        latin: "Anonymus vacuus",
        operator: "Unknown",
        about: "Sends no User-Agent header at all. Usually a script whose author never set one.",
        temperament: "silent. Says nothing, takes something.",
        robots_tokens: &[],
        docs_url: None,
    },
    Curated {
        family: "(unrecognised)",
        name: Some("Masked visitor"),
        latin: "Homo simulatus",
        operator: "Unknown",
        about: "Sends a user agent that is neither a real browser nor a declared bot: truncated, malformed or deliberately vague. Its user agent is not shown here, in case it is a person.",
        temperament: "evasive. Wears half a browser costume.",
        robots_tokens: &[],
        docs_url: None,
    },
];

/// Everything the zoo knows about a species, curated or generic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeciesInfo {
    pub family: String,
    pub name: String,
    pub slug: String,
    pub latin: String,
    pub operator: String,
    pub about: String,
    pub temperament: String,
    pub robots_tokens: Vec<String>,
    pub docs_url: Option<String>,
    pub category: UaCategory,
    /// UA strings are hidden for categories that might be a person.
    pub show_ua: bool,
}

/// Look up (or synthesise) the field-guide entry for a family.
pub fn species_info(family: &str, category: UaCategory) -> SpeciesInfo {
    let show_ua = !matches!(category, UaCategory::Unknown);
    if let Some(c) = CATALOGUE.iter().find(|c| c.family == family) {
        return SpeciesInfo {
            family: family.to_string(),
            name: c.name.unwrap_or(c.family).to_string(),
            slug: slugify(family),
            latin: c.latin.to_string(),
            operator: c.operator.to_string(),
            about: c.about.to_string(),
            temperament: c.temperament.to_string(),
            robots_tokens: c.robots_tokens.iter().map(|s| s.to_string()).collect(),
            docs_url: c.docs_url.map(str::to_string),
            category,
            show_ua,
        };
    }
    generic_species(family, category, show_ua)
}

fn generic_species(family: &str, category: UaCategory, show_ua: bool) -> SpeciesInfo {
    use UaCategory as C;
    let (about, temperament) = match category {
        C::SearchEngine => (
            "A search-engine crawler. It fetches pages to build a web search index.",
            "methodical. Reads the whole shelf, in order.",
        ),
        C::AiSearchApi => (
            "A crawler or fetcher behind a search API sold to AI builders.",
            "curious. Somebody's agent asked a question.",
        ),
        C::AiAssistant => (
            "Fetches pages on behalf of an AI assistant, usually because a person asked it something.",
            "obliging. Runs errands for a chatbot.",
        ),
        C::AiTraining => (
            "Collects public pages for an AI training corpus.",
            "ravenous. Every page is a meal.",
        ),
        C::Seo => (
            "An SEO or marketing crawler. It maps links and keywords for paying customers.",
            "acquisitive. Counts what others have built.",
        ),
        C::SocialPreview => (
            "A link unfurler: it fetches a page when someone shares its URL, to draw a preview card.",
            "gregarious. Only comes when someone mentions you.",
        ),
        C::Monitoring => (
            "An uptime or performance checker.",
            "anxious. Just wants to know you're alive.",
        ),
        C::Scanner => (
            "An internet-wide scanner probing for exposed or vulnerable software.",
            "suspicious. Rattles every door handle.",
        ),
        C::Script => (
            "A generic HTTP library or headless browser. Whoever runs it didn't give it a name.",
            "feral. Raised by a cron job.",
        ),
        C::OtherBot => (
            "Declares itself a bot, but isn't in the keepers' field guide yet.",
            "unclassified. The keepers are still taking notes.",
        ),
        C::Unknown => (
            "Sends a user agent that is neither a real browser nor a declared bot.",
            "evasive. Keeps its name to itself.",
        ),
        C::Internal | C::Browser => ("Not an exhibit.", "domestic."),
    };
    // A self-declared bot's family is its product token, which is what
    // a robots.txt group would name. Families with spaces/parens are
    // display names, not tokens.
    let robots_tokens = match category {
        C::Scanner | C::Script | C::Unknown | C::Monitoring => vec![],
        _ if family
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            vec![family.to_string()]
        }
        _ => vec![],
    };
    SpeciesInfo {
        family: family.to_string(),
        name: family.to_string(),
        slug: slugify(family),
        latin: "Incertae sedis".to_string(),
        operator: "Unknown".to_string(),
        about: about.to_string(),
        temperament: temperament.to_string(),
        robots_tokens,
        docs_url: None,
        category,
        show_ua,
    }
}

/// URL slug for a family: lowercase ASCII alphanumerics, everything
/// else collapsed to single dashes. `"ShapBot (Parallel)"` →
/// `"shapbot-parallel"`, `"(empty)"` → `"empty"`.
pub fn slugify(family: &str) -> String {
    let mut out = String::with_capacity(family.len());
    let mut dash = false;
    for ch in family.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

/// Upper-case Roman numeral (1..=3999; 0 → "N", like the old tables).
pub fn roman(mut n: u32) -> String {
    if n == 0 {
        return "N".to_string();
    }
    const TABLE: &[(u32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut s = String::new();
    for (v, r) in TABLE {
        while n >= *v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// Plate number for a day: days since the zoo opened, plus one.
pub fn plate_number(day: NaiveDate) -> u32 {
    let (y, m, d) = ZOO_OPENED;
    let opened = NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
    ((day - opened).num_days().max(0) + 1) as u32
}

/// Deterministic Bot-of-the-Day pick: FNV-1a of the ISO date over the
/// (sorted) candidate slugs. Same answer on every instance all day.
pub fn pick_of_the_day<'a>(day: NaiveDate, sorted_slugs: &[&'a str]) -> Option<&'a str> {
    if sorted_slugs.is_empty() {
        return None;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in day.to_string().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    Some(sorted_slugs[(h % sorted_slugs.len() as u64) as usize])
}

// ---------------------------------------------------------------------------
// Snapshot (home page + leaderboard)
// ---------------------------------------------------------------------------

/// Per-species aggregate over the windows the zoo shows.
#[derive(Debug, Clone)]
pub struct FamilyStats {
    pub info: SpeciesInfo,
    pub enclosure: Enclosure,
    pub today: i64,
    pub week: i64,
    pub month: i64,
    pub all_time: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// 30-day window.
    pub robots_reads: i64,
    /// 30-day window.
    pub disallowed: i64,
    /// 30-day window.
    pub trapped: i64,
    pub last_trapped: Option<DateTime<Utc>>,
    /// Most frequent UA string over 30 days (hidden when `!info.show_ua`).
    pub sample_ua: Option<String>,
    /// Every distinct UA string in the family (for detail queries).
    pub user_agents: Vec<String>,
}

impl FamilyStats {
    pub fn conservation(&self) -> Conservation {
        Conservation::classify(self.robots_reads, self.disallowed)
    }

    /// Compliance for display: one decimal, but never "100.0%" when the
    /// bot did fetch a disallowed page (1 in 5,000 would round up and
    /// contradict a Feral badge next to it).
    pub fn compliance_label(&self) -> String {
        let pct = self.compliance_pct();
        if self.disallowed > 0 && pct >= 99.95 {
            "99.9%".to_string()
        } else {
            format!("{pct:.1}%")
        }
    }

    /// Share of the 30-day requests that were NOT to disallowed paths.
    pub fn compliance_pct(&self) -> f64 {
        if self.month == 0 {
            return 100.0;
        }
        100.0 * (self.month - self.disallowed) as f64 / self.month as f64
    }
}

#[derive(Debug, Clone, Default)]
pub struct GateCount {
    pub total: i64,
    pub by_status: [i64; 4],
    pub disallowed: i64,
    pub trapped: i64,
    pub new_species: i64,
}

#[derive(Debug, Clone)]
pub struct Arrival {
    pub at: DateTime<Utc>,
    pub slug: String,
    pub name: String,
    pub latin: String,
    pub path: String,
    pub enclosure: Enclosure,
}

#[derive(Debug, Clone)]
pub struct ZooSnapshot {
    pub generated_at: DateTime<Utc>,
    pub today: NaiveDate,
    /// Sorted by `week` desc, then `month` desc, then name.
    pub families: Vec<FamilyStats>,
    pub gate: GateCount,
    /// Bot arrivals per UTC hour, today.
    pub hourly: [i64; 24],
    pub register: Vec<Arrival>,
    pub bot_of_the_day: Option<String>,
}

impl ZooSnapshot {
    pub fn family(&self, slug: &str) -> Option<&FamilyStats> {
        self.families.iter().find(|f| f.info.slug == slug)
    }
}

/// One row of the 30-day per-UA aggregate.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UaRow {
    pub user_agent: String,
    pub month: i64,
    pub week: i64,
    pub today: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub robots_reads: i64,
    pub disallowed: i64,
    pub trapped: i64,
    pub last_trapped: Option<DateTime<Utc>>,
    pub today_disallowed: i64,
    pub today_trapped: i64,
}

/// All-time first-seen + total per UA.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AllTimeRow {
    pub user_agent: String,
    pub first_seen: DateTime<Utc>,
    pub total: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct HourRow {
    user_agent: String,
    hour: i32,
    n: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RecentRow {
    user_agent: String,
    path: String,
    created_at: DateTime<Utc>,
}

fn day_start(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0).expect("midnight").and_utc()
}

async fn load_ua_rows(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<Vec<UaRow>> {
    let today = day_start(now.date_naive());
    sqlx::query_as::<_, UaRow>(
        "SELECT COALESCE(user_agent, '') AS user_agent, \
                COUNT(*) AS month, \
                COUNT(*) FILTER (WHERE created_at >= $2) AS week, \
                COUNT(*) FILTER (WHERE created_at >= $3) AS today, \
                MIN(created_at) AS first_seen, \
                MAX(created_at) AS last_seen, \
                COUNT(*) FILTER (WHERE path = '/robots.txt') AS robots_reads, \
                COUNT(*) FILTER (WHERE path LIKE ANY($4)) AS disallowed, \
                COUNT(*) FILTER (WHERE path LIKE $5) AS trapped, \
                MAX(created_at) FILTER (WHERE path LIKE $5) AS last_trapped, \
                COUNT(*) FILTER (WHERE created_at >= $3 AND path LIKE ANY($4)) AS today_disallowed, \
                COUNT(*) FILTER (WHERE created_at >= $3 AND path LIKE $5) AS today_trapped \
         FROM user_logs \
         WHERE event = 'visit' AND created_at >= $1 \
         GROUP BY 1",
    )
    .bind(now - Duration::days(30))
    .bind(now - Duration::days(7))
    .bind(today)
    .bind(disallowed_like_patterns())
    .bind(format!("{TRAP_PREFIX}%"))
    .fetch_all(pool)
    .await
}

async fn load_all_time(pool: &PgPool) -> sqlx::Result<Vec<AllTimeRow>> {
    sqlx::query_as::<_, AllTimeRow>(
        "SELECT COALESCE(user_agent, '') AS user_agent, \
                MIN(created_at) AS first_seen, COUNT(*) AS total \
         FROM user_logs WHERE event = 'visit' GROUP BY 1",
    )
    .fetch_all(pool)
    .await
}

async fn load_hourly(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<Vec<HourRow>> {
    sqlx::query_as::<_, HourRow>(
        "SELECT COALESCE(user_agent, '') AS user_agent, \
                EXTRACT(HOUR FROM created_at AT TIME ZONE 'UTC')::int AS hour, \
                COUNT(*) AS n \
         FROM user_logs \
         WHERE event = 'visit' AND created_at >= $1 \
         GROUP BY 1, 2",
    )
    .bind(day_start(now.date_naive()))
    .fetch_all(pool)
    .await
}

async fn load_recent(pool: &PgPool) -> sqlx::Result<Vec<RecentRow>> {
    sqlx::query_as::<_, RecentRow>(
        "SELECT COALESCE(user_agent, '') AS user_agent, COALESCE(path, '/') AS path, created_at \
         FROM user_logs WHERE event = 'visit' \
         ORDER BY created_at DESC LIMIT 600",
    )
    .fetch_all(pool)
    .await
}

fn snapshot_cache() -> &'static TtlCache<Option<std::sync::Arc<ZooSnapshot>>> {
    static C: OnceLock<TtlCache<Option<std::sync::Arc<ZooSnapshot>>>> = OnceLock::new();
    C.get_or_init(|| TtlCache::new(SNAPSHOT_TTL))
}

fn all_time_cache() -> &'static TtlCache<Option<std::sync::Arc<Vec<AllTimeRow>>>> {
    static C: OnceLock<TtlCache<Option<std::sync::Arc<Vec<AllTimeRow>>>>> = OnceLock::new();
    C.get_or_init(|| TtlCache::new(ALL_TIME_TTL))
}

/// The cached zoo snapshot. `None` when the DB is unavailable (the page
/// then renders an "enclosures closed" notice instead of a 500).
pub async fn snapshot(pool: &PgPool) -> Option<std::sync::Arc<ZooSnapshot>> {
    snapshot_cache()
        .get_or_fill("zoo", || async {
            match build_snapshot(pool, Utc::now()).await {
                Ok(s) => Some(std::sync::Arc::new(s)),
                Err(e) => {
                    tracing::warn!(error = %e, "zoo.snapshot.failed");
                    None
                }
            }
        })
        .await
}

async fn build_snapshot(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<ZooSnapshot> {
    let all_time = match all_time_cache().get("all") {
        Some(Some(rows)) => rows,
        _ => {
            let rows = std::sync::Arc::new(load_all_time(pool).await?);
            all_time_cache().put("all", Some(rows.clone()));
            rows
        }
    };
    let (ua_rows, hour_rows, recent) = tokio::try_join!(
        load_ua_rows(pool, now),
        load_hourly(pool, now),
        load_recent(pool)
    )?;
    Ok(assemble(now, &ua_rows, &all_time, &hour_rows, &recent))
}

/// Pure assembly of a snapshot from the raw per-UA rows (unit-tested).
fn assemble(
    now: DateTime<Utc>,
    ua_rows: &[UaRow],
    all_time: &[AllTimeRow],
    hour_rows: &[HourRow],
    recent: &[RecentRow],
) -> ZooSnapshot {
    let today = now.date_naive();
    let today_start = day_start(today);

    // Memoise classification per distinct UA (the same UA appears in
    // several row sets).
    let mut class_memo: HashMap<String, Option<(String, UaCategory)>> = HashMap::new();
    let mut classify = |ua: &str| -> Option<(String, UaCategory)> {
        class_memo
            .entry(ua.to_string())
            .or_insert_with(|| {
                let c = classify_ua(ua);
                enclosure_for(c.category).map(|_| (c.family, c.category))
            })
            .clone()
    };

    let all_time_by_ua: HashMap<&str, &AllTimeRow> = all_time
        .iter()
        .map(|r| (r.user_agent.as_str(), r))
        .collect();

    struct Acc {
        stats: FamilyStats,
        ua_counts: Vec<(i64, String)>,
        today_disallowed: i64,
        today_trapped: i64,
    }
    let mut fams: BTreeMap<String, Acc> = BTreeMap::new();
    for r in ua_rows {
        let Some((family, category)) = classify(&r.user_agent) else {
            continue;
        };
        let at = all_time_by_ua.get(r.user_agent.as_str());
        let first_seen = at
            .map(|a| a.first_seen.min(r.first_seen))
            .unwrap_or(r.first_seen);
        let all = at.map(|a| a.total).unwrap_or(0).max(r.month);
        let acc = fams.entry(family.clone()).or_insert_with(|| Acc {
            stats: FamilyStats {
                info: species_info(&family, category),
                enclosure: enclosure_for(category).expect("exhibit"),
                today: 0,
                week: 0,
                month: 0,
                all_time: 0,
                first_seen,
                last_seen: r.last_seen,
                robots_reads: 0,
                disallowed: 0,
                trapped: 0,
                last_trapped: None,
                sample_ua: None,
                user_agents: Vec::new(),
            },
            ua_counts: Vec::new(),
            today_disallowed: 0,
            today_trapped: 0,
        });
        let s = &mut acc.stats;
        s.today += r.today;
        s.week += r.week;
        s.month += r.month;
        s.all_time += all;
        s.first_seen = s.first_seen.min(first_seen);
        s.last_seen = s.last_seen.max(r.last_seen);
        s.robots_reads += r.robots_reads;
        s.disallowed += r.disallowed;
        s.trapped += r.trapped;
        s.last_trapped = s.last_trapped.max(r.last_trapped);
        s.user_agents.push(r.user_agent.clone());
        acc.ua_counts.push((r.month, r.user_agent.clone()));
        acc.today_disallowed += r.today_disallowed;
        acc.today_trapped += r.today_trapped;
    }

    let mut gate = GateCount::default();
    let mut families = Vec::with_capacity(fams.len());
    for (_, mut acc) in fams {
        acc.ua_counts
            .sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        if acc.stats.info.show_ua {
            acc.stats.sample_ua = acc.ua_counts.first().map(|(_, ua)| ua.clone());
        }
        let s = acc.stats;
        if s.today > 0 {
            gate.total += s.today;
            let idx = Conservation::ALL
                .iter()
                .position(|c| *c == s.conservation())
                .unwrap_or(3);
            gate.by_status[idx] += s.today;
            gate.disallowed += acc.today_disallowed;
            gate.trapped += acc.today_trapped;
            if s.first_seen >= today_start {
                gate.new_species += 1;
            }
        }
        families.push(s);
    }
    families.sort_by(|a, b| {
        b.week
            .cmp(&a.week)
            .then(b.month.cmp(&a.month))
            .then(a.info.name.cmp(&b.info.name))
    });

    let mut hourly = [0i64; 24];
    for h in hour_rows {
        if classify(&h.user_agent).is_some() {
            if let Some(slot) = hourly.get_mut(h.hour.clamp(0, 23) as usize) {
                *slot += h.n;
            }
        }
    }

    let mut register = Vec::with_capacity(REGISTER_LEN);
    for r in recent {
        if register.len() >= REGISTER_LEN {
            break;
        }
        let Some((family, category)) = classify(&r.user_agent) else {
            continue;
        };
        let info = species_info(&family, category);
        register.push(Arrival {
            at: r.created_at,
            slug: info.slug,
            name: info.name,
            latin: info.latin,
            path: r.path.clone(),
            enclosure: enclosure_for(category).expect("exhibit"),
        });
    }

    // Bot of the Day: any named species seen this week (the nameless
    // and masked don't get a plate).
    let mut candidates: Vec<&str> = families
        .iter()
        .filter(|f| f.week > 0 && f.info.category != UaCategory::Unknown)
        .map(|f| f.info.slug.as_str())
        .collect();
    candidates.sort_unstable();
    let bot_of_the_day = pick_of_the_day(today, &candidates).map(str::to_string);

    ZooSnapshot {
        generated_at: now,
        today,
        families,
        gate,
        hourly,
        register,
        bot_of_the_day,
    }
}

// ---------------------------------------------------------------------------
// Per-species detail
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct SpeciesDetail {
    /// `(day, visits)` for the last 30 days, oldest first, zero-filled.
    pub daily: Vec<(NaiveDate, i64)>,
    /// Top 5 paths over 30 days.
    pub diet: Vec<(String, i64)>,
    /// Visits per UTC hour over 30 days.
    pub hours: [i64; 24],
    /// Last few paths of the most recent visit day (newest first).
    pub latest_paths: Vec<(DateTime<Utc>, String)>,
}

impl SpeciesDetail {
    pub fn busiest_day(&self) -> Option<(NaiveDate, i64)> {
        self.daily
            .iter()
            .copied()
            .filter(|(_, n)| *n > 0)
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
    }

    pub fn peak_hour(&self) -> Option<u32> {
        let max = *self.hours.iter().max()?;
        if max == 0 {
            return None;
        }
        self.hours.iter().position(|n| *n == max).map(|h| h as u32)
    }
}

fn detail_cache() -> &'static TtlCache<Option<std::sync::Arc<SpeciesDetail>>> {
    static C: OnceLock<TtlCache<Option<std::sync::Arc<SpeciesDetail>>>> = OnceLock::new();
    C.get_or_init(|| TtlCache::new(DETAIL_TTL))
}

pub async fn species_detail(
    pool: &PgPool,
    family: &FamilyStats,
) -> Option<std::sync::Arc<SpeciesDetail>> {
    let key = family.info.slug.clone();
    let uas = family.user_agents.clone();
    detail_cache()
        .get_or_fill(&key, || async move {
            match build_detail(pool, &uas, Utc::now()).await {
                Ok(d) => Some(std::sync::Arc::new(d)),
                Err(e) => {
                    tracing::warn!(error = %e, "zoo.detail.failed");
                    None
                }
            }
        })
        .await
}

async fn build_detail(
    pool: &PgPool,
    uas: &[String],
    now: DateTime<Utc>,
) -> sqlx::Result<SpeciesDetail> {
    let since = now - Duration::days(30);
    let daily_q = sqlx::query_as::<_, (NaiveDate, i64)>(
        "SELECT (created_at AT TIME ZONE 'UTC')::date AS d, COUNT(*) \
         FROM user_logs \
         WHERE event = 'visit' AND created_at >= $1 AND COALESCE(user_agent, '') = ANY($2) \
         GROUP BY 1 ORDER BY 1",
    )
    .bind(since)
    .bind(uas)
    .fetch_all(pool);
    let diet_q = sqlx::query_as::<_, (String, i64)>(
        "SELECT COALESCE(path, '/'), COUNT(*) AS n \
         FROM user_logs \
         WHERE event = 'visit' AND created_at >= $1 AND COALESCE(user_agent, '') = ANY($2) \
         GROUP BY 1 ORDER BY n DESC, 1 LIMIT 5",
    )
    .bind(since)
    .bind(uas)
    .fetch_all(pool);
    let hours_q = sqlx::query_as::<_, (i32, i64)>(
        "SELECT EXTRACT(HOUR FROM created_at AT TIME ZONE 'UTC')::int, COUNT(*) \
         FROM user_logs \
         WHERE event = 'visit' AND created_at >= $1 AND COALESCE(user_agent, '') = ANY($2) \
         GROUP BY 1",
    )
    .bind(since)
    .bind(uas)
    .fetch_all(pool);
    let latest_q = sqlx::query_as::<_, (DateTime<Utc>, String)>(
        "SELECT created_at, COALESCE(path, '/') \
         FROM user_logs \
         WHERE event = 'visit' AND created_at >= $1 AND COALESCE(user_agent, '') = ANY($2) \
         ORDER BY created_at DESC LIMIT 3",
    )
    .bind(since)
    .bind(uas)
    .fetch_all(pool);
    let (daily_rows, diet, hour_rows, latest_paths) =
        tokio::try_join!(daily_q, diet_q, hours_q, latest_q)?;
    Ok(SpeciesDetail {
        daily: zero_fill_days(&daily_rows, now.date_naive(), 30),
        diet,
        hours: fold_hours(&hour_rows),
        latest_paths,
    })
}

fn zero_fill_days(rows: &[(NaiveDate, i64)], last: NaiveDate, days: i64) -> Vec<(NaiveDate, i64)> {
    let by_day: HashMap<NaiveDate, i64> = rows.iter().copied().collect();
    (0..days)
        .rev()
        .map(|i| {
            let d = last - Duration::days(i);
            (d, by_day.get(&d).copied().unwrap_or(0))
        })
        .collect()
}

fn fold_hours(rows: &[(i32, i64)]) -> [i64; 24] {
    let mut out = [0i64; 24];
    for (h, n) in rows {
        if (0..24).contains(h) {
            out[*h as usize] += n;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Copy helpers (keeper's voice)
// ---------------------------------------------------------------------------

/// A one-liner about when a species is most active.
pub fn visiting_hours_quip(hour: u32) -> &'static str {
    match hour {
        0..=4 => "The keepers are asleep. It knows.",
        5..=8 => "Before the coffee is made.",
        9..=11 => "Arrives with the morning crowd.",
        12..=13 => "Lunchtime regular.",
        14..=17 => "Afternoon visitor. Keeps office hours.",
        18..=20 => "Comes by after work.",
        _ => "A night owl.",
    }
}

/// Is `day` the same calendar day as `at`?
pub fn same_day(at: DateTime<Utc>, day: NaiveDate) -> bool {
    at.date_naive() == day
}

/// "16 Nov 2026".
pub fn fmt_day(d: NaiveDate) -> String {
    format!(
        "{} {} {}",
        d.day(),
        [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
        ][d.month0() as usize],
        d.year()
    )
}

/// "16:25".
pub fn fmt_hm(at: DateTime<Utc>) -> String {
    format!("{:02}:{:02}", at.hour(), at.minute())
}

/// "3 s ago" / "18 min ago" / "5 h ago" / "2 days ago".
pub fn ago(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - at).num_seconds().max(0);
    if secs < 60 {
        format!("{secs} s ago")
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else if secs < 86_400 {
        format!("{} h ago", secs / 3600)
    } else {
        let d = secs / 86_400;
        if d == 1 {
            "1 day ago".to_string()
        } else {
            format!("{d} days ago")
        }
    }
}

/// `1234567` → `"1,234,567"`.
pub fn fmt_int(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3 + 1);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, h, m, 0).unwrap()
    }

    const GPT: &str = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.4; +https://openai.com/gptbot)";
    const BYTE: &str = "Mozilla/5.0 (Linux; Android 5.0) AppleWebKit/537.36 (KHTML, like Gecko) Mobile Safari/537.36 (compatible; Bytespider; spider-feedback@bytedance.com)";
    const CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

    fn row(ua: &str, today: i64, robots: i64, disallowed: i64, trapped: i64) -> UaRow {
        UaRow {
            user_agent: ua.to_string(),
            month: today + 10,
            week: today + 5,
            today,
            first_seen: ts(1, 0) - Duration::days(20),
            last_seen: ts(10, 0),
            robots_reads: robots,
            disallowed,
            trapped,
            last_trapped: (trapped > 0).then(|| ts(9, 0)),
            today_disallowed: disallowed.min(today),
            today_trapped: trapped.min(today),
        }
    }

    #[test]
    fn disallowed_matches_robots_prefixes() {
        assert!(is_disallowed("/zoo/trap/"));
        assert!(is_disallowed("/zoo/trap/deeper"));
        assert!(is_disallowed("/profile/transactions"));
        assert!(is_disallowed("/notifications"));
        assert!(!is_disallowed("/zoo"));
        assert!(!is_disallowed("/zoo/bots/gptbot"));
        assert!(!is_disallowed("/profile"));
        assert!(!is_disallowed("/battles/x"));
        // LIKE patterns must not contain wildcards besides the trailing %.
        for p in disallowed_like_patterns() {
            assert!(!p[..p.len() - 1].contains(['%', '_']), "{p}");
        }
    }

    #[test]
    fn conservation_matrix() {
        assert_eq!(Conservation::classify(3, 0), Conservation::Respects);
        assert_eq!(Conservation::classify(3, 1), Conservation::Selective);
        assert_eq!(Conservation::classify(0, 1), Conservation::Feral);
        assert_eq!(Conservation::classify(0, 0), Conservation::Unassessed);
    }

    #[test]
    fn enclosures_cover_every_bot_category_and_exclude_humans_and_keepers() {
        for c in UaCategory::ALL {
            let e = enclosure_for(c);
            match c {
                UaCategory::Browser | UaCategory::Internal | UaCategory::Monitoring => {
                    assert!(e.is_none(), "{c:?}")
                }
                _ => assert!(e.is_some(), "{c:?}"),
            }
        }
        for e in Enclosure::ALL {
            assert_eq!(Enclosure::from_key(e.key()), Some(e));
        }
    }

    #[test]
    fn slugs_are_url_safe_and_stable() {
        assert_eq!(slugify("GPTBot"), "gptbot");
        assert_eq!(slugify("ShapBot (Parallel)"), "shapbot-parallel");
        assert_eq!(slugify("Googlebot Desktop"), "googlebot-desktop");
        assert_eq!(slugify("(empty)"), "empty");
        assert_eq!(slugify("(unrecognised)"), "unrecognised");
        assert_eq!(slugify("Keenable-User"), "keenable-user");
        assert_eq!(slugify("()"), "unnamed");
    }

    #[test]
    fn catalogue_families_match_the_classifier() {
        // Every curated entry must be a family the classifier actually
        // emits, or the entry is dead text. Check by slug uniqueness and
        // that a curated lookup round-trips.
        let mut slugs: Vec<String> = CATALOGUE.iter().map(|c| slugify(c.family)).collect();
        let n = slugs.len();
        slugs.sort();
        slugs.dedup();
        assert_eq!(slugs.len(), n, "duplicate curated slugs");
        let gpt = species_info("GPTBot", UaCategory::AiTraining);
        assert_eq!(gpt.latin, "Gptus vorax");
        assert_eq!(gpt.robots_tokens, vec!["GPTBot".to_string()]);
        assert_eq!(classify_ua(GPT).family, "GPTBot");
        assert_eq!(classify_ua(BYTE).family, "Bytespider");
    }

    #[test]
    fn generic_species_get_tokens_only_when_they_look_like_tokens() {
        let s = species_info("AionBot", UaCategory::OtherBot);
        assert_eq!(s.latin, "Incertae sedis");
        assert_eq!(s.robots_tokens, vec!["AionBot".to_string()]);
        let s = species_info("Scanner (other)", UaCategory::Scanner);
        assert!(s.robots_tokens.is_empty());
        let s = species_info("Weird Thing (x)", UaCategory::OtherBot);
        assert!(s.robots_tokens.is_empty());
        // The masked visitor never shows its UA.
        assert!(!species_info("(unrecognised)", UaCategory::Unknown).show_ua);
        assert_eq!(
            species_info("(unrecognised)", UaCategory::Unknown).name,
            "Masked visitor"
        );
    }

    #[test]
    fn roman_numerals() {
        assert_eq!(roman(1), "I");
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(127), "CXXVII");
        assert_eq!(roman(320), "CCCXX");
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(roman(0), "N");
    }

    #[test]
    fn plate_counts_from_opening_day() {
        assert_eq!(
            plate_number(NaiveDate::from_ymd_opt(2026, 7, 13).unwrap()),
            1
        );
        assert_eq!(
            plate_number(NaiveDate::from_ymd_opt(2026, 7, 14).unwrap()),
            2
        );
        assert_eq!(
            plate_number(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            1
        );
    }

    #[test]
    fn bot_of_the_day_is_deterministic_and_rotates() {
        let slugs = ["a", "b", "c", "d", "e", "f", "g"];
        let d1 = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        assert_eq!(pick_of_the_day(d1, &slugs), pick_of_the_day(d1, &slugs));
        let picks: std::collections::HashSet<_> = (0..14)
            .map(|i| pick_of_the_day(d1 + Duration::days(i), &slugs).unwrap())
            .collect();
        assert!(picks.len() > 1, "the plate never changes");
        assert_eq!(pick_of_the_day(d1, &[]), None);
    }

    #[test]
    fn assemble_skips_humans_and_rolls_up_the_gate() {
        let now = ts(12, 0);
        let ua_rows = vec![
            row(GPT, 5, 2, 0, 0),     // respects
            row(BYTE, 3, 0, 1, 1),    // feral, trapped
            row(CHROME, 50, 0, 0, 0), // human: not an exhibit
            row("", 2, 0, 0, 0),      // nameless: unassessed
        ];
        let all_time = vec![AllTimeRow {
            user_agent: GPT.to_string(),
            first_seen: ts(0, 0) - Duration::days(90),
            total: 1000,
        }];
        let hours = vec![
            HourRow {
                user_agent: GPT.into(),
                hour: 3,
                n: 5,
            },
            HourRow {
                user_agent: CHROME.into(),
                hour: 3,
                n: 50,
            },
            HourRow {
                user_agent: BYTE.into(),
                hour: 11,
                n: 3,
            },
        ];
        let recent = vec![
            RecentRow {
                user_agent: CHROME.into(),
                path: "/".into(),
                created_at: ts(11, 59),
            },
            RecentRow {
                user_agent: BYTE.into(),
                path: "/zoo/trap/".into(),
                created_at: ts(11, 58),
            },
        ];
        let s = assemble(now, &ua_rows, &all_time, &hours, &recent);

        assert_eq!(s.families.len(), 3, "Chrome must not be an exhibit");
        assert_eq!(s.gate.total, 5 + 3 + 2);
        assert_eq!(s.gate.by_status, [5, 0, 3, 2]);
        assert_eq!(s.gate.disallowed, 1);
        assert_eq!(s.gate.trapped, 1);
        assert_eq!(s.hourly[3], 5);
        assert_eq!(s.hourly[11], 3);
        assert_eq!(s.register.len(), 1);
        assert_eq!(s.register[0].name, "Bytespider");
        assert_eq!(s.register[0].enclosure, Enclosure::Aviary);

        let byte = s.family("bytespider").unwrap();
        // 1 disallowed hit out of 13 — and never rounds up to 100%.
        assert_eq!(byte.compliance_label(), "92.3%");
        let mut many = byte.clone();
        many.month = 5000;
        assert_eq!(many.compliance_label(), "99.9%");
        let gpt = s.family("gptbot").unwrap();
        assert_eq!(gpt.all_time, 1000);
        assert_eq!(gpt.conservation(), Conservation::Respects);
        assert_eq!(gpt.sample_ua.as_deref(), Some(GPT));
        let nameless = s.family("empty").unwrap();
        assert_eq!(nameless.info.name, "Nameless visitor");
        assert!(nameless.sample_ua.is_none(), "unknown UAs are withheld");
        // Sorted busiest-this-week first.
        assert_eq!(s.families[0].info.slug, "gptbot");
        // Nameless visitors never win Bot of the Day.
        assert_ne!(s.bot_of_the_day.as_deref(), Some("empty"));
    }

    #[test]
    fn new_species_counts_families_first_seen_today() {
        let now = ts(12, 0);
        let mut r = row(GPT, 1, 0, 0, 0);
        r.first_seen = ts(8, 0);
        let s = assemble(now, &[r], &[], &[], &[]);
        assert_eq!(s.gate.new_species, 1);
    }

    #[test]
    fn detail_helpers() {
        let last = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let rows = vec![(last - Duration::days(2), 7), (last, 3)];
        let filled = zero_fill_days(&rows, last, 5);
        assert_eq!(filled.len(), 5);
        assert_eq!(filled[4], (last, 3));
        assert_eq!(filled[2], (last - Duration::days(2), 7));
        assert_eq!(filled[0].1, 0);
        let d = SpeciesDetail {
            daily: filled,
            hours: fold_hours(&[(3, 9), (14, 2), (99, 5)]),
            ..Default::default()
        };
        assert_eq!(d.busiest_day(), Some((last - Duration::days(2), 7)));
        assert_eq!(d.peak_hour(), Some(3));
        assert_eq!(SpeciesDetail::default().peak_hour(), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(5205), "5,205");
        assert_eq!(fmt_int(1234567), "1,234,567");
        assert_eq!(
            fmt_day(NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()),
            "6 Nov 2026"
        );
        assert_eq!(ago(ts(12, 0) - Duration::seconds(5), ts(12, 0)), "5 s ago");
        assert_eq!(ago(ts(11, 42), ts(12, 0)), "18 min ago");
        assert_eq!(ago(ts(7, 0), ts(12, 0)), "5 h ago");
        assert_eq!(ago(ts(12, 0) - Duration::days(1), ts(12, 0)), "1 day ago");
        assert_eq!(visiting_hours_quip(3), "The keepers are asleep. It knows.");
    }
}
