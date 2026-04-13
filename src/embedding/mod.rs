mod openai;

#[allow(unused_imports)]
pub use openai::OpenAiEmbedding;

use anyhow::Result;
use async_trait::async_trait;

/// Abstract trait for text embedding providers.
///
/// An embedding provider converts text into dense vector representations
/// (embeddings) that capture semantic meaning. These vectors can then be
/// compared using cosine similarity for semantic search and retrieval.
///
/// This trait decouples the memory system from any specific embedding API,
/// allowing easy swapping between OpenAI, local models, etc.
#[allow(dead_code)]
#[async_trait]
pub trait Embedding: Send + Sync {
    /// Generate an embedding vector for a single text input.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Generate embedding vectors for multiple text inputs in a single batch.
    ///
    /// Default implementation calls `embed` sequentially. Providers that
    /// support batch APIs should override this for better performance.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    /// Return the dimensionality of the embedding vectors produced.
    fn dimensions(&self) -> usize;

    /// Return the model name used for embedding.
    fn model_name(&self) -> &str;
}

/// Compute cosine similarity between two vectors.
///
/// Returns a value in [-1, 1] where 1 means identical direction,
/// 0 means orthogonal, and -1 means opposite direction.
///
/// # Panics
/// Panics if the vectors have different lengths.
#[allow(dead_code)]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vectors must have the same length");

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_general() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let sim = cosine_similarity(&a, &b);
        // Expected: (4+10+18) / (sqrt(14) * sqrt(77)) ≈ 0.9746
        assert!((sim - 0.9746).abs() < 0.001);
    }
}