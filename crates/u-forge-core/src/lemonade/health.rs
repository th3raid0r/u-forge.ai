//! Lemonade Server health endpoint — loaded model detection.
//!
//! The `/health` response lists all models that are currently loaded and
//! ready to serve requests.  Use [`LemonadeHealth::fetch`] to get a snapshot,
//! then [`LemonadeHealth::is_model_loaded`] to check a specific model name.
//!
//! Unlike `GET /models`, which lists *downloaded* models, the health endpoint
//! reflects what is actually running in memory.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::client::LemonadeHttpClient;

/// A single model entry from the `all_models_loaded` array in the health response.
#[derive(Debug, Clone, Deserialize)]
pub struct LoadedModelEntry {
    pub model_name: String,
    #[serde(rename = "type", default)]
    pub model_type: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub recipe: String,
}

/// Snapshot from `GET /api/v1/health`.
///
/// Construct via [`LemonadeHealth::fetch`].  Falls back to an empty snapshot
/// (no models considered loaded) when the endpoint is unreachable or returns
/// an unexpected shape.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LemonadeHealth {
    /// Overall server status (`"ok"` when healthy).
    #[serde(default)]
    pub status: String,
    /// All models that are currently loaded and serving requests.
    #[serde(default)]
    pub all_models_loaded: Vec<LoadedModelEntry>,
}

impl LemonadeHealth {
    /// Fetch the health snapshot from `GET {base_url}/health`.
    pub async fn fetch(base_url: &str) -> Result<Self> {
        let client = LemonadeHttpClient::new(base_url);
        client
            .get_json("/health")
            .await
            .context("Failed to fetch Lemonade health endpoint")
    }

    /// Returns `true` if `model_name` appears in the currently-loaded model list.
    pub fn is_model_loaded(&self, model_name: &str) -> bool {
        self.all_models_loaded
            .iter()
            .any(|m| m.model_name == model_name)
    }
}
