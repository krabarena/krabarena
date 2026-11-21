use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, html};
use std::collections::HashMap;
use uuid::Uuid;

/// Resolve `claim:<id>` shorthand to a rich markdown link with the
/// claim's title. The KrabReviewer summarizer prompt instructs Gemini
/// to cite claims using this shorthand (with or without surrounding
/// backticks). Until 0.8.21 the references rendered verbatim — a
/// `claim:76cf4b68-…` text blob in the editorial body. Now the page
/// handler builds a `(uuid → (title, slug-or-uuid))` map for every
/// claim in the battle and runs this pass on the executive summary
/// before the markdown filter, turning each citation into
/// `[<title>](/claims/<slug>)` so readers can jump to the source.
///
/// Accepts both forms:
///   - **Full UUID** (`claim:d981f87a-509d-474a-9d29-e0cdaf585f4d`) —
///     the form the prompt asks for.
///   - **Truncated hex prefix** (`claim:d981f87a`) — what Gemini
///     actually emits a lot of the time, e.g. on
///     `python-vs-typescript-vs-rust-vs-go` where every cited claim
///     came through as an 8-char prefix that fell through to the
///     final render as a literal `claim:d981f87a` blob. Resolved by
///     prefix-match against the battle's claim UUIDs (no dashes,
///     case-insensitive). 4 hex chars is the minimum — shorter is
///     ambiguous and stays verbatim. If the prefix matches multiple
///     claims (collision), we also leave it verbatim rather than
///     guess.
///
/// Unknown UUIDs (refs to deleted claims, copy-paste from another
/// battle) and ambiguous prefixes pass through verbatim — better to
/// surface a stale id than silently swallow it.
///
/// ## Bare-hex citation form (0.21.0 corruption fix)
///
/// In **incremental** summary mode the model frequently drops the
/// `claim:` prefix entirely and cites the new claim as a bare,
/// backtick-wrapped hex prefix — e.g. `` `5118e4da` `` instead of
/// `claim:5118e4da`. This is reinforced by the prompt's own examples
/// ("latest claim `5118e4da` shows…", "latest claim 76cf4b68 holds…")
/// which the model mirrors. The pre-0.21 regex required a literal
/// `claim:` prefix, so every bare-hex citation survived verbatim into
/// the rendered HTML — that's the "still corrupted" incremental
/// summary reported on the python-vs-* and dataframe battles
/// (`` `5118e4da` ``, `` `c5250329` `` rendered as literal code spans).
///
/// Fix: a SECOND, deliberately strict pass resolves a
/// **backtick-wrapped** bare hex token (`` `<6-32 hex>` ``) when — and
/// only when — it UNIQUELY prefix-matches a claim UUID in this
/// battle's map. Constraints that keep false-positives out:
///   - backticks are MANDATORY (a bare `5118e4da` in prose stays put);
///   - ≥ 6 hex chars (so `` `100` `` / `` `0.78` `` style code spans
///     and short numeric literals never match);
///   - the token must be pure hex (no dots, no letters g-z);
///   - an ambiguous prefix (matches >1 claim) is left verbatim.
///
/// A backtick token that resolves becomes a normal `[title](/claims/…)`
/// link, dropping the backticks.
pub fn expand_claim_refs(input: &str, claims: &HashMap<Uuid, (String, String)>) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    // Pass 1 — explicit `claim:<id>` form, optionally wrapped in
    // backticks, where `<id>` is 4-36 hex chars with optional dashes.
    // Loose enough to catch the 8-char prefix form Gemini emits; tight
    // enough that random `claim:foo` mentions in prose don't false-match.
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"`?claim:([0-9a-fA-F][0-9a-fA-F\-]{3,35})`?").expect("claim-ref regex compiles")
    });
    let pass1 = re.replace_all(input, |caps: &regex::Captures| {
        let raw = &caps[0];
        let ref_str = &caps[1];
        let resolved = resolve_claim_ref(ref_str, claims);
        match resolved {
            Some((title, path)) => {
                // Strip `[` `]` from the link text so they don't break
                // markdown link parsing. Other special chars (parens,
                // asterisks) are fine inside `[…]`.
                let safe_title = title.replace(['[', ']'], "");
                format!("[{}](/claims/{})", safe_title.trim(), path)
            }
            None => raw.to_string(),
        }
    });

    // Pass 2 — bare backtick-wrapped hex prefix (the prefix-less form
    // the incremental summarizer emits). Strict: backticks required,
    // ≥6 hex chars, pure hex (no dashes — Gemini emits the short
    // un-dashed prefix), unique prefix match only.
    static RE_BARE: OnceLock<Regex> = OnceLock::new();
    let re_bare = RE_BARE
        .get_or_init(|| Regex::new(r"`([0-9a-fA-F]{6,32})`").expect("bare-hex regex compiles"));
    re_bare
        .replace_all(&pass1, |caps: &regex::Captures| {
            let raw = &caps[0];
            let hex = &caps[1];
            match resolve_claim_ref(hex, claims) {
                Some((title, path)) => {
                    let safe_title = title.replace(['[', ']'], "");
                    format!("[{}](/claims/{})", safe_title.trim(), path)
                }
                // Not a claim prefix (a real code span like `abcdef` or
                // a hex color) — leave the backtick span untouched.
                None => raw.to_string(),
            }
        })
        .into_owned()
}

