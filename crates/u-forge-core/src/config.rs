//! Application configuration — devices, model limits, and other tunables.
//!
//! Loaded from a TOML file at startup.  All fields have sensible defaults so
//! the file is entirely optional; the project runs correctly with zero config.
//!
//! # File locations (checked in order)
//!
//! 1. `./u-forge.toml` (working directory)
//! 2. `$XDG_CONFIG_HOME/u-forge/config.toml`
//!    (falls back to `$HOME/.config/u-forge/config.toml` on Linux)
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
//!
//! [models.context_limits]
//! "embed-gemma-300m-FLM"                = 2048
//! "some-new-model-FLM"                  = 4096
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::lemonade::load::ModelLoadOptions;

// ── EmbeddingDeviceConfig ─────────────────────────────────────────────────────

/// Per-device settings for the embedding subsystem.
///
/// Corresponds to the `[embedding]` section of `u-forge.toml`.
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

    /// Enable high-quality 4096-dim embedding via `Qwen3-Embedding-8B-GGUF`.
    ///
    /// When `true`, the registry includes the Qwen3 model and embeddings are
    /// stored in the `chunks_vec_hq` 4096-dim index alongside the standard
    /// 768-dim `chunks_vec` index.  NPU embedding should typically be disabled
    /// when this is active (the NPU model only produces 768-dim vectors).
    #[serde(default)]
    pub high_quality_embedding: bool,

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
            high_quality_embedding: false,
            npu_weight: default_npu_weight(),
            gpu_weight: default_gpu_weight(),
            cpu_weight: default_cpu_weight(),
        }
    }
}

// ── ModelLoadParams ───────────────────────────────────────────────────────────

/// Per-model load parameters stored in `u-forge.toml` under `[models.load_params]`.
///
/// All fields are optional; unset fields fall back to server defaults.
///
/// # Example TOML
///
/// ```toml
/// [models.load_params]
/// "embed-gemma-300m-FLM"    = { ctx_size = 2048 }
/// "bge-reranker-v2-m3-GGUF" = { ctx_size = 8192, batch_size = 512, ubatch_size = 512 }
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelLoadParams {
    /// Context window size in tokens passed to `POST /api/v1/load`.
    pub ctx_size: Option<usize>,

    /// Physical batch size for prompt processing (`--batch-size`).
    ///
    /// Applies only to llamacpp GGUF models.
    pub batch_size: Option<usize>,

    /// Micro-batch size (`--ubatch-size`).
    ///
    /// When `None` and `ctx_size` is set, `--ubatch-size` is auto-injected to
    /// match `ctx_size`.  Set this explicitly to use a different value.
    pub ubatch_size: Option<usize>,
}

// ── ModelConfig ───────────────────────────────────────────────────────────────

/// Model-level settings, primarily per-model load-parameter overrides.
///
/// Corresponds to the `[models]` section of `u-forge.toml`.
///
/// Built-in defaults cover all models shipped with u-forge.  Add entries to
/// `u-forge.toml` under `[models.load_params]` to tune new models without
/// recompiling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Per-model load parameters (ctx window, batch sizes).
    ///
    /// Keys are the exact model IDs reported by Lemonade Server.
    #[serde(default = "default_model_load_params")]
    pub load_params: HashMap<String, ModelLoadParams>,

    /// Models considered "high quality" for embedding (e.g. 4096-dim Qwen3).
    ///
    /// Models listed here receive [`QualityTier::High`] from `ModelSelector`
    /// and are routed to the separate HQ embedding queue/index.
    #[serde(default = "default_hq_embedding_models")]
    pub high_quality_embedding_models: Vec<String>,

    /// Preferred llamacpp backend order.  First installed backend wins.
    ///
    /// Default: `["rocm", "vulkan", "cpu"]`.
    #[serde(default = "default_llamacpp_backend_preference")]
    pub llamacpp_backend_preference: Vec<String>,

    /// Preference list for embedding models.  First downloaded match wins.
    ///
    /// When empty (user has not overridden), any downloaded model with the
    /// `"embeddings"` label is eligible; the list only controls ordering.
    #[serde(default = "default_embedding_model_preferences")]
    pub embedding_model_preferences: Vec<String>,

    /// Preference list for reranker models.
    #[serde(default = "default_reranker_model_preferences")]
    pub reranker_model_preferences: Vec<String>,

    /// Preference list for STT models.
    #[serde(default = "default_stt_model_preferences")]
    pub stt_model_preferences: Vec<String>,

    /// Preference list for LLM models.
    #[serde(default = "default_llm_model_preferences")]
    pub llm_model_preferences: Vec<String>,

    /// Preference list for TTS models.
    #[serde(default = "default_tts_model_preferences")]
    pub tts_model_preferences: Vec<String>,
}

