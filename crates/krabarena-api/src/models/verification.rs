use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VerificationRow {
    pub id: Uuid,
    pub claim_id: Uuid,
    pub author_id: Uuid,
    pub verdict: String,
    pub environment: Option<String>,
    pub output_hash: Option<String>,
    pub duration_ms: Option<i32>,
    pub failure_reason: Option<String>,
    pub conditions: Option<String>,
    pub reproduction_steps: Option<String>,
    pub is_author_response: bool,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct VerificationResponse {
    pub id: Uuid,
    pub claim_id: Uuid,
    pub author_id: Uuid,
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproduction_steps: Option<String>,
    pub is_author_response: bool,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateResponseRequest {
    pub verdict: String,
    // Confirmed fields
    pub environment: Option<String>,
    pub output_hash: Option<String>,
    pub duration_ms: Option<i32>,
    // Disconfirmed fields
    pub failure_reason: Option<String>,
    pub conditions: Option<String>,
    pub reproduction_steps: Option<String>,
    // Agent identity
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
}

impl From<VerificationRow> for VerificationResponse {
    fn from(row: VerificationRow) -> Self {
        Self {
            id: row.id,
            claim_id: row.claim_id,
            author_id: row.author_id,
            verdict: row.verdict,
            environment: row.environment,
            output_hash: row.output_hash,
            duration_ms: row.duration_ms,
            failure_reason: row.failure_reason,
            conditions: row.conditions,
            reproduction_steps: row.reproduction_steps,
            is_author_response: row.is_author_response,
            agent_backend: row.agent_backend,
            agent_model: row.agent_model,
            created_at: row.created_at,
        }
    }
}

/// Validate a CreateResponseRequest without touching the database.
/// Returns Ok(()) or an error describing the validation failure.
#[cfg(test)]
pub fn validate_response_request(req: &CreateResponseRequest) -> Result<(), &'static str> {
    let verdict = req.verdict.as_str();
    if verdict != "confirmed" && verdict != "disconfirmed" {
        return Err("verdict must be 'confirmed' or 'disconfirmed'");
    }

    if verdict == "confirmed" {
        if req.environment.as_ref().is_none_or(|s| s.is_empty()) {
            return Err("confirmed requires environment");
        }
        if req.output_hash.as_ref().is_none_or(|s| s.is_empty()) {
            return Err("confirmed requires output_hash");
        }
        if req.duration_ms.is_none_or(|d| d <= 0) {
            return Err("confirmed requires positive duration_ms");
        }
    } else {
        if req.failure_reason.as_ref().is_none_or(|s| s.is_empty()) {
            return Err("disconfirmed requires failure_reason");
        }
        if req.conditions.as_ref().is_none_or(|s| s.is_empty()) {
            return Err("disconfirmed requires conditions");
        }
        if req.reproduction_steps.as_ref().is_none_or(|s| s.is_empty()) {
            return Err("disconfirmed requires reproduction_steps");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_confirmed_request() -> CreateResponseRequest {
        CreateResponseRequest {
            verdict: "confirmed".into(),
            environment: Some("linux x86_64".into()),
            output_hash: Some("sha256:abc123".into()),
            duration_ms: Some(5000),
            failure_reason: None,
            conditions: None,
            reproduction_steps: None,
            agent_backend: Some("claude-code".into()),
            agent_model: Some("opus-4".into()),
        }
    }

    fn make_disconfirmed_request() -> CreateResponseRequest {
        CreateResponseRequest {
            verdict: "disconfirmed".into(),
            environment: None,
            output_hash: None,
            duration_ms: None,
            failure_reason: Some("assertion failed".into()),
            conditions: Some("Ubuntu 22.04, Python 3.12".into()),
            reproduction_steps: Some("pip install && python test.py".into()),
            agent_backend: None,
            agent_model: None,
        }
    }

    #[test]
    fn valid_confirmed_request() {
        assert!(validate_response_request(&make_confirmed_request()).is_ok());
    }

    #[test]
    fn valid_disconfirmed_request() {
        assert!(validate_response_request(&make_disconfirmed_request()).is_ok());
    }

    #[test]
    fn invalid_verdict_rejected() {
        let req = CreateResponseRequest {
            verdict: "maybe".into(),
            ..make_confirmed_request()
        };
        assert!(validate_response_request(&req).is_err());
    }

    #[test]
    fn confirmed_missing_environment_rejected() {
        let mut req = make_confirmed_request();
        req.environment = None;
        assert_eq!(
            validate_response_request(&req).unwrap_err(),
            "confirmed requires environment"
        );
    }

    #[test]
    fn confirmed_missing_output_hash_rejected() {
        let mut req = make_confirmed_request();
        req.output_hash = None;
        assert_eq!(
            validate_response_request(&req).unwrap_err(),
            "confirmed requires output_hash"
        );
    }

    #[test]
    fn confirmed_zero_duration_rejected() {
        let mut req = make_confirmed_request();
        req.duration_ms = Some(0);
        assert_eq!(
            validate_response_request(&req).unwrap_err(),
            "confirmed requires positive duration_ms"
        );
    }

    #[test]
    fn disconfirmed_missing_failure_reason_rejected() {
        let mut req = make_disconfirmed_request();
        req.failure_reason = None;
        assert_eq!(
            validate_response_request(&req).unwrap_err(),
            "disconfirmed requires failure_reason"
        );
    }

    #[test]
    fn disconfirmed_missing_conditions_rejected() {
        let mut req = make_disconfirmed_request();
        req.conditions = None;
        assert_eq!(
            validate_response_request(&req).unwrap_err(),
            "disconfirmed requires conditions"
        );
    }

    #[test]
    fn disconfirmed_missing_reproduction_steps_rejected() {
        let mut req = make_disconfirmed_request();
        req.reproduction_steps = None;
        assert_eq!(
            validate_response_request(&req).unwrap_err(),
            "disconfirmed requires reproduction_steps"
        );
    }

    #[test]
    fn response_from_row_preserves_all_fields() {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let claim_id = Uuid::new_v4();
        let author_id = Uuid::new_v4();

        let row = VerificationRow {
            id,
            claim_id,
            author_id,
            verdict: "confirmed".into(),
            environment: Some("test-env".into()),
            output_hash: Some("sha256:xyz".into()),
            duration_ms: Some(1234),
            failure_reason: None,
            conditions: None,
            reproduction_steps: None,
            is_author_response: true,
            agent_backend: Some("claude-code".into()),
            agent_model: Some("opus".into()),
            created_at: now,
        };

        let resp = VerificationResponse::from(row);
        assert_eq!(resp.id, id);
        assert_eq!(resp.claim_id, claim_id);
        assert_eq!(resp.author_id, author_id);
        assert_eq!(resp.verdict, "confirmed");
        assert_eq!(resp.environment.as_deref(), Some("test-env"));
        assert_eq!(resp.duration_ms, Some(1234));
        assert!(resp.is_author_response);
        assert_eq!(resp.agent_backend.as_deref(), Some("claude-code"));
    }

    #[test]
    fn confirmed_response_serializes_without_failure_fields() {
        let resp = VerificationResponse {
            id: Uuid::new_v4(),
            claim_id: Uuid::new_v4(),
            author_id: Uuid::new_v4(),
            verdict: "confirmed".into(),
            environment: Some("test".into()),
            output_hash: Some("hash".into()),
            duration_ms: Some(100),
            failure_reason: None,
            conditions: None,
            reproduction_steps: None,
            is_author_response: false,
            agent_backend: None,
            agent_model: None,
            created_at: Utc::now(),
        };

        let json = serde_json::to_value(&resp).unwrap();
        assert!(!json.as_object().unwrap().contains_key("failure_reason"));
        assert!(!json.as_object().unwrap().contains_key("conditions"));
        assert!(!json.as_object().unwrap().contains_key("reproduction_steps"));
        assert!(json.as_object().unwrap().contains_key("environment"));
    }
}
