use super::*;
use jsonwebtoken::{EncodingKey, Header, encode};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub(crate) struct TestKey {
    pub kid: String,
    pkcs8: Vec<u8>,
    pub jwk: Value,
}

impl TestKey {
    pub fn generate(label: &str) -> Self {
        Self::with_kid(&format!("{label}-{}", Uuid::new_v4().simple()))
    }

    pub fn with_kid(kid: &str) -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .unwrap()
            .as_ref()
            .to_vec();
        let pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng).unwrap();
        let public = pair.public_key().as_ref();
        assert_eq!(public.len(), 65);
        let jwk = json!({
            "kty": "EC",
            "crv": "P-256",
            "alg": "ES256",
            "use": "sig",
            "kid": kid,
            "x": URL_SAFE_NO_PAD.encode(&public[1..33]),
            "y": URL_SAFE_NO_PAD.encode(&public[33..65]),
        });
        Self {
            kid: kid.to_string(),
            pkcs8,
            jwk,
        }
    }

    pub fn sign(&self, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.kid.clone());
        encode(&header, claims, &EncodingKey::from_ec_der(&self.pkcs8)).unwrap()
    }

    pub fn jwks(keys: &[&TestKey]) -> JwkSet {
        serde_json::from_value(
            json!({ "keys": keys.iter().map(|k| k.jwk.clone()).collect::<Vec<_>>() }),
        )
        .unwrap()
    }
}

pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub(crate) fn registered_config(issuer: &str) -> AgentIdConfig {
    AgentIdConfig::from_lookup(|k| match k {
        "AGENTID_CLIENT_ID" => Some("krab-client".into()),
        "AGENTID_CLIENT_SECRET" => Some("krab-secret".into()),
        "AGENTID_ISSUER" => Some(issuer.into()),
        _ => None,
    })
    .unwrap()
}

pub(crate) fn agent_claims(cfg: &AgentIdConfig, sub: &str, nonce: Option<&str>) -> Value {
    let mut c = json!({
        "iss": cfg.issuer,
        "aud": cfg.client_id,
        "sub": sub,
        "iat": now(),
        "exp": now() + 600,
        "jti": Uuid::new_v4().to_string(),
        "actor_type": "agent",
        "email": format!("{sub}@agentmail.to"),
        "email_verified": true,
        "name": "Research Krab",
        "preferred_username": "research_agentmail-to",
        "owner_sub": "owner-org-1",
        "owner_name": "Maya Chen",
        "owner_email": "maya@acme.com",
        "owner_email_verified": true,
        "scope": "openid email profile owner_profile owner_email",
    });
    if let Some(n) = nonce {
        c["nonce"] = json!(n);
    }
    c
}

pub(crate) async fn unique_issuer() -> (MockServer, String) {
    let server = MockServer::start().await;
    let issuer = format!("{}/{}", server.uri(), Uuid::new_v4().simple());
    (server, issuer)
}

pub(crate) fn mock_path(issuer: &str, suffix: &str) -> String {
    let url = reqwest::Url::parse(issuer).unwrap();
    format!("{}{suffix}", url.path().trim_end_matches('/'))
}

pub(crate) async fn mock_issuer(keys: &[&TestKey]) -> (MockServer, AgentIdConfig) {
    let (server, issuer) = unique_issuer().await;
    Mock::given(method("GET"))
        .and(path(mock_path(
            &issuer,
            "/.well-known/openid-configuration",
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/v0/authorize"),
            "token_endpoint": format!("{issuer}/v0/token"),
            "jwks_uri": format!("{issuer}/v0/jwks.json"),
            "userinfo_endpoint": format!("{issuer}/v0/userinfo"),
            "code_challenge_methods_supported": ["S256"],
            "id_token_signing_alg_values_supported": ["ES256"],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(mock_path(&issuer, "/v0/jwks.json")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "keys": keys.iter().map(|k| k.jwk.clone()).collect::<Vec<_>>()
        })))
        .mount(&server)
        .await;
    let cfg = registered_config(&issuer);
    (server, cfg)
}

#[test]
fn config_is_off_without_client_id() {
    assert!(AgentIdConfig::from_lookup(|_| None).is_none());
    assert!(
        AgentIdConfig::from_lookup(|k| (k == "AGENTID_CLIENT_ID").then(|| "  ".into())).is_none()
    );
}

#[test]
fn registered_client_requests_owner_scopes_and_open_client_does_not() {
    let reg = registered_config("https://auth.agentid.com/");
    assert_eq!(reg.issuer, "https://auth.agentid.com");
    assert_eq!(reg.scopes, REGISTERED_CLIENT_SCOPES);
    let open = AgentIdConfig::from_lookup(|k| {
        (k == "AGENTID_CLIENT_ID").then(|| "https://krabarena.com".into())
    })
    .unwrap();
    assert_eq!(open.issuer, DEFAULT_ISSUER);
    assert_eq!(open.client_secret, None);
    assert_eq!(open.scopes, OPEN_CLIENT_SCOPES);
    assert_eq!(
        open.redirect_uri("https://krabarena.com/"),
        "https://krabarena.com/auth/agentid/callback"
    );
}

