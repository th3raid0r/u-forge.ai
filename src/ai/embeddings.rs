//! embeddings.rs - Text embedding providers for u-forge.ai.
//!
//! All embeddings are generated via Lemonade Server's OpenAI-compatible HTTP API.
//! FastEmbed and ONNX Runtime have been removed — there are no in-process embedding
//! models and no C++ compilation requirements.
//!
//! Transcription (speech-to-text / VTT) has been moved to
//! [`crate::transcription`].  The types are re-exported here for backward
//! compatibility.
//!
//! # Quick start
//!
//! ```no_run
//! # use u_forge_ai::ai::embeddings::EmbeddingManager;
//! // Auto: reads LEMONADE_URL env var, connects to Lemonade Server
//! # async fn example() {
//! let mgr = EmbeddingManager::try_new_auto(None, None).await.unwrap();
//! let embedding = mgr.get_provider().embed("Hello, world!").await.unwrap();
//! println!("dims: {}", embedding.len());
//! # }
//! ```

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use crate::lemonade::LemonadeHttpClient;

// ── Backward-compat re-exports ────────────────────────────────────────────────
// Transcription types moved to `crate::transcription`.  Re-exported here so
// existing code using `embeddings::TranscriptionProvider` continues to compile
// without changes.
pub use super::transcription::{
    LemonadeTranscriptionProvider, TranscriptionManager, TranscriptionProvider,
};

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
// LemonadeProvider
// ─────────────────────────────────────────────────────────────────────────────
/// Embedding provider backed by [Lemonade Server](https://github.com/lemonade-sdk/lemonade).
///
/// Uses the OpenAI-compatible `POST /api/v1/embeddings` endpoint.  The server
/// must be running before this provider is constructed; `new` probes the
/// dimensions by sending a single dummy request.
///
/// This provider is fully async — no Tokio threads are ever blocked (fixes BUG-5).
pub struct LemonadeProvider {
    client: LemonadeHttpClient,
    /// Model identifier, e.g. `"nomic-embed-text"`.
    model: String,
    /// Probed on construction.
    dimensions: usize,
}

