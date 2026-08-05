//! Lemonade Server system information — hardware display and logging.
//!
//! [`SystemInfo::fetch`] retrieves processor, memory, OS version, and device
//! presence from `GET /api/v1/system-info`.  Use this for human-readable
//! capability display; for model selection and provider construction use
//! [`LemonadeServerCatalog`](super::catalog::LemonadeServerCatalog).

use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use tracing::info;

use super::client::{LemonadeConnection, LemonadeHttpClient};

/// Raw device info from the `devices` section of `/system-info`.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemDeviceInfo {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub name: String,
}

/// Info for a single recipe backend (e.g. `llamacpp.rocm`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RecipeBackendInfo {
    /// Installation state: `"installed"`, `"installable"`, `"unsupported"`, etc.
    #[serde(default)]
    pub state: String,
    /// Lemonade device ids this backend runs on (e.g. `["amd_igpu"]`).
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// Snapshot of the Lemonade server's hardware state.
///
/// Fetched from `GET {base_url}/system-info`.  Use for display and logging.
/// For model selection use [`LemonadeServerCatalog::discover`](super::catalog::LemonadeServerCatalog::discover).
#[derive(Debug, Clone)]
pub struct SystemInfo {
    /// CPU/APU model string reported by the OS.
    pub processor: String,
    /// Physical RAM as a human-readable string (e.g. `"94.07 GB"`).
    pub physical_memory: String,
    /// OS version string.
    pub os_version: String,
    /// AMD NPU device info, if present.
    pub npu: Option<SystemDeviceInfo>,
    /// AMD integrated GPU device info, if present.
    pub igpu: Option<SystemDeviceInfo>,
    /// Discrete GPUs reported by current Lemonade releases.
    pub gpus: Vec<SystemDeviceInfo>,
}

impl SystemInfo {
    /// Fetch system info from `GET {base_url}/system-info`.
    pub async fn fetch(base_url: &str) -> Result<Self> {
        let connection = Arc::new(LemonadeConnection::external(base_url)?);
        Self::fetch_with_connection(connection).await
    }

    pub async fn fetch_with_connection(connection: Arc<LemonadeConnection>) -> Result<Self> {
        let raw: serde_json::Value = LemonadeHttpClient::from_connection(connection)
            .get_json("/system-info")
            .await?;

        let processor = raw
            .get("Processor")
            .and_then(|v| v.as_str())
            .or_else(|| raw.pointer("/cpu/name").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        let physical_memory = raw
            .get("Physical Memory")
            .and_then(|v| v.as_str())
            .or_else(|| raw.pointer("/memory/physical").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        let os_version = raw
            .get("OS Version")
            .and_then(|v| v.as_str())
            .or_else(|| raw.pointer("/os/version").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

        let npu = raw
            .pointer("/devices/amd_npu")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let igpu = raw
            .pointer("/devices/amd_igpu")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let gpus = raw
            .get("gpus")
            .or_else(|| raw.pointer("/devices/gpus"))
            .and_then(|v| v.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| serde_json::from_value(value.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        let info = Self {
            processor,
            physical_memory,
            os_version,
            npu,
            igpu,
            gpus,
        };

        info!(
            processor = %info.processor,
            os = %info.os_version,
            npu_available = info.npu.as_ref().map(|d| d.available).unwrap_or(false),
            igpu_available = info.igpu.as_ref().map(|d| d.available).unwrap_or(false),
            "Lemonade system-info loaded"
        );

        Ok(info)
    }
}
