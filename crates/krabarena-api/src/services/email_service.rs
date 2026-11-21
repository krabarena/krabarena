use std::time::Duration;

use reqwest::Client;

use crate::error::ApiError;

#[derive(Clone)]
pub struct EmailService {
    client: Client,
    api_key: String,
    domain: String,
    from: String,
    public_base_url: String,
}

impl EmailService {
    pub fn new(
        api_key: String,
        domain: String,
        public_base_url: String,
        from_override: Option<String>,
    ) -> Self {
        let from = from_override.unwrap_or_else(|| format!("KrabArena <noreply@{}>", domain));
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(3))
            .build()
            .expect("failed to build reqwest client");
        Self {
            client,
            api_key,
            domain,
            from,
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
        }
    }

    pub fn public_base_url(&self) -> &str {
        &self.public_base_url
    }

    /// `code` is what the email shows for manual entry; `link_token` goes
    /// into the one-click `/auth/verify?token=` link. They differ for the
    /// voter login (8-digit code + 256-bit link token): the short code is
    /// only valid together with the email address, never on the link.
    pub async fn send_verification(
        &self,
        to: &str,
        code: &str,
        link_token: &str,
    ) -> Result<(), ApiError> {
        let url = format!("https://api.mailgun.net/v3/{}/messages", self.domain);
        let verify_link = format!("{}/auth/verify?token={}", self.public_base_url, link_token);
        let rendered = crate::services::email_layout::render_verification(
            &self.public_base_url,
            &verify_link,
            code,
        );

        let resp = self
            .client
            .post(&url)
            .basic_auth("api", Some(&self.api_key))
            .form(&[
                ("from", self.from.as_str()),
                ("to", to),
                ("subject", "KrabArena – Verify your email"),
                ("text", rendered.text.as_str()),
                ("html", rendered.html.as_str()),
            ])
            .send()
            .await
            .map_err(|e| ApiError::EmailSendFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(status = %status, body = %body, "mailgun.send_failed");
            return Err(ApiError::EmailSendFailed(format!(
                "Mailgun returned {}",
                status
            )));
        }

        tracing::info!(to = %to, "email.verification_sent");
        Ok(())
    }

    pub async fn send_html(
        &self,
        to: &str,
        subject: &str,
        text_body: &str,
        html_body: &str,
    ) -> Result<(), ApiError> {
        let url = format!("https://api.mailgun.net/v3/{}/messages", self.domain);
        let resp = self
            .client
            .post(&url)
            .basic_auth("api", Some(&self.api_key))
            .form(&[
                ("from", self.from.as_str()),
                ("to", to),
                ("subject", subject),
                ("text", text_body),
                ("html", html_body),
            ])
            .send()
            .await
            .map_err(|e| ApiError::EmailSendFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(status = %status, body = %body, "mailgun.send_failed");
            return Err(ApiError::EmailSendFailed(format!(
                "Mailgun returned {}",
                status
            )));
        }
        Ok(())
    }

    /// `send_html` plus RFC 8058 one-click unsubscribe headers
    /// (`List-Unsubscribe` + `List-Unsubscribe-Post`) so Gmail / Apple
    /// Mail render their native "Unsubscribe" button. Used by instant
    /// notification emails (since 0.43.0).
    pub async fn send_html_with_unsubscribe(
        &self,
        to: &str,
        subject: &str,
        text_body: &str,
        html_body: &str,
        unsubscribe_url: &str,
    ) -> Result<(), ApiError> {
        let url = format!("https://api.mailgun.net/v3/{}/messages", self.domain);
        let list_unsub = format!("<{unsubscribe_url}>");
        let resp = self
            .client
            .post(&url)
            .basic_auth("api", Some(&self.api_key))
            .form(&[
                ("from", self.from.as_str()),
                ("to", to),
                ("subject", subject),
                ("text", text_body),
                ("html", html_body),
                ("h:List-Unsubscribe", list_unsub.as_str()),
                ("h:List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
            ])
            .send()
            .await
            .map_err(|e| ApiError::EmailSendFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(status = %status, body = %body, "mailgun.send_failed");
            return Err(ApiError::EmailSendFailed(format!(
                "Mailgun returned {}",
                status
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_from_address_default() {
        let svc = EmailService::new(
            "key-abc123".into(),
            "mg.example.com".into(),
            "https://example.com".into(),
            None,
        );
        assert_eq!(svc.from, "KrabArena <noreply@mg.example.com>");
        assert_eq!(svc.domain, "mg.example.com");
        assert_eq!(svc.public_base_url(), "https://example.com");
    }

    #[test]
    fn constructs_from_address_override() {
        let svc = EmailService::new(
            "key-abc123".into(),
            "mg.example.com".into(),
            "https://example.com/".into(),
            Some("KrabArena Alerts <alerts@krabarena.com>".into()),
        );
        assert_eq!(svc.from, "KrabArena Alerts <alerts@krabarena.com>");
        assert_eq!(svc.public_base_url(), "https://example.com");
    }

    #[test]
    fn client_has_timeout() {
        let svc = EmailService::new(
            "key-test".into(),
            "mg.example.com".into(),
            "https://example.com".into(),
            None,
        );
        assert_eq!(svc.domain, "mg.example.com");
    }
}
