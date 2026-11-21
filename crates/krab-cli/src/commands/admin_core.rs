use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use base64::Engine;
use krabarena_shared::manifest_builder::role_satisfies;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::{client::KrabClient, config::Config};

pub struct AdminContext {
    client: KrabClient,
    auth: AdminAuth,
}

impl AdminContext {
    pub fn from_config(
        api_url_override: Option<&str>,
        token_override: Option<&str>,
    ) -> Result<Self> {
        let cfg = Config::load()?;
        let api_url = api_url_override.unwrap_or(cfg.api_url()).to_string();
        let token = token_override
            .map(ToOwned::to_owned)
            .or_else(|| cfg.bearer_token.clone());
        let auth = AdminAuth::from_token(token.as_deref());
        let client = KrabClient::new(&api_url, token).with_json_output(true);
        Ok(Self { client, auth })
    }

    pub fn require_admin(&self, tool: &str) -> std::result::Result<(), AdminError> {
        self.auth.require_admin(tool)
    }

    pub fn admin_user_id(&self) -> Option<&str> {
        self.auth.admin_user_id()
    }

    pub async fn get_data(&self, tool: &str, path: &str) -> std::result::Result<Value, AdminError> {
        self.require_admin(tool)?;
        let (status, body) = self
            .client
            .get_envelope(path)
            .await
            .map_err(|e| AdminError::api_transport(tool, e.to_string()))?;

        if status.is_success() {
            return Ok(body.get("data").cloned().unwrap_or(Value::Null));
        }

        Err(AdminError::from_api_response(tool, status, &body))
    }

    pub async fn post_data(
        &self,
        tool: &str,
        path: &str,
        body: &Value,
    ) -> std::result::Result<Value, AdminError> {
        self.require_admin(tool)?;
        let (status, body) = self
            .client
            .post_envelope(path, body)
            .await
            .map_err(|e| AdminError::api_transport(tool, e.to_string()))?;
        envelope_data_or_error(tool, status, &body)
    }

    pub async fn patch_data(
        &self,
        tool: &str,
        path: &str,
        body: &Value,
    ) -> std::result::Result<Value, AdminError> {
        self.require_admin(tool)?;
        let (status, body) = self
            .client
            .patch_envelope(path, body)
            .await
            .map_err(|e| AdminError::api_transport(tool, e.to_string()))?;
        envelope_data_or_error(tool, status, &body)
    }

    pub async fn delete_data(
        &self,
        tool: &str,
        path: &str,
        body: &Value,
    ) -> std::result::Result<Value, AdminError> {
        self.require_admin(tool)?;
        let (status, body) = self
            .client
            .delete_envelope(path, body)
            .await
            .map_err(|e| AdminError::api_transport(tool, e.to_string()))?;
        envelope_data_or_error(tool, status, &body)
    }
}

