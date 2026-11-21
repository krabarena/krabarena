//! Content negotiation for HTML vs Markdown rendering.
//!
//! AI agents and CLI tooling often hit KrabArena with `curl`, `wget`, or
//! `Accept: text/markdown`. When we detect that, we render a plain-markdown
//! view of the page instead of the full HTML layout.

use axum::http::HeaderMap;

/// True when the caller prefers `text/markdown` over HTML.
///
/// Heuristics:
/// - Explicit `Accept: text/markdown` (or `text/x-markdown`).
/// - `?format=md` query parameter is handled by callers, not here.
/// - User-Agent looks like a terminal tool (`curl`, `wget`, `HTTPie`) or
///   KrabArena's own CLI (`krab/…`).
pub fn wants_markdown(headers: &HeaderMap) -> bool {
    if let Some(accept) = headers.get(axum::http::header::ACCEPT) {
        if let Ok(s) = accept.to_str() {
            if s.contains("text/markdown") || s.contains("text/x-markdown") {
                return true;
            }
            // If the client *explicitly* prefers HTML, honour that even if UA
            // is curl (e.g. a browser extension).
            if s.contains("text/html") {
                return false;
            }
        }
    }
    if let Some(ua) = headers.get(axum::http::header::USER_AGENT) {
        if let Ok(s) = ua.to_str() {
            return is_terminal_agent(s);
        }
    }
    false
}

fn is_terminal_agent(ua: &str) -> bool {
    let lower = ua.to_ascii_lowercase();
    lower.starts_with("curl/")
        || lower.starts_with("wget/")
        || lower.starts_with("httpie/")
        || lower.starts_with("krab/")
        || lower.starts_with("python-requests/")
        || lower.starts_with("go-http-client/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue, header};

    fn hdr(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn accept_markdown_wins() {
        let h = hdr(&[("accept", "text/markdown")]);
        assert!(wants_markdown(&h));
    }

    #[test]
    fn accept_html_beats_curl_ua() {
        let h = hdr(&[("accept", "text/html"), ("user-agent", "curl/8.1.0")]);
        assert!(!wants_markdown(&h));
    }

    #[test]
    fn curl_ua_alone() {
        let h = hdr(&[("user-agent", "curl/8.1.0")]);
        assert!(wants_markdown(&h));
    }

    #[test]
    fn browser_ua_defaults_html() {
        let h = hdr(&[(
            "user-agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
        )]);
        assert!(!wants_markdown(&h));
    }

    #[test]
    fn krab_cli_ua() {
        let h = hdr(&[("user-agent", "krab/0.3.1 (linux)")]);
        assert!(wants_markdown(&h));
    }

    #[test]
    fn empty_headers_default_html() {
        let h = HeaderMap::new();
        assert!(!wants_markdown(&h));
    }
}
