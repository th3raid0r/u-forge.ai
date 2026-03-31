//! Device configuration — which hardware backends to use for each capability.
//!
//! Loaded from a TOML file at startup.  All fields have sensible defaults so
//! the file is entirely optional; the project runs correctly with zero config.
//!
//! # File locations (checked in order)
//!
//! 1. `./u-forge-devices.toml` (working directory)
//! 2. `$XDG_CONFIG_HOME/u-forge/devices.toml`
//!    (falls back to `$HOME/.config/u-forge/devices.toml` on Linux)
//! 3. Built-in defaults (NPU weight=100, GPU weight=50, CPU weight=10)
//!
//! # Example file
//!
//! ```toml
//! [embedding]
//! npu_enabled  = true
//! gpu_enabled  = true
//! cpu_enabled  = false   # disable CPU worker when GPU handles llamacpp
//! npu_weight   = 100
//! gpu_weight   = 50
//! cpu_weight   = 10
//! ```
//!
//! # Typical use-cases for disabling a device
//!
//! Lemonade Server cannot run the same llamacpp embedding model on both GPU and
//! CPU simultaneously.  If your setup loads the GGUF model on the GPU, set
//! `cpu_enabled = false` to prevent the CPU worker from also trying to use it.
//!
//! NPU embedding uses a separate FLM model (not llamacpp), so the NPU worker
//! never conflicts with GPU/CPU llamacpp workers and can always remain enabled.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

// ── EmbeddingDeviceConfig ─────────────────────────────────────────────────────

/// Per-device settings for the embedding subsystem.
///
/// Corresponds to the `[embedding]` section of `u-forge-devices.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingDeviceConfig {
    /// Whether to use the NPU embedding worker (FLM model, highest quality).
    #[serde(default = "default_true")]
    pub npu_enabled: bool,

    /// Whether to use the GPU embedding worker (llamacpp GGUF model via ROCm/Vulkan).
    #[serde(default = "default_true")]
    pub gpu_enabled: bool,

    /// Whether to use the CPU embedding worker (llamacpp GGUF model, host CPU).
    #[serde(default = "default_true")]
    pub cpu_enabled: bool,

    /// Dispatch weight for the NPU worker.  Higher weight → preferred when idle.
    #[serde(default = "default_npu_weight")]
    pub npu_weight: u32,

    /// Dispatch weight for the GPU embedding worker.
    #[serde(default = "default_gpu_weight")]
    pub gpu_weight: u32,

    /// Dispatch weight for the CPU embedding worker.
    #[serde(default = "default_cpu_weight")]
    pub cpu_weight: u32,
}

impl Default for EmbeddingDeviceConfig {
    fn default() -> Self {
        Self {
            npu_enabled: true,
            gpu_enabled: true,
            cpu_enabled: true,
            npu_weight: default_npu_weight(),
            gpu_weight: default_gpu_weight(),
            cpu_weight: default_cpu_weight(),
        }
    }
}

// ── DeviceConfig ──────────────────────────────────────────────────────────────

/// Top-level device configuration.
///
/// Loaded from `u-forge-devices.toml` by [`DeviceConfig::load_default`].
/// Use [`DeviceConfig::default`] when no config file is present or required.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceConfig {
    /// Embedding-specific device settings.
    #[serde(default)]
    pub embedding: EmbeddingDeviceConfig,
}

impl DeviceConfig {
    /// Load from a specific TOML file path.
    ///
    /// Returns `Ok(DeviceConfig::default())` if the file does not exist, so
    /// callers never need to treat a missing config file as an error.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let text = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&text)?;

        info!(path = %path.display(), "DeviceConfig loaded");

        Ok(config)
    }

    /// Load from the canonical search path, returning defaults if nothing is found.
    ///
    /// Search order:
    /// 1. `./u-forge-devices.toml`
    /// 2. `$XDG_CONFIG_HOME/u-forge/devices.toml`
    ///    (or `$HOME/.config/u-forge/devices.toml` on Linux)
    /// 3. Built-in defaults
    pub fn load_default() -> Self {
        for path in Self::candidate_paths() {
            match Self::load(&path) {
                Ok(cfg) => return cfg,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "DeviceConfig: failed to load — skipping"
                    );
                }
            }
        }

        info!("DeviceConfig: no config file found — using defaults");
        Self::default()
    }

    /// Ordered list of paths to check when loading the default config.
    fn candidate_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from("u-forge-devices.toml")];

        // XDG_CONFIG_HOME / fallback to $HOME/.config
        let xdg_base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs_or_home().map(|h| h.join(".config")).unwrap_or_default()
            });

        if !xdg_base.as_os_str().is_empty() {
            paths.push(xdg_base.join("u-forge").join("devices.toml"));
        }

        paths
    }
}

/// Helper: `$HOME` path, if determinable.
fn dirs_or_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

// ── Default value helpers ─────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

fn default_npu_weight() -> u32 {
    100
}

fn default_gpu_weight() -> u32 {
    50
}

fn default_cpu_weight() -> u32 {
    10
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_values() {
        let cfg = DeviceConfig::default();
        assert!(cfg.embedding.npu_enabled);
        assert!(cfg.embedding.gpu_enabled);
        assert!(cfg.embedding.cpu_enabled);
        assert_eq!(cfg.embedding.npu_weight, 100);
        assert_eq!(cfg.embedding.gpu_weight, 50);
        assert_eq!(cfg.embedding.cpu_weight, 10);
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let path = PathBuf::from("/tmp/u-forge-nonexistent-config-xyz.toml");
        let cfg = DeviceConfig::load(&path).unwrap();
        assert!(cfg.embedding.npu_enabled);
        assert_eq!(cfg.embedding.npu_weight, 100);
    }

    #[test]
    fn test_load_full_toml() {
        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[embedding]
npu_enabled  = true
gpu_enabled  = true
cpu_enabled  = false
npu_weight   = 200
gpu_weight   = 75
cpu_weight   = 5
"#
        )
        .unwrap();

        let cfg = DeviceConfig::load(f.path()).unwrap();
        assert!(cfg.embedding.npu_enabled);
        assert!(cfg.embedding.gpu_enabled);
        assert!(!cfg.embedding.cpu_enabled);
        assert_eq!(cfg.embedding.npu_weight, 200);
        assert_eq!(cfg.embedding.gpu_weight, 75);
        assert_eq!(cfg.embedding.cpu_weight, 5);
    }

    #[test]
    fn test_load_partial_toml_uses_defaults_for_missing_fields() {
        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[embedding]
cpu_enabled = false
"#
        )
        .unwrap();

        let cfg = DeviceConfig::load(f.path()).unwrap();
        assert!(cfg.embedding.npu_enabled);      // default
        assert!(cfg.embedding.gpu_enabled);      // default
        assert!(!cfg.embedding.cpu_enabled);     // overridden
        assert_eq!(cfg.embedding.npu_weight, 100); // default
    }

    #[test]
    fn test_load_empty_toml_uses_all_defaults() {
        let f = NamedTempFile::new().unwrap();
        let cfg = DeviceConfig::load(f.path()).unwrap();
        assert!(cfg.embedding.npu_enabled);
        assert_eq!(cfg.embedding.npu_weight, 100);
        assert_eq!(cfg.embedding.gpu_weight, 50);
        assert_eq!(cfg.embedding.cpu_weight, 10);
    }
}
