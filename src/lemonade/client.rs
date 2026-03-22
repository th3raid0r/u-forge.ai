//! HTTP client wrapper for Lemonade Server API calls.
//!
//! Centralises request construction, Bearer token auth, error status checking,
//! and response deserialisation for all communication with the Lemonade Server
//! OpenAI-compatible API.
//!
//! # Exception
//!
//! [`SystemInfo::fetch`](super::SystemInfo::fetch) does **not** use
//! this client because the `/system-info` management endpoint is intentionally
//! accessed without the Bearer token.

use anyhow::{Context, Result};
use reqwest::multipart;
use serde::{de::DeserializeOwned, Serialize};

/// A thin wrapper around [`reqwest::Client`] pre-configured for Lemonade Server.
///
/// All methods automatically inject the `Authorization: Bearer lemonade` header
/// and call `error_for_status()` before deserialising the response body.
///
/// # URL construction
///
/// `base_url` is stored with any trailing slash removed.  Methods accept paths
/// with or without a leading slash — both `"/models"` and `"models"` work.
#[derive(Debug, Clone)]
pub struct LemonadeHttpClient {
    client: reqwest::Client,
    /// Lemonade Server API base URL with no trailing slash,
    /// e.g. `"http://localhost:8000/api/v1"`.
    pub base_url: String,
}

impl LemonadeHttpClient {
    /// Construct a new client targeting `base_url`.
    ///
    /// Trailing slashes are stripped from `base_url` automatically.
    pub fn new(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    /// `GET {base_url}/{path}` with the Bearer token, returning a typed JSON response.
    pub async fn get_json<Resp: DeserializeOwned>(&self, path: &str) -> Result<Resp> {
        let url = self.url(path);
        self.client
            .get(&url)
            .header("Authorization", "Bearer lemonade")
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?
            .error_for_status()
            .with_context(|| format!("GET {url} returned an error status"))?
            .json()
            .await
            .with_context(|| format!("Failed to parse JSON response from GET {url}"))
    }

    /// `POST {base_url}/{path}` with a JSON body and Bearer token, returning a typed JSON response.
    pub async fn post_json<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
    ) -> Result<Resp> {
        let url = self.url(path);
        self.client
            .post(&url)
            .header("Authorization", "Bearer lemonade")
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?
            .error_for_status()
            .with_context(|| format!("POST {url} returned an error status"))?
            .json()
            .await
            .with_context(|| format!("Failed to parse JSON response from POST {url}"))
    }

    /// `POST {base_url}/{path}` with a multipart form body, returning a typed JSON response.
    pub async fn post_multipart<Resp: DeserializeOwned>(
        &self,
        path: &str,
        form: multipart::Form,
    ) -> Result<Resp> {
        let url = self.url(path);
        self.client
            .post(&url)
            .header("Authorization", "Bearer lemonade")
            .multipart(form)
            .send()
            .await
            .with_context(|| format!("POST {url} (multipart) failed"))?
            .error_for_status()
            .with_context(|| format!("POST {url} returned an error status"))?
            .json()
            .await
            .with_context(|| format!("Failed to parse JSON response from POST {url}"))
    }

    /// `POST {base_url}/{path}` with a JSON body, returning raw response bytes.
    ///
    /// Used for endpoints that return binary data such as TTS audio.
    pub async fn post_bytes<Req: Serialize>(&self, path: &str, body: &Req) -> Result<Vec<u8>> {
        let url = self.url(path);
        let bytes = self
            .client
            .post(&url)
            .header("Authorization", "Bearer lemonade")
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?
            .error_for_status()
            .with_context(|| format!("POST {url} returned an error status"))?
            .bytes()
            .await
            .with_context(|| format!("Failed to read response bytes from POST {url}"))?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_joins_path_with_leading_slash() {
        let client = LemonadeHttpClient::new("http://localhost:8000/api/v1");
        assert_eq!(client.url("/models"), "http://localhost:8000/api/v1/models");
    }

    #[test]
    fn test_url_joins_path_without_leading_slash() {
        let client = LemonadeHttpClient::new("http://localhost:8000/api/v1");
        assert_eq!(client.url("models"), "http://localhost:8000/api/v1/models");
    }

    #[test]
    fn test_base_url_strips_trailing_slash() {
        let client = LemonadeHttpClient::new("http://localhost:8000/api/v1/");
        assert!(!client.base_url.ends_with('/'));
        assert_eq!(client.base_url, "http://localhost:8000/api/v1");
    }
}
