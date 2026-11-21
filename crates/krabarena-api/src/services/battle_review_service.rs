//! Battle AutoReview — validates proxy spec and reviews battle quality.

use serde::Serialize;

use crate::error::ApiError;
use crate::models::proxy::ProxySpec;
use crate::services::gemini_service::{self, GeminiClient};

const BATTLE_REVIEW_PROMPT: &str = include_str!("battle_review_prompt.txt");

#[derive(Debug, Serialize)]
pub struct BattleReviewResult {
    pub spec_validation: Vec<ServiceTestResult>,
    pub review: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ServiceTestResult {
    pub service: String,
    pub upstream: String,
    pub status: String, // "pass" | "fail"
    pub message: String,
}

/// Validate the proxy spec: test each service is reachable.
pub async fn validate_proxy_spec(spec: &ProxySpec) -> Vec<ServiceTestResult> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut results = Vec::new();

    for (name, svc) in &spec.services {
        let test_url = format!("{}/", svc.upstream.trim_end_matches('/'));
        let mut req = client.head(&test_url);
        for (k, v) in &svc.headers {
            req = req.header(k, v);
        }

        let result = match req.send().await {
            Ok(resp) => {
                let status_code = resp.status().as_u16();
                if status_code < 500 {
                    ServiceTestResult {
                        service: name.clone(),
                        upstream: svc.upstream.clone(),
                        status: "pass".into(),
                        message: format!("reachable (HTTP {})", status_code),
                    }
                } else {
                    ServiceTestResult {
                        service: name.clone(),
                        upstream: svc.upstream.clone(),
                        status: "fail".into(),
                        message: format!("server error (HTTP {})", status_code),
                    }
                }
            }
            Err(e) => ServiceTestResult {
                service: name.clone(),
                upstream: svc.upstream.clone(),
                status: "fail".into(),
                message: format!("unreachable: {}", e),
            },
        };
        results.push(result);
    }

    // Budget sanity
    if spec.budget_cents <= 0 && !spec.services.is_empty() {
        results.push(ServiceTestResult {
            service: "_budget".into(),
            upstream: String::new(),
            status: "fail".into(),
            message: "budget_cents must be > 0 when proxy services are configured".into(),
        });
    }

    results
}

/// Run Gemini quality review on the battle.
pub async fn review_battle_quality(
    gemini: &GeminiClient,
    title: &str,
    description: &str,
    domain_tags: &[String],
    spec: Option<&ProxySpec>,
) -> Result<Option<serde_json::Value>, ApiError> {
    let api_key = match gemini.api_key() {
        Some(k) => k.to_string(),
        None => return Ok(None),
    };

    let mut context = format!(
        "Battle title: {}\nDescription: {}\nDomain tags: {}\n",
        title,
        description,
        domain_tags.join(", ")
    );

    if let Some(s) = spec {
        context.push_str(&format!(
            "\nProxy services: {}\nBudget: {}¢ total, {}¢ per claimer\nRate limit: {} rpm\n",
            s.services.keys().cloned().collect::<Vec<_>>().join(", "),
            s.budget_cents,
            s.per_claim_cents,
            s.rate_limit_rpm,
        ));
    } else {
        context.push_str("\nNo proxy services configured.\n");
    }

    let body = serde_json::json!({
        "systemInstruction": {
            "parts": [{ "text": BATTLE_REVIEW_PROMPT }]
        },
        "contents": [{
            "role": "user",
            "parts": [{ "text": context }]
        }],
        "generationConfig": {
            "temperature": 0.3,
            "responseMimeType": "application/json",
        }
    });

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        crate::services::gemini_service::DEFAULT_MODEL,
        api_key
    );

    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    // Log token usage + estimated cost so the battle review's
    // Gemini spend is visible in tracing even though we don't have
    // a natural row to persist it to (the battle_review endpoint is
    // a one-off editor tool, not a per-claim persisted review).
    let usage = gemini_service::parse_usage(&json, gemini_service::DEFAULT_MODEL);
    tracing::info!(
        prompt_tokens = usage.prompt_tokens,
        completion_tokens = usage.completion_tokens,
        cost_micros = usage.cost_micros,
        "battle.review.gemini.usage"
    );
    let text = json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("{}");

    let review: serde_json::Value =
        serde_json::from_str(text).unwrap_or_else(|_| serde_json::json!({"error": "parse failed"}));

    Ok(Some(review))
}

/// Full battle review: spec validation + quality review.
pub async fn review_battle(
    gemini: &GeminiClient,
    title: &str,
    description: &str,
    domain_tags: &[String],
    spec: Option<&ProxySpec>,
) -> Result<BattleReviewResult, ApiError> {
    let spec_validation = if let Some(s) = spec {
        validate_proxy_spec(s).await
    } else {
        vec![]
    };

    let review = review_battle_quality(gemini, title, description, domain_tags, spec).await?;

    Ok(BattleReviewResult {
        spec_validation,
        review,
    })
}
