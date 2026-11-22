const KNOWN_ICONS: &[&str] = &[
    "bell",
    "check",
    "copy",
    "discord",
    "external-link",
    "github",
    "globe",
    "google",
    "linkedin",
    "mail",
    "message-circle",
    "play",
    "search",
    "tag",
    "user",
    "x",
    "x-logo",
];

pub fn is_known_icon(name: &str) -> bool {
    KNOWN_ICONS.contains(&name)
}

pub fn decorative(name: &str, _sprite_url: Option<&str>) -> String {
    render_icon(name, None)
}

pub fn labeled(name: &str, label: &str, _sprite_url: Option<&str>) -> String {
    render_icon(name, Some(label))
}

fn render_icon(name: &str, label: Option<&str>) -> String {
    assert!(is_known_icon(name), "unknown icon: {name}");
    match label {
        Some(label) => format!(
            r#"<span class="icon icon-{name}" role="img" aria-label="{label}"></span>"#,
            name = html_attr_escape(name),
            label = html_attr_escape(label),
        ),
        None => format!(
            r#"<span class="icon icon-{name}" aria-hidden="true"></span>"#,
            name = html_attr_escape(name),
        ),
    }
}

fn html_attr_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_decorative_icon() {
        let html = decorative("search", Some("/static/dist/assets/krab-icons-abcd.svg"));
        assert!(html.contains("aria-hidden=\"true\""));
        assert_eq!(
            html,
            r#"<span class="icon icon-search" aria-hidden="true"></span>"#
        );
    }

    #[test]
    fn renders_labeled_icon() {
        let html = labeled("github", "GitHub", Some("/sprite.svg"));
        assert!(html.contains("role=\"img\""));
        assert!(html.contains("aria-label=\"GitHub\""));
    }

    #[test]
    #[should_panic(expected = "unknown icon")]
    fn rejects_unknown_icon() {
        let _ = decorative("missing", Some("/sprite.svg"));
    }

    #[test]
    fn escapes_labels_and_urls() {
        let html = labeled("search", "\"Search\"", Some("/x.svg?bad=<tag>"));
        assert!(html.contains("aria-label=\"&quot;Search&quot;\""));
        assert!(!html.contains("/x.svg"));
    }
}