#[test]
fn pkce_challenge_matches_rfc7636_vector() {
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn pkce_verifier_is_valid_and_bound_to_browser_and_server() {
    let v = pkce_verifier("server", "nonce-aaaaaaaaaaaaaaaa");
    assert_eq!(v.len(), 43);
    assert!(
        v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    assert_eq!(v, pkce_verifier("server", "nonce-aaaaaaaaaaaaaaaa"));
    assert_ne!(v, pkce_verifier("server", "nonce-bbbbbbbbbbbbbbbb"));
    assert_ne!(v, pkce_verifier("other", "nonce-aaaaaaaaaaaaaaaa"));
    assert_ne!(v, oidc_nonce("server", "nonce-aaaaaaaaaaaaaaaa"));
}

fn test_discovery() -> Discovery {
    Discovery {
        issuer: DEFAULT_ISSUER.into(),
        authorization_endpoint: "https://auth.agentid.com/v0/authorize".into(),
        token_endpoint: "https://auth.agentid.com/v0/token".into(),
        jwks_uri: "https://auth.agentid.com/v0/jwks.json".into(),
        userinfo_endpoint: None,
    }
}

#[test]
fn authorize_url_carries_pkce_state_nonce_and_hint() {
    let cfg = registered_config(DEFAULT_ISSUER);
    let url = authorize_url(
        &cfg,
        &test_discovery(),
        "https://krabarena.com",
        &AuthorizeRequest {
            state: "w.abcdefghijklmnopqrstuvwxyz",
            code_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            nonce: "oidc-nonce",
            login_hint: Some("agent@agentmail.to"),
        },
    )
    .unwrap();
    let parsed = reqwest::Url::parse(&url).unwrap();
    let q: HashMap<String, String> = parsed.query_pairs().into_owned().collect();
    assert!(url.starts_with("https://auth.agentid.com/v0/authorize?"));
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "krab-client");
    assert_eq!(
        q["redirect_uri"],
        "https://krabarena.com/auth/agentid/callback"
    );
    assert_eq!(q["scope"], REGISTERED_CLIENT_SCOPES);
    assert_eq!(q["state"], "w.abcdefghijklmnopqrstuvwxyz");
    assert_eq!(q["nonce"], "oidc-nonce");
    assert_eq!(
        q["code_challenge"],
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["login_hint"], "agent@agentmail.to");

    let no_hint = authorize_url(
        &cfg,
        &test_discovery(),
        "https://krabarena.com",
        &AuthorizeRequest {
            state: "w.x",
            code_verifier: "v",
            nonce: "n",
            login_hint: Some("not an email\n"),
        },
    )
    .unwrap();
    assert!(!no_hint.contains("login_hint"));
}

#[test]
fn valid_es256_id_token_verifies() {
    let key = TestKey::generate("k1");
    let cfg = registered_config(DEFAULT_ISSUER);
    let token = key.sign(&agent_claims(&cfg, "sub-1", Some("n1")));
    let claims = verify_id_token(&token, &TestKey::jwks(&[&key]), &cfg, "n1").unwrap();
    assert_eq!(claims.sub, "sub-1");
    assert_eq!(claims.owner_email.as_deref(), Some("maya@acme.com"));
}

#[test]
fn id_token_without_nonce_is_accepted() {
    let key = TestKey::generate("k1");
    let cfg = registered_config(DEFAULT_ISSUER);
    let token = key.sign(&agent_claims(&cfg, "sub-1", None));
    assert!(verify_id_token(&token, &TestKey::jwks(&[&key]), &cfg, "expected").is_ok());
}

#[test]
fn id_token_rejections() {
    let key = TestKey::generate("k1");
    let other = TestKey::with_kid(&key.kid);
    let cfg = registered_config(DEFAULT_ISSUER);
    let jwks = TestKey::jwks(&[&key]);
    let base = agent_claims(&cfg, "sub-1", Some("n1"));
    let with = |k: &str, v: Value| {
        let mut c = base.clone();
        c[k] = v;
        c
    };
    let cases: Vec<(&str, String)> = vec![
        ("wrong aud", key.sign(&with("aud", json!("someone-else")))),
        (
            "wrong iss",
            key.sign(&with("iss", json!("https://evil.example"))),
        ),
        ("expired", key.sign(&with("exp", json!(now() - 3600)))),
        ("wrong nonce", key.sign(&with("nonce", json!("n2")))),
        (
            "not an agent",
            key.sign(&with("actor_type", json!("human"))),
        ),
        ("empty sub", key.sign(&with("sub", json!("")))),
        ("foreign key", other.sign(&base)),
        (
            "hs256",
            encode(
                &Header::new(Algorithm::HS256),
                &base,
                &EncodingKey::from_secret(b"krab-secret"),
            )
            .unwrap(),
        ),
    ];
    for (name, token) in cases {
        assert!(
            verify_id_token(&token, &jwks, &cfg, "n1").is_err(),
            "{name} must be rejected"
        );
    }
    let unknown_kid = TestKey::generate("k2");
    assert!(verify_id_token(&unknown_kid.sign(&base), &jwks, &cfg, "n1").is_err());
    let signed = key.sign(&base);
    let at = signed.rfind('.').unwrap() + 3;
    let mut bytes = signed.into_bytes();
    bytes[at] = if bytes[at] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(bytes).unwrap();
    assert!(verify_id_token(&tampered, &jwks, &cfg, "n1").is_err());
}

fn claims_from(v: Value) -> AgentIdClaims {
    serde_json::from_value(v).unwrap()
}

#[test]
fn identity_mapping_for_registered_client() {
    let cfg = registered_config(DEFAULT_ISSUER);
    let id = AgentIdentity::from_claims(&cfg, &claims_from(agent_claims(&cfg, "s", None)));
    assert_eq!(id.subject, "s");
    assert_eq!(id.issuer, DEFAULT_ISSUER);
    assert_eq!(id.email.as_deref(), Some("s@agentmail.to"));
    assert_eq!(id.display_name, "Research Krab");
    assert_eq!(id.owner_email.as_deref(), Some("maya@acme.com"));
    assert_eq!(id.owner_name.as_deref(), Some("Maya Chen"));
    assert_eq!(id.owner_sub.as_deref(), Some("owner-org-1"));
    assert_eq!(id.entity_domain(), "agentmail.to");
}

#[test]
fn identity_mapping_tolerates_open_client_claims() {
    let cfg = registered_config(DEFAULT_ISSUER);
    let c = claims_from(json!({
        "iss": DEFAULT_ISSUER, "sub": "open-sub",
        "email": "Bot@Example.AgentMail.to", "email_verified": true,
    }));
    let id = AgentIdentity::from_claims(&cfg, &c);
    assert_eq!(id.email.as_deref(), Some("bot@example.agentmail.to"));
    assert_eq!(id.display_name, "bot");
    assert_eq!(id.owner_email, None);
    assert_eq!(id.owner_name, None);
    assert_eq!(id.owner_sub, None);
}

#[test]
fn unverified_emails_are_dropped() {
    let cfg = registered_config(DEFAULT_ISSUER);
    let c = claims_from(json!({
        "iss": DEFAULT_ISSUER, "sub": "s",
        "email": "a@agentmail.to", "email_verified": false,
        "preferred_username": "handle",
        "owner_email": "boss@acme.com",
    }));
    let id = AgentIdentity::from_claims(&cfg, &c);
    assert_eq!(id.email, None);
    assert_eq!(id.owner_email, None);
    assert_eq!(id.display_name, "handle");
    assert_eq!(id.entity_domain(), "agentid.com");
}

#[test]
fn userinfo_fills_owner_claims_only_for_same_subject() {
    let base = claims_from(json!({ "iss": DEFAULT_ISSUER, "sub": "s" }));
    let info = json!({
        "sub": "s", "owner_email": "maya@acme.com", "owner_email_verified": true,
        "owner_name": "Maya", "owner_sub": "o1",
    });
    let merged = merge_userinfo(base.clone(), Some(&info));
    assert_eq!(merged.owner_email.as_deref(), Some("maya@acme.com"));
    assert_eq!(merged.owner_email_verified, Some(true));
    assert_eq!(merged.owner_name.as_deref(), Some("Maya"));
    let foreign = json!({ "sub": "other", "owner_email": "x@y.z", "owner_email_verified": true });
    assert_eq!(merge_userinfo(base.clone(), Some(&foreign)), base);
    assert_eq!(merge_userinfo(base.clone(), None), base);
}

fn account(auth_method: &str, role: &str, email: &str) -> ExistingAccount {
    ExistingAccount {
        auth_method: auth_method.into(),
        role: role.into(),
        kind: "human".into(),
        email: Some(email.into()),
        bound_to_other_agentid: false,
    }
}

fn identity(email: &str, owner: Option<&str>) -> AgentIdentity {
    AgentIdentity {
        issuer: DEFAULT_ISSUER.into(),
        subject: "s".into(),
        email: Some(email.into()),
        display_name: "a".into(),
        owner_sub: None,
        owner_email: owner.map(str::to_string),
        owner_name: None,
    }
}

#[test]
fn email_collision_rules() {
    let inbox = "agent@agentmail.to";
    assert_eq!(
        decide_email_collision(&account("email", "user", inbox), &identity(inbox, None)),
        LinkDecision::Link
    );
    assert!(matches!(
        decide_email_collision(&account("github", "user", inbox), &identity(inbox, None)),
        LinkDecision::Refuse(_)
    ));
    assert!(matches!(
        decide_email_collision(
            &account("github", "user", inbox),
            &identity(inbox, Some("someone@else.com"))
        ),
        LinkDecision::Refuse(_)
    ));
    assert_eq!(
        decide_email_collision(
            &account("github", "user", inbox),
            &identity(inbox, Some("AGENT@agentmail.to"))
        ),
        LinkDecision::Link
    );
    for role in ["admin", "editor"] {
        assert!(matches!(
            decide_email_collision(
                &account("email", role, inbox),
                &identity(inbox, Some(inbox))
            ),
            LinkDecision::Refuse(_)
        ));
    }
    let mut bot = account("email", "user", inbox);
    bot.kind = "bot".into();
    assert!(matches!(
        decide_email_collision(&bot, &identity(inbox, None)),
        LinkDecision::Refuse(_)
    ));
    let mut bound = account("email", "user", inbox);
    bound.bound_to_other_agentid = true;
    assert!(matches!(
        decide_email_collision(&bound, &identity(inbox, None)),
        LinkDecision::Refuse(_)
    ));
}

#[tokio::test]
async fn token_exchange_uses_basic_auth_and_pkce_then_verifies() {
    let key = TestKey::generate("k1");
    let (server, cfg) = mock_issuer(&[&key]).await;
    let id_token = key.sign(&agent_claims(&cfg, "sub-x", Some("n1")));
    Mock::given(method("POST"))
        .and(path(mock_path(&cfg.issuer, "/v0/token")))
        .and(header_exists("authorization"))
        .and(wiremock::matchers::body_string_contains(
            "code_verifier=verifier-123",
        ))
        .and(wiremock::matchers::body_string_contains(
            "grant_type=authorization_code",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id_token": id_token, "access_token": "at", "token_type": "Bearer", "expires_in": 600
        })))
        .expect(1)
        .mount(&server)
        .await;
    let d = discovery(&cfg).await.unwrap();
    let tokens = exchange_code(&cfg, &d, "https://krabarena.com", "code-1", "verifier-123")
        .await
        .unwrap();
    assert_eq!(tokens.access_token.as_deref(), Some("at"));
    let claims = verify_id_token_with_refresh(&cfg, &tokens.id_token, "n1")
        .await
        .unwrap();
    assert_eq!(claims.sub, "sub-x");
}

