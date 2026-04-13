//! Shared helpers for u-forge.ai CLI examples.
//!
//! Included via `#[path = "common/mod.rs"] mod common;` in each example.
//! Contains demo-specific config loading and CLI argument resolution.
//! Core orchestration (graph setup, embedding, HQ queue construction) lives
//! in `u_forge_core::ingest`.

use anyhow::Result;

// ── Config ────────────────────────────────────────────────────────────────────

/// Database path + clear-on-start flag shared by both demo config files.
#[derive(serde::Deserialize, Default)]
pub struct DatabaseConfig {
    /// Path to the persistent database directory.  Relative paths are resolved
    /// from the current working directory.  Defaults to `./demo_data/kg`.
    pub path: Option<String>,
    /// When `true`, clear all data from the database before loading.
    #[serde(default)]
    pub clear: bool,
}

/// Load a TOML file into any `serde::Deserialize` type.
///
/// Both examples define their own top-level config struct (with different
/// optional sections) but share the same file-read + parse logic.
pub fn load_toml_config<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Could not read config file '{path}': {e}"))?;
    toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("Could not parse config file '{path}': {e}"))
}

// ── Argument parsing ──────────────────────────────────────────────────────────

/// Resolved CLI arguments common to both demo examples.
pub struct DemoArgs {
    /// Path to the JSONL data file.
    pub data_file: String,
    /// Path to the schema directory.
    pub schema_dir: String,
    /// Path to the TOML demo config file, if one was found.
    pub config_path: Option<String>,
    /// True when `--help` / `-h` was passed.
    pub help_requested: bool,
}

/// Resolve the standard argument set used by both demo examples.
///
/// Priority for each value (first wins):
/// 1. Positional CLI argument
/// 2. Environment variable (`UFORGE_DATA_FILE` / `UFORGE_SCHEMA_DIR`)
/// 3. Path relative to `CARGO_MANIFEST_DIR` (compile-time default)
///
/// Config path priority:
/// 1. `--config <path>` flag
/// 2. `UFORGE_DEMO_CONFIG` env var
/// 3. `defaults/demo_config.toml` next to the crate manifest (if it exists)
pub fn resolve_demo_args() -> DemoArgs {
    let args: Vec<String> = std::env::args().collect();

    let help_requested = args.iter().any(|a| a == "--help" || a == "-h");

    let data_file = args
        .get(1)
        .cloned()
        .or_else(|| std::env::var("UFORGE_DATA_FILE").ok())
        .unwrap_or_else(|| {
            format!(
                "{}/../../defaults/data/memory.json",
                env!("CARGO_MANIFEST_DIR")
            )
        });

    let schema_dir = args
        .get(2)
        .cloned()
        .or_else(|| std::env::var("UFORGE_SCHEMA_DIR").ok())
        .unwrap_or_else(|| format!("{}/../../defaults/schemas", env!("CARGO_MANIFEST_DIR")));

    let default_config_path = format!(
        "{}/../../defaults/demo_config.toml",
        env!("CARGO_MANIFEST_DIR")
    );

    let config_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| w[1].clone())
        .or_else(|| std::env::var("UFORGE_DEMO_CONFIG").ok())
        .or_else(|| {
            if std::path::Path::new(&default_config_path).exists() {
                Some(default_config_path)
            } else {
                None
            }
        });

    DemoArgs {
        data_file,
        schema_dir,
        config_path,
        help_requested,
    }
}
