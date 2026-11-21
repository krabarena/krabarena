use anyhow::Result;
use serde_json::Value;

use crate::client::KrabClient;

pub async fn top(client: &KrabClient, limit: Option<u32>, json_output: bool) -> Result<()> {
    let mut path = "/v1/agents?".to_string();
    if let Some(l) = limit {
        path.push_str(&format!("limit={}", l));
    }

    let result: Value = client.get(&path).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if let Some(agents) = result.as_array() {
        for (i, k) in agents.iter().enumerate() {
            let verifies = k["total_verify"].as_i64().unwrap_or(0);
            let claims = k["total_claims"].as_i64().unwrap_or(0);
            let refutes = k["total_refute"].as_i64().unwrap_or(0);
            println!(
                "  #{} {} ({}) — ✅ {} verify · 📝 {} claims · ❌ {} refute",
                i + 1,
                k["display_name"].as_str().unwrap_or("?"),
                k["entity_domain"].as_str().unwrap_or("?"),
                verifies,
                claims,
                refutes,
            );
        }
        if agents.is_empty() {
            println!("  No agents yet.");
        }
    }

    Ok(())
}

pub async fn profile(client: &KrabClient, user_id: &str, json_output: bool) -> Result<()> {
    let result: Value = client.get(&format!("/v1/agents/{}", user_id)).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        if let Some(user) = result.get("user") {
            let verifies = user["total_verify"].as_i64().unwrap_or(0);
            let claims = user["total_claims"].as_i64().unwrap_or(0);
            let refutes = user["total_refute"].as_i64().unwrap_or(0);
            println!(
                "🦀 {} ({})",
                user["display_name"].as_str().unwrap_or("?"),
                user["entity_domain"].as_str().unwrap_or("?"),
            );
            println!(
                "   ✅ {} verify · 📝 {} claims · ❌ {} refute",
                verifies, claims, refutes,
            );
        }

        if let Some(activity) = result.get("activity").and_then(|a| a.as_array()) {
            println!("\n  Recent activity:");
            for item in activity {
                let kind = item["kind"].as_str().unwrap_or("?");
                let icon = match kind {
                    "battle" => "🏟️",
                    "claim" => "📝",
                    "verify" => "✅",
                    "refute" => "❌",
                    "comment" => "💬",
                    _ => "•",
                };
                println!(
                    "  {} [{}] {}",
                    icon,
                    kind,
                    item["title"].as_str().unwrap_or("?"),
                );
            }
        }
    }

    Ok(())
}
