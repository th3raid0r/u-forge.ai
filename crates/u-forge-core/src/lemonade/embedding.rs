//! Lemonade-backed embedding provider.
//!
//! This module contains the [`LemonadeProvider`] implementation of
//! [`EmbeddingProvider`].  The trait definitions live in
//! [`crate::ai::embeddings`] and are dependency-free; this module handles
//! all Lemonade-specific HTTP logic.

use anyhow::{anyhow, Result};
use async_openai::{Client, config::OpenAIConfig};
use async_openai::types::embeddings::{CreateEmbeddingRequest, EmbeddingInput};
use async_trait::async_trait;
use tracing::info;

use crate::ai::embeddings::{
    EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType,
};

use super::client::make_lemonade_openai_client;

// ── LemonadeProvider ──────────────────────────────────────────────────────────

/// Embedding provider backed by [Lemonade Server](https://github.com/lemonade-sdk/lemonade).
///
/// Uses the OpenAI-compatible `POST /api/v1/embeddings` endpoint.  The server
/// must be running before this provider is constructed; `new` probes the
/// dimensions by sending a single dummy request.
///
/// This provider is fully async — no Tokio threads are ever blocked (fixes BUG-5).
pub struct LemonadeProvider {
    client: Client<OpenAIConfig>,
    /// Model identifier, e.g. `"nomic-embed-text"`.
    pub model: String,
    /// Human-readable base URL, stored for model_info display only.
    pub base_url: String,
    /// Probed on construction.
    dimensions: usize,
}

impl LemonadeProvider {
    /// Connect to a Lemonade Server instance and probe the embedding dimensions.
    ///
    /// Does **not** call `/api/v1/load` first — the model must already be loaded
    /// (or auto-loaded by the server on first request).  Use
    /// [`new_with_load`](Self::new_with_load) to explicitly load the model with
    /// custom options such as a larger context window.
    ///
    /// # Errors
    /// Returns an error if the server is unreachable or the model is not loaded.
    pub async fn new(base_url: &str, model: &str) -> Result<Self> {
        let client = make_lemonade_openai_client(base_url);

        let probe_req = CreateEmbeddingRequest {
            model: model.to_string(),
            input: EmbeddingInput::StringArray(vec!["dimension probe".to_string()]),
            encoding_format: None,
            dimensions: None,
            user: None,
        };

        let probe_resp = client
            .embeddings()
            .create(probe_req)
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to connect to Lemonade Server at {}: {}",
                    base_url,
                    e
                )
            })?;

        let dimensions = probe_resp
            .data
            .first()
            .map(|e| e.embedding.len())
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
            base_url: base_url.to_string(),
            dimensions,
        })
    }

    /// Explicitly load `model` via `POST /api/v1/load` with the given options,
    /// then connect and probe embedding dimensions.
    pub async fn new_with_load(
        base_url: &str,
        model: &str,
        load_opts: &crate::lemonade::ModelLoadOptions,
    ) -> Result<Self> {
        crate::lemonade::load_model(base_url, model, load_opts)
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to load model '{}' before connecting to Lemonade Server: {}",
                    model,
                    e
                )
            })?;
        Self::new(base_url, model).await
    }
}

#[async_trait]
impl EmbeddingProvider for LemonadeProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let req = CreateEmbeddingRequest {
            model: self.model.clone(),
            input: EmbeddingInput::StringArray(vec![text.to_string()]),
            encoding_format: None,
            dimensions: None,
            user: None,
        };

        let resp = self.client.embeddings().create(req).await?;

        resp.data
            .into_iter()
            .next()
            .map(|e| e.embedding)
            .ok_or_else(|| anyhow!("Lemonade Server returned no embedding in response"))
    }

    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let req = CreateEmbeddingRequest {
            model: self.model.clone(),
            input: EmbeddingInput::StringArray(texts.clone()),
            encoding_format: None,
            dimensions: None,
            user: None,
        };

        let resp = self.client.embeddings().create(req).await?;

        if resp.data.len() != texts.len() {
            tracing::debug!(
                expected = texts.len(),
                got = resp.data.len(),
                model = %self.model,
                "Batch size mismatch — falling back to sequential single-embed calls"
            );
            let mut results = Vec::with_capacity(texts.len());
            for text in &texts {
                results.push(self.embed(text).await?);
            }
            return Ok(results);
        }

        let mut embeddings: Vec<(usize, Vec<f32>)> = resp
            .data
            .into_iter()
            .map(|e| (e.index as usize, e.embedding))
            .collect();
        embeddings.sort_unstable_by_key(|(idx, _)| *idx);
        Ok(embeddings.into_iter().map(|(_, v)| v).collect())
    }

    fn dimensions(&self) -> Result<usize> {
        Ok(self.dimensions)
    }

    fn max_tokens(&self) -> Result<usize> {
        Ok(crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS)
    }

    fn provider_type(&self) -> EmbeddingProviderType {
        EmbeddingProviderType::Lemonade
    }

    fn model_info(&self) -> Option<EmbeddingModelInfo> {
        Some(EmbeddingModelInfo {
            name: self.model.clone(),
            dimensions: self.dimensions,
            description: Some(format!("Lemonade Server model at {}", self.base_url)),
        })
    }
}

