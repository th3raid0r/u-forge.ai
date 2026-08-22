//! Cross-encoder reranking via Lemonade Server.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

use std::sync::Arc;

use super::client::{LemonadeConnection, LemonadeHttpClient};
use super::load::{ModelLoadOptions, load_model_with_connection};

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

/// Provider abstraction for document reranking.
///
/// The production implementation calls Lemonade Server, while tests can inject
/// deterministic in-memory providers through the normal [`InferenceQueue`]
/// worker path.
#[async_trait]
pub trait RerankProvider: Send + Sync {
    /// Rank `documents` by relevance to `query`.
    async fn rerank(
        &self,
        query: &str,
        documents: Vec<String>,
        top_n: Option<usize>,
    ) -> Result<Vec<RerankDocument>>;
}

/// Reranker via `POST /api/v1/rerank` on Lemonade Server.
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

    pub fn from_connection(connection: Arc<LemonadeConnection>, model: &str) -> Self {
        Self {
            client: LemonadeHttpClient::from_connection(connection),
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
    /// Pass `already_loaded` (from the server catalog) to skip the round-trip
    /// when the model is already running.
    ///
    /// # Errors
    ///
    /// Returns an error if the server is unreachable or rejects the load request.
    pub async fn load(&self, opts: &ModelLoadOptions, already_loaded: &[String]) -> Result<()> {
        load_model_with_connection(
            self.client.connection().clone(),
            &self.model,
            opts,
            already_loaded,
        )
        .await
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
            .post_json("/rerank", &body)
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

#[async_trait]
impl RerankProvider for LemonadeRerankProvider {
    async fn rerank(
        &self,
        query: &str,
        documents: Vec<String>,
        top_n: Option<usize>,
    ) -> Result<Vec<RerankDocument>> {
        LemonadeRerankProvider::rerank(self, query, documents, top_n).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn rerank_uses_the_canonical_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let body = r#"{"results":[]}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            String::from_utf8(request).unwrap()
        });

        let provider = LemonadeRerankProvider::new(&format!("http://{address}/v1"), "reranker");
        assert!(
            provider
                .rerank("query", vec!["document".into()], None)
                .await
                .is_ok()
        );
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /v1/rerank HTTP/1.1\r\n"));
    }
}