/// Look up a claim ref against the (uuid → (title, path)) map.
/// Tries the full-UUID parse first, then falls back to prefix match
/// on the dash-stripped hex. Returns `None` for unresolved (unknown
/// uuid, prefix too short, or ambiguous prefix that matches >1
/// claim).
fn resolve_claim_ref<'a>(
    ref_str: &str,
    claims: &'a HashMap<Uuid, (String, String)>,
) -> Option<&'a (String, String)> {
    // Full-UUID fast path.
    if let Ok(uuid) = Uuid::parse_str(ref_str)
        && let Some(v) = claims.get(&uuid)
    {
        return Some(v);
    }
    // Prefix-match path. Strip dashes + lowercase so `9D54F89E` and
    // `9d54f89e` both match `9d54f89e7ea8…` on the map's simple
    // (un-dashed) form.
    let needle = ref_str.replace('-', "").to_lowercase();
    if needle.len() < 4 {
        return None;
    }
    let mut hit: Option<&'a (String, String)> = None;
    for (uuid, val) in claims {
        // `Uuid::simple` is the 32-char un-dashed lowercase form, so
        // `starts_with(&needle)` is a clean prefix test.
        if uuid.simple().to_string().starts_with(&needle) {
            if hit.is_some() {
                // Two claims share this prefix — ambiguous, bail.
                return None;
            }
            hit = Some(val);
        }
    }
    hit
}

/// Render Markdown to sanitized HTML.
///
/// Uses pulldown-cmark for parsing and ammonia for sanitization
/// to prevent XSS from user-supplied Markdown content.
/// Auto-links bare URLs before parsing.
///
/// For claim bodies whose images live inside a published artifact
/// bundle, use [`render_with_bundle`] instead so relative `![]()`
/// references resolve to the bundle's public CDN URL.
pub fn render(input: &str) -> String {
    render_with_bundle(input, None)
}

/// Render Markdown to sanitized HTML, resolving relative image
/// references against the claim's artifact bundle on GCS when
/// `bundle_base` is set.
///
/// The pipeline: every `![alt](path)` event from pulldown-cmark is
/// inspected before HTML emission. When `path` is a relative URL with
/// a whitelisted image extension (`.png`, `.jpg`/`.jpeg`), it gets
/// rewritten to `{bundle_base}/{path}`. Absolute http(s) URLs pass
/// through unchanged; SVG (XSS-prone — can carry `<script>`) and
/// other types pass through unchanged so the markdown layer never
/// silently promotes them to GCS hits — the upstream
/// `services::claim_media` pipeline already rejects them on upload.
///
/// `bundle_base` should be the full bucket-public prefix where the
/// bundle was uploaded — e.g.
/// `https://storage.googleapis.com/krabarena-artifacts/bundles/sha256:abcd…`.
/// The current submit path (`services::claim_media`) also rewrites
/// bodies on upload, so this is mostly **defense in depth**: legacy
/// claims with un-rewritten relative paths and claims where the
/// upload-time rewrite missed something still render correctly.
pub fn render_with_bundle(input: &str, bundle_base: Option<&str>) -> String {
    let input = autolink_urls(input);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(&input, options);
    let mut html_output = String::new();
    if let Some(base) = bundle_base {
        let normalized_base = base.trim_end_matches('/').to_string();
        let rewritten = parser.map(|ev| rewrite_image_event(ev, &normalized_base));
        html::push_html(&mut html_output, rewritten);
    } else {
        html::push_html(&mut html_output, parser);
    }

    let mut builder = ammonia::Builder::default();
    builder.link_rel(Some("noopener noreferrer"));
    let sanitized = builder.clean(&html_output).to_string();

    // Add target="_blank" to all links so they open in new tab
    let with_links = sanitized.replace("<a ", "<a target=\"_blank\" ");

    // Inject GitHub-style heading anchors so TOC / cross-doc fragment
    // links (#1-vocabulary, #3-trust-model, …) actually scroll. Slugs
    // are derived from the visible heading text and contain only
    // [a-z0-9_-], so doing this after sanitization is safe.
    inject_heading_ids(&with_links)
}

/// Intercept `Tag::Image` events from pulldown-cmark and resolve any
/// relative image URL against the claim's GCS bundle prefix. Returns
/// the event unchanged when `dest_url` is absolute, has a non-image
/// extension, or has an extension that isn't on the whitelist (SVG
/// and friends pass through verbatim — the upload pipeline already
/// rejects them).
fn rewrite_image_event<'a>(event: Event<'a>, bundle_base: &str) -> Event<'a> {
    match event {
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let resolved = resolve_bundle_image_url(&dest_url, bundle_base)
                .map(CowStr::from)
                .unwrap_or(dest_url);
            Event::Start(Tag::Image {
                link_type,
                dest_url: resolved,
                title,
                id,
            })
        }
        other => other,
    }
}

