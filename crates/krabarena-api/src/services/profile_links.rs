use reqwest::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocialSite {
    X,
    Github,
    Linkedin,
}

impl SocialSite {
    pub fn label(self) -> &'static str {
        match self {
            Self::X => "x.com",
            Self::Github => "GitHub",
            Self::Linkedin => "LinkedIn",
        }
    }

    fn host_allowed(self, host: &str) -> bool {
        match self {
            Self::X => matches!(host, "x.com" | "twitter.com"),
            Self::Github => host == "github.com",
            Self::Linkedin => host == "linkedin.com" || host.ends_with(".linkedin.com"),
        }
    }

    fn handle_from_segments<'a>(self, segments: &[&'a str]) -> Option<&'a str> {
        match (self, segments) {
            (Self::X | Self::Github, [handle]) => Some(handle),
            (Self::Linkedin, ["in", slug]) => Some(slug),
            _ => None,
        }
    }

    fn handle_valid(self, handle: &str) -> bool {
        match self {
            Self::X => {
                (1..=15).contains(&handle.len())
                    && handle
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !X_RESERVED.contains(&handle.to_ascii_lowercase().as_str())
            }
            Self::Github => {
                (1..=39).contains(&handle.len())
                    && handle
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !handle.starts_with('-')
                    && !handle.ends_with('-')
                    && !handle.contains("--")
                    && !GITHUB_RESERVED.contains(&handle.to_ascii_lowercase().as_str())
            }
            Self::Linkedin => {
                (3..=100).contains(&handle.chars().count())
                    && handle
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '%'))
            }
        }
    }

    fn canonical(self, handle: &str) -> String {
        match self {
            Self::X => format!("https://x.com/{handle}"),
            Self::Github => format!("https://github.com/{handle}"),
            Self::Linkedin => format!("https://www.linkedin.com/in/{handle}"),
        }
    }

    fn expected(self) -> &'static str {
        match self {
            Self::X => "an x.com handle (@name) or https://x.com/<name>",
            Self::Github => "a GitHub login or https://github.com/<login>",
            Self::Linkedin => "a LinkedIn profile slug or https://www.linkedin.com/in/<slug>",
        }
    }
}

const X_RESERVED: &[&str] = &[
    "home",
    "explore",
    "i",
    "intent",
    "search",
    "settings",
    "messages",
    "notifications",
    "share",
    "hashtag",
    "login",
    "signup",
    "tos",
    "privacy",
];

const GITHUB_RESERVED: &[&str] = &[
    "settings",
    "login",
    "join",
    "about",
    "features",
    "marketplace",
    "topics",
    "explore",
    "sponsors",
    "pricing",
    "enterprise",
    "orgs",
    "notifications",
];

fn looks_like_url(input: &str) -> bool {
    input.contains("://") || input.contains('/') || input.contains('.')
}

fn parse_web_url(input: &str) -> Option<Url> {
    let with_scheme = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let url = Url::parse(&with_scheme).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url)
}

fn bare_host(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("mobile."))
        .or_else(|| host.strip_prefix("m."))
        .map(str::to_string)
        .unwrap_or(host);
    Some(host)
}

pub fn normalize_social(site: SocialSite, input: &str) -> Result<Option<String>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let err = || format!("{}: expected {}", site.label(), site.expected());
    let handle = if let Some(rest) = input.strip_prefix('@') {
        rest.to_string()
    } else if site == SocialSite::Linkedin && input.starts_with("in/") {
        input
            .trim_start_matches("in/")
            .trim_end_matches('/')
            .to_string()
    } else if looks_like_url(input) {
        let url = parse_web_url(input).ok_or_else(err)?;
        let host = bare_host(&url).ok_or_else(err)?;
        if !site.host_allowed(&host) {
            return Err(format!(
                "{}: only {} links are accepted here",
                site.label(),
                site.label()
            ));
        }
        let segments: Vec<&str> = url
            .path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();
        site.handle_from_segments(&segments)
            .ok_or_else(err)?
            .to_string()
    } else {
        input.to_string()
    };
    if !site.handle_valid(&handle) {
        return Err(err());
    }
    Ok(Some(site.canonical(&handle)))
}

pub fn normalize_website(input: &str) -> Result<Option<String>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let err = || "website: expected an http(s) URL such as https://example.com".to_string();
    if input.chars().any(char::is_whitespace) {
        return Err(err());
    }
    let url = parse_web_url(input).ok_or_else(err)?;
    let host = url.host_str().ok_or_else(err)?;
    if !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
        return Err(err());
    }
    Ok(Some(url.to_string()))
}

