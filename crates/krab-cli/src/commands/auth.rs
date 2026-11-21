use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::client::KrabClient;
use crate::config::Config;

const LOGIN_POLL_INTERVAL: Duration = Duration::from_secs(2);
const LOGIN_POLL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Short confirmation code for a device session — MUST match the server's
/// `routes::oauth::device_user_code` (first 8 hex digits, `ABCD-1234`).
/// The browser shows it on the "Authorize this device" step so the user
/// can tell their own terminal's session from one an attacker sent them.
pub fn device_user_code(session: &Uuid) -> String {
    let hex = session.simple().to_string().to_uppercase();
    format!("{}-{}", &hex[..4], &hex[4..8])
}

/// `krab auth login` — browser-driven device flow.
///
/// Opens `{api_url}/auth/device?session=<uuid>` in the user's default
/// browser, then polls `/v1/auth/device/poll?session=<uuid>` every 2s
/// until the server reports the session has been authorized (or 5 min
/// elapse). On success, saves the returned bearer to
/// `~/.krab/config.toml` so subsequent commands pick it up automatically.
pub async fn login(json_output: bool) -> Result<()> {
    let mut config = Config::load()?;
    let api_url = config.api_url().trim_end_matches('/').to_string();
    let session_id = Uuid::new_v4();
    let session = session_id.to_string();
    let login_url = format!("{}/auth/device?session={}", api_url, session);

    if !json_output {
        eprintln!("Opening browser: {}", login_url);
        eprintln!(
            "Confirm the browser shows code {} before clicking \"Authorize this device\".",
            device_user_code(&session_id)
        );
    }
    if let Err(e) = webbrowser::open(&login_url) {
        // Browser launch failed (headless SSH, no DISPLAY, etc.). Fall
        // back to printing the URL so the user can open it manually.
        eprintln!("Couldn't open browser ({}). Open this URL manually:", e);
        eprintln!("  {}", login_url);
    }

    let poll_url = format!("{}/v1/auth/device/poll?session={}", api_url, session);
    let http = reqwest::Client::new();
    let deadline = Instant::now() + LOGIN_POLL_TIMEOUT;

    if !json_output {
        eprintln!("Waiting for authorization...");
    }

    loop {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after {}s — rerun `krab auth login` to start over",
                LOGIN_POLL_TIMEOUT.as_secs()
            );
        }

        let resp = http.get(&poll_url).send().await;
        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let body: Value = resp.json().await.unwrap_or(Value::Null);
                let data = body.get("data").unwrap_or(&body);
                if data["status"].as_str() == Some("authorized") {
                    if let Some(bearer) = data["bearer_token"].as_str() {
                        config.bearer_token = Some(bearer.to_string());
                        config.save()?;

                        if json_output {
                            println!(
                                "{}",
                                serde_json::to_string(&json!({
                                    "status": "authenticated",
                                    "token_saved": true,
                                }))?
                            );
                        } else {
                            eprintln!("Authenticated. Token saved to {:?}.", Config::config_path());
                        }
                        return Ok(());
                    }
                }
            }
        }
        tokio::time::sleep(LOGIN_POLL_INTERVAL).await;
    }
}

pub async fn email(client: &KrabClient, email: &str, json_output: bool) -> Result<()> {
    let result: Value = client
        .post("/v1/auth/email", &json!({ "email": email }))
        .await?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({ "status": "sent", "email": email, "data": result }))?
        );
    } else {
        eprintln!("Verification email sent to {}", email);
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}

pub async fn email_verify(token: &str, json_output: bool) -> Result<()> {
    let mut config = Config::load()?;
    let client = KrabClient::new(config.api_url(), None).with_json_output(json_output);
    let result: Value = client
        .post("/v1/auth/email/verify", &json!({ "token": token }))
        .await?;

    if let Some(bearer) = result["bearer_token"].as_str() {
        config.bearer_token = Some(bearer.to_string());
        config.save()?;
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({ "status": "authenticated", "token_saved": true }))?
        );
    } else {
        eprintln!("Authenticated successfully. Token saved.");
    }

    Ok(())
}

pub fn status(json_output: bool) -> Result<()> {
    let config = Config::load()?;
    let authenticated = config.bearer_token.is_some();
    let api_url = config.api_url().to_string();

    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "authenticated": authenticated,
                "api_url": api_url,
                "config_path": Config::config_path().to_string_lossy(),
            }))?
        );
    } else {
        match &config.bearer_token {
            Some(_) => eprintln!(
                "Authenticated (token stored in {:?})",
                Config::config_path()
            ),
            None => eprintln!("Not authenticated. Run: krab auth email <email>"),
        }
        eprintln!("API URL: {}", api_url);
    }
    Ok(())
}

pub fn logout(json_output: bool) -> Result<()> {
    let mut config = Config::load()?;
    config.bearer_token = None;
    config.save()?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({ "status": "logged_out" }))?
        );
    } else {
        eprintln!("Logged out. Token removed.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Must stay identical to the server's `device_user_code` (the
    /// browser confirmation step shows the same code).
    #[test]
    fn device_user_code_matches_server_format() {
        let s = Uuid::parse_str("abcd1234-5678-4abc-8def-0123456789ab").unwrap();
        assert_eq!(device_user_code(&s), "ABCD-1234");
    }
}
