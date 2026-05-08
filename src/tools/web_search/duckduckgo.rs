//! DuckDuckGo search backend — free, no API key required.
//!
//! Uses the DuckDuckGo HTML endpoint and parses results from the response.
//! This is the default backend when no provider is configured.

use anyhow::{Context, Result};

use super::WebSearchTool;

/// Execute a DuckDuckGo web search.
pub async fn search(tool: &WebSearchTool, query: &str, max_results: usize) -> Result<String> {
    let url = "https://html.duckduckgo.com/html/";

    let response = tool
        .client
        .post(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .form(&[("q", query), ("kl", "")])
        .send()
        .await
        .context("Failed to send request to DuckDuckGo")?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("DuckDuckGo returned HTTP {}", status);
    }

    let body = response
        .text()
        .await
        .context("Failed to read DuckDuckGo response")?;

    let results = parse_html(&body, max_results);

    if results.is_empty() {
        return Ok(format!("No results found for: {}", query));
    }

    Ok(format_results(query, &results))
}

// ── HTML parsing ──

/// A single search result extracted from DuckDuckGo HTML.
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parse DuckDuckGo HTML response to extract search results.
///
/// DuckDuckGo HTML endpoint returns results in a predictable structure
/// with class="result__a" for links and class="result__snippet" for snippets.
fn parse_html(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Split by result blocks
    let result_blocks: Vec<&str> = html.split("class=\"result__body\"").collect();

    for block in result_blocks.iter().skip(1).take(max_results) {
        let title = extract_between(block, "class=\"result__a\"", "</a>")
            .map(|s| strip_html_tags(&s))
            .unwrap_or_default();

        let url = extract_href(block, "class=\"result__a\"").unwrap_or_default();

        let snippet = extract_between(block, "class=\"result__snippet\"", "</a>")
            .or_else(|| extract_between(block, "class=\"result__snippet\"", "</td>"))
            .map(|s| strip_html_tags(&s))
            .unwrap_or_default();

        if !title.is_empty() && !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }
    }

    results
}

/// Format search results into a readable string.
fn format_results(query: &str, results: &[SearchResult]) -> String {
    let mut output = String::new();
    output.push_str(&format!("## Web Search Results for: {}\n\n", query));

    for (i, result) in results.iter().enumerate() {
        output.push_str(&format!("### {}. {}\n", i + 1, result.title));
        output.push_str(&format!("**URL**: {}\n", result.url));
        if !result.snippet.is_empty() {
            output.push_str(&format!("{}\n", result.snippet));
        }
        output.push('\n');
    }

    output
}

// ── HTML utility functions ──

/// Extract text between a start marker and end marker.
fn extract_between(text: &str, start_marker: &str, end_marker: &str) -> Option<String> {
    let start_idx = text.find(start_marker)?;
    let after_marker = &text[start_idx + start_marker.len()..];
    // Find the closing > of the tag containing the start marker
    let content_start = after_marker.find('>')? + 1;
    let content = &after_marker[content_start..];
    let end_idx = content.find(end_marker)?;
    Some(content[..end_idx].to_string())
}

/// Extract href attribute from a tag with the given marker.
fn extract_href(text: &str, marker: &str) -> Option<String> {
    let start_idx = text.find(marker)?;
    // Look backwards for href="
    let before = &text[..start_idx];
    let href_start = before.rfind("href=\"").or_else(|| {
        // Look forward
        let after = &text[start_idx..];
        after.find("href=\"").map(|i| start_idx + i)
    })?;

    let href_content = if href_start < start_idx {
        &text[href_start + 6..]
    } else {
        &text[href_start + 6..]
    };

    let end = href_content.find('"')?;
    let url = &href_content[..end];

    // DuckDuckGo wraps URLs in a redirect, extract the actual URL
    if url.contains("uddg=") {
        let uddg_start = url.find("uddg=")? + 5;
        let uddg_end = url[uddg_start..].find('&').unwrap_or(url.len() - uddg_start);
        let encoded_url = &url[uddg_start..uddg_start + uddg_end];
        Some(url_decode(encoded_url))
    } else if url.starts_with("//duckduckgo.com/l/?") {
        if let Some(kh_start) = url.find("uddg=") {
            let after = &url[kh_start + 5..];
            let end = after.find('&').unwrap_or(after.len());
            Some(url_decode(&after[..end]))
        } else {
            Some(url.to_string())
        }
    } else {
        Some(url.to_string())
    }
}

/// Simple URL decode (handles %XX encoding).
fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
}

/// Strip HTML tags from a string.
fn strip_html_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
        assert_eq!(strip_html_tags("a &amp; b"), "a & b");
        assert_eq!(strip_html_tags("<a href=\"x\">link</a>"), "link");
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("https%3A%2F%2Fexample.com"), "https://example.com");
    }
}
