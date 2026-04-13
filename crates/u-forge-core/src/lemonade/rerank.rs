//! Cross-encoder reranking via Lemonade Server.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::client::LemonadeHttpClient;
use super::load::{load_model, ModelLoadOptions};

/// A single ranked document returned by [`LemonadeRerankProvider::rerank`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankDocument {
    /// Original zero-based index in the input `documents` slice.
    pub index: usize,
    /// Relevance score — higher is more relevant.
    pub score: f32,
    /// The original document text, if the server echoed it back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
}

/// Reranker via `POST /api/v1/reranking` on Lemonade Server.
///
/// Unlike the GPU/NPU providers there is no shared-resource contention for
/// reranking — requests are sent directly to Lemonade Server which serialises
/// them internally.
#[derive(Debug, Clone)]
pub struct LemonadeRerankProvider {
    client: LemonadeHttpClient,
    /// The reranker model id (e.g. `"bge-reranker-v2-m3-GGUF"`).
    pub model: String,
}

impl LemonadeRerankProvider {
    /// Construct with an explicit base URL and model id.
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: LemonadeHttpClient::new(base_url),
            model: model.to_string(),
        }
    }

    /// Explicitly load this model via `POST /api/v1/load` with the given options.
    ///
    /// Call this before the first [`rerank`](Self::rerank) to override server
    /// defaults — in particular `ctx_size` and batch sizes.  Without an
    /// explicit load the server may use a very small default context window
    /// (e.g. 512 tokens) that causes truncation on longer document passages.
    ///
    /// # Errors
    ///
    /// Returns an error if the server is unreachable or rejects the load request.
    pub async fn load(&self, opts: &ModelLoadOptions) -> Result<()> {
        load_model(&self.client.base_url, &self.model, opts).await
    }

    /// Rerank `documents` by relevance to `query`.
    ///
    /// # Arguments
    ///
    /// * `query`     — The search query or reference text.
    /// * `documents` — Candidate documents to score and rank.
    /// * `top_n`     — If `Some(n)`, only the top-n results are returned.
    ///   Pass `None` to return scores for every document.
    ///
    /// Results are returned **sorted by descending score** (most relevant first).
    pub async fn rerank(
        &self,
        query: &str,
        documents: Vec<String>,
        top_n: Option<usize>,
    ) -> Result<Vec<RerankDocument>> {
        let mut body = serde_json::json!({
            "model":     self.model,
            "query":     query,
            "documents": documents,
            "return_documents": true,
        });
        if let Some(n) = top_n {
            body["top_n"] = serde_json::json!(n);
        }

        let start = std::time::Instant::now();

        #[derive(Deserialize)]
        struct RerankResponseItem {
            index: usize,
            relevance_score: f32,
            #[serde(default)]
            document: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct RerankResponse {
            results: Vec<RerankResponseItem>,
        }

        let resp: RerankResponse = self
            .client
            .post_json("/reranking", &body)
            .await
            .context("Rerank HTTP request failed")?;

        let mut results: Vec<RerankDocument> = resp
            .results
            .into_iter()
            .map(|item| {
                let document = item.document.and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s),
                    serde_json::Value::Object(ref o) => {
                        o.get("text").and_then(|t| t.as_str()).map(str::to_string)
                    }
                    _ => None,
                });
                RerankDocument {
                    index: item.index,
                    score: item.relevance_score,
                    document,
                }
            })
            .collect();

        // Sort by descending relevance score.
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        debug!(
            model = %self.model,
            n_docs = results.len(),
            duration_ms = start.elapsed().as_millis(),
            "Rerank complete"
        );

        Ok(results)
    }
}