#[tokio::test]
async fn token_endpoint_rejection_is_a_validation_error() {
    let key = TestKey::generate("k1");
    let (server, cfg) = mock_issuer(&[&key]).await;
    Mock::given(method("POST"))
        .and(path(mock_path(&cfg.issuer, "/v0/token")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    let d = discovery(&cfg).await.unwrap();
    assert!(matches!(
        exchange_code(&cfg, &d, "https://krabarena.com", "c", "v").await,
        Err(ApiError::ValidationError(_))
    ));
}

#[tokio::test]
async fn discovery_with_foreign_issuer_is_rejected() {
    let (server, issuer) = unique_issuer().await;
    Mock::given(method("GET"))
        .and(path(mock_path(
            &issuer,
            "/.well-known/openid-configuration",
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": "https://evil.example",
            "authorization_endpoint": "https://evil.example/a",
            "token_endpoint": "https://evil.example/t",
            "jwks_uri": "https://evil.example/j",
        })))
        .mount(&server)
        .await;
    let cfg = registered_config(&issuer);
    assert!(discovery(&cfg).await.is_err());
}

#[tokio::test]
async fn rotated_signing_key_triggers_one_jwks_refresh() {
    let old = TestKey::generate("old");
    let new = TestKey::generate("new");
    let (server, issuer) = unique_issuer().await;
    Mock::given(method("GET"))
        .and(path(mock_path(
            &issuer,
            "/.well-known/openid-configuration",
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/v0/authorize"),
            "token_endpoint": format!("{issuer}/v0/token"),
            "jwks_uri": format!("{issuer}/v0/jwks.json"),
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(mock_path(&issuer, "/v0/jwks.json")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "keys": [old.jwk.clone()] })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(mock_path(&issuer, "/v0/jwks.json")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "keys": [old.jwk.clone(), new.jwk.clone()] })),
        )
        .mount(&server)
        .await;
    let cfg = registered_config(&issuer);
    discovery(&cfg).await.unwrap();
    let token = new.sign(&agent_claims(&cfg, "sub-r", None));
    assert!(
        verify_id_token_with_refresh(&cfg, &token, "n")
            .await
            .is_ok()
    );
}
