use anyhow::Result;
use serde_json::{Value, json};

use crate::client::KrabClient;

#[allow(clippy::too_many_arguments)]
pub async fn create_verify(
    client: &KrabClient,
    claim_id: &str,
    env: &str,
    output_hash: &str,
    duration: i32,
    tokens: Option<i32>,
    cost: Option<f64>,
    agent_backend: Option<&str>,
    agent_model: Option<&str>,
    json_output: bool,
) -> Result<()> {
    // API still accepts the legacy `confirmed` / `disconfirmed` verdict strings.
    let mut body = json!({
        "verdict": "confirmed",
        "environment": env,
        "output_hash": output_hash,
        "duration_ms": duration,
    });
    attach_pow(&mut body, tokens, cost, agent_backend, agent_model);

    let result: Value = client
        .post(&format!("/v1/claims/{}/verifications", claim_id), &body)
        .await?;

    print_verification("verified", &result, json_output);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create_refute(
    client: &KrabClient,
    claim_id: &str,
    reason: &str,
    conditions: &str,
    steps: &str,
    tokens: Option<i32>,
    cost: Option<f64>,
    agent_backend: Option<&str>,
    agent_model: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let mut body = json!({
        "verdict": "disconfirmed",
        "failure_reason": reason,
        "conditions": conditions,
        "reproduction_steps": steps,
    });
    attach_pow(&mut body, tokens, cost, agent_backend, agent_model);

    let result: Value = client
        .post(&format!("/v1/claims/{}/verifications", claim_id), &body)
        .await?;

    print_verification("refuted", &result, json_output);
    Ok(())
}

fn attach_pow(
    body: &mut Value,
    tokens: Option<i32>,
    cost: Option<f64>,
    agent_backend: Option<&str>,
    agent_model: Option<&str>,
) {
    if let Some(t) = tokens {
        body["tokens_consumed"] = json!(t);
    }
    if let Some(c) = cost {
        body["cost_usd"] = json!(c);
    }
    if let Some(ab) = agent_backend {
        body["agent_backend"] = json!(ab);
    }
    if let Some(am) = agent_model {
        body["agent_model"] = json!(am);
    }
}

fn print_verification(label: &str, result: &Value, json_output: bool) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(result).unwrap_or_default()
        );
        return;
    }
    let id = result["id"].as_str().unwrap_or("?");
    println!("{}", id);
    eprintln!("Claim {}: {}", label, id);
    if result["is_author_response"].as_bool() == Some(true) {
        eprintln!("  (author response flagged)");
    }
    if let Some(ab) = result["agent_backend"].as_str() {
        eprintln!(
            "  agent: {} / {}",
            ab,
            result["agent_model"].as_str().unwrap_or("?")
        );
    }
}

pub async fn list(
    client: &KrabClient,
    claim_id: &str,
    verdict: Option<&str>,
    limit: Option<u32>,
    json_output: bool,
) -> Result<()> {
    let mut path = format!("/v1/claims/{}/verifications?", claim_id);
    if let Some(v) = verdict {
        let mapped = match v {
            "verified" => "confirmed",
            "refuted" => "disconfirmed",
            other => other,
        };
        path.push_str(&format!("verdict={}&", mapped));
    }
    if let Some(l) = limit {
        path.push_str(&format!("limit={}", l));
    }

    let result = client.get_paginated(&path).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if let Some(verifications) = result["data"].as_array() {
        for r in verifications {
            let verdict = r["verdict"].as_str().unwrap_or("?");
            let id = r["id"].as_str().unwrap_or("?");
            let pretty = match verdict {
                "confirmed" | "verified" => "Verify",
                "disconfirmed" | "refuted" => "Refute",
                other => other,
            };
            match verdict {
                "confirmed" | "verified" => println!(
                    "  {} [{}] [{}ms] env: {}",
                    id,
                    pretty,
                    r["duration_ms"],
                    r["environment"].as_str().unwrap_or("?"),
                ),
                "disconfirmed" | "refuted" => println!(
                    "  {} [{}] reason: {}",
                    id,
                    pretty,
                    r["failure_reason"].as_str().unwrap_or("?"),
                ),
                _ => println!("  {} [{}]", id, pretty),
            }
        }
    }

    Ok(())
}
