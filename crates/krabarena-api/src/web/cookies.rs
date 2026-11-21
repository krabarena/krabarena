//! Dependency-free cookie helpers for the voter auth cookies. The crate
//! has no cookie library (only `tower-http`), and our needs are simple:
//! read a named cookie from the `Cookie` header and build/clear
//! `Set-Cookie` header values with fixed attributes.

use axum::http::HeaderMap;

pub const DEVICE: &str = "ka_device";
pub const SESSION: &str = "ka_session";
pub const CSRF: &str = "ka_csrf";
/// JS-readable presence flag ("1" when logged in) so the frontend can
/// render logged-in UI without reading the HttpOnly session cookie.
pub const VOTER: &str = "ka_voter";
pub const DEVICE_MAX_AGE: i64 = 63_072_000; // 2 years
pub const SESSION_MAX_AGE: i64 = 5_184_000; // 60 days

/// Read a single cookie value from the request `Cookie` header.
pub fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get("cookie")?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Build a `Set-Cookie` header value. `SameSite=Lax`, `Path=/` always.
pub fn build(name: &str, value: &str, max_age_secs: i64, http_only: bool, secure: bool) -> String {
    let mut s = format!("{name}={value}; Path=/; Max-Age={max_age_secs}; SameSite=Lax");
    if http_only {
        s.push_str("; HttpOnly");
    }
    if secure {
        s.push_str("; Secure");
    }
    s
}

/// Build an expiring `Set-Cookie` that removes the named cookie.
pub fn clear(name: &str, secure: bool) -> String {
    let mut s = format!("{name}=; Path=/; Max-Age=0; SameSite=Lax");
    if secure {
        s.push_str("; Secure");
    }
    s
}

/// Constant-time string equality — avoids a timing oracle on the CSRF
/// double-submit compare.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Whether cookies should carry the `Secure` attribute — true in prod
/// (https), false on a local `http://` dev server so login works locally.
pub fn secure_cookies() -> bool {
    std::env::var("PUBLIC_BASE_URL")
        .map(|u| u.starts_with("https://"))
        .unwrap_or(true)
}

/// A fresh random CSRF double-submit token (URL-safe base64, 24 bytes).
pub fn new_csrf() -> String {
    use base64::Engine;
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Append `Set-Cookie` for the login session (HttpOnly) + a fresh CSRF
/// token (JS-readable). Called by every voter-login endpoint.
pub fn set_login_cookies(headers: &mut HeaderMap, token: &str) {
    let secure = secure_cookies();
    let cookies = [
        build(SESSION, token, SESSION_MAX_AGE, true, secure),
        build(CSRF, &new_csrf(), SESSION_MAX_AGE, false, secure),
        build(VOTER, "1", SESSION_MAX_AGE, false, secure),
    ];
    for c in cookies {
        if let Ok(hv) = c.parse() {
            headers.append(axum::http::header::SET_COOKIE, hv);
        }
    }
}

/// Append `Set-Cookie` clearing the session + CSRF + voter-flag cookies
/// (keeps the device cookie). Called by logout.
pub fn clear_login_cookies(headers: &mut HeaderMap) {
    let secure = secure_cookies();
    for c in [
        clear(SESSION, secure),
        clear(CSRF, secure),
        clear(VOTER, secure),
    ] {
        if let Ok(hv) = c.parse() {
            headers.append(axum::http::header::SET_COOKIE, hv);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn hdrs(cookie: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("cookie", cookie.parse().unwrap());
        h
    }

    #[test]
    fn parses_named_cookie() {
        let h = hdrs("ka_device=abc; ka_session=xyz; other=1");
        assert_eq!(get_cookie(&h, "ka_session").as_deref(), Some("xyz"));
        assert_eq!(get_cookie(&h, "ka_device").as_deref(), Some("abc"));
        assert_eq!(get_cookie(&h, "missing"), None);
    }

    #[test]
    fn build_sets_attributes() {
        let c = build("ka_session", "tok", SESSION_MAX_AGE, true, true);
        assert!(c.starts_with("ka_session=tok;"));
        assert!(c.contains("HttpOnly"));
        assert!(c.contains("Secure"));
        assert!(c.contains("SameSite=Lax"));
        assert!(c.contains("Max-Age=5184000"));
        assert!(c.contains("Path=/"));
    }

    #[test]
    fn build_omits_httponly_when_false() {
        assert!(!build("ka_csrf", "t", SESSION_MAX_AGE, false, true).contains("HttpOnly"));
    }

    #[test]
    fn clear_expires() {
        assert!(clear("ka_session", true).contains("Max-Age=0"));
    }

    #[test]
    fn ct_eq_matches() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
    }

    #[test]
    fn set_login_cookies_sets_session_and_csrf() {
        let mut h = HeaderMap::new();
        set_login_cookies(&mut h, "tok123");
        let set: Vec<_> = h
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert!(
            set.iter()
                .any(|c| c.contains("ka_session=tok123") && c.contains("HttpOnly"))
        );
        assert!(
            set.iter()
                .any(|c| c.starts_with("ka_csrf=") && !c.contains("HttpOnly"))
        );
    }
}