impl LemonadeProvider {
    /// Connect to a Lemonade Server instance and probe the embedding dimensions.
    ///
    /// # Errors
    /// Returns an error if the server is unreachable or the model is not loaded.
    pub async fn new(base_url: &str, model: &str) -> Result<Self> {
        let client = LemonadeHttpClient::new(base_url);

        let resp: serde_json::Value = client
            .post_json(
                "/embeddings",
                &serde_json::json!({
                    "model": model,
                    "input": ["dimension probe"]
                }),
            )
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to connect to Lemonade Server at {}: {}",
                    base_url,
                    e
                )
            })?;

        let dimensions = resp["data"][0]["embedding"]
            .as_array()
            .map(|a| a.len())
            .ok_or_else(|| {
                anyhow!(
                    "Failed to probe embedding dimensions from Lemonade Server — \
                     check that model '{}' is loaded (run: lemonade-server pull {})",
                    model,
                    model
                )
            })?;

        info!(base_url, model, dimensions, "LemonadeProvider connected");

        Ok(Self {
            client,
            model: model.to_string(),
            dimensions,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for LemonadeProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let resp: serde_json::Value = self
            .client
            .post_json(
                "/embeddings",
                &serde_json::json!({
                    "model": self.model,
                    "input": [text]
                }),
            )
            .await?;

        let embedding: Vec<f32> = resp["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| {
                anyhow!(
                    "Lemonade Server returned no embedding in response (raw: {})",
                    resp
                )
            })?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
            .collect();

        Ok(embedding)
    }

    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let resp: serde_json::Value = self
            .client
            .post_json(
                "/embeddings",
                &serde_json::json!({
                    "model": self.model,
                    "input": texts
                }),
            )
            .await?;

        let data = resp["data"]
            .as_array()
            .ok_or_else(|| anyhow!("Lemonade Server returned no 'data' array in batch response"))?;

        // Some backends (e.g. FLM/NPU recipe) do not support true multi-input
        // batching and silently return only the first result.  Detect the mismatch
        // and fall back to sequential single-item calls so callers always receive
        // exactly one embedding per input regardless of backend capability.
        if data.len() != texts.len() {
            tracing::debug!(
                expected = texts.len(),
                got = data.len(),
                model = %self.model,
                "Batch size mismatch — falling back to sequential single-embed calls"
            );
            let mut results = Vec::with_capacity(texts.len());
            for text in &texts {
                results.push(self.embed(text).await?);
            }
            return Ok(results);
        }

        data.iter()
            .map(|item| {
                item["embedding"]
                    .as_array()
                    .ok_or_else(|| anyhow!("Missing 'embedding' field in Lemonade batch item"))
                    .map(|arr| {
                        arr.iter()
                            .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                            .collect()
                    })
            })
            .collect()
    }

    fn dimensions(&self) -> Result<usize> {
        Ok(self.dimensions)
    }

    fn max_tokens(&self) -> Result<usize> {
        // nomic-embed-text and most Lemonade models support 8 K context.
        Ok(8192)
    }

    fn provider_type(&self) -> EmbeddingProviderType {
        EmbeddingProviderType::Lemonade
    }

    fn model_info(&self) -> Option<EmbeddingModelInfo> {
        Some(EmbeddingModelInfo {
            name: self.model.clone(),
            dimensions: self.dimensions,
            description: Some(format!("Lemonade Server model at {}", self.client.base_url)),
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
// EmbeddingManager
// ─────────────────────────────────────────────────────────────────────────────

/// Owns a single [`EmbeddingProvider`] and hands out `Arc` references to it.
///
/// Construct via [`EmbeddingManager::try_new_auto`] for production use, or
/// [`EmbeddingManager::try_new_lemonade`] when the URL is known.
pub struct EmbeddingManager {
    provider: Arc<dyn EmbeddingProvider>,
}

impl std::fmt::Debug for EmbeddingManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingManager")
            .field("provider_type", &self.provider.provider_type())
            .field("dimensions", &self.provider.dimensions().ok())
            .finish()
    }
}

impl EmbeddingManager {
    /// Connect directly to a Lemonade Server instance.
    pub async fn try_new_lemonade(base_url: &str, model: &str) -> Result<Self> {
        let provider = LemonadeProvider::new(base_url, model).await?;
        info!(base_url, model, "EmbeddingManager: using Lemonade Server");
        Ok(Self {
            provider: Arc::new(provider),
        })
    }

    /// Auto-select a provider.
    ///
    /// Resolution order:
    /// 1. `lemonade_url` argument (if provided)
    /// 2. `LEMONADE_URL` environment variable
    /// 3. Localhost probe — `http://localhost:8000` then `http://127.0.0.1:8000`
    ///    via [`crate::lemonade::resolve_lemonade_url`]
    /// 4. Hard error — no server could be found
    ///
    /// `model` defaults to `"embed-gemma-300m-FLM"` when `None`.
    pub async fn try_new_auto(lemonade_url: Option<&str>, model: Option<&str>) -> Result<Self> {
        let resolved_url =
            crate::lemonade::resolve_provider_url(lemonade_url, "LEMONADE_URL", true).await;

        match resolved_url {
            Some(url) => {
                let lemonade_model = model.unwrap_or("embed-gemma-300m-FLM");
                match Self::try_new_lemonade(&url, lemonade_model).await {
                    Ok(mgr) => {
                        info!(url, "Auto-selected Lemonade Server");
                        Ok(mgr)
                    }
                    Err(e) => {
                        warn!(url, error = %e, "Lemonade Server not available");
                        Err(anyhow!(
                            "Lemonade Server not available at {} ({}). \
                             Ensure lemonade-server is running and the model is pulled:\n  \
                             lemonade-server serve\n  \
                             lemonade-server pull {}",
                            url,
                            e,
                            lemonade_model
                        ))
                    }
                }
            }
            None => Err(anyhow!(
                "No Lemonade Server URL configured and none found on localhost. \
                 Start lemonade-server or set the LEMONADE_URL environment variable:\n  \
                 lemonade-server serve\n  \
                 export LEMONADE_URL=http://localhost:8000/api/v1"
            )),
        }
    }

    /// Return a clone of the inner provider, suitable for passing to async tasks.
    pub fn get_provider(&self) -> Arc<dyn EmbeddingProvider> {
        self.provider.clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::lemonade_url;

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

    #[tokio::test]
    async fn test_try_new_auto_fails_without_url() {
        // Skip if any Lemonade Server is reachable (localhost probe or env var).
        if lemonade_url().await.is_some() {
            eprintln!("Skipping test_try_new_auto_fails_without_url — server is reachable");
            return;
        }
        let result = EmbeddingManager::try_new_auto(None, None).await;
        assert!(
            result.is_err(),
            "Expected error when no URL is configured, got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LEMONADE_URL"),
            "Error message should mention LEMONADE_URL, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_try_new_lemonade_unreachable() {
        let result = EmbeddingManager::try_new_lemonade(
            "http://127.0.0.1:19999/api/v1",
            "embed-gemma-300m-FLM",
        )
        .await;
        assert!(
            result.is_err(),
            "Expected connection error for unreachable server"
        );
    }

    // ── Integration tests (require a running Lemonade Server) ─────────────────

    #[tokio::test]
    async fn test_lemonade_provider_connect_and_dimensions() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable");
            return;
        };
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
    async fn test_lemonade_embed_single() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable");
            return;
        };
        let provider = LemonadeProvider::new(&url, "embed-gemma-300m-FLM")
            .await
            .expect("Connect to Lemonade");
        let dims = provider.dimensions().unwrap();

        let embedding = provider.embed("The quick brown fox").await;
        assert!(embedding.is_ok(), "embed() failed: {:?}", embedding.err());
        let embedding = embedding.unwrap();
        assert_eq!(embedding.len(), dims, "Dimension mismatch");
        assert!(
            embedding.iter().all(|&x| x.is_finite()),
            "Embedding contains non-finite values"
        );
    }

    #[tokio::test]
    async fn test_lemonade_embed_batch() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable");
            return;
        };
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

    #[tokio::test]
    async fn test_embedding_manager_try_new_auto() {
        let Some(url) = lemonade_url().await else {
            eprintln!("Skipping: no Lemonade Server reachable");
            return;
        };
        let mgr = EmbeddingManager::try_new_auto(Some(&url), None).await;
        assert!(mgr.is_ok(), "try_new_auto failed: {:?}", mgr.err());
        let provider = mgr.unwrap().get_provider();
        assert_eq!(provider.provider_type(), EmbeddingProviderType::Lemonade);
        assert!(provider.dimensions().unwrap() > 0);
    }
}
