//! Minimal AWS Signature Version 4 request signer for the Battle Proxy.
//!
//! Some upstreams (Amazon Bedrock AgentCore `InvokeWebSearch`, and AWS
//! APIs in general) don't take a static API key: every request must be
//! signed with IAM credentials over its method, path, query, headers and
//! body. A static `headers` map in the proxy spec can't express that, so a
//! service can carry an `aws_sigv4` block and the proxy signs each
//! forwarded request with the sponsor's credentials.
//!
//! Spec: <https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html>
//!
//! Signs `host`, `x-amz-content-sha256`, `x-amz-date` (and
//! `x-amz-security-token` for temporary credentials). Non-S3 canonical
//! URI rules: each path segment of the on-the-wire (already-encoded) path
//! is URI-encoded again.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::models::proxy::AwsSigV4Config;

type HmacSha256 = Hmac<Sha256>;

/// Headers to attach to the outgoing request (lower-case names).
pub type SignedHeaders = Vec<(&'static str, String)>;

/// Sign a request. `url` must be the exact URL the request is sent to
/// (its path / query are used as they appear on the wire).
pub fn sign(
    cfg: &AwsSigV4Config,
    method: &str,
    url: &reqwest::Url,
    body: &[u8],
    now: DateTime<Utc>,
) -> Result<SignedHeaders, String> {
    let host = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        (None, _) => return Err("upstream URL has no host".into()),
    };
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let payload_hash = hex::encode(Sha256::digest(body));

    // Canonical headers, sorted by name.
    let mut canon: Vec<(&'static str, String)> = vec![
        ("host", host),
        ("x-amz-content-sha256", payload_hash.clone()),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(tok) = cfg.session_token.as_deref().filter(|t| !t.is_empty()) {
        canon.push(("x-amz-security-token", tok.to_string()));
    }
    canon.sort_by(|a, b| a.0.cmp(b.0));
    let signed_headers = canon.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(";");
    let canonical_headers: String = canon
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();

    let canonical_request = format!(
        "{method}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        canonical_uri(url.path()),
        canonical_query(url.query().unwrap_or("")),
    );

    let scope = format!("{date}/{}/{}/aws4_request", cfg.region, cfg.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let k_date = hmac(
        format!("AWS4{}", cfg.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, cfg.region.as_bytes());
    let k_service = hmac(&k_region, cfg.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex::encode(hmac(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        cfg.access_key_id
    );

    // `host` is set by reqwest from the URL; everything else goes on.
    let mut out: SignedHeaders = canon.into_iter().filter(|(k, _)| *k != "host").collect();
    out.push(("authorization", authorization));
    Ok(out)
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// RFC 3986 encoding as SigV4 wants it: everything except unreserved
/// characters is `%XX` (upper-case hex).
fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Non-S3 canonical URI: the wire path with each segment encoded again.
fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".into();
    }
    path.split('/')
        .map(uri_encode)
        .collect::<Vec<_>>()
        .join("/")
}

fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (
                uri_encode(&percent_decode(k)),
                uri_encode(&percent_decode(v)),
            )
        })
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn cfg(service: &str) -> AwsSigV4Config {
        AwsSigV4Config {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            region: "us-east-1".into(),
            service: service.into(),
            session_token: None,
        }
    }

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2015, 8, 30, 12, 36, 0).unwrap()
    }

    fn header<'a>(h: &'a SignedHeaders, name: &str) -> &'a str {
        h.iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
            .unwrap()
    }

    /// Derived signing key from the AWS docs example (date 20150830,
    /// us-east-1, iam) — pins the key-derivation chain.
    #[test]
    fn signing_key_matches_aws_docs_example() {
        let k = hmac(b"AWS4wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", b"20150830");
        let k = hmac(&k, b"us-east-1");
        let k = hmac(&k, b"iam");
        let k = hmac(&k, b"aws4_request");
        assert_eq!(
            hex::encode(k),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn signs_post_with_body_deterministically() {
        let url = reqwest::Url::parse(
            "https://bedrock-agentcore.us-east-1.amazonaws.com/web-search/web-search.v1/invoke",
        )
        .unwrap();
        let body = br#"{"searchQuery":"krabarena"}"#;
        let h = sign(&cfg("bedrock-agentcore"), "POST", &url, body, ts()).unwrap();
        assert_eq!(header(&h, "x-amz-date"), "20150830T123600Z");
        assert_eq!(
            header(&h, "x-amz-content-sha256"),
            hex::encode(Sha256::digest(body))
        );
        let auth = header(&h, "authorization");
        assert!(auth.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock-agentcore/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="
        ));
        // Same inputs → same signature; different body → different one.
        let h2 = sign(&cfg("bedrock-agentcore"), "POST", &url, body, ts()).unwrap();
        assert_eq!(auth, header(&h2, "authorization"));
        let h3 = sign(&cfg("bedrock-agentcore"), "POST", &url, b"{}", ts()).unwrap();
        assert_ne!(auth, header(&h3, "authorization"));
        assert!(h.iter().all(|(k, _)| *k != "host"));
    }

    #[test]
    fn session_token_is_signed_and_attached() {
        let mut c = cfg("bedrock-agentcore");
        c.session_token = Some("TOKEN".into());
        let url = reqwest::Url::parse("https://example.amazonaws.com/").unwrap();
        let h = sign(&c, "GET", &url, b"", ts()).unwrap();
        assert_eq!(header(&h, "x-amz-security-token"), "TOKEN");
        assert!(
            header(&h, "authorization").contains(
                "SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token"
            )
        );
    }

    #[test]
    fn canonical_uri_double_encodes_segments() {
        assert_eq!(canonical_uri(""), "/");
        assert_eq!(canonical_uri("/"), "/");
        assert_eq!(
            canonical_uri("/web-search/web-search.v1/invoke"),
            "/web-search/web-search.v1/invoke"
        );
        assert_eq!(canonical_uri("/a%20b"), "/a%2520b");
    }

    #[test]
    fn canonical_query_sorts_and_encodes() {
        assert_eq!(canonical_query(""), "");
        assert_eq!(
            canonical_query("Version=2010-05-08&Action=ListUsers"),
            "Action=ListUsers&Version=2010-05-08"
        );
        assert_eq!(canonical_query("q=a%20b&flag"), "flag=&q=a%20b");
    }

    /// Live check against real AWS: signs STS `GetCallerIdentity` (free,
    /// read-only) as both a GET-with-query and a POST-with-body and
    /// expects 200. Opt-in:
    ///   eval "$(aws configure export-credentials --format env)"
    ///   cargo test -p krabarena-api --lib aws_sigv4 -- --ignored
    #[tokio::test]
    #[ignore]
    async fn live_sts_get_caller_identity_accepts_signature() {
        let c = AwsSigV4Config {
            access_key_id: std::env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID"),
            secret_access_key: std::env::var("AWS_SECRET_ACCESS_KEY")
                .expect("AWS_SECRET_ACCESS_KEY"),
            region: "us-east-1".into(),
            service: "sts".into(),
            session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
        };
        let http = reqwest::Client::new();

        let url = reqwest::Url::parse(
            "https://sts.amazonaws.com/?Version=2011-06-15&Action=GetCallerIdentity",
        )
        .unwrap();
        let mut req = http.get(url.clone());
        for (k, v) in sign(&c, "GET", &url, b"", Utc::now()).unwrap() {
            req = req.header(k, v);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        assert!(
            status.is_success(),
            "GET {status}: {}",
            resp.text().await.unwrap()
        );

        let url = reqwest::Url::parse("https://sts.amazonaws.com/").unwrap();
        let body = b"Action=GetCallerIdentity&Version=2011-06-15";
        let mut req = http
            .post(url.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body.to_vec());
        for (k, v) in sign(&c, "POST", &url, body, Utc::now()).unwrap() {
            req = req.header(k, v);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        assert!(
            status.is_success(),
            "POST {status}: {}",
            resp.text().await.unwrap()
        );
    }

    /// Live check of the exact call the proxy makes for Amazon Web Search
    /// (Bedrock AgentCore `InvokeWebSearch`, $0.007/call). Opt-in, with
    /// the proxy IAM user's key in AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY:
    ///   cargo test -p krabarena-api --lib live_agentcore -- --ignored
    #[tokio::test]
    #[ignore]
    async fn live_agentcore_invoke_web_search() {
        let c = AwsSigV4Config {
            access_key_id: std::env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID"),
            secret_access_key: std::env::var("AWS_SECRET_ACCESS_KEY")
                .expect("AWS_SECRET_ACCESS_KEY"),
            region: "us-east-1".into(),
            service: "bedrock-agentcore".into(),
            session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
        };
        let url = reqwest::Url::parse(
            "https://bedrock-agentcore.us-east-1.amazonaws.com/web-search/web-search.v1/invoke",
        )
        .unwrap();
        let body = br#"{"searchQuery":"how tall is mount kilimanjaro","maxResults":3}"#;
        let mut req = reqwest::Client::new()
            .post(url.clone())
            .header("content-type", "application/json")
            .body(body.to_vec());
        for (k, v) in sign(&c, "POST", &url, body, Utc::now()).unwrap() {
            req = req.header(k, v);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        assert!(status.is_success(), "{status}: {text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(!v["results"].as_array().unwrap().is_empty(), "{text}");
        println!("{}", &text[..text.len().min(600)]);
    }
}
