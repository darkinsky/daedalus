use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::Embedding;

/// OpenAI-compatible embedding provider.
///
/// Works with any API that implements the OpenAI `/v1/embeddings` endpoint,
/// including OpenAI itself, Azure OpenAI, Venus proxy, and local servers
/// (e.g., vLLM, Ollama with OpenAI-compatible mode).
#[allow(dead_code)]
pub struct OpenAiEmbedding {
    client: Client,
    api_key: String,
    api_base: String,
    model: String,
    dimensions: usize,
}

#[allow(dead_code)]
impl OpenAiEmbedding {
    /// Create a new OpenAI embedding provider.
    ///
    /// # Arguments
    /// * `api_key` - API key for authentication
    /// * `api_base` - Base URL (e.g., "https://api.openai.com/v1")
    /// * `model` - Model name (e.g., "text-embedding-3-small")
    /// * `dimensions` - Expected embedding dimensions (e.g., 1536)
    pub fn new(api_key: &str, api_base: &str, model: &str, dimensions: usize) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.to_string(),
            api_base: api_base.trim_end_matches('/').to_string(),
            model: model.to_string(),
            dimensions,
        }
    }

    /// Create from environment variables.
    ///
    /// Uses `OPENAI_API_KEY` and optionally `OPENAI_BASE_URL` (defaults to
    /// "https://api.openai.com/v1"). Model defaults to "text-embedding-3-small"
    /// with 1536 dimensions.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY not set")?;
        let api_base = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = std::env::var("DAEDALUS_EMBEDDING_MODEL")
            .unwrap_or_else(|_| "text-embedding-3-small".to_string());
        let dimensions: usize = std::env::var("DAEDALUS_EMBEDDING_DIMENSIONS")
            .unwrap_or_else(|_| "1536".to_string())
            .parse()
            .context("Invalid DAEDALUS_EMBEDDING_DIMENSIONS")?;

        Ok(Self::new(&api_key, &api_base, &model, dimensions))
    }
}

// ── OpenAI Embeddings API request/response types ──

#[derive(Serialize)]
struct EmbeddingRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    #[allow(dead_code)]
    index: usize,
}

#[async_trait]
impl Embedding for OpenAiEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed_batch(&[text]).await?;
        results
            .into_iter()
            .next()
            .context("Empty embedding response")
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let request = EmbeddingRequest {
            model: self.model.clone(),
            input: texts.iter().map(|s| s.to_string()).collect(),
        };

        let url = format!("{}/embeddings", self.api_base);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send embedding request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Embedding API error (status {}): {}",
                status,
                body
            );
        }

        let embedding_response: EmbeddingResponse = response
            .json()
            .await
            .context("Failed to parse embedding response")?;

        // Sort by index to ensure correct ordering
        let mut data = embedding_response.data;
        data.sort_by_key(|d| d.index);

        Ok(data.into_iter().map(|d| d.embedding).collect())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
