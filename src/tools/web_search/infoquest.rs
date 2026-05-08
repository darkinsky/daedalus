//! InfoQuest search backend — supports time range filtering.
//!
//! Requires `INFOQUEST_API_KEY` environment variable or `web_search.api_key` in config.
//! Provides web_search + web_fetch + image_search with time range filtering.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::WebSearchTool;

/// Execute an InfoQuest web search.
pub async fn search(
    tool: &WebSearchTool,
    query: &str,
    max_results: usize,
    time_range: &str,
) -> Result<String> {
    let api_key = tool.resolve_api_key("INFOQUEST_API_KEY")?;
    let api_base = tool
        .config
        .api_base
        .as_deref()
        .unwrap_or("https://api.infoquest.ai");

    let url = format!("{}/v1/search", api_base);

    let mut body = serde_json::json!({
        "query": query,
        "max_results": max_results,
    });

    if time_range != "all" {
        body.as_object_mut()
            .unwrap()
            .insert("time_range".to_string(), serde_json::json!(time_range));
    }

    let response = tool
        .client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send request to InfoQuest")?;

    let status = response.status();
    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        anyhow::bail!("InfoQuest API returned HTTP {}: {}", status, error_body);
    }

    let result: InfoQuestResponse = response
        .json()
        .await
        .context("Failed to parse InfoQuest response")?;

    let mut output = String::new();
    output.push_str(&format!("## Web Search Results for: {}\n\n", query));

    for (i, item) in result.results.iter().enumerate().take(max_results) {
        output.push_str(&format!("### {}. {}\n", i + 1, item.title));
        output.push_str(&format!("**URL**: {}\n", item.url));
        if let Some(ref snippet) = item.snippet {
            output.push_str(&format!("{}\n", snippet));
        }
        if let Some(ref published) = item.published_date {
            output.push_str(&format!("*Published: {}*\n", published));
        }
        output.push('\n');
    }

    Ok(output)
}

// ── API response types ──

#[derive(Debug, Deserialize)]
struct InfoQuestResponse {
    #[serde(default)]
    results: Vec<InfoQuestResult>,
}

#[derive(Debug, Deserialize)]
struct InfoQuestResult {
    title: String,
    url: String,
    #[serde(default)]
    snippet: Option<String>,
    #[serde(default)]
    published_date: Option<String>,
}
