//! Embedding trait definitions and provider types.
//!
//! The [`EmbeddingProvider`] trait and supporting types live here.
//! The [`LemonadeProvider`] implementation lives in
//! [`crate::lemonade::embedding`] and is re-exported below.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Re-exports ────────────────────────────────────────────────────────────────
pub use crate::lemonade::embedding::LemonadeProvider;

// ─────────────────────────────────────────────────────────────────────────────
// Provider type identifiers
// ─────────────────────────────────────────────────────────────────────────────

/// Identifies which backend is powering an [`EmbeddingProvider`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmbeddingProviderType {
    /// Lemonade Server HTTP provider (AMD OpenAI-compatible API).
    Lemonade,
    /// Placeholder for future Ollama integration.
    Ollama,
    /// Placeholder for future cloud API integration.
    Cloud,
}

// ─────────────────────────────────────────────────────────────────────────────
// Model info (our own type — no fastembed dependency)
// ─────────────────────────────────────────────────────────────────────────────

/// Human-readable metadata about an embedding model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingModelInfo {
    /// Model identifier as understood by the backend (e.g. `"nomic-embed-text"`).
    pub name: String,
    /// Output vector dimensionality.
    pub dimensions: usize,
    /// Free-form description.
    pub description: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// EmbeddingProvider trait
// ─────────────────────────────────────────────────────────────────────────────

/// Core trait for all embedding backends.
///
/// Implementations must be `Send + Sync` so they can be shared across async tasks.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a single text string into a dense vector.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts, returning one vector per input in the same order.
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;

    /// Number of dimensions in each output vector.
    fn dimensions(&self) -> Result<usize>;

    /// Maximum number of tokens the model can process in one call.
    fn max_tokens(&self) -> Result<usize>;

    /// Which backend powers this provider.
    fn provider_type(&self) -> EmbeddingProviderType;

    /// Optional model metadata.  Returns `None` when unavailable.
    fn model_info(&self) -> Option<EmbeddingModelInfo>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::require_integration_url;

    // ── Unit tests (no server required) ──────────────────────────────────────

    #[test]
    fn test_embedding_provider_type_serialize() {
        let t = EmbeddingProviderType::Lemonade;
        let json = serde_json::to_string(&t).unwrap();
        let back: EmbeddingProviderType = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn test_embedding_model_info_fields() {
        let info = EmbeddingModelInfo {
            name: "embed-gemma-300m-FLM".to_string(),
            dimensions: 768,
            description: Some("Test model".to_string()),
        };
        assert_eq!(info.name, "embed-gemma-300m-FLM");
        assert_eq!(info.dimensions, 768);
        assert!(info.description.is_some());
    }

    // ── Integration tests (require a running Lemonade Server) ─────────────────

    #[tokio::test]
    async fn test_lemonade_provider_connect_and_dimensions() {
        let url = require_integration_url!();
        let provider = LemonadeProvider::new(&url, "embed-gemma-300m-FLM").await;
        assert!(
            provider.is_ok(),
            "Failed to connect to Lemonade Server: {:?}",
            provider.err()
        );
        let provider = provider.unwrap();
        let dims = provider.dimensions().unwrap();
        assert!(dims > 0, "Expected non-zero dimensions, got {dims}");
        assert_eq!(provider.provider_type(), EmbeddingProviderType::Lemonade);
        let info = provider.model_info().unwrap();
        assert_eq!(info.name, "embed-gemma-300m-FLM");
        assert_eq!(info.dimensions, dims);
    }

    #[tokio::test]
    async fn test_lemonade_embed_batch() {
        let url = require_integration_url!();
        let provider = LemonadeProvider::new(&url, "embed-gemma-300m-FLM")
            .await
            .expect("Connect to Lemonade");
        let dims = provider.dimensions().unwrap();

        let texts = vec![
            "First sentence.".to_string(),
            "Second sentence, a bit longer.".to_string(),
            "Third.".to_string(),
        ];
        let embeddings = provider.embed_batch(texts.clone()).await;
        assert!(
            embeddings.is_ok(),
            "embed_batch() failed: {:?}",
            embeddings.err()
        );
        let embeddings = embeddings.unwrap();
        assert_eq!(embeddings.len(), texts.len(), "Wrong number of embeddings");
        for emb in &embeddings {
            assert_eq!(emb.len(), dims, "Dimension mismatch in batch");
            assert!(
                emb.iter().all(|&x| x.is_finite()),
                "Non-finite value in batch embedding"
            );
        }
    }
}
