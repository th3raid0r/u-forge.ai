//! Lemonade Server system information and capability detection.

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::info;

/// Capabilities derived from a [`SystemInfo`] snapshot.
///
/// Use [`SystemInfo::lemonade_capabilities`] to build this from a live server.
#[derive(Debug, Clone, Default)]
pub struct LemonadeCapabilities {
    // ── Device availability ───────────────────────────────────────────────
    /// AMD NPU (XDNA or similar) is present and accessible.
    pub npu_available: bool,
    /// AMD integrated GPU is present (iGPU shared memory).
    pub igpu_available: bool,

    // ── Installed recipe backends ─────────────────────────────────────────
    /// FLM (FastFlowLM) backend installed for NPU — enables embedding, NPU
    /// whisper, and NPU LLM.
    pub flm_npu_installed: bool,
    /// Kokoro TTS backend installed on CPU.
    pub kokoro_cpu_installed: bool,
    /// llama.cpp ROCm backend installed (iGPU via ROCm).
    pub llamacpp_rocm_installed: bool,
    /// llama.cpp Vulkan backend installed (iGPU or CPU via Vulkan).
    pub llamacpp_vulkan_installed: bool,
    /// whisper.cpp Vulkan backend installed.
    pub whispercpp_vulkan_installed: bool,
    /// whisper.cpp CPU backend installed.
    pub whispercpp_cpu_installed: bool,

    // ── Derived capability flags ──────────────────────────────────────────
    /// NPU embedding is possible (FLM + NPU).
    pub can_embed_npu: bool,
    /// NPU speech-to-text is possible (FLM + NPU).
    pub can_stt_npu: bool,
    /// NPU LLM inference is possible (FLM + NPU).
    pub can_llm_npu: bool,
    /// GPU speech-to-text is possible (whispercpp + iGPU).
    pub can_stt_gpu: bool,
    /// GPU LLM inference is possible (llamacpp ROCm or Vulkan + iGPU).
    pub can_llm_gpu: bool,
    /// CPU TTS is possible (Kokoro).
    pub can_tts_cpu: bool,
}

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

/// Snapshot of the Lemonade server's hardware and recipe state.
///
/// Fetched from `GET {base_url}/../system-info` (one path level above the
/// `/api/v1` prefix).
///
/// Use [`SystemInfo::fetch`] to obtain a live snapshot, then
/// [`SystemInfo::lemonade_capabilities`] to derive which inference paths are
/// available and testable.
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
    /// Recipe backend states, keyed by `"<recipe>/<backend>"`.
    /// E.g. `"flm/npu"`, `"llamacpp/rocm"`, `"whispercpp/vulkan"`.
    pub backends: std::collections::HashMap<String, RecipeBackendInfo>,
}

impl SystemInfo {
    /// Fetch system info from `GET {base_url}/system-info`.
    ///
    /// The Lemonade system-info endpoint lives at `/api/v1/system-info`.
    /// Pass the `/api/v1` base URL directly (e.g. `http://localhost:8000/api/v1`)
    /// and this function will append `/system-info` to it.
    pub async fn fetch(base_url: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        // The system-info endpoint lives at /api/v1/system-info — keep the
        // /api/v1 prefix intact and just append /system-info.
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

        // Parse device availability.
        let npu = raw
            .pointer("/devices/amd_npu")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let igpu = raw
            .pointer("/devices/amd_igpu")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Flatten all recipe/backend combinations into a single map.
        let mut backends = std::collections::HashMap::new();
        if let Some(recipes) = raw.get("recipes").and_then(|v| v.as_object()) {
            for (recipe, recipe_val) in recipes {
                if let Some(bmap) = recipe_val.get("backends").and_then(|v| v.as_object()) {
                    for (backend, bval) in bmap {
                        let key = format!("{recipe}/{backend}");
                        if let Ok(info) = serde_json::from_value::<RecipeBackendInfo>(bval.clone())
                        {
                            backends.insert(key, info);
                        }
                    }
                }
            }
        }

        let info = Self {
            processor,
            physical_memory,
            os_version,
            npu,
            igpu,
            backends,
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

    /// Returns `true` if the given recipe/backend is installed and available.
    pub fn is_installed(&self, recipe: &str, backend: &str) -> bool {
        let key = format!("{recipe}/{backend}");
        self.backends
            .get(&key)
            .map(|b| b.state == "installed")
            .unwrap_or(false)
    }

    /// Derive a [`LemonadeCapabilities`] snapshot from this system-info.
    pub fn lemonade_capabilities(&self) -> LemonadeCapabilities {
        let npu_available = self.npu.as_ref().map(|d| d.available).unwrap_or(false);
        let igpu_available = self.igpu.as_ref().map(|d| d.available).unwrap_or(false);

        let flm_npu_installed = self.is_installed("flm", "npu");
        let kokoro_cpu_installed = self.is_installed("kokoro", "cpu");
        let llamacpp_rocm_installed = self.is_installed("llamacpp", "rocm");
        let llamacpp_vulkan_installed = self.is_installed("llamacpp", "vulkan");
        let whispercpp_vulkan_installed = self.is_installed("whispercpp", "vulkan");
        let whispercpp_cpu_installed = self.is_installed("whispercpp", "cpu");

        LemonadeCapabilities {
            npu_available,
            igpu_available,
            flm_npu_installed,
            kokoro_cpu_installed,
            llamacpp_rocm_installed,
            llamacpp_vulkan_installed,
            whispercpp_vulkan_installed,
            whispercpp_cpu_installed,
            can_embed_npu: npu_available && flm_npu_installed,
            can_stt_npu: npu_available && flm_npu_installed,
            can_llm_npu: npu_available && flm_npu_installed,
            can_stt_gpu: igpu_available
                && (whispercpp_vulkan_installed || whispercpp_cpu_installed),
            can_llm_gpu: igpu_available && (llamacpp_rocm_installed || llamacpp_vulkan_installed),
            can_tts_cpu: kokoro_cpu_installed,
        }
    }
}
