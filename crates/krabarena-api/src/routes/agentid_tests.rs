use super::*;
use axum::body::to_bytes;
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use crate::services::agentid::tests::{TestKey, agent_claims, mock_issuer, mock_path};

const SECRET: &str = "test-secret";

fn state_with(pool: PgPool) -> AppState {
    AppState {
        proxy_pool: pool.clone(),
        pool,
        jwt_secret: SECRET.into(),
        blob_store_url: "http://localhost:3001/blob".into(),
        private_blob_store_url: None,
        public_base_url: "https://krabarena.test".into(),
        discord_monitor: crate::services::discord_monitor::DiscordMonitorConfig::from_lookup(
            |_| None,
        ),
        email_service: None,
        gemini: crate::services::gemini_service::GeminiClient::new(None),
        proxy_http: reqwest::Client::new(),
        rate_limiter: None,
        leaderboards: std::sync::Arc::new(crate::LeaderboardCaches::new()),
        recaptcha: crate::RecaptchaConfig::default(),
    }
}

fn lazy_state() -> AppState {
    state_with(
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://localhost:1/krabarena_unreachable")
            .expect("lazy pool"),
    )
}

async fn body(resp: Response) -> String {
    String::from_utf8(
        to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn set_cookie_nonce(resp: &Response) -> Option<String> {
    resp.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| {
            c.strip_prefix(&format!("{}=", crate::routes::oauth::OAUTH_STATE_COOKIE))
                .map(|rest| rest.split(';').next().unwrap_or("").to_string())
        })
        .filter(|v| !v.is_empty())
}

fn location(resp: &Response) -> String {
    resp.headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

fn query_param(url: &str, key: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn cookie_headers(nonce: &str, accept: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        header::COOKIE,
        format!("{}={nonce}", crate::routes::oauth::OAUTH_STATE_COOKIE)
            .parse()
            .unwrap(),
    );
    h.insert(header::ACCEPT, accept.parse().unwrap());
    h
}

#[tokio::test]
async fn routes_are_404_when_agentid_is_not_configured() {
    if std::env::var("AGENTID_CLIENT_ID").is_ok_and(|v| !v.trim().is_empty()) {
        eprintln!("SKIP: AGENTID_CLIENT_ID is set in this environment");
        return;
    }
    let resp = start(State(lazy_state()), Query(StartQuery::default())).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = callback(
        State(lazy_state()),
        HeaderMap::new(),
        Query(CallbackQuery::default()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(!crate::services::agentid::enabled());
}

#[tokio::test]
async fn start_redirects_with_pkce_and_sets_state_cookie() {
    let key = TestKey::generate("k1");
    let (_server, cfg) = mock_issuer(&[&key]).await;
    let resp = start_with(
        &lazy_state(),
        &cfg,
        StartQuery {
            login_hint: Some("agent@agentmail.to".into()),
            iss: Some(cfg.issuer.clone()),
            ..Default::default()
        },
    )
    .await;
    assert!(resp.status().is_redirection(), "{}", resp.status());
    let url = location(&resp);
    assert!(url.starts_with(&format!("{}/v0/authorize?", cfg.issuer)));
    let nonce = set_cookie_nonce(&resp).expect("state cookie");
    assert_eq!(
        query_param(&url, "state").as_deref(),
        Some(format!("w.{nonce}").as_str())
    );
    let verifier = agentid::pkce_verifier(SECRET, &nonce);
    assert_eq!(
        query_param(&url, "code_challenge"),
        Some(agentid::pkce_challenge(&verifier))
    );
    assert_eq!(
        query_param(&url, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        query_param(&url, "nonce"),
        Some(agentid::oidc_nonce(SECRET, &nonce))
    );
    assert_eq!(
        query_param(&url, "login_hint").as_deref(),
        Some("agent@agentmail.to")
    );
    assert_eq!(
        query_param(&url, "redirect_uri").as_deref(),
        Some("https://krabarena.test/auth/agentid/callback")
    );
    let cookie = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains(&format!("Max-Age={STATE_COOKIE_MAX_AGE}")));
}

#[tokio::test]
async fn start_rejects_a_foreign_iss_and_bad_session() {
    let key = TestKey::generate("k1");
    let (_server, cfg) = mock_issuer(&[&key]).await;
    let resp = start_with(
        &lazy_state(),
        &cfg,
        StartQuery {
            iss: Some("https://evil.example".into()),
            ..Default::default()
        },
    )
    .await;
    assert!(location(&resp).is_empty());
    assert!(body(resp).await.contains("unexpected identity provider"));

    let resp = start_with(
        &lazy_state(),
        &cfg,
        StartQuery {
            session: Some("not-a-uuid".into()),
            ..Default::default()
        },
    )
    .await;
    assert!(location(&resp).is_empty());
    assert!(body(resp).await.contains("Invalid CLI session"));
}

#[tokio::test]
async fn callback_without_matching_state_cookie_is_refused_before_any_exchange() {
    let key = TestKey::generate("k1");
    let (server, cfg) = mock_issuer(&[&key]).await;
    Mock::given(method("POST"))
        .and(path(mock_path(&cfg.issuer, "/v0/token")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let nonce = "abcdefghijklmnopqrstuvwx";
    let q = || CallbackQuery {
        code: "c".into(),
        state: format!("w.{nonce}"),
        ..Default::default()
    };
    let resp = callback_with(&lazy_state(), &cfg, &HeaderMap::new(), q()).await;
    assert!(body(resp).await.contains("Session Expired"));
    let other = cookie_headers("zyxwvutsrqponmlkjihgfedc", "text/html");
    let resp = callback_with(&lazy_state(), &cfg, &other, q()).await;
    assert!(body(resp).await.contains("Session Expired"));
    let mut mixed_up = q();
    mixed_up.iss = Some("https://evil.example".into());
    let resp = callback_with(
        &lazy_state(),
        &cfg,
        &cookie_headers(nonce, "text/html"),
        mixed_up,
    )
    .await;
    assert!(body(resp).await.contains("Session Expired"));
    let mut denied = q();
    denied.code.clear();
    denied.error = Some("access_denied<script>".into());
    let resp = callback_with(
        &lazy_state(),
        &cfg,
        &cookie_headers(nonce, "text/html"),
        denied,
    )
    .await;
    let html = body(resp).await;
    assert!(html.contains("AgentID sign-in did not complete (access_deniedscript)"));
    assert!(html.contains(START_PATH));
}

#[test]
fn retry_goes_back_to_the_cli_session_for_device_states() {
    let s = Uuid::new_v4();
    assert_eq!(
        retry_url(&format!("d.{s}.abcdefghijklmnopqrstuvwx")),
        format!("/auth/device?session={s}&provider=agentid")
    );
    assert_eq!(retry_url("w.abcdefghijklmnopqrstuvwx"), START_PATH);
    assert_eq!(retry_url("garbage"), START_PATH);
    assert_eq!(
        start_path(Some(&s)),
        format!("/auth/agentid/start?session={s}")
    );
}

#[test]
fn response_format_follows_accept() {
    let f = |accept: &str| {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, accept.parse().unwrap());
        ResponseFormat::from_headers(&h)
    };
    assert_eq!(
        ResponseFormat::from_headers(&HeaderMap::new()),
        ResponseFormat::Html
    );
    assert_eq!(
        f("text/html,application/xhtml+xml,*/*;q=0.8"),
        ResponseFormat::Html
    );
    assert_eq!(f("application/json"), ResponseFormat::Json);
    assert_eq!(f("text/markdown"), ResponseFormat::Markdown);
    assert_eq!(f("text/plain"), ResponseFormat::Markdown);
    assert_eq!(f("*/*"), ResponseFormat::Html);
}

fn sample_session(name: &str) -> AgentSession {
    AgentSession {
        status: "authenticated",
        provider: "agentid",
        token_type: "Bearer",
        bearer_token: "eyJhbGciOiJIUzI1NiJ9.payload.sig".into(),
        expires_in_days: 30,
        user_id: Uuid::nil(),
        display_name: name.into(),
        email: Some("agent@agentmail.to".into()),
        api_base_url: "https://krabarena.test".into(),
        next_steps: next_steps("https://krabarena.test/"),
    }
}

#[tokio::test]
async fn signed_in_page_has_token_steps_and_machine_readable_variants() {
    let s = sample_session("Krab </pre><b>x</b>");
    let html_resp = render_session("https://krabarena.test", &s, ResponseFormat::Html);
    assert_eq!(
        html_resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let html = body(html_resp).await;
    assert!(html.contains("krab auth set-token eyJhbGciOiJIUzI1NiJ9.payload.sig"));
    assert!(html.contains("curl -fsSL https://krabarena.test/install.sh | sh"));
    assert!(!html.contains("</pre><b>"));
    let escaped = html
        .split("data-agent-session>")
        .nth(1)
        .and_then(|rest| rest.split("</pre>").next())
        .unwrap();
    let unescaped = escaped
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&#60;", "<")
        .replace("&gt;", ">")
        .replace("&#62;", ">")
        .replace("&amp;", "&")
        .replace("&#38;", "&");
    let parsed: Value = serde_json::from_str(&unescaped).unwrap();
    assert_eq!(parsed["bearer_token"], "eyJhbGciOiJIUzI1NiJ9.payload.sig");
    assert_eq!(parsed["display_name"], "Krab </pre><b>x</b>");

    let md = body(render_session(
        "https://krabarena.test",
        &s,
        ResponseFormat::Markdown,
    ))
    .await;
    assert!(md.starts_with("# Signed in to KrabArena with AgentID"));
    assert!(md.contains("krab auth set-token eyJhbGciOiJIUzI1NiJ9.payload.sig"));
    assert!(md.contains("https://krabarena.test/docs/agent"));

    let json_resp = render_session("https://krabarena.test", &s, ResponseFormat::Json);
    let v: Value = serde_json::from_str(&body(json_resp).await).unwrap();
    assert_eq!(v["data"]["provider"], "agentid");
    assert_eq!(
        v["data"]["next_steps"][0],
        "curl -fsSL https://krabarena.test/install.sh | sh"
    );
}

async fn mount_token_endpoint(
    server: &wiremock::MockServer,
    issuer: &str,
    id_token: String,
    sub: &str,
) {
    Mock::given(method("POST"))
        .and(path(mock_path(issuer, "/v0/token")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id_token": id_token, "access_token": "access", "token_type": "Bearer", "expires_in": 600
        })))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(mock_path(issuer, "/v0/userinfo")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sub": sub, "owner_email": "maya@acme.com", "owner_email_verified": true
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn web_sign_in_end_to_end_issues_an_agent_token_and_reuses_the_account() {
    let Some(pool) = crate::services::agentid_db_tests::test_pool().await else {
        return;
    };
    let state = state_with(pool.clone());
    let key = TestKey::generate("k1");
    let (server, cfg) = mock_issuer(&[&key]).await;
    let sub = Uuid::new_v4().simple().to_string();

    let mut user_ids = Vec::new();
    for _ in 0..2 {
        let started = start_with(&state, &cfg, StartQuery::default()).await;
        let nonce = set_cookie_nonce(&started).unwrap();
        let mut claims = agent_claims(&cfg, &sub, Some(&agentid::oidc_nonce(SECRET, &nonce)));
        claims.as_object_mut().unwrap().remove("owner_email");
        mount_token_endpoint(&server, &cfg.issuer, key.sign(&claims), &sub).await;
        let resp = callback_with(
            &state,
            &cfg,
            &cookie_headers(&nonce, "application/json"),
            CallbackQuery {
                code: "code".into(),
                state: query_param(&location(&started), "state").unwrap(),
                iss: Some(cfg.issuer.clone()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let cleared = resp
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|v| v.to_str().unwrap().contains("Max-Age=0"));
        assert!(cleared, "state cookie must be cleared after use");
        let v: Value = serde_json::from_str(&body(resp).await).unwrap();
        let token = v["data"]["bearer_token"].as_str().unwrap();
        let jwt = crate::services::auth_service::validate_jwt(token, SECRET).unwrap();
        assert_eq!(jwt.via.as_deref(), Some("agentid"));
        assert_eq!(v["data"]["user_id"], json!(jwt.sub));
        user_ids.push(jwt.sub);
    }
    assert_eq!(user_ids[0], user_ids[1]);
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT owner_email FROM user_identities WHERE provider = 'agentid' AND subject = $1",
    )
    .bind(&sub)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner.as_deref(), Some("maya@acme.com"));
}

#[tokio::test]
async fn device_sign_in_stages_the_token_behind_confirmation() {
    let Some(pool) = crate::services::agentid_db_tests::test_pool().await else {
        return;
    };
    let state = state_with(pool.clone());
    let key = TestKey::generate("k1");
    let (server, cfg) = mock_issuer(&[&key]).await;
    let session = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pending_verifications (verification_type, identifier, code, expires_at) \
         VALUES ('device', $1, $1, NOW() + INTERVAL '10 minutes')",
    )
    .bind(session.to_string())
    .execute(&pool)
    .await
    .unwrap();

    let started = start_with(
        &state,
        &cfg,
        StartQuery {
            session: Some(session.to_string()),
            ..Default::default()
        },
    )
    .await;
    let nonce = set_cookie_nonce(&started).expect("redirect with cookie");
    let raw_state = query_param(&location(&started), "state").unwrap();
    assert_eq!(raw_state, format!("d.{session}.{nonce}"));
    let sub = Uuid::new_v4().simple().to_string();
    mount_token_endpoint(
        &server,
        &cfg.issuer,
        key.sign(&agent_claims(
            &cfg,
            &sub,
            Some(&agentid::oidc_nonce(SECRET, &nonce)),
        )),
        &sub,
    )
    .await;
    let resp = callback_with(
        &state,
        &cfg,
        &cookie_headers(&nonce, "text/html"),
        CallbackQuery {
            code: "code".into(),
            state: raw_state,
            ..Default::default()
        },
    )
    .await;
    let html = body(resp).await;
    assert!(html.contains("Authorize this device"), "{html}");
    assert!(html.contains(&crate::routes::oauth::device_user_code(&session)));
    let (pending, result): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT pending_token, result_token FROM pending_verifications \
         WHERE verification_type = 'device' AND code = $1",
    )
    .bind(session.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(pending.is_some());
    assert!(
        result.is_none(),
        "token must not be pollable before confirmation"
    );

    let expired = start_with(
        &state,
        &cfg,
        StartQuery {
            session: Some(Uuid::new_v4().to_string()),
            ..Default::default()
        },
    )
    .await;
    assert!(body(expired).await.contains("CLI sign-in expired"));
}