/// Decide whether `src` should be rewritten to point inside
/// `bundle_base`, and return the rewritten URL when so.
///
/// Rules:
///   - Absolute URLs (`http://`, `https://`, `data:`, `gs://`, `//…`,
///     `mailto:`, etc.) pass through unchanged.
///   - Fragment-only refs (`#foo`) and root-relative refs (`/foo`)
///     pass through — they aren't bundle-local.
///   - Paths containing `..` segments pass through — defense against
///     attempts to escape the bundle root.
///   - The extension MUST be on the image whitelist (`png`, `jpg`,
///     `jpeg`); otherwise return `None`. SVG is **intentionally
///     omitted** for XSS reasons — SVG can embed `<script>` and
///     `<foreignObject>`.
fn resolve_bundle_image_url(src: &str, bundle_base: &str) -> Option<String> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Absolute scheme or protocol-relative — leave alone.
    if is_absolute_url(trimmed) {
        return None;
    }
    // Fragment / root-relative — not bundle-local.
    if trimmed.starts_with('#') || trimmed.starts_with('/') {
        return None;
    }
    // Path-escape attempts.
    if trimmed.contains("..") {
        return None;
    }
    // Strip a single leading `./`.
    let path = trimmed.strip_prefix("./").unwrap_or(trimmed);
    if !has_allowed_image_extension(path) {
        return None;
    }
    Some(format!("{bundle_base}/{path}"))
}

/// Whitelist test for the rendered image extension. Lower-cased,
/// query/fragment stripped. The whitelist is deliberately narrow:
/// PNG + JPEG. SVG is rejected (XSS risk — can carry `<script>` /
/// event handlers / `<foreignObject>`); GIF / WebP fall through too
/// since they aren't part of the supported set yet. See
/// `docs/claim-structure.md` → "Images".
fn has_allowed_image_extension(path: &str) -> bool {
    // Strip ?query and #fragment if present.
    let mut end = path.len();
    for (i, c) in path.char_indices() {
        if c == '?' || c == '#' {
            end = i;
            break;
        }
    }
    let clean = &path[..end];
    let lower = clean.to_ascii_lowercase();
    lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

/// `true` when `src` carries an explicit scheme or is protocol-
/// relative — anything we wouldn't try to resolve against the bundle.
fn is_absolute_url(src: &str) -> bool {
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("//") {
        return true;
    }
    // Common schemes we'd see in a claim body.
    for scheme in [
        "http://",
        "https://",
        "data:",
        "gs://",
        "mailto:",
        "ftp://",
        "javascript:",
        "blob:",
        "file:",
    ] {
        if lower.starts_with(scheme) {
            return true;
        }
    }
    false
}

/// Compute the public GCS URL prefix for a claim's bundle, derived
/// from the configured `blob_store_url` (which is a `gs://<bucket>`
/// in prod and an HTTP URL in dev).
///
/// Mirrors `services::claim_media`'s upload path:
/// `bundles/<artifact_hash>/<relative_path>` — so relative refs in
/// the claim body resolve to the same objects the upload pipeline
/// wrote there.
pub fn bundle_image_base(blob_store_url: &str, artifact_hash: &str) -> String {
    if let Some(bucket) = blob_store_url.strip_prefix("gs://") {
        let bucket = bucket.trim_end_matches('/');
        format!("https://storage.googleapis.com/{bucket}/bundles/{artifact_hash}")
    } else {
        let base = blob_store_url.trim_end_matches('/');
        format!("{base}/bundles/{artifact_hash}")
    }
}

fn inject_heading_ids(html: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    // pulldown_cmark emits well-formed, non-nested headings, so a
    // single regex matching `<h([1-6])>…</h([1-6])>` reliably pairs
    // open with close even though Rust's regex crate has no
    // backreferences to assert level equality.
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"<h([1-6])>(.+?)</h[1-6]>").expect("heading regex compiles"));
    re.replace_all(html, |caps: &regex::Captures| {
        let level = &caps[1];
        let inner = &caps[2];
        let slug = slugify_gfm(&strip_tags(inner));
        if slug.is_empty() {
            format!("<h{level}>{inner}</h{level}>")
        } else {
            format!("<h{level} id=\"{slug}\">{inner}</h{level}>")
        }
    })
    .into_owned()
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut depth = 0u32;
    for c in html.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn slugify_gfm(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    let mut last_was_hyphen = false;
    for c in text.chars() {
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == '_' {
            s.push(lower);
            last_was_hyphen = false;
        } else if (lower == ' ' || lower == '-') && !s.is_empty() && !last_was_hyphen {
            s.push('-');
            last_was_hyphen = true;
        }
    }
    while s.ends_with('-') {
        s.pop();
    }
    s
}

