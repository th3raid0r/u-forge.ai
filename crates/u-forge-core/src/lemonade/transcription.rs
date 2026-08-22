//! Lemonade-backed transcription provider (no GPU lock).
//!
//! This module contains the [`LemonadeTranscriptionProvider`] implementation of
//! [`TranscriptionProvider`].  The trait definitions live in
//! [`crate::ai::transcription`] and are dependency-free; this module handles
//! all Lemonade-specific HTTP logic.
//!
//! For GPU-locked STT with resource contention management, see
//! [`LemonadeSttProvider`](super::stt::LemonadeSttProvider).

use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;

use super::client::{LemonadeConnection, LemonadeHttpClient};
use crate::ai::transcription::{TranscriptionProvider, mime_for_filename};

// ── LemonadeTranscriptionProvider ─────────────────────────────────────────────

/// Transcription provider backed by
/// [Lemonade Server](https://github.com/lemonade-sdk/lemonade).
///
/// Uses the OpenAI-compatible `POST /api/v1/audio/transcriptions` endpoint with
/// a `multipart/form-data` body.  The server must be running and the whisper
/// model must be pulled before use.
///
/// This provider is fully async — no Tokio threads are ever blocked.
///
/// Unlike [`LemonadeSttProvider`](crate::lemonade::LemonadeSttProvider), this
/// provider has **no** [`GpuResourceManager`](crate::lemonade::GpuResourceManager)
/// attached — it is intentionally simple and stateless.  Use it when:
///
/// * The model runs on the **NPU** (dedicated silicon, no GPU contention), or
/// * You are managing resource exclusion at a higher level (e.g. the
///   [`InferenceQueue`](crate::queue::InferenceQueue)).
pub struct LemonadeTranscriptionProvider {
    pub(crate) client: LemonadeHttpClient,
    /// Whisper model identifier, e.g. `"whisper-v3-turbo-FLM"`.
    model: String,
}

impl LemonadeTranscriptionProvider {
    /// Create a new provider pointed at the given Lemonade Server.
    ///
    /// Construction is cheap and **synchronous** — no probe request is made.
    /// Errors are only surfaced when [`transcribe`](TranscriptionProvider::transcribe)
    /// is called.
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
}

#[async_trait]
impl TranscriptionProvider for LemonadeTranscriptionProvider {
    async fn transcribe(&self, audio_bytes: Vec<u8>, filename: &str) -> Result<String> {
        use tracing::debug;
        let start = std::time::Instant::now();

        let mime = mime_for_filename(filename);

        let part = reqwest::multipart::Part::bytes(audio_bytes)
            .file_name(filename.to_string())
            .mime_str(&mime)
            .map_err(|e| anyhow!("Invalid MIME type '{}': {}", mime, e))?;

        let form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "json")
            .part("file", part);

        let resp: serde_json::Value = self
            .client
            .post_multipart("/audio/transcriptions", form)
            .await
            .map_err(|e| anyhow!("Lemonade transcription request failed: {}", e))?;

        // Surface server-side errors as Rust errors.
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("Lemonade transcription error: {}", err));
        }

        let text = resp["text"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing 'text' field in transcription response: {}", resp))?
            .trim()
            .to_string();

        debug!(
            model    = %self.model,
            filename,
            text_len = text.len(),
            duration_ms = start.elapsed().as_millis(),
            "Transcription completed"
        );

        Ok(text)
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn transcription_multipart_explicitly_asks_for_json() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2048];
            let header_end = loop {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0, "client closed before sending request headers");
                request.extend_from_slice(&buffer[..read]);
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap();
            while request.len() < header_end + content_length {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0, "client closed before sending request body");
                request.extend_from_slice(&buffer[..read]);
            }
            let body = r#"{"text":"transcript"}"#;
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

        let provider =
            LemonadeTranscriptionProvider::new(&format!("http://{address}/v1"), "whisper");
        let text = provider
            .transcribe(vec![1, 2, 3], "clip.wav")
            .await
            .unwrap();
        assert_eq!(text, "transcript");

        let request = server.await.unwrap();
        assert!(request.starts_with("POST /v1/audio/transcriptions HTTP/1.1\r\n"));
        assert!(request.contains("name=\"response_format\""));
        assert!(request.contains("\r\n\r\njson\r\n"));
    }
}
