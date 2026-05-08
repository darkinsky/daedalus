//! Exa search backend — supports neural/keyword/auto search types.
//!
//! Requires `EXA_API_KEY` environment variable or `web_search.api_key` in config.
//! Provides semantic (neural) search, traditional keyword search, or auto mode.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::WebSearchTool;

/// Execute an Exa web search.
pub async fn search(
    tool: &WebSearchTool,
    query: &str,
    max_results: usize,
    search_type: &str,
) -> Result<String> {
    let api_key = tool.resolve_api_key("EXA_API_KEY")?;
    let api_base = tool
        .config
        .api_base
        .as_deref()
        .unwrap_or("https://api.exa.ai");

    let url = format!("{}/search", api_base);

    let body = serde_json::json!({
        "query": query,
        "num_results": max_results,
        "type": search_type,
        "contents": {
            "text": {
                "max_characters": 1000
            },
            "highlights": true
        }
    });

    let response = tool
        .client
        .post(&url)
        .header("x-api-key", &api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send request to Exa")?;

    let status = response.status();
    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        anyhow::bail!("Exa API returned HTTP {}: {}", status, error_body);
    }

    let result: ExaResponse = response
        .json()
        .await
        .context("Failed to parse Exa response")?;

    let mut output = String::new();
    output.push_str(&format!("## Web Search Results for: {}\n\n", query));

    for (i, item) in result.results.iter().enumerate().take(max_results) {
        output.push_str(&format!("### {}. {}\n", i + 1, item.title));
        output.push_str(&format!("**URL**: {}\n", item.url));
        if let Some(ref text) = item.text {
            output.push_str(&format!("{}\n", text));
        }
        if let Some(ref highlights) = item.highlights {
            if !highlights.is_empty() {
                output.push_str("**Highlights**:\n");
                for h in highlights {
                    output.push_str(&format!("- {}\n", h));
                }
            }
        }
        output.push('\n');
    }

    Ok(output)
}

// ── API response types ──

#[derive(Debug, Deserialize)]
struct ExaResponse {
    #[serde(default)]
    results: Vec<ExaResult>,
}

#[derive(Debug, Deserialize)]
struct ExaResult {
    title: String,
    url: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    highlights: Option<Vec<String>>,
}
