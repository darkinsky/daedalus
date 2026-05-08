//! Tavily search backend — AI-optimized search with structured summaries.
//!
//! Requires `TAVILY_API_KEY` environment variable or `web_search.api_key` in config.
//! Provides web_search with AI-generated answer summaries.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::WebSearchTool;

/// Execute a Tavily web search.
pub async fn search(tool: &WebSearchTool, query: &str, max_results: usize) -> Result<String> {
    let api_key = tool.resolve_api_key("TAVILY_API_KEY")?;
    let api_base = tool
        .config
        .api_base
        .as_deref()
        .unwrap_or("https://api.tavily.com");

    let url = format!("{}/search", api_base);

    let body = serde_json::json!({
        "api_key": api_key,
        "query": query,
        "max_results": max_results,
        "include_answer": true,
        "include_raw_content": false,
    });

    let response = tool
        .client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send request to Tavily")?;

    let status = response.status();
    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        anyhow::bail!("Tavily API returned HTTP {}: {}", status, error_body);
    }

    let result: TavilyResponse = response
        .json()
        .await
        .context("Failed to parse Tavily response")?;

    let mut output = String::new();
    output.push_str(&format!("## Web Search Results for: {}\n\n", query));

    // Include the AI-generated answer if available
    if let Some(ref answer) = result.answer {
        if !answer.is_empty() {
            output.push_str(&format!("**Summary**: {}\n\n---\n\n", answer));
        }
    }

    for (i, item) in result.results.iter().enumerate().take(max_results) {
        output.push_str(&format!(
            "### {}. {}\n**URL**: {}\n{}\n\n",
            i + 1,
            item.title,
            item.url,
            item.content,
        ));
    }

    Ok(output)
}

// ── API response types ──

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}
