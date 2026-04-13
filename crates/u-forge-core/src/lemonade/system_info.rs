//! Lemonade Server system information — hardware display and logging.
//!
//! [`SystemInfo::fetch`] retrieves processor, memory, OS version, and device
//! presence from `GET /api/v1/system-info`.  Use this for human-readable
//! capability display; for model selection and provider construction use
//! [`LemonadeServerCatalog`](super::catalog::LemonadeServerCatalog).

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::info;

/// Raw device info from the `devices` section of `/system-info`.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemDeviceInfo {
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
}

impl SystemInfo {
    /// Fetch system info from `GET {base_url}/system-info`.
    pub async fn fetch(base_url: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        let base = base_url.trim_end_matches('/');
        let url = format!("{base}/system-info");

        let raw: serde_json::Value = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Failed to reach Lemonade system-info at {url}"))?
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
        let physical_memory = raw
            .get("Physical Memory")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let os_version = raw
            .get("OS Version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let npu = raw
            .pointer("/devices/amd_npu")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let igpu = raw
            .pointer("/devices/amd_igpu")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        let info = Self { processor, physical_memory, os_version, npu, igpu };

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
