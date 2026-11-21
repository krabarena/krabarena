//! Static registry of battle "producers" — curated brand partnerships
//! whose battles carry a small interactive "produced by <logo>" label on
//! the home feed card and the battle detail hero.
//!
//! A producer is a rare, hand-curated brand: each ships a bundled SVG
//! asset under `static/brand/` plus marketing copy here, so the registry
//! lives in code (adding one is a registry entry + asset + deploy) rather
//! than a runtime-editable table. Battles only store the producer *slug*
//! in `battles.produced_by` (migration 060); the display metadata is
//! resolved here at render time.

/// One registered producer. All fields are `'static` — the registry is a
/// compile-time constant, so the web layer can hand `&'static Producer`
/// straight to the template without allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Producer {
    /// Lookup key stored in `battles.produced_by` (lowercase, e.g. `fermatix`).
    pub slug: &'static str,
    /// Display name shown in the label and tooltip (e.g. `Fermatix.AI`).
    pub name: &'static str,
    /// One- to two-sentence description shown in the hover popover.
    pub description: &'static str,
    /// Homepage; opened in a new tab from both the logo and the popover link.
    pub website_url: &'static str,
    /// Site-relative URL of the brand logo asset under `static/brand/`.
    pub logo_url: &'static str,
}

/// The registered producers. Extend this list (and drop the matching SVG
/// into `static/brand/`) to add a new brand partner.
static PRODUCERS: &[Producer] = &[Producer {
    slug: "fermatix",
    name: "Fermatix.AI",
    description: "End-to-end data solutions for code LLMs and AI agents — proprietary \
                  training datasets, SWE-Bench-style evaluation benchmarks, and \
                  human-reviewed evaluation environments.",
    website_url: "https://fermatix.ai/",
    logo_url: "/static/brand/fermatix.svg",
}];

/// Resolve a producer slug to its registry entry. Case- and
/// whitespace-insensitive; returns `None` for unknown / empty slugs.
pub fn lookup(slug: &str) -> Option<&'static Producer> {
    let normalized = slug.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    PRODUCERS.iter().find(|p| p.slug == normalized.as_str())
}

/// `true` when `slug` names a registered producer (post-normalization).
pub fn is_valid(slug: &str) -> bool {
    lookup(slug).is_some()
}

/// Resolve a list of producer slugs to their registry entries for
/// rendering, preserving order, skipping unknown slugs, and de-duplicating
/// by slug (so a battle that lists the same producer twice shows one chip).
pub fn resolve_all(slugs: &[String]) -> Vec<&'static Producer> {
    let mut seen = std::collections::HashSet::new();
    slugs
        .iter()
        .filter_map(|s| lookup(s))
        .filter(|p| seen.insert(p.slug))
        .collect()
}

/// Every registered slug, for building a friendly validation error.
pub fn valid_slugs() -> Vec<&'static str> {
    PRODUCERS.iter().map(|p| p.slug).collect()
}

/// Normalize a user-supplied slug to its canonical stored form (trim +
/// lowercase). Returns `None` for blank input so callers can treat it as
/// "clear the producer".
pub fn normalize(slug: &str) -> Option<String> {
    let normalized = slug.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_slug_resolves_case_insensitively() {
        assert_eq!(lookup("fermatix").unwrap().name, "Fermatix.AI");
        assert_eq!(lookup("  FERMATIX  ").unwrap().slug, "fermatix");
        assert!(is_valid("Fermatix"));
    }

    #[test]
    fn unknown_or_empty_slug_is_none() {
        assert!(lookup("acme").is_none());
        assert!(lookup("").is_none());
        assert!(lookup("   ").is_none());
        assert!(!is_valid("nope"));
    }

    #[test]
    fn normalize_blanks_to_none() {
        assert_eq!(normalize("  Fermatix "), Some("fermatix".to_string()));
        assert_eq!(normalize("   "), None);
    }
}