/// Convert bare URLs to markdown links.
///
/// Matches `https://...` and `http://...` that aren't already inside
/// markdown link syntax `[...](url)` or `<url>`.
fn autolink_urls(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.char_indices().peekable();

    while let Some(&(i, c)) = chars.peek() {
        // Skip URLs already in markdown link syntax: [text](url) or <url>
        if c == '(' || c == '<' || c == '[' {
            // Check if this is a markdown link — just pass through
            result.push(c);
            chars.next();
            continue;
        }

        // Check for bare URL
        let rest = &input[i..];
        if (rest.starts_with("https://") || rest.starts_with("http://"))
            && !is_inside_markdown_link(input, i)
        {
            // Find end of URL (stop at whitespace, ), >, or end of string)
            let url_end = rest
                .find(|c: char| c.is_whitespace() || c == ')' || c == '>')
                .unwrap_or(rest.len());
            let url = &rest[..url_end];

            // Strip trailing punctuation that's likely not part of the URL
            let url = url.trim_end_matches(['.', ',', ';', ':', '!', '?']);

            result.push('[');
            // Use domain + path as link text for readability
            let display = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .unwrap_or(url);
            // Truncate display if too long. Byte-slicing here panicked
            // when byte 57 landed inside a multi-byte char (e.g. a URL
            // path with non-ASCII text) — take by char, not by byte.
            if display.chars().count() > 60 {
                let head: String = display.chars().take(57).collect();
                result.push_str(&head);
                result.push_str("...");
            } else {
                result.push_str(display);
            }
            result.push_str("](");
            result.push_str(url);
            result.push(')');

            // Advance past the URL
            for _ in 0..url.len() {
                chars.next();
            }
        } else {
            result.push(c);
            chars.next();
        }
    }

    result
}

/// Check if position `i` is inside a markdown link `[...](...)`.
fn is_inside_markdown_link(input: &str, i: usize) -> bool {
    let before = &input[..i];
    // If we see "](" before us without a closing ")" in between, we're inside a link URL
    if let Some(bracket_pos) = before.rfind("](") {
        let between = &input[bracket_pos + 2..i];
        !between.contains(')')
    } else {
        false
    }
}

/// Strip markdown syntax, returning plain text.
pub fn strip(input: &str) -> String {
    input
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .replace("# ", "")
        .replace("## ", "")
        .replace("### ", "")
        .replace("- ", "")
        .replace("* ", "")
}

/// Build a single-line preview of a markdown body suitable for a card
/// blurb or meta description. Drops heading lines, list markers, and
/// fenced code blocks entirely (their literal markup is noise in a
/// preview), strips inline emphasis / inline-code markers from the
/// remaining prose, collapses whitespace, and truncates at a word
/// boundary with an ellipsis.
///
/// This is what battle / claim cards on listing pages should use —
/// the previous direct `{{ battle.description }}` was leaking the raw
/// `## Summary` heading into the card text.
pub fn excerpt(input: &str, max: usize) -> String {
    let mut prose: Vec<String> = Vec::new();
    let mut in_fence = false;
    for raw in input.lines() {
        let line = raw.trim();
        // Toggle code-fence and skip every line inside the fence.
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Drop heading lines outright — the section title is repetitive
        // in a preview ("Summary" / "Methodology" / etc.).
        if line.starts_with('#') {
            continue;
        }
        if line.is_empty() {
            continue;
        }
        // Drop blockquote markers and list markers, keep the content.
        let body = line
            .trim_start_matches('>')
            .trim_start()
            .trim_start_matches(['-', '*', '+'])
            .trim_start();
        // Numbered list (`1. `, `12) `).
        let body = body
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(['.', ')'])
            .trim_start();
        if body.is_empty() {
            continue;
        }
        // Strip inline emphasis + inline-code markers.
        let cleaned = body.replace("**", "").replace("__", "").replace('`', "");
        prose.push(cleaned);
    }
    let joined = prose.join(" ");
    crate::web::meta::truncate_description(&joined, max)
}

/// Percent-encode a string for safe use as a URL query value (RFC 3986).
/// Mirrors the behaviour of Python's `urllib.parse.quote_plus` minus the
/// plus-for-space substitution — we percent-encode spaces too, since share
/// URLs on Twitter / LinkedIn expect them.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Askama custom filters — use `{{ value|markdown }}` or `{{ value|plain }}` in templates.
pub mod filters {
    use askama::filters::HtmlSafeOutput;

    /// Render Markdown to sanitized HTML, marked safe so askama won't double-escape.
    #[askama::filter_fn]
    pub fn markdown(
        input: &dyn std::fmt::Display,
        _values: &dyn askama::Values,
    ) -> askama::Result<HtmlSafeOutput<String>> {
        Ok(HtmlSafeOutput(super::render(&input.to_string())))
    }

    /// Strip markdown formatting, return plain text.
    #[askama::filter_fn]
    pub fn plain(
        input: &dyn std::fmt::Display,
        _values: &dyn askama::Values,
    ) -> askama::Result<String> {
        Ok(super::strip(&input.to_string()))
    }

    /// Single-line markdown preview for cards / meta descriptions.
    /// Drops headings, list markers, code fences; strips inline emphasis;
    /// truncates at ~220 chars on a word boundary.
    #[askama::filter_fn]
    pub fn excerpt(
        input: &dyn std::fmt::Display,
        _values: &dyn askama::Values,
    ) -> askama::Result<String> {
        Ok(super::excerpt(&input.to_string(), 220))
    }