impl ModelConfig {
    /// Build a [`ModelLoadOptions`] for `model_id` from the configured params.
    ///
    /// Returns an all-`None` (server-default) `ModelLoadOptions` when the
    /// model is not listed in `[models.load_params]`.
    pub fn load_options_for(&self, model_id: &str) -> ModelLoadOptions {
        match self.load_params.get(model_id) {
            Some(p) => ModelLoadOptions {
                ctx_size: p.ctx_size,
                batch_size: p.batch_size,
                ubatch_size: p.ubatch_size,
                ..Default::default()
            },
            None => ModelLoadOptions::default(),
        }
    }

    /// Return the configured context-window size for `model_id`.
    ///
    /// Falls back to [`DEFAULT_EMBEDDING_CONTEXT_TOKENS`](crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS)
    /// when the model is not listed or its `ctx_size` is unset.
    pub fn ctx_size_for(&self, model_id: &str) -> usize {
        self.load_params
            .get(model_id)
            .and_then(|p| p.ctx_size)
            .unwrap_or(crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS)
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            load_params: default_model_load_params(),
            high_quality_embedding_models: default_hq_embedding_models(),
            llamacpp_backend_preference: default_llamacpp_backend_preference(),
            embedding_model_preferences: default_embedding_model_preferences(),
            reranker_model_preferences: default_reranker_model_preferences(),
            stt_model_preferences: default_stt_model_preferences(),
            llm_model_preferences: default_llm_model_preferences(),
            tts_model_preferences: default_tts_model_preferences(),
        }
    }
}

// ── ChatConfig ────────────────────────────────────────────────────────────────

/// Which device to use for LLM chat inference.
///
/// Corresponds to the `preferred_device` key in the `[chat]` section of
/// `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatDevice {
    /// Let u-forge choose based on available hardware.
    ///
    /// Currently resolves to `gpu`.  A smarter selection policy (latency,
    /// model quality, task complexity) will be added in a future release.
    #[default]
    Auto,
    /// AMD/Nvidia GPU — llamacpp GGUF models via ROCm / Vulkan / CUDA.
    Gpu,
    /// AMD NPU — FLM models via the Ryzen AI stack.
    Npu,
    /// Host CPU — llamacpp GGUF models, lowest power.
    Cpu,
}

/// Per-device LLM model and generation overrides.
///
/// Corresponds to `[chat.gpu]`, `[chat.npu]`, and `[chat.cpu]` in
/// `u-forge.toml`.  All fields are optional; `None` falls back to the
/// provider default baked into [`LemonadeChatProvider`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatDeviceConfig {
    /// Override the model id for this device (e.g. `"Gemma-4-26B-A4B-it-GGUF"`).
    ///
    /// When `None`, the model is auto-selected from the Lemonade registry.
    pub model: Option<String>,

    /// Token ceiling for generation requests on this device.
    pub max_tokens: Option<u32>,

    /// Sampling temperature (0.0 = deterministic, 2.0 = very creative).
    /// Lower values make tool calls more reliable.
    pub temperature: Option<f32>,

    /// Nucleus sampling: only consider tokens whose cumulative probability
    /// exceeds this threshold (0.0–1.0). Lower = more focused.
    pub top_p: Option<f32>,

    /// Top-k sampling: only consider the k most likely tokens.
    /// Supported by llama.cpp backends. 0 = disabled.
    pub top_k: Option<u32>,

    /// Min-p sampling: discard tokens with probability below
    /// `min_p * max_token_probability`. Supported by llama.cpp backends.
    pub min_p: Option<f32>,

    /// Penalise tokens that have already appeared in the output,
    /// scaled by how often they appeared (-2.0 to 2.0). Reduces repetition.
    pub frequency_penalty: Option<f32>,

    /// Penalise tokens that have appeared at all in the output (-2.0 to 2.0).
    /// Encourages topic diversity.
    pub presence_penalty: Option<f32>,

    /// Repetition penalty (llama.cpp style, typically 1.0–1.5).
    /// Values > 1.0 discourage repeating previous tokens.
    pub repetition_penalty: Option<f32>,

    /// RNG seed for reproducible generation. Same seed + same prompt
    /// should yield the same output (backend-dependent).
    pub seed: Option<u64>,

    /// Stop sequences: generation halts when any of these strings is emitted.
    pub stop: Option<Vec<String>>,
}

