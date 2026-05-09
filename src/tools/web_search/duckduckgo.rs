//! DuckDuckGo search backend — free, no API key required.
//!
//! Uses the DuckDuckGo Lite endpoint (`lite.duckduckgo.com/lite/`) which
//! is less likely to trigger CAPTCHA challenges compared to the HTML endpoint.
//! This is the default backend when no provider is configured.

use anyhow::{Context, Result};

use super::WebSearchTool;

/// Maximum number of retry attempts when CAPTCHA is encountered.
const MAX_RETRIES: u32 = 2;

/// Base delay between retries in milliseconds (increases with each attempt).
const RETRY_BASE_DELAY_MS: u64 = 2000;

/// Execute a DuckDuckGo web search via the Lite endpoint.
///
/// Includes retry logic with exponential backoff to handle CAPTCHA challenges
/// that occur when multiple requests are sent in quick succession.
pub async fn search(tool: &WebSearchTool, query: &str, max_results: usize) -> Result<String> {
    let url = format!(
        "https://lite.duckduckgo.com/lite/?q={}",
        url_encode(query)
    );

    let mut last_error = None;

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            // Exponential backoff: 2s, 4s
            let delay = RETRY_BASE_DELAY_MS * (1 << (attempt - 1));
            tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
        }

        let body = match send_request(tool, &url).await {
            Ok(body) => body,
            Err(e) => {
                last_error = Some(e);
                continue;
            }
        };

        // Detect CAPTCHA/bot challenge page
        if body.contains("anomaly-modal") || body.contains("bots use DuckDuckGo") {
            last_error = Some(anyhow::anyhow!(
                "DuckDuckGo returned a CAPTCHA challenge (attempt {})",
                attempt + 1
            ));
            continue;
        }

        let results = parse_lite_html(&body, max_results);

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        return Ok(format_results(query, &results));
    }

    // All retries exhausted
    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!("DuckDuckGo search failed after {} retries", MAX_RETRIES)
    }))
}

/// Send a single HTTP request to DuckDuckGo Lite.
async fn send_request(tool: &WebSearchTool, url: &str) -> Result<String> {
    let response = tool
        .client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64; rv:109.0) Gecko/20100101 Firefox/115.0")
        .header("Accept", "text/html")
        .header("Accept-Language", "en-US,en;q=0.5")
        .send()
        .await
        .context("Failed to send request to DuckDuckGo")?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("DuckDuckGo returned HTTP {}", status);
    }

    response
        .text()
        .await
        .context("Failed to read DuckDuckGo response")
}

// ── HTML parsing for Lite endpoint ──

/// A single search result extracted from DuckDuckGo Lite HTML.
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parse DuckDuckGo Lite HTML response to extract search results.
///
/// The Lite endpoint uses a table-based layout with:
/// - Links: `<a ... class='result-link'>Title</a>`
/// - Snippets: `<td class='result-snippet'>Content</td>`
/// - URLs in href: `//duckduckgo.com/l/?uddg=ENCODED_URL&rut=...`
fn parse_lite_html(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Find all result-link anchors and their corresponding snippets
    let mut search_from = 0;

    while results.len() < max_results {
        // Find next result-link
        let link_marker = "class='result-link'>";
        let link_start = match html[search_from..].find(link_marker) {
            Some(idx) => search_from + idx,
            None => break,
        };

        // Extract title (text between > and </a>)
        let title_start = link_start + link_marker.len();
        let title_end = match html[title_start..].find("</a>") {
            Some(idx) => title_start + idx,
            None => break,
        };
        let title = strip_html_tags(&html[title_start..title_end]);

        // Skip sponsored links
        let after_link = &html[title_end..title_end + 200.min(html.len() - title_end)];
        if after_link.contains("Sponsored link") {
            search_from = title_end + 4;
            continue;
        }

        // Extract URL from href attribute before the class='result-link'
        let href_search_start = if link_start > 200 { link_start - 200 } else { 0 };
        let href_region = &html[href_search_start..link_start];
        let url = extract_url_from_href(href_region).unwrap_or_default();

        // Find the corresponding snippet (next result-snippet after this link)
        let snippet_marker = "class='result-snippet'>";
        let snippet = if let Some(snippet_offset) = html[title_end..].find(snippet_marker) {
            let snippet_start = title_end + snippet_offset + snippet_marker.len();
            // Find the end of the snippet (</td>)
            if let Some(snippet_end_offset) = html[snippet_start..].find("</td>") {
                let raw_snippet = &html[snippet_start..snippet_start + snippet_end_offset];
                strip_html_tags(raw_snippet)
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        if !title.is_empty() && !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }

        search_from = title_end + 4;
    }

    results
}

/// Extract the actual URL from a DuckDuckGo redirect href.
///
/// DuckDuckGo Lite wraps URLs as: `href="//duckduckgo.com/l/?uddg=ENCODED_URL&rut=..."`
fn extract_url_from_href(region: &str) -> Option<String> {
    // Find the last href=" in the region (closest to the result-link class)
    let href_start = region.rfind("href=\"")?;
    let href_content = &region[href_start + 6..];
    let href_end = href_content.find('"')?;
    let raw_href = &href_content[..href_end];

    // Decode HTML entities in the href (e.g., &amp; -> &)
    let href = raw_href.replace("&amp;", "&");

    // Extract the actual URL from the uddg parameter
    if let Some(uddg_start) = href.find("uddg=") {
        let url_part = &href[uddg_start + 5..];
        let url_end = url_part.find('&').unwrap_or(url_part.len());
        let encoded_url = &url_part[..url_end];
        Some(url_decode(encoded_url))
    } else {
        // Not a redirect, return as-is (strip leading //)
        let cleaned = href.trim_start_matches("//");
        if cleaned.starts_with("duckduckgo.com") {
            None // Internal DDG link, skip
        } else {
            Some(format!("https://{}", cleaned))
        }
    }
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

// ── Utility functions ──

/// Simple URL encode for query strings.
fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                result.push(c);
            }
            ' ' => result.push('+'),
            _ => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                for byte in encoded.bytes() {
                    result.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    result
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

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello+world");
        assert_eq!(url_encode("rust programming"), "rust+programming");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn test_extract_url_from_href() {
        let region = r#"<a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=abc123" class='"#;
        let url = extract_url_from_href(region);
        assert_eq!(url, Some("https://rust-lang.org/".to_string()));
    }

    #[test]
    fn test_parse_lite_html_basic() {
        let html = r#"
        <table>
            <tr>
                <td>
                  <a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F&amp;rut=abc" class='result-link'>Example Site</a>
                </td>
            </tr>
            <tr>
                <td class='result-snippet'>
                    This is an example website for testing.
                </td>
            </tr>
        </table>
        "#;
        let results = parse_lite_html(html, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example Site");
        assert_eq!(results[0].url, "https://example.com/");
        assert!(results[0].snippet.contains("example website"));
    }
}
