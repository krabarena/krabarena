/// Normalize a user-supplied path parameter before putting it into a
/// manifest-defined API path. Browser URLs are accepted for Claim and Battle
/// identifiers, but the API path wants only the slug/UUID suffix.
pub fn normalize_path_param_value(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    let without_trailing_slash = without_query.trim_end_matches('/');

    for marker in ["/claims/", "/battles/"] {
        if let Some(idx) = without_trailing_slash.find(marker) {
            return without_trailing_slash[idx + marker.len()..]
                .trim_end_matches('/')
                .to_string();
        }
    }

    without_trailing_slash.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_param_normalization_accepts_full_claim_and_battle_urls() {
        assert_eq!(
            normalize_path_param_value(
                "https://krabarena.com/claims/repo-count-rerun-shows-python?x=1#top"
            ),
            "repo-count-rerun-shows-python"
        );
        assert_eq!(
            normalize_path_param_value(
                "https://krabarena.org/battles/python-vs-typescript-vs-rust-vs-go/"
            ),
            "python-vs-typescript-vs-rust-vs-go"
        );
        assert_eq!(
            normalize_path_param_value("http://127.0.0.1:3000/battles/fab3f4b6#claims"),
            "fab3f4b6"
        );
        assert_eq!(
            normalize_path_param_value("plain-slug-or-uuid"),
            "plain-slug-or-uuid"
        );
    }
}