/// Global chat / RAG settings.
///
/// Corresponds to the `[chat]` section of `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConfig {
    /// Preferred inference device (`auto` | `gpu` | `npu` | `cpu`).
    #[serde(default)]
    pub preferred_device: ChatDevice,

    /// GPU device overrides — model, token limit, temperature.
    #[serde(default)]
    pub gpu: ChatDeviceConfig,

    /// NPU device overrides — model, token limit, temperature.
    #[serde(default)]
    pub npu: ChatDeviceConfig,

    /// CPU device overrides — model, token limit, temperature.
    #[serde(default)]
    pub cpu: ChatDeviceConfig,

    /// System prompt sent to the LLM before every conversation.
    #[serde(default = "ChatConfig::default_system_prompt")]
    pub system_prompt: String,

    /// Maximum number of prior turns (user + assistant pairs) kept in context.
    ///
    /// Used as a coarse fallback when token counting is unavailable (e.g. the
    /// direct streaming path). Prefer `max_context_tokens` for the agent path.
    #[serde(default = "ChatConfig::default_max_history_turns")]
    pub max_history_turns: usize,

    /// Total context-window budget in tokens.
    ///
    /// History is trimmed to the most-recent messages that fit inside
    /// `max_context_tokens - response_reserve` tokens (see `response_reserve`).
    /// Set this to match your model's actual context window (e.g. 8192 for an
    /// 8 k model). Defaults to 4096 — conservative enough for most local models.
    #[serde(default = "ChatConfig::default_max_context_tokens")]
    pub max_context_tokens: usize,

    /// Number of tokens reserved for the model's response.
    ///
    /// Deducted from `max_context_tokens` when computing how much history fits.
    /// Defaults to 1024.
    #[serde(default = "ChatConfig::default_response_reserve")]
    pub response_reserve: usize,

    /// Hybrid-search balance: 0.0 = FTS5-only, 1.0 = semantic-only.
    #[serde(default = "ChatConfig::default_alpha")]
    pub alpha: f32,

    /// Number of knowledge-graph nodes returned per query.
    #[serde(default = "ChatConfig::default_search_limit")]
    pub search_limit: usize,

    /// RRF score multiplier for the high-quality 4096-dim semantic path.
    ///
    /// See [`HybridSearchConfig::hq_semantic_boost`] for full semantics.
    #[serde(default = "ChatConfig::default_hq_semantic_boost")]
    pub hq_semantic_boost: f32,

    /// Maximum tool-call round-trips the agent may make per user message.
    ///
    /// Each "turn" is one LLM call that may invoke tools; the agent loop
    /// stops after this many turns even if the model wants to call more.
    /// Defaults to 5.
    #[serde(default = "ChatConfig::default_max_tool_turns")]
    pub max_tool_turns: usize,
}

impl ChatConfig {
    /// Return the device config that matches the current `preferred_device`.
    ///
    /// `Auto` currently resolves to `Gpu`.  When a smarter selection policy
    /// lands, this method is the single place to update.
    pub fn active_device_config(&self) -> &ChatDeviceConfig {
        match self.preferred_device {
            ChatDevice::Auto | ChatDevice::Gpu => &self.gpu,
            ChatDevice::Npu => &self.npu,
            ChatDevice::Cpu => &self.cpu,
        }
    }

