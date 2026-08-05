//! Shared connection and HTTP policy for Lemonade Server.
//!
//! Every Lemonade transport starts from [`LemonadeConnection`].  This keeps URL
//! normalization, optional credentials, redaction, and timeout classes
//! consistent across the custom management plane and OpenAI-compatible clients.

use std::{fmt, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_openai::{Client, config::OpenAIConfig};
use futures::StreamExt;
use reqwest::{RequestBuilder, multipart};
use serde::{Serialize, de::DeserializeOwned};

const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
const KEYLESS_SDK_PLACEHOLDER: &str = "lemonade";

/// Independent timeout classes for the Lemonade protocol phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LemonadeTimeouts {
    pub connect: Duration,
    pub metadata: Duration,
    pub readiness_load: Duration,
    pub first_token: Duration,
    pub stream_idle: Duration,
    pub non_stream_completion: Duration,
}

impl Default for LemonadeTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            metadata: Duration::from_secs(30),
            readiness_load: Duration::from_secs(300),
            first_token: Duration::from_secs(120),
            stream_idle: Duration::from_secs(60),
            non_stream_completion: Duration::from_secs(300),
        }
    }
}

/// Whether u-forge owns the Lemonade process behind a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LemonadeOwnership {
    Embedded,
    External,
}

/// A string whose debug representation never reveals its contents.
#[derive(Clone, PartialEq, Eq)]
pub struct LemonadeSecret(Arc<str>);

impl LemonadeSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Arc::from(value.into()))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LemonadeSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Clone)]
struct LemonadeClients {
    metadata: reqwest::Client,
    load: reqwest::Client,
    completion: reqwest::Client,
    stream: reqwest::Client,
}

impl fmt::Debug for LemonadeClients {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LemonadeClients { .. }")
    }
}

impl LemonadeClients {
    fn build(timeouts: LemonadeTimeouts) -> Result<Self> {
        let client = |timeout: Option<Duration>| -> Result<reqwest::Client> {
            let mut builder = reqwest::Client::builder().connect_timeout(timeouts.connect);
            if let Some(timeout) = timeout {
                builder = builder.timeout(timeout);
            }
            builder
                .build()
                .context("failed to build Lemonade HTTP client")
        };

        Ok(Self {
            metadata: client(Some(timeouts.metadata))?,
            load: client(Some(timeouts.readiness_load))?,
            completion: client(Some(timeouts.non_stream_completion))?,
            // Streaming has first-token and idle deadlines at the protocol
            // adapter. A reqwest total timeout would incorrectly cap long output.
            stream: client(None)?,
        })
    }
}

/// Normalized Lemonade endpoint, credentials, ownership, and HTTP policy.
#[derive(Clone)]
pub struct LemonadeConnection {
    origin: String,
    api_base: String,
    ownership: LemonadeOwnership,
    api_key: Option<LemonadeSecret>,
    admin_api_key: Option<LemonadeSecret>,
    timeouts: LemonadeTimeouts,
    clients: LemonadeClients,
    llm_runtime_gate: Arc<tokio::sync::Mutex<()>>,
}

impl fmt::Debug for LemonadeConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LemonadeConnection")
            .field("origin", &self.origin)
            .field("api_base", &self.api_base)
            .field("ownership", &self.ownership)
            .field("api_key", &self.api_key)
            .field("admin_api_key", &self.admin_api_key)
            .field("timeouts", &self.timeouts)
            .finish_non_exhaustive()
    }
}

impl LemonadeConnection {
    /// Build an external connection, using environment credentials when set.
    pub fn external(url: &str) -> Result<Self> {
        Self::with_credentials(
            url,
            LemonadeOwnership::External,
            std::env::var("LEMONADE_API_KEY").ok(),
            std::env::var("LEMONADE_ADMIN_API_KEY").ok(),
            LemonadeTimeouts::default(),
        )
    }

