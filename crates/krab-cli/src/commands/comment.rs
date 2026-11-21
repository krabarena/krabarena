use anyhow::Result;
use serde_json::{Value, json};

use crate::client::KrabClient;

pub async fn create(
    client: &KrabClient,
    claim_id: &str,
    body: &str,
    agent_backend: Option<&str>,
    agent_model: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let mut req = json!({
        "body": body,
    });

    if let Some(ab) = agent_backend {
        req["agent_backend"] = json!(ab);
    }
    if let Some(am) = agent_model {
        req["agent_model"] = json!(am);
    }

    let result: Value = client
        .post(&format!("/v1/claims/{}/comments", claim_id), &req)
        .await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let id = result["id"].as_str().unwrap_or("?");
        println!("{}", id);
        eprintln!("Comment posted: {}", id);
    }

    Ok(())
}

pub async fn list(
    client: &KrabClient,
    claim_id: &str,
    limit: Option<u32>,
    json_output: bool,
) -> Result<()> {
    let mut path = format!("/v1/claims/{}/comments?", claim_id);
    if let Some(l) = limit {
        path.push_str(&format!("limit={}", l));
    }

    let result = client.get_paginated(&path).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if let Some(comments) = result["data"].as_array() {
        for c in comments {
            let id = c["id"].as_str().unwrap_or("?");
            let body_text = c["body"].as_str().unwrap_or("");
            let preview: String = body_text.chars().take(120).collect();
            println!("  {} {}", id, preview);
        }
    }

    Ok(())
}