    fn default_system_prompt() -> String {
        "You are a knowledgeable assistant for a TTRPG worldbuilding tool. \
         Answer questions accurately based on the provided knowledge graph context. \
         Be concise and informative."
            .to_string()
    }

    fn default_max_history_turns() -> usize {
        10
    }

    fn default_max_context_tokens() -> usize {
        4096
    }

    fn default_response_reserve() -> usize {
        1024
    }

    fn default_alpha() -> f32 {
        0.5
    }

    fn default_search_limit() -> usize {
        3
    }

    fn default_hq_semantic_boost() -> f32 {
        3.0
    }

    fn default_max_tool_turns() -> usize {
        5
    }
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            preferred_device: ChatDevice::Auto,
            gpu: ChatDeviceConfig::default(),
            npu: ChatDeviceConfig::default(),
            cpu: ChatDeviceConfig::default(),
            system_prompt: Self::default_system_prompt(),
            max_history_turns: Self::default_max_history_turns(),
            max_context_tokens: Self::default_max_context_tokens(),
            response_reserve: Self::default_response_reserve(),
            alpha: Self::default_alpha(),
            search_limit: Self::default_search_limit(),
            hq_semantic_boost: Self::default_hq_semantic_boost(),
            max_tool_turns: Self::default_max_tool_turns(),
        }
    }
}

// ── AppConfig ─────────────────────────────────────────────────────────────────

// ── StorageConfig ─────────────────────────────────────────────────────────────

/// Storage / persistence settings.
///
/// Corresponds to the `[storage]` section of `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Path to the SQLite database directory.
    ///
    /// Defaults to `./data/db/` relative to the working directory.
    #[serde(default = "StorageConfig::default_db_path")]
    pub db_path: PathBuf,
}

impl StorageConfig {
    fn default_db_path() -> PathBuf {
        PathBuf::from("./data/db")
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: Self::default_db_path(),
        }
    }
}

// ── DataConfig ────────────────────────────────────────────────────────────────

/// Data import settings.
///
/// Corresponds to the `[data]` section of `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataConfig {
    /// Path to the JSONL file loaded on startup (and by File > Import Data).
    ///
    /// Defaults to `./defaults/data/memory.json` relative to the working
    /// directory.  Override in `u-forge.toml` to point at your own world file.
    ///
    /// # Example
    /// ```toml
    /// [data]
    /// import_file = "./my-campaign/world.jsonl"
    /// ```
    #[serde(default = "DataConfig::default_import_file")]
    pub import_file: PathBuf,

    /// Directory containing `*.schema.json` files.
    ///
    /// Defaults to `./defaults/schemas`.
    #[serde(default = "DataConfig::default_schema_dir")]
    pub schema_dir: PathBuf,
}

impl DataConfig {
    fn default_import_file() -> PathBuf {
        PathBuf::from("./defaults/data/memory.json")
    }

    fn default_schema_dir() -> PathBuf {
        PathBuf::from("./defaults/schemas")
    }
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            import_file: Self::default_import_file(),
            schema_dir: Self::default_schema_dir(),
        }
    }
}

// ── UiConfig ──────────────────────────────────────────────────────────────────

/// UI / display settings.
///
/// Corresponds to the `[ui]` section of `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Base font size in pixels, used as the rem unit for all UI text.
    ///
    /// GPUI's semantic text sizes (`text_xs`, `text_sm`, etc.) scale relative
    /// to this value:
    /// - `text_xs` = 0.75 × font_size  (labels, timestamps, captions)
    /// - `text_sm` = 0.875 × font_size (body, menu items, panel headers)
    ///
    /// Defaults to `16.0` (standard web/desktop baseline).  Increase for
    /// high-DPI displays or accessibility; decrease for a more compact UI.
    #[serde(default = "UiConfig::default_font_size")]
    pub font_size: f32,
}

impl UiConfig {
    fn default_font_size() -> f32 {
        16.0
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            font_size: Self::default_font_size(),
        }
    }
}

// ── AppConfig ─────────────────────────────────────────────────────────────────

