//! Unified Lemonade Server catalog — replaces the registry + system_info +
//! capabilities trio.
//!
//! [`LemonadeServerCatalog::discover`] requires `/models` and independently
//! enriches the result with `/system-info` and `/health`. Capability predicates
//! (`has_npu`, `has_gpu`, etc.) are computed on-the-fly from the cached data
//! rather than stored as a 16-boolean struct.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Result;
use tracing::info;

use super::client::{LemonadeConnection, LemonadeHttpClient};

// ── Wire-format helpers ───────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct RawModelsResponse {
    data: Vec<RawModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct RawModelEntry {
    id: String,
    #[serde(default)]
    recipe: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    downloaded: Option<bool>,
    #[serde(default)]
    size: Option<f64>,
    #[serde(default)]
    checkpoint: String,
    #[serde(default, alias = "max_context_length", alias = "context_window")]
    max_context_window: Option<usize>,
    #[serde(default)]
    recipe_options: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
struct RawHealthResponse {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    all_models_loaded: Vec<RawLoadedModel>,
    #[serde(default)]
    max_models: HashMap<String, usize>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct RawLoadedModel {
    model_name: String,
    #[serde(default)]
    recipe: String,
    #[serde(default)]
    device: String,
    #[serde(rename = "type", default)]
    model_type: String,
    #[serde(default)]
    backend_url: String,
    #[serde(default)]
    checkpoint: String,
    #[serde(default)]
    recipe_options: serde_json::Value,
    #[serde(default)]
    max_context_window: Option<usize>,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    is_busy: bool,
    #[serde(default)]
    is_streaming: bool,
}

// ── Public catalog types ──────────────────────────────────────────────────────

/// A model entry as returned by `GET /api/v1/models`.
///
/// No role classification — just the raw server data.  Selection logic that
/// interprets labels and recipes lives in `ModelSelector`.
#[derive(Debug, Clone, Default)]
pub struct CatalogModel {
    pub id: String,
    /// Recipe name: `"llamacpp"`, `"flm"`, `"whispercpp"`, `"kokoro"`, `"sd-cpp"`.
    pub recipe: String,
    /// Server-supplied labels: `"embeddings"`, `"reranking"`, `"audio"`, `"tts"`, etc.
    pub labels: HashSet<String>,
    /// Whether the model weights have been downloaded locally.
    pub downloaded: bool,
    /// Model size in gigabytes, when reported by the server.
    pub size_gb: Option<f64>,
    /// Checkpoint path or identifier, when reported by the server.
    pub checkpoint: String,
    /// Maximum context accepted by this model, when the server can determine it.
    pub max_context_window: Option<usize>,
    /// Per-model recipe defaults returned by Lemonade.
    pub recipe_options: serde_json::Value,
}

impl CatalogModel {
    pub fn supports_tool_calling(&self) -> bool {
        self.labels.contains("tool-calling")
    }

    pub fn supports_reasoning(&self) -> bool {
        self.labels.contains("reasoning")
    }
}

/// An installed recipe/backend combination from `GET /api/v1/system-info`.
#[derive(Debug, Clone, Default)]
pub struct InstalledBackend {
    /// Recipe name, e.g. `"llamacpp"`, `"flm"`, `"whispercpp"`, `"kokoro"`.
    pub recipe: String,
    /// Backend name, e.g. `"rocm"`, `"vulkan"`, `"cpu"`, `"npu"`.
    pub backend: String,
    /// Lemonade device IDs this backend targets, e.g. `["amd_igpu"]`.
    pub devices: Vec<String>,
    /// Installation state: `"installed"`, `"installable"`, `"unsupported"`, etc.
    pub state: String,
    pub message: String,
    pub action: String,
    pub version: Option<String>,
}

/// A model that is currently loaded and serving requests, from `GET /api/v1/health`.
#[derive(Debug, Clone, Default)]
pub struct LoadedModel {
    pub model_name: String,
    pub recipe: String,
    /// Active compute device(s), e.g. `"gpu"`, `"npu"`, `"cpu"`, `"gpu npu"`.
    pub device: String,
    /// Lemonade type tag: `"llm"`, `"embedding"`, `"reranking"`, `"audio"`, `"tts"`.
    pub model_type: String,
    /// Backend-specific URL for direct calls, when reported by the server.
    pub backend_url: String,
    pub checkpoint: String,
    pub recipe_options: serde_json::Value,
    pub max_context_window: Option<usize>,
    pub pinned: bool,
    pub is_busy: bool,
    pub is_streaming: bool,
}

/// Optional discovery failures retained for degraded-mode UI diagnostics.
#[derive(Debug, Clone, Default)]
pub struct CatalogDiagnostics {
    pub health: Option<String>,
    pub system_info: Option<String>,
}

/// One-shot discovery snapshot: fetches `/models`, `/system-info`, and
/// `/health` concurrently and caches the results.
///
/// Construct via [`LemonadeServerCatalog::discover`].
///
/// Capability predicates are computed on-the-fly from the cached data —
/// no 16-boolean struct, no stored capability flags.
#[derive(Debug, Clone, Default)]
pub struct LemonadeServerCatalog {
    pub base_url: String,
    /// All models returned by the server (downloaded and not yet downloaded).
    pub models: Vec<CatalogModel>,
    /// All recipe/backend combinations reported by `/system-info`.
    pub backends: Vec<InstalledBackend>,
    /// Models currently loaded and serving requests, from `/health`.
    pub loaded: Vec<LoadedModel>,
    /// Processor description string from `/system-info`.
    pub processor: String,
    /// Physical RAM reported by `/system-info`, in gigabytes.
    pub memory_gb: f64,
    /// Lemonade version reported by `/health`.
    pub server_version: Option<String>,
    /// Overall health status when returned by the server.
    pub health_status: Option<String>,
    /// Maximum simultaneously loaded models, keyed by model type.
    pub max_models: HashMap<String, usize>,
    /// Failures from optional enrichment endpoints.
    pub diagnostics: CatalogDiagnostics,
}

impl LemonadeServerCatalog {
    /// Fetch `/models`, `/system-info`, and `/health` concurrently and build
    /// a catalog snapshot.
    ///
    /// Returns an error only when the required `/models` endpoint fails.
    pub async fn discover(base_url: &str) -> Result<Self> {
        let connection = Arc::new(LemonadeConnection::external(base_url)?);
        Self::discover_with_connection(connection).await
    }

    pub async fn discover_with_connection(connection: Arc<LemonadeConnection>) -> Result<Self> {
        let client = LemonadeHttpClient::from_connection(connection);
        let base = client.base_url.clone();

        let (models_result, sysinfo_result, health_result) = tokio::join!(
            // Setup needs the full built-in catalog, not OpenAI's default
            // downloaded-only view.
            client.get_json::<RawModelsResponse>("/models?show_all=true"),
            Self::fetch_system_info(&client),
            client.get_json::<RawHealthResponse>("/health"),
        );
        let models_resp = models_result?;

        let models: Vec<CatalogModel> = models_resp
            .data
            .into_iter()
            .map(|m| CatalogModel {
                id: m.id,
                recipe: m.recipe,
                labels: m.labels.into_iter().collect(),
                downloaded: m.downloaded.unwrap_or(false),
                size_gb: m.size,
                checkpoint: m.checkpoint,
                max_context_window: m.max_context_window,
                recipe_options: m.recipe_options,
            })
            .collect();

        let health_error = health_result.as_ref().err().map(ToString::to_string);
        let health_resp = health_result.ok();
        let loaded: Vec<LoadedModel> = health_resp
            .as_ref()
            .map(|health| health.all_models_loaded.as_slice())
            .unwrap_or_default()
            .iter()
            .cloned()
            .map(|m| LoadedModel {
                model_name: m.model_name,
                recipe: m.recipe,
                device: m.device,
                model_type: m.model_type,
                backend_url: m.backend_url,
                checkpoint: m.checkpoint,
                recipe_options: m.recipe_options,
                max_context_window: m.max_context_window,
                pinned: m.pinned,
                is_busy: m.is_busy,
                is_streaming: m.is_streaming,
            })
            .collect();

        let system_info_error = sysinfo_result.as_ref().err().map(ToString::to_string);
        let (backends, processor, memory_gb) = sysinfo_result.unwrap_or_default();
        let server_version = health_resp
            .as_ref()
            .and_then(|health| health.version.clone());
        let health_status = health_resp
            .as_ref()
            .and_then(|health| health.status.clone());
        let max_models = health_resp
            .map(|health| health.max_models)
            .unwrap_or_default();

        info!(
            model_count = models.len(),
            downloaded_count = models.iter().filter(|m| m.downloaded).count(),
            backend_count = backends.len(),
            loaded_count = loaded.len(),
            %processor,
            memory_gb,
            "Lemonade server catalog built",
        );

        Ok(Self {
            base_url: base,
            models,
            backends,
            loaded,
            processor,
            memory_gb,
            server_version,
            health_status,
            max_models,
            diagnostics: CatalogDiagnostics {
                health: health_error,
                system_info: system_info_error,
            },
        })
    }

    /// Returns `true` if the given recipe/backend is installed
    /// (state = `"installed"`).
    pub fn has_installed_backend(&self, recipe: &str, backend: &str) -> bool {
        self.backends
            .iter()
            .any(|b| b.recipe == recipe && b.backend == backend && b.state == "installed")
    }

    /// Returns `true` if any installed backend targets an NPU device
    /// (`"amd_npu"`).
    pub fn has_npu(&self) -> bool {
        self.backends
            .iter()
            .any(|b| b.state == "installed" && b.devices.iter().any(|d| d == "amd_npu"))
    }

    /// Returns `true` if any installed backend targets an integrated GPU
    /// (`"amd_igpu"` or a string containing `"gpu"`).
    pub fn has_gpu(&self) -> bool {
        self.backends.iter().any(|b| {
            b.state == "installed"
                && b.devices
                    .iter()
                    .any(|d| d == "amd_igpu" || d.contains("gpu"))
        })
    }

    /// All downloaded models carrying the given label.
    pub fn downloaded_models_with_label(&self, label: &str) -> Vec<&CatalogModel> {
        self.models
            .iter()
            .filter(|m| m.downloaded && m.labels.contains(label))
            .collect()
    }

    /// All downloaded models using the given recipe.
    pub fn downloaded_models_with_recipe(&self, recipe: &str) -> Vec<&CatalogModel> {
        self.models
            .iter()
            .filter(|m| m.downloaded && m.recipe == recipe)
            .collect()
    }

    /// Returns `true` if the model with the given ID is currently loaded and
    /// serving requests.
    pub fn is_model_loaded(&self, model_id: &str) -> bool {
        self.loaded.iter().any(|m| m.model_name == model_id)
    }

    /// Capacity for a model type, conservatively defaulting to one when the
    /// server did not report current capacity data.
    pub fn capacity_for(&self, model_type: &str) -> usize {
        self.max_models.get(model_type).copied().unwrap_or(1)
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Fetch `/system-info` and parse into `(backends, processor, memory_gb)`.
    ///
    async fn fetch_system_info(
        client: &LemonadeHttpClient,
    ) -> Result<(Vec<InstalledBackend>, String, f64)> {
        let raw: serde_json::Value = client.get_json("/system-info").await?;

        let processor = raw
            .get("Processor")
            .and_then(|v| v.as_str())
            .or_else(|| raw.pointer("/cpu/name").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        // "Physical Memory" is reported as e.g. "94.07 GB" — extract the number.
        let memory_gb = raw
            .get("Physical Memory")
            .and_then(|v| v.as_str())
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        let mut backends = Vec::new();
        if let Some(recipes) = raw.get("recipes").and_then(|v| v.as_object()) {
            for (recipe, recipe_val) in recipes {
                if let Some(bmap) = recipe_val.get("backends").and_then(|v| v.as_object()) {
                    for (backend, bval) in bmap {
                        let state = bval
                            .get("state")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let devices: Vec<String> = bval
                            .get("devices")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(String::from)
                                    .collect()
                            })
                            .unwrap_or_default();
                        let message = bval
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let action = bval
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let version = bval
                            .get("version")
                            .and_then(|v| v.as_str())
                            .map(ToString::to_string);
                        backends.push(InstalledBackend {
                            recipe: recipe.clone(),
                            backend: backend.clone(),
                            devices,
                            state,
                            message,
                            action,
                            version,
                        });
                    }
                }
            }
        }

        Ok((backends, processor, memory_gb))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::require_integration_url;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn make_model(id: &str, recipe: &str, labels: &[&str], downloaded: bool) -> CatalogModel {
        CatalogModel {
            id: id.to_string(),
            recipe: recipe.to_string(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            downloaded,
            size_gb: None,
            checkpoint: String::new(),
            ..Default::default()
        }
    }

    fn installed(recipe: &str, backend: &str, devices: &[&str]) -> InstalledBackend {
        InstalledBackend {
            recipe: recipe.to_string(),
            backend: backend.to_string(),
            devices: devices.iter().map(|s| s.to_string()).collect(),
            state: "installed".to_string(),
            ..Default::default()
        }
    }

    fn not_installed(recipe: &str, backend: &str, devices: &[&str]) -> InstalledBackend {
        InstalledBackend {
            recipe: recipe.to_string(),
            backend: backend.to_string(),
            devices: devices.iter().map(|s| s.to_string()).collect(),
            state: "installable".to_string(),
            ..Default::default()
        }
    }

    fn empty_catalog(
        models: Vec<CatalogModel>,
        backends: Vec<InstalledBackend>,
    ) -> LemonadeServerCatalog {
        LemonadeServerCatalog {
            base_url: String::new(),
            models,
            backends,
            loaded: vec![],
            processor: String::new(),
            memory_gb: 0.0,
            ..Default::default()
        }
    }

    // ── Unit tests ────────────────────────────────────────────────────────────

    #[test]
    fn test_downloaded_models_with_label_only_returns_downloaded() {
        let catalog = empty_catalog(
            vec![
                make_model("embed-flm", "flm", &["embeddings"], true),
                make_model("embed-gguf", "llamacpp", &["embeddings"], false), // not downloaded
                make_model("kokoro-v1", "kokoro", &["tts"], true),
            ],
            vec![],
        );

        let embeds = catalog.downloaded_models_with_label("embeddings");
        assert_eq!(embeds.len(), 1);
        assert_eq!(embeds[0].id, "embed-flm");

        let tts = catalog.downloaded_models_with_label("tts");
        assert_eq!(tts.len(), 1);
        assert_eq!(tts[0].id, "kokoro-v1");

        let none = catalog.downloaded_models_with_label("reranking");
        assert!(none.is_empty());
    }

    #[test]
    fn test_downloaded_models_with_recipe_only_returns_downloaded() {
        let catalog = empty_catalog(
            vec![
                make_model("llm-gguf", "llamacpp", &["reasoning"], true),
                make_model("tts-cpu", "kokoro", &["tts"], false), // not downloaded
                make_model("embed-flm", "flm", &["embeddings"], true),
            ],
            vec![],
        );

        let llamacpp = catalog.downloaded_models_with_recipe("llamacpp");
        assert_eq!(llamacpp.len(), 1);
        assert_eq!(llamacpp[0].id, "llm-gguf");

        let kokoro = catalog.downloaded_models_with_recipe("kokoro");
        assert!(kokoro.is_empty(), "Not downloaded, so should be excluded");
    }

    #[test]
    fn test_has_installed_backend_checks_state() {
        let catalog = empty_catalog(
            vec![],
            vec![
                installed("flm", "npu", &["amd_npu"]),
                not_installed("llamacpp", "rocm", &["amd_igpu"]),
            ],
        );

        assert!(catalog.has_installed_backend("flm", "npu"));
        assert!(!catalog.has_installed_backend("llamacpp", "rocm")); // installable only
        assert!(!catalog.has_installed_backend("whispercpp", "vulkan")); // not present
    }

    #[test]
    fn test_has_npu_derives_from_backends() {
        let with_npu = empty_catalog(vec![], vec![installed("flm", "npu", &["amd_npu"])]);
        assert!(with_npu.has_npu());
        assert!(!with_npu.has_gpu());

        let no_npu = empty_catalog(vec![], vec![not_installed("flm", "npu", &["amd_npu"])]);
        assert!(!no_npu.has_npu()); // installable, not installed
    }

    #[test]
    fn test_has_gpu_derives_from_backends() {
        let with_gpu = empty_catalog(vec![], vec![installed("llamacpp", "rocm", &["amd_igpu"])]);
        assert!(with_gpu.has_gpu());
        assert!(!with_gpu.has_npu());

        let no_gpu = empty_catalog(vec![], vec![installed("flm", "npu", &["amd_npu"])]);
        assert!(!no_gpu.has_gpu());
    }

    #[test]
    fn test_is_model_loaded() {
        let catalog = LemonadeServerCatalog {
            base_url: String::new(),
            models: vec![],
            backends: vec![],
            loaded: vec![LoadedModel {
                model_name: "embed-gemma-300m-FLM".to_string(),
                recipe: "flm".to_string(),
                device: "npu".to_string(),
                model_type: "embedding".to_string(),
                backend_url: String::new(),
                ..Default::default()
            }],
            processor: String::new(),
            memory_gb: 0.0,
            ..Default::default()
        };

        assert!(catalog.is_model_loaded("embed-gemma-300m-FLM"));
        assert!(!catalog.is_model_loaded("kokoro-v1"));
    }

    #[tokio::test]
    async fn partial_discovery_keeps_optional_failure_and_propagates_auth() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for _ in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(
                    request.contains("authorization: Bearer test-key")
                        || request.contains("Authorization: Bearer test-key")
                );
                let path = request.split_whitespace().nth(1).unwrap().to_string();
                paths.push(path.clone());
                let (status, body) = match path.as_str() {
                    "/v1/models?show_all=true" => (
                        "200 OK",
                        r#"{"data":[{"id":"tool-model","recipe":"llamacpp","labels":["tool-calling","reasoning"],"downloaded":true,"max_context_window":8192,"future_field":true}],"unknown":1}"#,
                    ),
                    "/v1/system-info" => (
                        "200 OK",
                        r#"{"cpu":{"name":"Test CPU"},"gpus":[],"recipes":{},"future_field":true}"#,
                    ),
                    "/v1/health" => ("503 Service Unavailable", r#"{"error":"warming"}"#),
                    _ => panic!("unexpected path {path}"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            paths
        });
        let connection = Arc::new(
            LemonadeConnection::with_credentials(
                &format!("http://{address}"),
                super::super::LemonadeOwnership::External,
                Some("test-key".into()),
                None,
                super::super::LemonadeTimeouts::default(),
            )
            .unwrap(),
        );
        let catalog = LemonadeServerCatalog::discover_with_connection(connection)
            .await
            .unwrap();
        let paths = server.await.unwrap();
        assert_eq!(paths.len(), 3);
        assert_eq!(catalog.models.len(), 1);
        assert!(catalog.models[0].supports_tool_calling());
        assert_eq!(catalog.models[0].max_context_window, Some(8192));
        assert_eq!(catalog.processor, "Test CPU");
        assert!(
            catalog
                .diagnostics
                .health
                .as_deref()
                .is_some_and(|error| error.contains("503"))
        );
        assert!(catalog.diagnostics.system_info.is_none());
    }

    // ── Integration test (requires running Lemonade Server) ───────────────────

    #[tokio::test]
    async fn test_catalog_discover() {
        let url = require_integration_url!();
        let catalog = LemonadeServerCatalog::discover(&url).await.unwrap();

        assert!(
            !catalog.models.is_empty(),
            "Catalog must contain at least one model"
        );

        let downloaded: Vec<_> = catalog.models.iter().filter(|m| m.downloaded).collect();
        assert!(
            !downloaded.is_empty(),
            "At least one model should be downloaded"
        );

        assert!(
            !catalog.processor.is_empty(),
            "Processor string should be non-empty"
        );
        assert!(catalog.memory_gb > 0.0, "Memory should be > 0 GB");
        assert!(
            !catalog.backends.is_empty(),
            "At least one backend should be present"
        );
    }
}
