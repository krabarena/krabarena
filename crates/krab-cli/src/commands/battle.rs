use anyhow::Result;
use serde_json::{Value, json};

use crate::client::KrabClient;

pub async fn create(
    client: &KrabClient,
    title: &str,
    description: &str,
    tags: &[String],
    parent_id: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let body = json!({
        "title": title,
        "description": description,
        "domain_tags": tags,
        "parent_id": parent_id,
    });

    let result: Value = client.post("/v1/battles", &body).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let id = result["id"].as_str().unwrap_or("?");
        println!("{}", id);
        eprintln!("Battle opened: {}", id);
        eprintln!("Status: {}", result["status"].as_str().unwrap_or("?"));
    }

    Ok(())
}

pub async fn edit(
    client: &KrabClient,
    battle_id: &str,
    title: Option<&str>,
    description: Option<&str>,
    tags: Option<&[String]>,
    json_output: bool,
) -> Result<()> {
    let mut body = json!({});
    if let Some(t) = title {
        body["title"] = json!(t);
    }
    if let Some(d) = description {
        body["description"] = json!(d);
    }
    if let Some(t) = tags {
        body["domain_tags"] = json!(t);
    }

    let result: Value = client
        .patch(&format!("/v1/battles/{}", battle_id), &body)
        .await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        eprintln!("Battle updated: {}", battle_id);
        eprintln!("Title: {}", result["title"].as_str().unwrap_or("?"));
    }

    Ok(())
}

pub async fn list(
    client: &KrabClient,
    status: Option<&str>,
    domain: Option<&str>,
    limit: Option<u32>,
    json_output: bool,
) -> Result<()> {
    let mut path = "/v1/battles?".to_string();
    if let Some(s) = status {
        path.push_str(&format!("status={}&", s));
    }
    if let Some(d) = domain {
        path.push_str(&format!("domain={}&", d));
    }
    if let Some(l) = limit {
        path.push_str(&format!("limit={}&", l));
    }

    let result = client.get_paginated(&path).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if let Some(battles) = result["data"].as_array() {
        for battle in battles {
            println!(
                "  {} [{}] {}",
                battle["id"].as_str().unwrap_or("?"),
                battle["status"].as_str().unwrap_or("?"),
                battle["title"].as_str().unwrap_or("?"),
            );
        }
        if battles.is_empty() {
            println!("  No battles found.");
        }
    }

    Ok(())
}

pub async fn get(client: &KrabClient, battle_id: &str, json_output: bool) -> Result<()> {
    let result: Value = client.get(&format!("/v1/battles/{}", battle_id)).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Title:  {}", result["title"].as_str().unwrap_or("?"));
        println!("Status: {}", result["status"].as_str().unwrap_or("?"));
        println!(
            "Claims: {}  ·  Verify: {}  ·  Refute: {}",
            result["claim_count"].as_i64().unwrap_or(0),
            result["total_verify"]
                .as_i64()
                .or_else(|| result["total_verify"].as_i64())
                .unwrap_or(0),
            result["total_refute"]
                .as_i64()
                .or_else(|| result["total_buries"].as_i64())
                .unwrap_or(0),
        );
        if let Some(desc) = result["description"].as_str() {
            println!("\n{}", desc);
        }
    }

    Ok(())
}

pub async fn sub_battles(client: &KrabClient, battle_id: &str, json_output: bool) -> Result<()> {
    let result: Value = client
        .get(&format!("/v1/battles/{}/sub-battles", battle_id))
        .await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if let Some(battles) = result.as_array() {
        for battle in battles {
            println!(
                "  {} [{}] {}",
                battle["id"].as_str().unwrap_or("?"),
                battle["status"].as_str().unwrap_or("?"),
                battle["title"].as_str().unwrap_or("?"),
            );
        }
    }

    Ok(())
}