    /// Build a connection with explicit credentials.
    pub fn with_credentials(
        url: &str,
        ownership: LemonadeOwnership,
        api_key: Option<String>,
        admin_api_key: Option<String>,
        timeouts: LemonadeTimeouts,
    ) -> Result<Self> {
        let (origin, api_base) = normalize_url(url)?;
        Ok(Self {
            origin,
            api_base,
            ownership,
            api_key: nonempty_secret(api_key),
            admin_api_key: nonempty_secret(admin_api_key),
            timeouts,
            clients: LemonadeClients::build(timeouts)?,
            llm_runtime_gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    pub fn ownership(&self) -> LemonadeOwnership {
        self.ownership
    }

    pub fn timeouts(&self) -> LemonadeTimeouts {
        self.timeouts
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn has_admin_api_key(&self) -> bool {
        self.admin_api_key.is_some()
    }

    /// Credential for SDK adapters that cannot accept this connection type.
    /// Callers must never log or persist the returned value.
    pub fn api_credential(&self) -> Option<&str> {
        self.api_key()
    }

    /// Completion-class HTTP client shared with SDK adapters.
    pub fn completion_http_client(&self) -> reqwest::Client {
        self.clients.completion.clone()
    }

    pub(crate) fn api_key(&self) -> Option<&str> {
        self.api_key.as_ref().map(LemonadeSecret::expose)
    }

    pub(crate) fn admin_api_key(&self) -> Option<&str> {
        self.admin_api_key.as_ref().map(LemonadeSecret::expose)
    }

    pub(crate) fn llm_runtime_gate(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.llm_runtime_gate.clone()
    }

    pub(crate) fn api_url(&self, path: &str) -> String {
        join_url(&self.api_base, path)
    }

    pub(crate) fn origin_url(&self, path: &str) -> String {
        join_url(&self.origin, path)
    }

    pub(crate) fn internal_url(&self, path: &str) -> String {
        join_url(
            &self.origin,
            &format!("internal/{}", path.trim_start_matches('/')),
        )
    }
}

fn nonempty_secret(value: Option<String>) -> Option<LemonadeSecret> {
    value
        .filter(|value| !value.is_empty())
        .map(LemonadeSecret::new)
}

fn normalize_url(input: &str) -> Result<(String, String)> {
    let mut url = reqwest::Url::parse(input).context("invalid Lemonade URL")?;
    if url.cannot_be_a_base() || url.host_str().is_none() {
        return Err(anyhow!("Lemonade URL must include an HTTP(S) host"));
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(anyhow!("Lemonade URL must use http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow!(
            "Lemonade URL must not contain credentials; use the API key environment variables"
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(anyhow!("Lemonade URL must not include a query or fragment"));
    }

    let path = url.path().trim_end_matches('/').to_string();
    let api_path = match path.as_str() {
        "" => "/v1".to_string(),
        "/v1" | "/v0" | "/api/v1" | "/api/v0" => path,
        _ => {
            return Err(anyhow!(
                "Lemonade URL path must be empty or a supported API prefix"
            ));
        }
    };

    url.set_path("");
    let origin = url.as_str().trim_end_matches('/').to_string();
    let api_base = format!("{origin}{api_path}");
    Ok((origin, api_base))
}

fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Custom HTTP adapter for Lemonade management and compatibility deviations.
#[derive(Debug, Clone)]
pub struct LemonadeHttpClient {
    connection: Arc<LemonadeConnection>,
    /// Compatibility field for existing providers. New code should use
    /// [`LemonadeHttpClient::connection`].
    pub base_url: String,
}

impl LemonadeHttpClient {
    /// Construct from a URL and optional environment credentials.
    pub fn new(base_url: &str) -> Self {
        let connection = LemonadeConnection::external(base_url).unwrap_or_else(|error| {
            tracing::warn!(%error, "invalid Lemonade URL; HTTP calls will fail");
            // Preserve the old constructor's infallible behavior for provider
            // factories. The fallback URL is deliberately unreachable.
            LemonadeConnection::with_credentials(
                "http://127.0.0.1:1/v1",
                LemonadeOwnership::External,
                None,
                None,
                LemonadeTimeouts::default(),
            )
            .expect("static fallback Lemonade URL is valid")
        });
        Self::from_connection(Arc::new(connection))
    }

    pub fn from_connection(connection: Arc<LemonadeConnection>) -> Self {
        Self {
            base_url: connection.api_base().to_string(),
            connection,
        }
    }

    pub fn connection(&self) -> &Arc<LemonadeConnection> {
        &self.connection
    }

    fn api_request(&self, request: RequestBuilder) -> RequestBuilder {
        add_bearer(request, self.connection.api_key())
    }

    fn admin_request(&self, request: RequestBuilder) -> RequestBuilder {
        add_bearer(request, self.connection.admin_api_key())
    }

    pub async fn get_json<Resp: DeserializeOwned>(&self, path: &str) -> Result<Resp> {
        let url = self.connection.api_url(path);
        let response = self
            .api_request(self.connection.clients.metadata.get(&url))
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?;
        parse_json_response(response, "GET", &url).await
    }

    pub async fn get_origin_json<Resp: DeserializeOwned>(&self, path: &str) -> Result<Resp> {
        let url = self.connection.origin_url(path);
        let response = self
            .api_request(self.connection.clients.metadata.get(&url))
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?;
        parse_json_response(response, "GET", &url).await
    }

    pub async fn get_admin_json<Resp: DeserializeOwned>(&self, path: &str) -> Result<Resp> {
        let url = self.connection.internal_url(path);
        let response = self
            .admin_request(self.connection.clients.metadata.get(&url))
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?;
        parse_json_response(response, "GET", &url).await
    }

    pub async fn post_json<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
    ) -> Result<Resp> {
        self.post_json_with_client(path, body, &self.connection.clients.completion)
            .await
    }

    pub async fn post_json_load<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
    ) -> Result<Resp> {
        self.post_json_with_client(path, body, &self.connection.clients.load)
            .await
    }

    async fn post_json_with_client<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
        client: &reqwest::Client,
    ) -> Result<Resp> {
        let url = self.connection.api_url(path);
        let response = self
            .api_request(client.post(&url).json(body))
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;
        parse_json_response(response, "POST", &url).await
    }

    pub async fn post_admin_json<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
    ) -> Result<Resp> {
        let url = self.connection.internal_url(path);
        let response = self
            .admin_request(self.connection.clients.load.post(&url).json(body))
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;
        parse_json_response(response, "POST", &url).await
    }

    pub async fn post_admin_empty<Req: Serialize>(&self, path: &str, body: &Req) -> Result<()> {
        let url = self.connection.internal_url(path);
        let response = self
            .admin_request(self.connection.clients.load.post(&url).json(body))
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;
        ensure_success(response, "POST", &url).await?;
        Ok(())
    }

    pub async fn post_multipart<Resp: DeserializeOwned>(
        &self,
        path: &str,
        form: multipart::Form,
    ) -> Result<Resp> {
        let url = self.connection.api_url(path);
        let response = self
            .api_request(
                self.connection
                    .clients
                    .completion
                    .post(&url)
                    .multipart(form),
            )
            .send()
            .await
            .with_context(|| format!("POST {url} (multipart) failed"))?;
        parse_json_response(response, "POST", &url).await
    }

    pub async fn post_stream<Req: Serialize>(
        &self,
        path: &str,
        body: &Req,
    ) -> Result<reqwest::Response> {
        let url = self.connection.api_url(path);
        let response = self
            .api_request(self.connection.clients.stream.post(&url).json(body))
            .send()
            .await
            .with_context(|| format!("POST {url} (stream) failed"))?;
        ensure_success(response, "POST", &url).await
    }

    pub async fn post_bytes<Req: Serialize>(&self, path: &str, body: &Req) -> Result<Vec<u8>> {
        let url = self.connection.api_url(path);
        let response = self
            .api_request(self.connection.clients.completion.post(&url).json(body))
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;
        let response = ensure_success(response, "POST", &url).await?;
        Ok(response
            .bytes()
            .await
            .with_context(|| format!("failed to read response bytes from POST {url}"))?
            .to_vec())
    }
}

fn add_bearer(request: RequestBuilder, key: Option<&str>) -> RequestBuilder {
    match key {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

async fn ensure_success(
    response: reqwest::Response,
    method: &str,
    url: &str,
) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while bytes.len() < MAX_ERROR_BODY_BYTES {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let Ok(chunk) = chunk else { break };
        let remaining = MAX_ERROR_BODY_BYTES - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    let body = String::from_utf8_lossy(&bytes);
    Err(anyhow!("{method} {url} returned {status}: {body}"))
}

async fn parse_json_response<Resp: DeserializeOwned>(
    response: reqwest::Response,
    method: &str,
    url: &str,
) -> Result<Resp> {
    ensure_success(response, method, url)
        .await?
        .json()
        .await
        .with_context(|| format!("failed to parse JSON response from {method} {url}"))
}

/// Create an `async-openai` client from the shared connection.
pub fn make_lemonade_openai_client_for(connection: &LemonadeConnection) -> Client<OpenAIConfig> {
    let config = OpenAIConfig::new()
        .with_api_base(connection.api_base())
        .with_api_key(connection.api_key().unwrap_or(KEYLESS_SDK_PLACEHOLDER));
    Client::with_config(config).with_http_client(connection.completion_http_client())
}

/// Compatibility constructor using environment credentials.
pub fn make_lemonade_openai_client(base_url: &str) -> Client<OpenAIConfig> {
    let connection = LemonadeConnection::external(base_url).unwrap_or_else(|_| {
        LemonadeConnection::with_credentials(
            "http://127.0.0.1:1/v1",
            LemonadeOwnership::External,
            None,
            None,
            LemonadeTimeouts::default(),
        )
        .expect("static fallback Lemonade URL is valid")
    });
    make_lemonade_openai_client_for(&connection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn normalizes_origin_to_current_api() {
        let connection = LemonadeConnection::with_credentials(
            "http://localhost:13305/",
            LemonadeOwnership::External,
            None,
            None,
            LemonadeTimeouts::default(),
        )
        .unwrap();
        assert_eq!(connection.origin(), "http://localhost:13305");
        assert_eq!(connection.api_base(), "http://localhost:13305/v1");
    }

    #[test]
    fn preserves_supported_legacy_prefix() {
        let connection = LemonadeConnection::with_credentials(
            "http://localhost:13305/api/v1/",
            LemonadeOwnership::External,
            None,
            None,
            LemonadeTimeouts::default(),
        )
        .unwrap();
        assert_eq!(connection.origin(), "http://localhost:13305");
        assert_eq!(connection.api_base(), "http://localhost:13305/api/v1");
        assert_eq!(
            connection.origin_url("/live"),
            "http://localhost:13305/live"
        );
    }

    #[test]
    fn rejects_unsupported_paths_queries_and_schemes() {
        for url in [
            "ftp://localhost:13305/v1",
            "http://localhost:13305/custom",
            "http://localhost:13305/v1?token=secret",
            "http://secret@localhost:13305/v1",
        ] {
            assert!(LemonadeConnection::external(url).is_err(), "{url}");
        }
    }

    #[test]
    fn credentials_are_redacted() {
        let connection = LemonadeConnection::with_credentials(
            "http://localhost:13305/v1",
            LemonadeOwnership::External,
            Some("api-super-secret".into()),
            Some("admin-super-secret".into()),
            LemonadeTimeouts::default(),
        )
        .unwrap();
        let debug = format!("{connection:?}");
        assert!(!debug.contains("api-super-secret"));
        assert!(!debug.contains("admin-super-secret"));
        assert_eq!(debug.matches("<redacted>").count(), 2);
    }

    #[test]
    fn timeout_defaults_match_runtime_policy() {
        let timeouts = LemonadeTimeouts::default();
        assert_eq!(timeouts.connect, Duration::from_secs(5));
        assert_eq!(timeouts.metadata, Duration::from_secs(30));
        assert_eq!(timeouts.readiness_load, Duration::from_secs(300));
        assert_eq!(timeouts.first_token, Duration::from_secs(120));
        assert_eq!(timeouts.stream_idle, Duration::from_secs(60));
        assert_eq!(timeouts.non_stream_completion, Duration::from_secs(300));
    }

    #[tokio::test]
    async fn routes_split_credentials_to_api_and_internal_endpoints() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    bytes.extend_from_slice(&buffer[..read]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                requests.push(request);
                let body = "{}";
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{body}").as_bytes()).await.unwrap();
            }
            requests
        });
        let connection = Arc::new(
            LemonadeConnection::with_credentials(
                &format!("http://{address}"),
                LemonadeOwnership::External,
                Some("regular-key".into()),
                Some("admin-key".into()),
                LemonadeTimeouts::default(),
            )
            .unwrap(),
        );
        let client = LemonadeHttpClient::from_connection(connection);
        let _: serde_json::Value = client.get_json("/models").await.unwrap();
        let _: serde_json::Value = client.get_admin_json("/config").await.unwrap();
        let requests = server.await.unwrap();
        let api = requests
            .iter()
            .find(|request| request.contains("/v1/models"))
            .unwrap();
        let admin = requests
            .iter()
            .find(|request| request.contains("/internal/config"))
            .unwrap();
        assert!(api.contains("regular-key"));
        assert!(!api.contains("admin-key"));
        assert!(admin.contains("admin-key"));
        assert!(!admin.contains("regular-key"));
    }

    #[tokio::test]
    async fn server_error_bodies_are_read_with_a_hard_bound() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 2048];
            let _ = socket.read(&mut buffer).await;
            let body = vec![b'x'; MAX_ERROR_BODY_BYTES * 2];
            let header = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(&body).await;
        });
        let connection = Arc::new(
            LemonadeConnection::with_credentials(
                &format!("http://{address}"),
                LemonadeOwnership::External,
                None,
                None,
                LemonadeTimeouts::default(),
            )
            .unwrap(),
        );
        let error = LemonadeHttpClient::from_connection(connection)
            .get_json::<serde_json::Value>("/models")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("500 Internal Server Error"));
        assert!(error.len() < MAX_ERROR_BODY_BYTES + 512);
        server.await.unwrap();
    }
}
