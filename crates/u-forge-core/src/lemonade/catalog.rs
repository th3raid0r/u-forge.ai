//! Unified Lemonade Server catalog — replaces the registry + system_info +
//! capabilities trio.
//!
//! [`LemonadeServerCatalog::discover`] fetches `/models`, `/system-info`, and
//! `/health` concurrently and caches the results.  Capability predicates
//! (`has_npu`, `has_gpu`, etc.) are computed on-the-fly from the cached data
//! rather than stored as a 16-boolean struct.

use std::collections::HashSet;

use anyhow::{Context, Result};
use tracing::info;

use super::client::LemonadeHttpClient;

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
}

#[derive(Debug, serde::Deserialize)]
struct RawHealthResponse {
    #[serde(default)]
    all_models_loaded: Vec<RawLoadedModel>,
}

#[derive(Debug, serde::Deserialize)]
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
}

// ── Public catalog types ──────────────────────────────────────────────────────

/// A model entry as returned by `GET /api/v1/models`.
///
/// No role classification — just the raw server data.  Selection logic that
/// interprets labels and recipes lives in `ModelSelector`.
#[derive(Debug, Clone)]
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
}

/// An installed recipe/backend combination from `GET /api/v1/system-info`.
#[derive(Debug, Clone)]
pub struct InstalledBackend {
    /// Recipe name, e.g. `"llamacpp"`, `"flm"`, `"whispercpp"`, `"kokoro"`.
    pub recipe: String,
    /// Backend name, e.g. `"rocm"`, `"vulkan"`, `"cpu"`, `"npu"`.
    pub backend: String,
    /// Lemonade device IDs this backend targets, e.g. `["amd_igpu"]`.
    pub devices: Vec<String>,
    /// Installation state: `"installed"`, `"installable"`, `"unsupported"`, etc.
    pub state: String,
}

/// A model that is currently loaded and serving requests, from `GET /api/v1/health`.
#[derive(Debug, Clone)]
pub struct LoadedModel {
    pub model_name: String,
    pub recipe: String,
    /// Active compute device(s), e.g. `"gpu"`, `"npu"`, `"cpu"`, `"gpu npu"`.
    pub device: String,
    /// Lemonade type tag: `"llm"`, `"embedding"`, `"reranking"`, `"audio"`, `"tts"`.
    pub model_type: String,
    /// Backend-specific URL for direct calls, when reported by the server.
    pub backend_url: String,
}

/// One-shot discovery snapshot: fetches `/models`, `/system-info`, and
/// `/health` concurrently and caches the results.
///
/// Construct via [`LemonadeServerCatalog::discover`].
///
/// Capability predicates are computed on-the-fly from the cached data —
/// no 16-boolean struct, no stored capability flags.
#[derive(Debug, Clone)]
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
}

impl LemonadeServerCatalog {
    /// Fetch `/models`, `/system-info`, and `/health` concurrently and build
    /// a catalog snapshot.
    ///
    /// Returns an error if any of the three endpoints fails.
    pub async fn discover(base_url: &str) -> Result<Self> {
        let client = LemonadeHttpClient::new(base_url);
        let base = client.base_url.clone();

        let (models_resp, sysinfo, health_resp) = tokio::try_join!(
            client.get_json::<RawModelsResponse>("/models"),
            Self::fetch_system_info(&base),
            client.get_json::<RawHealthResponse>("/health"),
        )?;

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
            })
            .collect();

        let loaded: Vec<LoadedModel> = health_resp
            .all_models_loaded
            .into_iter()
            .map(|m| LoadedModel {
                model_name: m.model_name,
                recipe: m.recipe,
                device: m.device,
                model_type: m.model_type,
                backend_url: m.backend_url,
            })
            .collect();

        let (backends, processor, memory_gb) = sysinfo;

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
        self.backends.iter().any(|b| {
            b.state == "installed" && b.devices.iter().any(|d| d == "amd_npu")
        })
    }

    /// Returns `true` if any installed backend targets an integrated GPU
    /// (`"amd_igpu"` or a string containing `"gpu"`).
    pub fn has_gpu(&self) -> bool {
        self.backends.iter().any(|b| {
            b.state == "installed"
                && b.devices.iter().any(|d| d == "amd_igpu" || d.contains("gpu"))
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

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Fetch `/system-info` and parse into `(backends, processor, memory_gb)`.
    ///
    /// The system-info endpoint does not use the Bearer token, so a plain
    /// `reqwest::Client` is used here (same approach as `SystemInfo::fetch`).
    async fn fetch_system_info(
        base_url: &str,
    ) -> Result<(Vec<InstalledBackend>, String, f64)> {
        let url = format!("{base_url}/system-info");
        let raw: serde_json::Value = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Failed to reach Lemonade /system-info at {url}"))?
            .error_for_status()
            .context("Lemonade /system-info returned an error status")?
            .json()
            .await
            .context("Failed to parse /system-info JSON")?;

        let processor = raw
            .get("Processor")
            .and_then(|v| v.as_str())
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
                        backends.push(InstalledBackend {
                            recipe: recipe.clone(),
                            backend: backend.clone(),
                            devices,
                            state,
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

    fn make_model(id: &str, recipe: &str, labels: &[&str], downloaded: bool) -> CatalogModel {
        CatalogModel {
            id: id.to_string(),
            recipe: recipe.to_string(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            downloaded,
            size_gb: None,
            checkpoint: String::new(),
        }
    }

    fn installed(recipe: &str, backend: &str, devices: &[&str]) -> InstalledBackend {
        InstalledBackend {
            recipe: recipe.to_string(),
            backend: backend.to_string(),
            devices: devices.iter().map(|s| s.to_string()).collect(),
            state: "installed".to_string(),
        }
    }

    fn not_installed(recipe: &str, backend: &str, devices: &[&str]) -> InstalledBackend {
        InstalledBackend {
            recipe: recipe.to_string(),
            backend: backend.to_string(),
            devices: devices.iter().map(|s| s.to_string()).collect(),
            state: "installable".to_string(),
        }
    }

    fn empty_catalog(models: Vec<CatalogModel>, backends: Vec<InstalledBackend>) -> LemonadeServerCatalog {
        LemonadeServerCatalog {
            base_url: String::new(),
            models,
            backends,
            loaded: vec![],
            processor: String::new(),
            memory_gb: 0.0,
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
        let with_gpu = empty_catalog(
            vec![],
            vec![installed("llamacpp", "rocm", &["amd_igpu"])],
        );
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
            }],
            processor: String::new(),
            memory_gb: 0.0,
        };

        assert!(catalog.is_model_loaded("embed-gemma-300m-FLM"));
        assert!(!catalog.is_model_loaded("kokoro-v1"));
    }

    // ── Integration test (requires running Lemonade Server) ───────────────────

    #[tokio::test]
    async fn test_catalog_discover() {
        let url = require_integration_url!();
        let catalog = LemonadeServerCatalog::discover(&url).await.unwrap();

        assert!(!catalog.models.is_empty(), "Catalog must contain at least one model");

        let downloaded: Vec<_> = catalog.models.iter().filter(|m| m.downloaded).collect();
        assert!(!downloaded.is_empty(), "At least one model should be downloaded");

        assert!(!catalog.processor.is_empty(), "Processor string should be non-empty");
        assert!(catalog.memory_gb > 0.0, "Memory should be > 0 GB");
        assert!(!catalog.backends.is_empty(), "At least one backend should be present");
    }
}