fn envelope_data_or_error(
    tool: &str,
    status: StatusCode,
    body: &Value,
) -> std::result::Result<Value, AdminError> {
    if status.is_success() {
        Ok(body.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(AdminError::from_api_response(tool, status, body))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AdminAuth {
    status: AdminAuthStatus,
}

impl AdminAuth {
    pub fn from_token(token: Option<&str>) -> Self {
        let status = match token {
            None => AdminAuthStatus::NoToken,
            Some(raw) if raw.trim().is_empty() => AdminAuthStatus::NoToken,
            Some(raw) => decode_admin_claims(raw)
                .map(AdminAuthStatus::from_claims)
                .unwrap_or(AdminAuthStatus::TokenInvalid),
        };
        Self { status }
    }

    pub fn require_admin(&self, tool: &str) -> std::result::Result<(), AdminError> {
        match &self.status {
            AdminAuthStatus::Admin { .. } => Ok(()),
            AdminAuthStatus::NoToken => Err(AdminError::new(
                tool,
                "NO_TOKEN",
                "Admin MCP requires a bearer token in krab config or --token.",
                json!({}),
            )),
            AdminAuthStatus::TokenInvalid => Err(AdminError::new(
                tool,
                "TOKEN_INVALID",
                "Bearer token is not a readable KrabArena JWT.",
                json!({}),
            )),
            AdminAuthStatus::TokenExpired { exp } => Err(AdminError::new(
                tool,
                "TOKEN_EXPIRED",
                "Bearer token is expired; authenticate again before using admin MCP tools.",
                json!({ "exp": exp }),
            )),
            AdminAuthStatus::NotAdmin { role } => Err(AdminError::new(
                tool,
                "NOT_ADMIN",
                "Admin access is required to use KrabArena admin MCP tools.",
                json!({ "role": role }),
            )),
        }
    }

    fn admin_user_id(&self) -> Option<&str> {
        match &self.status {
            AdminAuthStatus::Admin { user_id } => Some(user_id.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum AdminAuthStatus {
    Admin { user_id: String },
    NoToken,
    TokenInvalid,
    TokenExpired { exp: i64 },
    NotAdmin { role: String },
}

impl AdminAuthStatus {
    fn from_claims(claims: DecodedClaims) -> Self {
        if claims.exp < current_unix_timestamp() {
            return Self::TokenExpired { exp: claims.exp };
        }
        if role_satisfies(Some("admin"), Some(claims.role.as_str())) {
            Self::Admin {
                user_id: claims.sub,
            }
        } else {
            Self::NotAdmin { role: claims.role }
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DecodedClaims {
    sub: String,
    role: String,
    exp: i64,
}

#[derive(Debug, Clone)]
pub struct AdminError {
    code: &'static str,
    message: String,
    tool: String,
    details: Value,
}

impl AdminError {
    pub fn new(tool: &str, code: &'static str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            tool: tool.to_string(),
            details,
        }
    }

    #[cfg(test)]
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn structured_content(&self) -> Value {
        json!({
            "code": self.code,
            "tool": self.tool,
            "message": self.message,
            "details": self.details,
        })
    }

    fn api_transport(tool: &str, message: String) -> Self {
        Self::new(
            tool,
            "API_ERROR",
            "Could not reach KrabArena API for admin MCP tool.",
            json!({ "error": sanitize_detail_string(&message) }),
        )
    }

    fn from_api_response(tool: &str, status: StatusCode, body: &Value) -> Self {
        let server_code = body["error"]["code"].as_str().unwrap_or("");
        let server_message = body["error"]["message"]
            .as_str()
            .unwrap_or("KrabArena API returned an error.");
        let code = map_api_error_code(status, server_code, server_message);
        Self::new(
            tool,
            code,
            server_message,
            json!({
                "status": status.as_u16(),
                "server_code": server_code,
                "request_id": body["meta"]["request_id"].as_str().unwrap_or(""),
            }),
        )
    }
}

fn decode_admin_claims(token: &str) -> Option<DecodedClaims> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let exp = value.get("exp")?.as_i64().or_else(|| {
        value
            .get("exp")?
            .as_u64()
            .and_then(|v| i64::try_from(v).ok())
    })?;
    let sub = value.get("sub")?.as_str()?.to_string();
    let role = value.get("role")?.as_str()?.to_string();

    Some(DecodedClaims { sub, role, exp })
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
        .try_into()
        .unwrap_or(i64::MAX)
}

fn map_api_error_code(status: StatusCode, server_code: &str, server_message: &str) -> &'static str {
    if server_code == "LAST_ADMIN" {
        return "LAST_ADMIN";
    }
    if status == StatusCode::UNAUTHORIZED {
        return "TOKEN_INVALID";
    }
    if status == StatusCode::FORBIDDEN {
        return "FORBIDDEN";
    }
    if status == StatusCode::NOT_FOUND
        || server_code.ends_with("_NOT_FOUND")
        || server_message.to_lowercase().contains("not found")
    {
        return "NOT_FOUND";
    }
    if status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY {
        return "VALIDATION_ERROR";
    }
    if status.is_server_error() {
        return "API_ERROR";
    }
    "API_ERROR"
}

fn sanitize_detail_string(raw: &str) -> String {
    raw.replace("Bearer ", "Bearer [redacted] ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn jwt_with_claims(role: Option<&str>, exp: i64) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"typ":"JWT","alg":"HS256"}"#);
        let role_field = role
            .map(|role| format!(r#","role":"{role}""#))
            .unwrap_or_default();
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"sub":"test","exp":{exp}{role_field}}}"#).as_bytes());
        format!("{header}.{payload}.signature-not-checked")
    }

    fn future_exp() -> i64 {
        current_unix_timestamp() + 3600
    }

    #[test]
    fn admin_token_passes_gate() {
        let token = jwt_with_claims(Some("admin"), future_exp());
        let auth = AdminAuth::from_token(Some(&token));
        assert!(auth.require_admin("admin_user_list").is_ok());
    }

    #[test]
    fn lower_roles_do_not_pass_gate() {
        for role in ["editor", "moderator", "user"] {
            let token = jwt_with_claims(Some(role), future_exp());
            let auth = AdminAuth::from_token(Some(&token));
            let err = auth.require_admin("admin_user_list").unwrap_err();
            assert_eq!(err.code(), "NOT_ADMIN");
        }
    }

    #[test]
    fn no_token_is_distinct_from_non_admin() {
        let auth = AdminAuth::from_token(None);
        let err = auth.require_admin("admin_user_list").unwrap_err();
        assert_eq!(err.code(), "NO_TOKEN");
    }

    #[test]
    fn malformed_token_is_invalid() {
        for token in ["not-a-jwt", "a.b.c"] {
            let auth = AdminAuth::from_token(Some(token));
            let err = auth.require_admin("admin_user_list").unwrap_err();
            assert_eq!(err.code(), "TOKEN_INVALID");
        }
    }

    #[test]
    fn token_without_role_is_invalid() {
        let token = jwt_with_claims(None, future_exp());
        let auth = AdminAuth::from_token(Some(&token));
        let err = auth.require_admin("admin_user_list").unwrap_err();
        assert_eq!(err.code(), "TOKEN_INVALID");
    }

    #[test]
    fn expired_token_is_distinct() {
        let token = jwt_with_claims(Some("admin"), current_unix_timestamp() - 60);
        let auth = AdminAuth::from_token(Some(&token));
        let err = auth.require_admin("admin_user_list").unwrap_err();
        assert_eq!(err.code(), "TOKEN_EXPIRED");
    }

    #[test]
    fn api_404_and_validation_not_found_map_to_not_found() {
        let err = AdminError::from_api_response(
            "admin_user_inspect",
            StatusCode::NOT_FOUND,
            &json!({
                "error": { "code": "USER_NOT_FOUND", "message": "not found" },
                "meta": { "request_id": "r1" }
            }),
        );
        assert_eq!(err.code(), "NOT_FOUND");

        let err = AdminError::from_api_response(
            "admin_user_inspect",
            StatusCode::BAD_REQUEST,
            &json!({
                "error": { "code": "VALIDATION_ERROR", "message": "user 'x' not found" },
                "meta": { "request_id": "r2" }
            }),
        );
        assert_eq!(err.code(), "NOT_FOUND");
    }
}