    /// Percent-encode for use inside a URL query string.
    #[askama::filter_fn]
    pub fn urlenc(
        input: &dyn std::fmt::Display,
        _values: &dyn askama::Values,
    ) -> askama::Result<String> {
        Ok(super::url_encode(&input.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encode_basics() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(url_encode("100%"), "100%25");
        assert_eq!(url_encode("path/to?q=1"), "path%2Fto%3Fq%3D1");
    }

    #[test]
    fn url_encode_unreserved_passthrough() {
        assert_eq!(url_encode("abcXYZ-._~"), "abcXYZ-._~");
    }

    #[test]
    fn excerpt_drops_heading_lines() {
        let out = excerpt(
            "## Summary\n\nClickHouse claims its Cloud offering is cheaper than Snowflake.\n",
            220,
        );
        assert!(
            !out.contains("Summary"),
            "heading-text leaked into excerpt: {out}"
        );
        assert!(out.starts_with("ClickHouse claims"), "{out}");
    }

    #[test]
    fn excerpt_drops_list_markers_keeps_content() {
        let out = excerpt(
            "## What we test\n\n- Authority rate\n- Latency\n- Cost\n",
            220,
        );
        assert_eq!(out, "Authority rate Latency Cost");
    }

    #[test]
    fn excerpt_strips_code_fence() {
        let out = excerpt(
            "Run the script.\n\n```bash\npython run.py --seed=42\n```\n\nResults are in artifact.json.\n",
            220,
        );
        assert_eq!(out, "Run the script. Results are in artifact.json.");
    }

    #[test]
    fn excerpt_strips_emphasis_and_inline_code() {
        let out = excerpt("This is **bold** and `code` and __also_bold__.", 220);
        assert_eq!(out, "This is bold and code and also_bold.");
    }

    #[test]
    fn slugify_matches_github_style() {
        assert_eq!(slugify_gfm("3. Trust model"), "3-trust-model");
        assert_eq!(slugify_gfm("1. Vocabulary"), "1-vocabulary");
        assert_eq!(
            slugify_gfm("How a Claim is produced and verified"),
            "how-a-claim-is-produced-and-verified"
        );
        assert_eq!(slugify_gfm("FAQ"), "faq");
        assert_eq!(
            slugify_gfm("Tasks, runners, fixtures"),
            "tasks-runners-fixtures"
        );
        assert_eq!(slugify_gfm("  spaced  "), "spaced");
        assert_eq!(slugify_gfm("---"), "");
    }

    #[test]
    fn render_emits_heading_ids() {
        let html = render("## 3. Trust model\n\n## How a Claim is produced and verified\n");
        assert!(
            html.contains(r#"<h2 id="3-trust-model">"#),
            "missing id on h2: {html}"
        );
        assert!(
            html.contains(r#"<h2 id="how-a-claim-is-produced-and-verified">"#),
            "missing id on second h2: {html}"
        );
    }

    #[test]
    fn render_strips_inline_tags_for_slug() {
        let html = render("## **Bold** part of heading\n");
        assert!(
            html.contains(r#"id="bold-part-of-heading""#),
            "id should be derived from visible text only: {html}"
        );
    }

    #[test]
    fn excerpt_truncates_at_word_boundary() {
        let long =
            "## Summary\n\n".to_string() + &"alpha beta gamma delta epsilon zeta ".repeat(20);
        let out = excerpt(&long, 60);
        assert!(out.ends_with('…'), "{out}");
        assert!(out.chars().count() <= 61, "{out}");
        assert!(!out.contains("Summary"));
    }

    /// Fixture: claim UUIDs + slugs + titles from the
    /// `python-vs-typescript-vs-rust-vs-go` battle on prod where the
    /// unresolved-shorthand bug was reported. Every value is a real
    /// row from the prod DB at the time of the report. Used by the
    /// short-prefix tests below so a regression there matches the
    /// production failure mode exactly.
    fn python_vs_battle_claims() -> HashMap<Uuid, (String, String)> {
        // (full_uuid, title, slug-or-uuid)
        let rows: &[(&str, &str, &str)] = &[
            (
                "d981f87a-509d-474a-9d29-e0cdaf585f4d",
                "Claude Sonnet 4.6 single-shot HumanEval-50: TS 100%, Rust 88%, Go 80%, Python 78%",
                "claude-sonnet-4-6-single-shot-humaneval-50-ts-100-rust-88-go-80-python-78-type-s",
            ),
            (
                "175cb300-9049-4cd3-8838-500524e00fac",
                "Rust appears in 26/80 AI-era paper titles, Python in just 11",
                "rust-appears-in-26-80-ai-era-paper-titles-python-in-just-11-the-default-is-invis",
            ),
            (
                "4463420b-b087-4888-b9a6-4f60f8cd4144",
                "Rust's rank-1 in AI paper titles holds 4 days later",
                "rust-s-rank-1-in-ai-paper-titles-holds-4-days-later-but-python-and-go-swap-rank-",
            ),
            (
                "bdc897e6-cb32-47c1-9ec6-4d96ec3807b3",
                "MCP server registry census (n=2225): TypeScript 52% > Python 37%",
                "mcp-server-registry-census-n-2225-typescript-52-python-37-go-6-rust-3-ts-leads-b",
            ),
            (
                "9d54f89e-7ea8-4cb6-9da1-3d9ea13a7a4c",
                "Engineer-importable AI libraries (n=962, programmatic rule): Python 60% by stars",
                "engineer-importable-ai-libraries-n-962-programmatic-rule-python-60-by-stars-2",
            ),
        ];
        rows.iter()
            .map(|(uuid, title, slug)| {
                (
                    Uuid::parse_str(uuid).unwrap(),
                    (title.to_string(), slug.to_string()),
                )
            })
            .collect()
    }

    /// Regression test: a paragraph copy-pasted from the broken
    /// battle's executive summary. With the pre-fix regex (full
    /// 36-char UUID only) every `claim:<8hex>` survived to the
    /// rendered HTML; with the prefix-match fix each is resolved to
    /// a markdown link.
    #[test]
    fn expand_short_claim_refs_from_python_vs_battle() {
        let claims = python_vs_battle_claims();
        // Real form Gemini emitted on the broken page (truncated 8-char
        // hex prefix). Both with and without surrounding backticks
        // since the same battle had both in different paragraphs.
        let input = "Python reclaims the lead with a dominant 60.2% share of \
             stars in the AI library ecosystem (`claim:9d54f89e`). The \
             inaugural claim, `claim:d981f87a`, showed Claude Sonnet 4.6 \
             solving HumanEval in TS at 100%. Naming-survey signal \
             (claim:175cb300) backed by a 4-day-later replication \
             (claim:4463420b) makes the trend hold. Tooling registry \
             census (`claim:bdc897e6`) tilts the same way.";

        let out = expand_claim_refs(input, &claims);

        // Every `claim:<8hex>` should be replaced with a markdown
        // link to the resolved slug — no literal `claim:xxxxxxxx`
        // should survive.
        assert!(
            !out.contains("claim:9d54f89e"),
            "9d54f89e should have been linked. got: {out}"
        );
        assert!(
            !out.contains("claim:d981f87a"),
            "d981f87a should have been linked. got: {out}"
        );
        assert!(
            !out.contains("claim:175cb300"),
            "175cb300 should have been linked. got: {out}"
        );
        assert!(
            !out.contains("claim:4463420b"),
            "4463420b should have been linked. got: {out}"
        );
        assert!(
            !out.contains("claim:bdc897e6"),
            "bdc897e6 should have been linked. got: {out}"
        );
        // And the resolved markdown links actually point at the right
        // slugs.
        assert!(
            out.contains(
                "/claims/engineer-importable-ai-libraries-n-962-programmatic-rule-python-60-by-stars-2"
            ),
            "9d54f89e didn't land on its slug. got: {out}"
        );
        assert!(
            out.contains(
                "/claims/claude-sonnet-4-6-single-shot-humaneval-50-ts-100-rust-88-go-80-python-78-type-s"
            ),
            "d981f87a didn't land on its slug. got: {out}"
        );
    }

    /// Full-UUID form still works (the original code path) — guards
    /// against an accidental regression when loosening the regex.
    #[test]
    fn expand_full_uuid_claim_ref_still_works() {
        let claims = python_vs_battle_claims();
        let input = "See `claim:d981f87a-509d-474a-9d29-e0cdaf585f4d` for HumanEval.";
        let out = expand_claim_refs(input, &claims);
        assert!(
            out.contains(
                "[Claude Sonnet 4.6 single-shot HumanEval-50: TS 100%, Rust 88%, Go 80%, Python 78%](/claims/claude-sonnet-4-6-single-shot-humaneval-50-ts-100-rust-88-go-80-python-78-type-s)"
            ),
            "full UUID form should still be resolved. got: {out}"
        );
    }

    /// Unknown prefix (not in map) stays verbatim — we'd rather
    /// surface a stale id than silently swallow a typo.
    #[test]
    fn expand_unknown_claim_ref_passes_through() {
        let claims = python_vs_battle_claims();
        let input = "Stale ref `claim:deadbeef` should not be linked.";
        let out = expand_claim_refs(input, &claims);
        assert!(
            out.contains("claim:deadbeef"),
            "unknown prefix got mangled: {out}"
        );
        assert!(!out.contains("/claims/"), "unknown prefix linked: {out}");
    }

    // ----- bundle image-URL resolution --------------------------------

    const TEST_BUNDLE_BASE: &str =
        "https://storage.googleapis.com/krabarena-artifacts/bundles/sha256:abcd1234";

    #[test]
    fn bundle_relative_png_rewritten() {
        let html = render_with_bundle(
            "Look at this:\n\n![chart](chart.png)\n",
            Some(TEST_BUNDLE_BASE),
        );
        assert!(
            html.contains(&format!("src=\"{TEST_BUNDLE_BASE}/chart.png\"")),
            "expected resolved GCS URL in: {html}"
        );
    }

    #[test]
    fn bundle_relative_jpeg_rewritten() {
        let html = render_with_bundle(
            "![alt](images/foo.jpeg) and ![](b.jpg)",
            Some(TEST_BUNDLE_BASE),
        );
        assert!(
            html.contains(&format!("{TEST_BUNDLE_BASE}/images/foo.jpeg")),
            "expected images/foo.jpeg rewrite: {html}"
        );
        assert!(
            html.contains(&format!("{TEST_BUNDLE_BASE}/b.jpg")),
            "expected b.jpg rewrite: {html}"
        );
    }

    #[test]
    fn bundle_dot_slash_prefix_normalised() {
        let html = render_with_bundle("![x](./screenshots/a.png)", Some(TEST_BUNDLE_BASE));
        assert!(
            html.contains(&format!("{TEST_BUNDLE_BASE}/screenshots/a.png")),
            "expected ./ prefix stripped before join: {html}"
        );
        assert!(!html.contains("./screenshots"), "leftover ./ in: {html}");
    }

    #[test]
    fn bundle_absolute_https_passes_through() {
        let html = render_with_bundle(
            "![ext](https://example.com/chart.png)",
            Some(TEST_BUNDLE_BASE),
        );
        assert!(
            html.contains("src=\"https://example.com/chart.png\""),
            "absolute URL got mangled: {html}"
        );
        assert!(
            !html.contains("storage.googleapis.com"),
            "absolute URL was rewritten anyway: {html}"
        );
    }

    #[test]
    fn bundle_absolute_http_passes_through() {
        // http:// stays http:// — we don't upgrade for the author.
        let html = render_with_bundle("![ext](http://example.com/x.jpg)", Some(TEST_BUNDLE_BASE));
        assert!(html.contains("src=\"http://example.com/x.jpg\""));
    }

    #[test]
    fn bundle_data_uri_not_rewritten() {
        // `resolve_bundle_image_url` itself must refuse to rewrite a
        // data: URI (it'd be a security hole to expand into a GCS
        // path). The full render pipeline still strips the data: URL
        // downstream via ammonia's default scheme allowlist — that's a
        // separate layer; the assertion here pins our resolver's
        // contract regardless of what ammonia does later.
        let small = "data:image/png;base64,iVBORw0KGgoAAA==";
        assert_eq!(resolve_bundle_image_url(small, TEST_BUNDLE_BASE), None);
    }

    #[test]
    fn bundle_svg_not_rewritten() {
        // SVG is XSS-prone (can carry <script>); the upload pipeline
        // rejects it, and the renderer must not silently promote a
        // relative .svg path to a GCS URL either.
        let html = render_with_bundle("![logo](logo.svg)", Some(TEST_BUNDLE_BASE));
        assert!(
            !html.contains("storage.googleapis.com"),
            "SVG was rewritten to GCS: {html}"
        );
    }

    #[test]
    fn bundle_other_extensions_not_rewritten() {
        // .gif / .webp not on whitelist (yet). Path-traversal too.
        for path in ["pic.gif", "pic.webp", "../etc/passwd.png", "pic"] {
            let body = format!("![x]({path})");
            let html = render_with_bundle(&body, Some(TEST_BUNDLE_BASE));
            assert!(
                !html.contains(&format!("{TEST_BUNDLE_BASE}/{path}")),
                "{path}: should not have been rewritten: {html}"
            );
        }
    }

    #[test]
    fn bundle_fragment_and_root_relative_not_rewritten() {
        let html = render_with_bundle("![x](#anchor) ![y](/abs.png)", Some(TEST_BUNDLE_BASE));
        // Fragments aren't bundle-local.
        assert!(
            !html.contains(&format!("{TEST_BUNDLE_BASE}/#anchor")),
            "fragment rewritten: {html}"
        );
        // Root-relative paths aren't bundle-local either.
        assert!(
            !html.contains(&format!("{TEST_BUNDLE_BASE}//abs.png")),
            "root-relative rewritten: {html}"
        );
    }

    #[test]
    fn bundle_render_no_base_leaves_relative_path() {
        // `render` (no bundle base) leaves relative paths alone — keeps
        // existing behaviour for non-claim renders (docs, summaries).
        let html = render("![x](chart.png)");
        assert!(html.contains("src=\"chart.png\""), "{html}");
    }

    #[test]
    fn bundle_image_base_prod_format() {
        let base = bundle_image_base("gs://krabarena-artifacts", "sha256:abcd");
        assert_eq!(
            base,
            "https://storage.googleapis.com/krabarena-artifacts/bundles/sha256:abcd"
        );
    }

    #[test]
    fn bundle_image_base_dev_format() {
        let base = bundle_image_base("http://localhost:3001/blob", "sha256:abcd");
        assert_eq!(base, "http://localhost:3001/blob/bundles/sha256:abcd");
    }

    #[test]
    fn bundle_image_base_trims_trailing_slash() {
        let base = bundle_image_base("gs://krabarena-artifacts/", "sha256:abcd");
        assert_eq!(
            base,
            "https://storage.googleapis.com/krabarena-artifacts/bundles/sha256:abcd"
        );
    }

    #[test]
    fn has_allowed_image_extension_strips_query() {
        assert!(has_allowed_image_extension("chart.png?v=2"));
        assert!(has_allowed_image_extension("chart.JPG"));
        assert!(has_allowed_image_extension("a/b.jpeg#frag"));
        assert!(!has_allowed_image_extension("logo.svg"));
        assert!(!has_allowed_image_extension("notes.md"));
    }

    /// 0.21.0 incremental-corruption regression. The bug: incremental
    /// summaries cited the new claim as a BARE backtick-wrapped hex
    /// prefix (`` `5118e4da` ``) with NO `claim:` prefix — mirroring
    /// the prompt's own example phrasing — and the old single-pass
    /// regex (which required the literal `claim:`) let it survive into
    /// the rendered HTML as a literal code span. Reproduced from the
    /// real prod data on the python-vs-* / language battle where
    /// `` `5118e4da` `` and `` `c5250329` `` rendered un-linked. The
    /// pass-2 bare-hex resolver must turn each into a real link.
    #[test]
    fn expand_bare_backtick_hex_resolves_incremental_corruption() {
        let mut claims: HashMap<Uuid, (String, String)> = python_vs_battle_claims();
        // The actual JSON-parsing claim cited as a bare `5118e4da`.
        claims.insert(
            Uuid::parse_str("5118e4da-715e-4cb2-9b00-aa273d81e92e").unwrap(),
            (
                "Rust is ~1.4x faster than Go at JSON parsing".into(),
                "rust-is-1-4x-faster-than-go-at-json-parsing-on-this-benchmark".into(),
            ),
        );
        // Exactly what the broken summary rendered: bare backtick hex,
        // no `claim:` prefix, in prose.
        let input = "A new performance benchmark from claim `5118e4da` shows \
                     Rust ~1.4x faster than Go; meanwhile `d981f87a` set the bar.";
        let out = expand_claim_refs(input, &claims);
        assert!(
            !out.contains("`5118e4da`"),
            "bare-hex `5118e4da` survived un-linked (the corruption). got: {out}"
        );
        assert!(
            !out.contains("`d981f87a`"),
            "bare-hex `d981f87a` survived un-linked. got: {out}"
        );
        assert!(
            out.contains("/claims/rust-is-1-4x-faster-than-go-at-json-parsing-on-this-benchmark"),
            "5118e4da didn't link to its slug. got: {out}"
        );
        assert!(
            out.contains("/claims/claude-sonnet-4-6-single-shot-humaneval-50-"),
            "d981f87a bare-hex didn't link. got: {out}"
        );
    }

    /// Pass-2 must NOT clobber a legitimate hex-looking code span that
    /// doesn't match any claim in the battle (e.g. a git short-sha or a
    /// hex color in a code span). Those stay as verbatim code spans.
    #[test]
    fn bare_backtick_hex_non_claim_stays_verbatim() {
        let claims = python_vs_battle_claims();
        // `deadbeef` is hex + ≥6 chars but not a claim prefix → leave it.
        let input = "The commit `deadbeef` and color `ff0000` are not claims.";
        let out = expand_claim_refs(input, &claims);
        assert!(out.contains("`deadbeef`"), "non-claim hex mangled: {out}");
        assert!(out.contains("`ff0000`"), "hex color mangled: {out}");
        assert!(!out.contains("/claims/"), "non-claim hex linked: {out}");
    }

    /// Short numeric / metric code spans (`100`, `0.78`, 5-char hex)
    /// must never trip pass-2 — they're below the 6-hex floor or not
    /// pure hex.
    #[test]
    fn bare_backtick_short_or_nonhex_never_matches() {
        let claims = python_vs_battle_claims();
        let input = "Scores `100`, `78.0`, `abcd` (4 chars), `1234` should not link.";
        let out = expand_claim_refs(input, &claims);
        assert!(
            !out.contains("/claims/"),
            "short/non-hex span linked: {out}"
        );
        assert!(out.contains("`100`") && out.contains("`abcd`"), "{out}");
    }

    /// An ambiguous bare-hex prefix (matches >1 claim) stays verbatim
    /// in pass 2 too — same safety contract as the `claim:` path.
    #[test]
    fn bare_backtick_ambiguous_prefix_stays_verbatim() {
        let mut claims: HashMap<Uuid, (String, String)> = HashMap::new();
        claims.insert(
            Uuid::parse_str("abcdef00-1111-1111-1111-111111111111").unwrap(),
            ("One".into(), "one".into()),
        );
        claims.insert(
            Uuid::parse_str("abcdef00-2222-2222-2222-222222222222").unwrap(),
            ("Two".into(), "two".into()),
        );
        let out = expand_claim_refs("ref `abcdef00` is ambiguous", &claims);
        assert!(
            out.contains("`abcdef00`"),
            "ambiguous bare-hex linked: {out}"
        );
        assert!(!out.contains("/claims/"), "{out}");
    }

    /// Ambiguous prefix (matches more than one claim) stays
    /// verbatim — guessing would silently mis-link.
    #[test]
    fn expand_ambiguous_short_prefix_stays_verbatim() {
        // Two synthetic UUIDs sharing the prefix `aaaa`.
        let mut claims: HashMap<Uuid, (String, String)> = HashMap::new();
        claims.insert(
            Uuid::parse_str("aaaaaaaa-1111-1111-1111-111111111111").unwrap(),
            ("Claim One".into(), "claim-one".into()),
        );
        claims.insert(
            Uuid::parse_str("aaaaaaaa-2222-2222-2222-222222222222").unwrap(),
            ("Claim Two".into(), "claim-two".into()),
        );
        let out = expand_claim_refs("ref `claim:aaaaaaaa` is ambiguous", &claims);
        assert!(
            out.contains("claim:aaaaaaaa"),
            "ambiguous prefix should stay verbatim. got: {out}"
        );
        assert!(
            !out.contains("/claims/"),
            "ambiguous prefix linked anyway. got: {out}"
        );
    }
}