/// Top-level application configuration.
///
/// Loaded from `u-forge.toml` by [`AppConfig::load_default`].
/// Use [`AppConfig::default`] when no config file is present or required.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    /// Embedding-specific device settings.
    #[serde(default)]
    pub embedding: EmbeddingDeviceConfig,

    /// Model-level settings (context-window limits, etc.).
    #[serde(default)]
    pub models: ModelConfig,

    /// Global chat / RAG settings.
    #[serde(default)]
    pub chat: ChatConfig,

    /// Storage / persistence settings.
    #[serde(default)]
    pub storage: StorageConfig,

    /// Data import settings.
    #[serde(default)]
    pub data: DataConfig,

    /// UI / display settings.
    #[serde(default)]
    pub ui: UiConfig,
}

impl AppConfig {
    /// Load from a specific TOML file path.
    ///
    /// Returns `Ok(AppConfig::default())` if the file does not exist, so
    /// callers never need to treat a missing config file as an error.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let text = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&text)?;

        info!(path = %path.display(), "AppConfig loaded");

        Ok(config)
    }

    /// Load from the canonical search path, returning defaults if nothing is found.
    ///
    /// Search order:
    /// 1. `./u-forge.toml`
    /// 2. `$XDG_CONFIG_HOME/u-forge/config.toml`
    ///    (or `$HOME/.config/u-forge/config.toml` on Linux)
    /// 3. Built-in defaults
    pub fn load_default() -> Self {
        for path in Self::candidate_paths() {
            match Self::load(&path) {
                Ok(cfg) => return cfg,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "AppConfig: failed to load — skipping"
                    );
                }
            }
        }

        info!("AppConfig: no config file found — using defaults");
        Self::default()
    }

    /// Ordered list of paths to check when loading the default config.
    fn candidate_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from("u-forge.toml")];

        // XDG_CONFIG_HOME / fallback to $HOME/.config
        let xdg_base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs_or_home()
                    .map(|h| h.join(".config"))
                    .unwrap_or_default()
            });

        if !xdg_base.as_os_str().is_empty() {
            paths.push(xdg_base.join("u-forge").join("config.toml"));
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

fn default_hq_embedding_models() -> Vec<String> {
    vec!["Qwen3-Embedding-8B-GGUF".to_string()]
}

fn default_llamacpp_backend_preference() -> Vec<String> {
    vec!["rocm".to_string(), "vulkan".to_string(), "cpu".to_string()]
}

fn default_embedding_model_preferences() -> Vec<String> {
    vec![
        "embed-gemma-300m-FLM".to_string(),
        "user.ggml-org/embeddinggemma-300M-GGUF".to_string(),
    ]
}

fn default_reranker_model_preferences() -> Vec<String> {
    vec!["bge-reranker-v2-m3-GGUF".to_string()]
}

fn default_stt_model_preferences() -> Vec<String> {
    vec![
        "whisper-v3-turbo-FLM".to_string(),
        "Whisper-Large-v3-Turbo".to_string(),
    ]
}

fn default_llm_model_preferences() -> Vec<String> {
    vec![
        "Gemma-4-26B-A4B-it-GGUF".to_string(),
        "qwen3.5-4B-FLM".to_string(),
    ]
}

fn default_tts_model_preferences() -> Vec<String> {
    vec!["kokoro-v1".to_string()]
}

/// Built-in load parameters for known models.
fn default_model_load_params() -> HashMap<String, ModelLoadParams> {
    fn ctx(ctx_size: usize) -> ModelLoadParams {
        ModelLoadParams {
            ctx_size: Some(ctx_size),
            ..Default::default()
        }
    }
    let mut m = HashMap::new();
    m.insert("embed-gemma-300m-FLM".to_string(), ctx(2048));
    m.insert("embed-gemma-300M-GGUF".to_string(), ctx(2048));
    m.insert(
        "user.ggml-org/embeddinggemma-300M-GGUF".to_string(),
        ctx(2048),
    );
    m.insert("Qwen3-Embedding-8B-GGUF".to_string(), ctx(32768));
    m.insert("bge-reranker-v2-m3-GGUF".to_string(), ctx(8192));
    m
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_values() {
        let cfg = AppConfig::default();
        assert!(cfg.embedding.npu_enabled);
        assert!(cfg.embedding.gpu_enabled);
        assert!(cfg.embedding.cpu_enabled);
        assert_eq!(cfg.embedding.npu_weight, 100);
        assert_eq!(cfg.embedding.gpu_weight, 50);
        assert_eq!(cfg.embedding.cpu_weight, 10);
    }

    #[test]
    fn test_default_model_load_params() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.models.ctx_size_for("embed-gemma-300m-FLM"), 2048);
        assert_eq!(
            cfg.models
                .ctx_size_for("user.ggml-org/embeddinggemma-300M-GGUF"),
            2048
        );
        assert_eq!(cfg.models.ctx_size_for("bge-reranker-v2-m3-GGUF"), 8192);
        assert_eq!(cfg.models.ctx_size_for("Qwen3-Embedding-8B-GGUF"), 32768);
        // Unknown model falls back to default
        assert_eq!(
            cfg.models.ctx_size_for("unknown-model-GGUF"),
            crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS
        );
    }

    #[test]
    fn test_load_options_for_returns_full_params() {
        let cfg = AppConfig::default();
        let opts = cfg.models.load_options_for("bge-reranker-v2-m3-GGUF");
        assert_eq!(opts.ctx_size, Some(8192));
        // Default entry has no explicit batch/ubatch
        assert!(opts.batch_size.is_none());
        assert!(opts.ubatch_size.is_none());
    }

    #[test]
    fn test_load_options_for_unknown_model_returns_defaults() {
        let cfg = AppConfig::default();
        let opts = cfg.models.load_options_for("unknown-model-GGUF");
        assert!(opts.ctx_size.is_none());
        assert!(opts.batch_size.is_none());
        assert!(opts.ubatch_size.is_none());
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let path = PathBuf::from("/tmp/u-forge-nonexistent-config-xyz.toml");
        let cfg = AppConfig::load(&path).unwrap();
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

        let cfg = AppConfig::load(f.path()).unwrap();
        assert!(cfg.embedding.npu_enabled);
        assert!(cfg.embedding.gpu_enabled);
        assert!(!cfg.embedding.cpu_enabled);
        assert_eq!(cfg.embedding.npu_weight, 200);
        assert_eq!(cfg.embedding.gpu_weight, 75);
        assert_eq!(cfg.embedding.cpu_weight, 5);
    }

    #[test]
    fn test_load_model_load_params_from_toml() {
        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[models.load_params]
"embed-gemma-300m-FLM"    = {{ ctx_size = 1024 }}
"my-custom-model-FLM"     = {{ ctx_size = 8192 }}
"bge-reranker-v2-m3-GGUF" = {{ ctx_size = 8192, batch_size = 512, ubatch_size = 512 }}
"#
        )
        .unwrap();

        let cfg = AppConfig::load(f.path()).unwrap();
        assert_eq!(cfg.models.ctx_size_for("embed-gemma-300m-FLM"), 1024);
        assert_eq!(cfg.models.ctx_size_for("my-custom-model-FLM"), 8192);

        let rerank_opts = cfg.models.load_options_for("bge-reranker-v2-m3-GGUF");
        assert_eq!(rerank_opts.ctx_size, Some(8192));
        assert_eq!(rerank_opts.batch_size, Some(512));
        assert_eq!(rerank_opts.ubatch_size, Some(512));
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

        let cfg = AppConfig::load(f.path()).unwrap();
        assert!(cfg.embedding.npu_enabled); // default
        assert!(cfg.embedding.gpu_enabled); // default
        assert!(!cfg.embedding.cpu_enabled); // overridden
        assert_eq!(cfg.embedding.npu_weight, 100); // default
    }

    #[test]
    fn test_load_empty_toml_uses_all_defaults() {
        let f = NamedTempFile::new().unwrap();
        let cfg = AppConfig::load(f.path()).unwrap();
        assert!(cfg.embedding.npu_enabled);
        assert_eq!(cfg.embedding.npu_weight, 100);
        assert_eq!(cfg.embedding.gpu_weight, 50);
        assert_eq!(cfg.embedding.cpu_weight, 10);
    }
}