pub fn normalize_text(
    field: &str,
    input: &str,
    allow_newlines: bool,
) -> Result<Option<String>, String> {
    let cleaned = if allow_newlines {
        input
            .replace("\r\n", "\n")
            .replace('\t', " ")
            .trim()
            .to_string()
    } else {
        input.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    if cleaned.is_empty() {
        return Ok(None);
    }
    if cleaned
        .chars()
        .any(|c| c.is_control() && !(allow_newlines && c == '\n'))
    {
        return Err(format!("{field}: control characters are not allowed"));
    }
    Ok(Some(cleaned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(site: SocialSite, input: &str) -> String {
        normalize_social(site, input)
            .unwrap_or_else(|e| panic!("{input:?} rejected: {e}"))
            .expect("non-empty")
    }

    #[test]
    fn x_accepts_handles_and_urls() {
        let want = "https://x.com/krab_arena";
        for input in [
            "krab_arena",
            "@krab_arena",
            " @krab_arena ",
            "x.com/krab_arena",
            "https://x.com/krab_arena",
            "https://www.x.com/krab_arena/",
            "http://twitter.com/krab_arena",
            "https://mobile.twitter.com/krab_arena?lang=en",
            "https://x.com/krab_arena#top",
        ] {
            assert_eq!(ok(SocialSite::X, input), want, "{input}");
        }
    }

    #[test]
    fn x_rejects_other_domains_and_bad_handles() {
        for input in [
            "https://github.com/krab",
            "https://evil.com/x.com/krab",
            "https://x.com.evil.com/krab",
            "https://x.com/krab/status/123",
            "https://x.com/",
            "https://x.com/home",
            "@this_handle_is_way_too_long",
            "bad-handle",
            "javascript:alert(1)",
            "https://user:pw@x.com/krab",
            "ftp://x.com/krab",
        ] {
            assert!(normalize_social(SocialSite::X, input).is_err(), "{input}");
        }
    }

    #[test]
    fn github_accepts_handles_and_urls() {
        let want = "https://github.com/octo-cat";
        for input in [
            "octo-cat",
            "@octo-cat",
            "github.com/octo-cat",
            "https://github.com/octo-cat",
            "https://www.github.com/octo-cat/",
        ] {
            assert_eq!(ok(SocialSite::Github, input), want, "{input}");
        }
    }

    #[test]
    fn github_rejects_invalid() {
        for input in [
            "-octo",
            "octo-",
            "oc--to",
            "octo_cat",
            "https://gitlab.com/octo",
            "https://github.com/octo/repo",
            "https://github.com/settings",
            "https://gist.github.com/octo",
        ] {
            assert!(
                normalize_social(SocialSite::Github, input).is_err(),
                "{input}"
            );
        }
    }

    #[test]
    fn linkedin_accepts_slugs_and_urls() {
        let want = "https://www.linkedin.com/in/jane-doe-123";
        for input in [
            "jane-doe-123",
            "@jane-doe-123",
            "in/jane-doe-123",
            "linkedin.com/in/jane-doe-123",
            "https://www.linkedin.com/in/jane-doe-123/",
            "https://uk.linkedin.com/in/jane-doe-123?trk=x",
        ] {
            assert_eq!(ok(SocialSite::Linkedin, input), want, "{input}");
        }
    }

    #[test]
    fn linkedin_rejects_invalid() {
        for input in [
            "https://www.linkedin.com/company/acme",
            "https://www.linkedin.com/in/",
            "https://linkedin.com.evil.com/in/jane",
            "https://x.com/in/jane",
            "ab",
            "jane doe",
        ] {
            assert!(
                normalize_social(SocialSite::Linkedin, input).is_err(),
                "{input}"
            );
        }
    }

    #[test]
    fn empty_clears() {
        for site in [SocialSite::X, SocialSite::Github, SocialSite::Linkedin] {
            assert_eq!(normalize_social(site, "   ").unwrap(), None);
        }
        assert_eq!(normalize_website("").unwrap(), None);
        assert_eq!(normalize_text("bio", " \n ", true).unwrap(), None);
    }

    #[test]
    fn website_normalises() {
        assert_eq!(
            normalize_website("example.com").unwrap().unwrap(),
            "https://example.com/"
        );
        assert_eq!(
            normalize_website("http://blog.example.org/about?x=1")
                .unwrap()
                .unwrap(),
            "http://blog.example.org/about?x=1"
        );
        for bad in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "localhost",
            "https://user:pw@example.com",
            "https://exa mple.com",
            "ftp://example.com",
        ] {
            assert!(normalize_website(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn text_normalises_whitespace_and_rejects_controls() {
        assert_eq!(
            normalize_text("full_name", "  Jane   Q.\tDoe ", false)
                .unwrap()
                .unwrap(),
            "Jane Q. Doe"
        );
        assert_eq!(
            normalize_text("bio", "Line one\nLine two", true)
                .unwrap()
                .unwrap(),
            "Line one\nLine two"
        );
        assert!(normalize_text("full_name", "a\u{0}b", false).is_err());
        assert!(normalize_text("bio", "a\u{7}b", true).is_err());
    }
}
