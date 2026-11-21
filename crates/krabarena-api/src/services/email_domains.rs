//! Personal vs work email domain classification.
//!
//! Used by `/profile/me` to decide whether to render a "My company
//! agents" section. A voter signed in with `alice@acme.com`
//! belongs to the acme.com work entity and gets the section; a
//! voter on `alice@gmail.com` doesn't (gmail.com isn't a company).
//!
//! The exclusion list is intentionally maintained in code rather than
//! in the DB — these are common consumer providers, not a thing we'd
//! ever want a runtime override for. Adding a new provider is a
//! one-line PR.
//!
//! Match is exact + case-insensitive on the bare host. Subdomains
//! aren't expanded (`mail.gmail.com` would not match) because real
//! consumer email always lands on the apex.

const PERSONAL_DOMAINS: &[&str] = &[
    // Google
    "gmail.com",
    "googlemail.com",
    // Microsoft
    "outlook.com",
    "outlook.co.uk",
    "hotmail.com",
    "hotmail.co.uk",
    "live.com",
    "live.co.uk",
    "msn.com",
    // Apple
    "icloud.com",
    "me.com",
    "mac.com",
    // Yahoo / AOL
    "yahoo.com",
    "yahoo.co.uk",
    "yahoo.co.jp",
    "yahoo.fr",
    "yahoo.de",
    "ymail.com",
    "rocketmail.com",
    "aol.com",
    // Privacy-focused
    "proton.me",
    "protonmail.com",
    "pm.me",
    "tutanota.com",
    "tutanota.de",
    "tuta.io",
    "fastmail.com",
    "fastmail.fm",
    // Generic free providers
    "mail.com",
    "gmx.com",
    "gmx.net",
    "gmx.de",
    "zoho.com",
    "hey.com",
    "duck.com",
    // RU / CN / IN / BR / KR
    "mail.ru",
    "yandex.ru",
    "yandex.com",
    "ya.ru",
    "list.ru",
    "bk.ru",
    "inbox.ru",
    "qq.com",
    "163.com",
    "126.com",
    "sina.com",
    "sina.cn",
    "naver.com",
    "daum.net",
    "rediffmail.com",
    "uol.com.br",
    "bol.com.br",
];

/// True if `domain` is a known personal-email provider — i.e. should
/// NOT be treated as a "company" when grouping agents.
pub fn is_personal_email_domain(domain: &str) -> bool {
    let lower = domain.trim().to_ascii_lowercase();
    PERSONAL_DOMAINS.iter().any(|d| *d == lower)
}

/// Extract the host portion of an email address ("a@b.com" → Some("b.com")).
/// Returns None for malformed inputs (no `@`, empty after `@`, etc.) so
/// callers can decide whether to default-classify or refuse.
pub fn email_domain(email: &str) -> Option<String> {
    let (_, host) = email.split_once('@')?;
    let host = host.trim();
    if host.is_empty() || host.contains('@') {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_consumer_providers() {
        for d in [
            "gmail.com",
            "GMAIL.COM",
            "yahoo.com",
            "icloud.com",
            "proton.me",
            "qq.com",
            "yandex.ru",
        ] {
            assert!(is_personal_email_domain(d), "{d} should be personal");
        }
    }

    #[test]
    fn classifies_work_domains() {
        for d in [
            "keenable.ai",
            "krabarena.com",
            "krabarena.org",
            "anthropic.com",
            "openai.com",
        ] {
            assert!(!is_personal_email_domain(d), "{d} should be work");
        }
    }

    #[test]
    fn email_domain_extracts_lowercase_host() {
        assert_eq!(email_domain("Alice@Acme.COM").as_deref(), Some("acme.com"));
        assert_eq!(
            email_domain("alice@gmail.com").as_deref(),
            Some("gmail.com")
        );
    }

    #[test]
    fn email_domain_rejects_malformed() {
        assert_eq!(email_domain("notanemail"), None);
        assert_eq!(
            email_domain("@onlydomain.com").as_deref(),
            Some("onlydomain.com")
        );
        assert_eq!(email_domain("trailing@"), None);
        assert_eq!(email_domain("two@at@sign.com"), None);
    }

    #[test]
    fn personal_check_handles_whitespace() {
        assert!(is_personal_email_domain("  gmail.com  "));
    }
}
