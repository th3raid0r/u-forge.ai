//! Application configuration — devices, model limits, and other tunables.
//!
//! The desktop application creates and loads one canonical per-user file at
//! `$XDG_CONFIG_HOME/u-forge/u-forge.toml` (falling back to
//! `$HOME/.config/u-forge/u-forge.toml`). Durable application data follows
//! `XDG_DATA_HOME`; regenerable Lemonade state follows `XDG_CACHE_HOME`.
//!
//! # Example file
//!
//! ```toml
//! [lemonade]
//! max_loaded_models = 3
//!
//! [embedding]
//! npu_enabled  = true
//! gpu_enabled  = true
//! cpu_enabled  = false   # disable CPU worker when GPU handles llamacpp
//! npu_weight   = 100
//! gpu_weight   = 50
//! cpu_weight   = 10
//!
//! [models.load_params]
//! "embed-gemma-300m-FLM" = { ctx_size = 2048 }
//! "some-new-model-FLM"   = { ctx_size = 4096 }
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

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::lemonade::load::ModelLoadOptions;

const DEFAULTS_REVISION: u32 = 1;

/// Canonical per-user paths used by the desktop application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub config_file: PathBuf,
    pub defaults_dir: PathBuf,
    pub db_dir: PathBuf,
}

impl UserPaths {
    /// Resolve the XDG bases. Relative XDG values are invalid and therefore
    /// fall back to the corresponding directory below an absolute `$HOME`.
    pub fn discover() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| anyhow!("cannot determine an absolute HOME for u-forge user data"))?;
        let config_base =
            absolute_env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let data_base =
            absolute_env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        let cache_base = absolute_env_path("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));
        Ok(Self::from_bases(config_base, data_base, cache_base))
    }

    fn from_bases(config_base: PathBuf, data_base: PathBuf, cache_base: PathBuf) -> Self {
        let config_dir = config_base.join("u-forge");
        let data_dir = data_base.join("u-forge");
        let cache_dir = cache_base.join("u-forge");
        Self {
            config_file: config_dir.join("u-forge.toml"),
            defaults_dir: data_dir.join("defaults"),
            db_dir: data_dir.join("db"),
            config_dir,
            data_dir,
            cache_dir,
        }
    }

    fn initialize(&self, packaged_defaults: &Path) -> Result<()> {
        if !packaged_defaults.is_dir() {
            bail!(
                "packaged defaults directory is missing: {}",
                packaged_defaults.display()
            );
        }
        std::fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))?;
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating {}", self.data_dir.display()))?;
        std::fs::create_dir_all(&self.db_dir)
            .with_context(|| format!("creating {}", self.db_dir.display()))?;
        std::fs::create_dir_all(self.cache_dir.join("lemonade"))
            .with_context(|| format!("creating {}", self.cache_dir.display()))?;

        self.seed_defaults(packaged_defaults)?;
        if !self.config_file.exists() {
            self.write_config_template(packaged_defaults)?;
        }
        Ok(())
    }

    fn seed_defaults(&self, packaged_defaults: &Path) -> Result<()> {
        let marker = self.data_dir.join(".defaults-revision");
        if marker.exists() {
            let revision = std::fs::read_to_string(&marker)
                .with_context(|| format!("reading {}", marker.display()))?
                .trim()
                .parse::<u32>()
                .with_context(|| format!("parsing {}", marker.display()))?;
            if revision > DEFAULTS_REVISION {
                bail!(
                    "user defaults revision {revision} is newer than supported revision {DEFAULTS_REVISION}"
                );
            }
            // Every future revision must add an explicit migration here.
            if revision == DEFAULTS_REVISION {
                return Ok(());
            }
            bail!("no defaults migration is defined from revision {revision}");
        }

        for name in ["schemas", "example_data"] {
            let source = packaged_defaults.join(name);
            if !source.is_dir() {
                bail!("packaged defaults are missing {}", source.display());
            }
            copy_missing_tree(&source, &self.defaults_dir.join(name))?;
        }
        atomic_write(&marker, format!("{DEFAULTS_REVISION}\n").as_bytes())
    }

    fn write_config_template(&self, packaged_defaults: &Path) -> Result<()> {
        use toml_edit::value;

        let template = packaged_defaults.join("config/u-forge.toml");
        let text = std::fs::read_to_string(&template)
            .with_context(|| format!("reading config template {}", template.display()))?;
        let mut document = text
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("parsing config template {}", template.display()))?;
        document["storage"]["db_path"] = value(path_text(&self.db_dir));
        document["data"]["import_file"] = value(path_text(
            &self
                .defaults_dir
                .join("example_data/foundation-example.jsonl"),
        ));
        document["data"]["schema_dir"] =
            value(path_text(&self.defaults_dir.join("schemas/Sine Nomine")));
        atomic_write(&self.config_file, document.to_string().as_bytes())
    }
}

fn absolute_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn copy_missing_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("creating {}", destination.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("reading {}", source.display()))?
    {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_missing_tree(&entry.path(), &target)?;
        } else if !target.exists() {
            let bytes = std::fs::read(entry.path())?;
            atomic_write(&target, &bytes)?;
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("u-forge"),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

/// Lemonade Server runtime settings managed by u-forge for its owned server.
///
/// Corresponds to the `[lemonade]` section of `u-forge.toml`. External
/// Lemonade processes remain operator-owned and are never mutated implicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LemonadeConfig {
    /// Maximum simultaneously loaded models per model type.
    ///
    /// Three permits the standard CPU/GPU embedding model, optional NPU
    /// embedding model, and high-quality embedding model to remain resident.
    #[serde(default = "LemonadeConfig::default_max_loaded_models")]
    pub max_loaded_models: usize,
}

impl LemonadeConfig {
    pub const fn default_max_loaded_models() -> usize {
        1
    }
}

impl Default for LemonadeConfig {
    fn default() -> Self {
        Self {
            max_loaded_models: Self::default_max_loaded_models(),
        }
    }
}

// ── EmbeddingDeviceConfig ─────────────────────────────────────────────────────

/// Per-device settings for the embedding subsystem.
///
/// Corresponds to the `[embedding]` section of `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// When `true`, downloaded configured HQ models are eligible and embeddings
    /// are stored in the `chunks_vec_hq` 4096-dim index alongside the standard
    /// 768-dim `chunks_vec` index.
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
#[serde(deny_unknown_fields)]
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

    /// Additional safe arguments forwarded to llama-server.
    pub llamacpp_args: Option<String>,
}

// ── ModelConfig ───────────────────────────────────────────────────────────────

/// Model-level settings, primarily per-model load-parameter overrides.
///
/// Corresponds to the `[models]` section of `u-forge.toml`.
///
/// Built-in defaults cover the preferred Lemonade catalog models. Additional
/// entries under `[models.load_params]` configure models without recompiling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
                llamacpp_args: p.llamacpp_args.clone(),
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
    /// Currently resolves to `gpu`; automatic selection does not score latency,
    /// model quality, or task complexity.
    #[default]
    Auto,
    /// AMD/Nvidia GPU — llamacpp GGUF models via ROCm / Vulkan / CUDA.
    Gpu,
    /// AMD NPU — FLM models via the Ryzen AI stack.
    Npu,
    /// Host CPU — llamacpp GGUF models, lowest power.
    Cpu,
}

/// How reasoning policy affects the loaded Lemonade model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningControl {
    /// Use Lemonade's request-scoped `enable_thinking` control.
    #[default]
    Request,
    /// Reload reasoning-capable llama.cpp models with managed template kwargs.
    Reload,
}

/// Per-device LLM model and generation overrides.
///
/// Corresponds to `[chat.gpu]`, `[chat.npu]`, and `[chat.cpu]` in
/// `u-forge.toml`.  All fields are optional; `None` falls back to the
/// provider default baked into [`LemonadeChatProvider`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
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

/// Limits applied to one multi-turn graph-agent request.
///
/// These values live below `[chat.agent]` so provider sampling and agent-loop
/// safety remain independently configurable. Model-specific context limits are
/// reconciled at activation time by [`AgentBudgetConfig::reconcile`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentBudgetConfig {
    /// Legacy schema-summary cap retained for configuration compatibility.
    ///
    /// Schema records are now admitted against the active model context rather
    /// than an independent static ceiling.
    #[serde(
        default = "AgentBudgetConfig::default_schema_summary_tokens",
        skip_serializing
    )]
    pub schema_summary_tokens: usize,

    /// Legacy cumulative request cap retained only for configuration compatibility.
    ///
    /// Per-request cumulative caps proved too aggressive for multi-turn agents.
    /// The value is accepted from older TOML files but is no longer enforced or
    /// written by the settings persistence path.
    #[serde(default, skip_serializing)]
    pub cumulative_request_tokens: Option<usize>,

    /// Legacy cumulative tool-output cap retained only for configuration
    /// compatibility. Individual tool results are now bounded against the active
    /// model window instead.
    #[serde(default, skip_serializing)]
    pub cumulative_tool_output_tokens: Option<usize>,

    /// Number of unchanged repeats allowed for one canonical tool call.
    #[serde(default = "AgentBudgetConfig::default_repeated_call_limit")]
    pub repeated_call_limit: usize,
}

impl AgentBudgetConfig {
    fn default_schema_summary_tokens() -> usize {
        768
    }

    fn default_repeated_call_limit() -> usize {
        1
    }

    /// Reconcile configured agent limits with the active model context.
    pub fn reconcile(
        &self,
        context: usize,
        _max_tool_turns: usize,
    ) -> anyhow::Result<EffectiveAgentBudget> {
        if context < 2 {
            anyhow::bail!("effective model context is too small for an agent request");
        }
        let mut diagnostics = Vec::new();

        if self.cumulative_request_tokens.is_some() || self.cumulative_tool_output_tokens.is_some()
        {
            diagnostics.push(
                "legacy cumulative agent token caps are ignored; each model call is fitted to the active context window"
                    .to_string(),
            );
        }

        Ok(EffectiveAgentBudget {
            context_tokens: context,
            repeated_call_limit: self.repeated_call_limit,
            diagnostics,
        })
    }
}

impl Default for AgentBudgetConfig {
    fn default() -> Self {
        Self {
            schema_summary_tokens: Self::default_schema_summary_tokens(),
            cumulative_request_tokens: None,
            cumulative_tool_output_tokens: None,
            repeated_call_limit: Self::default_repeated_call_limit(),
        }
    }
}

/// Active-model-safe limits consumed by `u-forge-agent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveAgentBudget {
    pub context_tokens: usize,
    pub repeated_call_limit: usize,
    pub diagnostics: Vec<String>,
}

impl Default for EffectiveAgentBudget {
    fn default() -> Self {
        AgentBudgetConfig::default()
            .reconcile(usize::MAX, 5)
            .expect("default agent budget is valid")
    }
}

/// Global chat / RAG settings.
///
/// Corresponds to the `[chat]` section of `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatConfig {
    /// Preferred inference device (`auto` | `gpu` | `npu` | `cpu`).
    #[serde(default)]
    pub preferred_device: ChatDevice,

    /// Whether reasoning is request-scoped or part of loaded-model identity.
    #[serde(default)]
    pub reasoning_control: ReasoningControl,

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
    /// Retained for persisted-config compatibility. Active chat requests use
    /// token fitting against an explicit Lemonade load context when present.
    #[serde(default = "ChatConfig::default_max_history_turns")]
    pub max_history_turns: usize,

    /// Legacy application-side context limit, retained only so older configs
    /// deserialize. Lemonade owns automatic context sizing when `ctx_size` is
    /// omitted, so this value is never an active request ceiling.
    #[serde(default = "ChatConfig::default_max_context_tokens", skip_serializing)]
    pub max_context_tokens: usize,

    /// Legacy fallback generation maximum.
    ///
    /// Per-device `max_tokens` is used when configured. This value is no longer
    /// subtracted from the model's input context.
    #[serde(default = "ChatConfig::default_response_reserve")]
    pub response_reserve: usize,

    /// Multi-turn graph-agent safety limits.
    #[serde(default)]
    pub agent: AgentBudgetConfig,

    /// Hybrid-search balance: 0.0 = FTS5-only, 1.0 = semantic-only.
    #[serde(default = "ChatConfig::default_alpha")]
    pub alpha: f32,

    /// Number of knowledge-graph nodes returned per query.
    #[serde(default = "ChatConfig::default_search_limit")]
    pub search_limit: usize,

    /// Maximum lexical candidates gathered before fusion.
    #[serde(default = "ChatConfig::default_candidate_limit")]
    pub fts_limit: usize,

    /// Maximum semantic candidates gathered before fusion.
    #[serde(default = "ChatConfig::default_candidate_limit")]
    pub semantic_limit: usize,

    /// Apply the configured cross-encoder reranker in hybrid searches.
    #[serde(default = "default_true")]
    pub rerank: bool,

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

    fn default_candidate_limit() -> usize {
        20
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
            reasoning_control: ReasoningControl::Request,
            gpu: ChatDeviceConfig::default(),
            npu: ChatDeviceConfig::default(),
            cpu: ChatDeviceConfig::default(),
            system_prompt: Self::default_system_prompt(),
            max_history_turns: Self::default_max_history_turns(),
            max_context_tokens: Self::default_max_context_tokens(),
            response_reserve: Self::default_response_reserve(),
            agent: AgentBudgetConfig::default(),
            alpha: Self::default_alpha(),
            search_limit: Self::default_search_limit(),
            fts_limit: Self::default_candidate_limit(),
            semantic_limit: Self::default_candidate_limit(),
            rerank: true,
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
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Path to the SQLite database directory.
    ///
    /// Defaults to `./data/db/` relative to the working directory.
    #[serde(default = "StorageConfig::default_db_path")]
    pub db_path: PathBuf,

    /// Standard embedding vector width for the SQLite retrieval index.
    ///
    /// Defaults to 768. Changing this requires rebuilding or re-indexing the
    /// database because sqlite-vec table dimensions are fixed at creation.
    #[serde(default = "StorageConfig::default_embedding_dimensions")]
    pub embedding_dimensions: usize,

    /// High-quality embedding vector width for the SQLite retrieval index.
    ///
    /// Defaults to 4096 and has the same rebuild requirement as the standard
    /// embedding lane.
    #[serde(default = "StorageConfig::default_high_quality_embedding_dimensions")]
    pub high_quality_embedding_dimensions: usize,
}

impl StorageConfig {
    fn default_db_path() -> PathBuf {
        PathBuf::from("./data/db")
    }

    fn default_embedding_dimensions() -> usize {
        crate::EMBEDDING_DIMENSIONS
    }

    fn default_high_quality_embedding_dimensions() -> usize {
        crate::HIGH_QUALITY_EMBEDDING_DIMENSIONS
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: Self::default_db_path(),
            embedding_dimensions: Self::default_embedding_dimensions(),
            high_quality_embedding_dimensions: Self::default_high_quality_embedding_dimensions(),
        }
    }
}

// ── DataConfig ────────────────────────────────────────────────────────────────

/// Data import settings.
///
/// Corresponds to the `[data]` section of `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataConfig {
    /// Path to the JSONL file loaded on startup (and by File > Import Data).
    ///
    /// Defaults to `./defaults/data/memory.jsonl` relative to the working
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
        PathBuf::from("./defaults/data/memory.jsonl")
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

/// Default logical-pixel baseline for interface geometry and icons.
pub const DEFAULT_UI_INTERFACE_SIZE: f32 = 22.0;

/// UI / display settings.
///
/// Corresponds to the `[ui]` section of `u-forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    /// Base content font size in pixels, used as the rem unit for application
    /// text without also forcing controls and icons to the same scale.
    ///
    /// GPUI's semantic text sizes (`text_xs`, `text_sm`, etc.) scale relative
    /// to this value:
    /// - `text_xs` = 0.75 × font_size  (labels, timestamps, captions)
    /// - `text_sm` = 0.875 × font_size (body, menu items, panel headers)
    ///
    /// Defaults to `16.0` (standard web/desktop baseline). Increase for text
    /// accessibility without changing the interface geometry.
    #[serde(default = "UiConfig::default_font_size")]
    pub font_size: f32,

    /// Independent interface baseline in logical pixels. Controls, panel
    /// chrome, spacing, and icons scale from this value while content text
    /// continues to use `font_size`.
    ///
    /// Defaults to `22.0`, matching the comfortable Zed-scale workspace used
    /// for the parity pass.
    #[serde(default = "UiConfig::default_interface_size")]
    pub interface_size: f32,

    /// Reveal diagnostics and low-level runtime controls intended for
    /// troubleshooting or expert configuration. Ordinary worldbuilding,
    /// relationships, import, model choice, and reasoning on/off remain
    /// available when this is false.
    #[serde(default)]
    pub show_advanced_controls: bool,

    /// Place application-rendered window controls on the left edge of a
    /// client-side title bar. The default follows the common Linux/Windows
    /// convention and keeps minimize, maximize, and close on the right.
    #[serde(default)]
    pub window_controls_left: bool,
}

impl UiConfig {
    fn default_font_size() -> f32 {
        16.0
    }

    fn default_interface_size() -> f32 {
        DEFAULT_UI_INTERFACE_SIZE
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            font_size: Self::default_font_size(),
            interface_size: Self::default_interface_size(),
            show_advanced_controls: false,
            window_controls_left: false,
        }
    }
}

// ── AppConfig ─────────────────────────────────────────────────────────────────

/// Top-level application configuration.
///
/// Loaded from the canonical XDG `u-forge.toml` by [`AppConfig::load_user`].
/// Use [`AppConfig::default`] when no config file is present or required.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// File this configuration was loaded from, or the per-user path selected
    /// for first persistence. Never serialized into TOML.
    #[serde(skip)]
    pub source_path: Option<PathBuf>,

    /// Runtime configuration for the app-owned Lemonade Server.
    #[serde(default)]
    pub lemonade: LemonadeConfig,

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
            return Ok(Self {
                source_path: Some(path.to_path_buf()),
                ..Self::default()
            });
        }

        let text = std::fs::read_to_string(path)?;
        let mut config: Self = toml::from_str(&text)?;
        config.source_path = Some(path.to_path_buf());

        info!(path = %path.display(), "AppConfig loaded");

        Ok(config)
    }

    /// Create the canonical XDG profile from packaged defaults when necessary,
    /// then load its single authoritative configuration file.
    pub fn load_user(packaged_defaults: &Path) -> Result<Self> {
        let paths = UserPaths::discover()?;
        paths.initialize(packaged_defaults)?;
        Self::load(&paths.config_file)
            .with_context(|| format!("loading user configuration {}", paths.config_file.display()))
    }

    fn per_user_config_path() -> Option<PathBuf> {
        UserPaths::discover().ok().map(|paths| paths.config_file)
    }

    /// Persist setup choices while preserving comments and unknown keys.
    pub fn persist_lemonade_setup(
        &self,
        high_quality_embedding: bool,
        preferred_device: ChatDevice,
        selected_chat_model: &str,
        reasoning_control: ReasoningControl,
    ) -> Result<PathBuf> {
        use toml_edit::{DocumentMut, Item, Table, value};

        let path = self
            .source_path
            .clone()
            .or_else(Self::per_user_config_path)
            .ok_or_else(|| anyhow::anyhow!("cannot determine a configuration path"))?;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut document = if text.trim().is_empty() {
            DocumentMut::new()
        } else {
            text.parse::<DocumentMut>()?
        };
        for section in ["embedding", "chat"] {
            if document.get(section).is_none_or(|item| !item.is_table()) {
                document[section] = Item::Table(Table::new());
            }
        }
        document["embedding"]["high_quality_embedding"] = value(high_quality_embedding);
        let device = match preferred_device {
            ChatDevice::Auto => "auto",
            ChatDevice::Gpu => "gpu",
            ChatDevice::Npu => "npu",
            ChatDevice::Cpu => "cpu",
        };
        document["chat"]["preferred_device"] = value(device);
        document["chat"]["reasoning_control"] = value(match reasoning_control {
            ReasoningControl::Request => "request",
            ReasoningControl::Reload => "reload",
        });
        let device_section = if device == "auto" { "gpu" } else { device };
        if document["chat"]
            .get(device_section)
            .is_none_or(|item| !item.is_table())
        {
            document["chat"][device_section] = Item::Table(Table::new());
        }
        document["chat"][device_section]["model"] = value(selected_chat_model);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&temp, document.to_string())?;
        std::fs::rename(temp, &path)?;
        Ok(path)
    }

    /// Persist user-facing UI choices while preserving comments and unrelated
    /// configuration keys.
    pub fn persist_ui_settings(
        &self,
        font_size: f32,
        interface_size: f32,
        show_advanced_controls: bool,
        window_controls_left: bool,
    ) -> Result<PathBuf> {
        use toml_edit::{DocumentMut, Item, Table, value};

        let path = self
            .source_path
            .clone()
            .or_else(Self::per_user_config_path)
            .ok_or_else(|| anyhow::anyhow!("cannot determine a configuration path"))?;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut document = if text.trim().is_empty() {
            DocumentMut::new()
        } else {
            text.parse::<DocumentMut>()?
        };
        if document.get("ui").is_none_or(|item| !item.is_table()) {
            document["ui"] = Item::Table(Table::new());
        }
        document["ui"]["font_size"] = value(font_size as f64);
        document["ui"]["interface_size"] = value(interface_size as f64);
        document["ui"]["show_advanced_controls"] = value(show_advanced_controls);
        document["ui"]["window_controls_left"] = value(window_controls_left);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&temp, document.to_string())?;
        std::fs::rename(temp, &path)?;
        Ok(path)
    }

    /// Persist retrieval controls revealed by the advanced Settings view.
    pub fn persist_retrieval_settings(
        &self,
        fts_limit: usize,
        semantic_limit: usize,
        rerank: bool,
    ) -> Result<PathBuf> {
        use toml_edit::{DocumentMut, Item, Table, value};

        let path = self
            .source_path
            .clone()
            .or_else(Self::per_user_config_path)
            .ok_or_else(|| anyhow::anyhow!("cannot determine a configuration path"))?;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut document = if text.trim().is_empty() {
            DocumentMut::new()
        } else {
            text.parse::<DocumentMut>()?
        };
        if document.get("chat").is_none_or(|item| !item.is_table()) {
            document["chat"] = Item::Table(Table::new());
        }
        document["chat"]["fts_limit"] = value(fts_limit as i64);
        document["chat"]["semantic_limit"] = value(semantic_limit as i64);
        document["chat"]["rerank"] = value(rerank);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&temp, document.to_string())?;
        std::fs::rename(temp, &path)?;
        Ok(path)
    }

    /// Persist the complete typed settings model while retaining comments and
    /// unrelated keys already present in the user's TOML document.
    ///
    /// Existing scalar decoration is copied onto replacement values. Tables are
    /// merged recursively, so application-owned values are updated without
    /// flattening the file into a generated configuration dump.
    pub fn persist_settings(&self, settings: &AppConfig) -> Result<PathBuf> {
        use toml_edit::{DocumentMut, Item};

        fn merge_item(target: &mut Item, source: &Item) {
            if let (Some(target_table), Some(source_table)) =
                (target.as_table_mut(), source.as_table())
            {
                for (key, source_value) in source_table.iter() {
                    if let Some(target_value) = target_table.get_mut(key) {
                        merge_item(target_value, source_value);
                    } else {
                        target_table.insert(key, source_value.clone());
                    }
                }
                return;
            }

            let decor = target.as_value().map(|value| value.decor().clone());
            *target = source.clone();
            if let (Some(decor), Some(value)) = (decor, target.as_value_mut()) {
                *value.decor_mut() = decor;
            }
        }

        let path = self
            .source_path
            .clone()
            .or_else(Self::per_user_config_path)
            .ok_or_else(|| anyhow::anyhow!("cannot determine a configuration path"))?;
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let mut document = if existing.trim().is_empty() {
            DocumentMut::new()
        } else {
            existing.parse::<DocumentMut>()?
        };
        let serialized = toml::to_string(settings)?;
        let desired = serialized.parse::<DocumentMut>()?;
        for section in [
            "lemonade",
            "embedding",
            "models",
            "chat",
            "storage",
            "data",
            "ui",
        ] {
            if let Some(source) = desired.get(section) {
                if let Some(target) = document.get_mut(section) {
                    merge_item(target, source);
                } else {
                    document[section] = source.clone();
                }
            }
        }
        if let Some(agent) = document
            .get_mut("chat")
            .and_then(Item::as_table_mut)
            .and_then(|chat| chat.get_mut("agent"))
            .and_then(Item::as_table_mut)
        {
            agent.remove("cumulative_request_tokens");
            agent.remove("cumulative_tool_output_tokens");
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&temp, document.to_string())?;
        std::fs::rename(temp, &path)?;
        Ok(path)
    }
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
        "ggml-org/embeddinggemma-300M-GGUF".to_string(),
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
    m.insert("ggml-org/embeddinggemma-300M-GGUF".to_string(), ctx(2048));
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
        assert_eq!(cfg.ui.font_size, 16.0);
        assert_eq!(cfg.ui.interface_size, DEFAULT_UI_INTERFACE_SIZE);
        assert_eq!(cfg.lemonade.max_loaded_models, 1);
        assert!(!cfg.ui.window_controls_left);
        assert!(cfg.embedding.npu_enabled);
        assert!(cfg.embedding.gpu_enabled);
        assert!(cfg.embedding.cpu_enabled);
        assert_eq!(cfg.embedding.npu_weight, 100);
        assert_eq!(cfg.embedding.gpu_weight, 50);
        assert_eq!(cfg.embedding.cpu_weight, 10);
        assert_eq!(
            cfg.storage.embedding_dimensions,
            crate::EMBEDDING_DIMENSIONS
        );
        assert_eq!(
            cfg.storage.high_quality_embedding_dimensions,
            crate::HIGH_QUALITY_EMBEDDING_DIMENSIONS
        );
        assert_eq!(
            cfg.data.import_file,
            PathBuf::from("./defaults/data/memory.jsonl")
        );
        assert_eq!(cfg.chat.agent.schema_summary_tokens, 768);
        assert_eq!(cfg.chat.agent.cumulative_request_tokens, None);
        assert_eq!(cfg.chat.agent.cumulative_tool_output_tokens, None);
        assert_eq!(cfg.chat.agent.repeated_call_limit, 1);
        assert_eq!(cfg.chat.fts_limit, 20);
        assert_eq!(cfg.chat.semantic_limit, 20);
        assert!(cfg.chat.rerank);
    }

    #[test]
    fn default_agent_budget_has_no_application_context_ceiling() {
        let chat = ChatConfig::default();
        let effective = chat
            .agent
            .reconcile(usize::MAX, chat.max_tool_turns)
            .unwrap();

        assert_eq!(effective.context_tokens, usize::MAX);
        assert!(
            effective.diagnostics.is_empty(),
            "default agent budgets should be valid without adjustment: {:?}",
            effective.diagnostics
        );
    }

    #[test]
    fn legacy_agent_caps_do_not_limit_the_active_context() {
        let configured = AgentBudgetConfig {
            schema_summary_tokens: 10_000,
            cumulative_request_tokens: None,
            cumulative_tool_output_tokens: None,
            repeated_call_limit: 2,
        };
        let effective = configured.reconcile(4_096, 3).unwrap();
        assert_eq!(effective.context_tokens, 4_096);
        assert_eq!(effective.repeated_call_limit, 2);
        assert!(effective.diagnostics.is_empty());

        let invalid = AgentBudgetConfig {
            schema_summary_tokens: 31,
            ..AgentBudgetConfig::default()
        };
        assert!(invalid.reconcile(4_096, 5).is_ok());
    }

    #[test]
    fn agent_budget_config_deserializes_below_chat() {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            "[chat.agent]\nschema_summary_tokens = 321\ncumulative_request_tokens = 4321\n\
             cumulative_tool_output_tokens = 654\nrepeated_call_limit = 3\n"
        )
        .unwrap();
        let config = AppConfig::load(file.path()).unwrap();
        assert_eq!(config.chat.agent.schema_summary_tokens, 321);
        assert_eq!(config.chat.agent.cumulative_request_tokens, Some(4_321));
        assert_eq!(config.chat.agent.cumulative_tool_output_tokens, Some(654));
        assert_eq!(config.chat.agent.repeated_call_limit, 3);
        let effective = config.chat.agent.reconcile(4_096, 5).unwrap();
        assert!(
            effective
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("ignored"))
        );
    }

    #[test]
    fn user_profile_seeds_and_transforms_packaged_defaults_once() {
        let temp = tempfile::tempdir().unwrap();
        let packaged = temp.path().join("package/defaults");
        std::fs::create_dir_all(packaged.join("config")).unwrap();
        std::fs::create_dir_all(packaged.join("schemas/Sine Nomine")).unwrap();
        std::fs::create_dir_all(packaged.join("example_data")).unwrap();
        std::fs::write(
            packaged.join("config/u-forge.toml"),
            "# retained\n[storage]\ndb_path = \"template\"\n[data]\nimport_file = \"template\"\nschema_dir = \"template\"\n",
        )
        .unwrap();
        let schema = packaged.join("schemas/Sine Nomine/location.schema.json");
        std::fs::write(&schema, "{}\n").unwrap();
        std::fs::write(
            packaged.join("example_data/foundation-example.jsonl"),
            "{}\n",
        )
        .unwrap();

        let paths = UserPaths::from_bases(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        paths.initialize(&packaged).unwrap();

        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(text.contains("# retained"));
        assert!(text.contains(&path_text(&paths.db_dir)));
        assert!(
            text.contains(&path_text(
                &paths
                    .defaults_dir
                    .join("example_data/foundation-example.jsonl")
            ))
        );
        let copied_schema = paths
            .defaults_dir
            .join("schemas/Sine Nomine/location.schema.json");
        assert!(copied_schema.is_file());
        assert_eq!(
            std::fs::read_to_string(paths.data_dir.join(".defaults-revision")).unwrap(),
            "1\n"
        );

        std::fs::remove_file(&copied_schema).unwrap();
        std::fs::write(&paths.config_file, "[storage]\ndb_path = \"custom\"\n").unwrap();
        paths.initialize(&packaged).unwrap();
        assert!(
            !copied_schema.exists(),
            "deleted defaults must stay deleted"
        );
        assert!(
            std::fs::read_to_string(&paths.config_file)
                .unwrap()
                .contains("custom")
        );
    }

    #[test]
    fn setup_persistence_preserves_comments_and_unknown_keys() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep this comment\nunknown = 42\n\n[chat]\npreferred_device = \"cpu\"\n",
        )
        .unwrap();
        // Persistence is a lossless TOML edit, independent of strict runtime
        // deserialization (which intentionally rejects unknown config keys).
        let config = AppConfig {
            source_path: Some(path.clone()),
            ..AppConfig::default()
        };
        let written = config
            .persist_lemonade_setup(
                true,
                ChatDevice::Npu,
                "chat-model-FLM",
                ReasoningControl::Reload,
            )
            .unwrap();
        assert_eq!(written, path);
        let text = std::fs::read_to_string(written).unwrap();
        assert!(text.contains("# keep this comment"));
        assert!(text.contains("unknown = 42"));
        assert!(text.contains("preferred_device = \"npu\""));
        assert!(text.contains("reasoning_control = \"reload\""));
        assert!(text.contains("model = \"chat-model-FLM\""));
    }

    #[test]
    fn ui_persistence_preserves_comments_and_unrelated_sections() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep this comment\n\n[chat]\npreferred_device = \"cpu\"\n",
        )
        .unwrap();
        let config = AppConfig {
            source_path: Some(path.clone()),
            ..AppConfig::default()
        };

        let written = config.persist_ui_settings(18.0, 24.0, true, true).unwrap();
        assert_eq!(written, path);
        let text = std::fs::read_to_string(written).unwrap();
        assert!(text.contains("# keep this comment"));
        assert!(text.contains("preferred_device = \"cpu\""));
        assert!(text.contains("font_size = 18.0"));
        assert!(text.contains("interface_size = 24.0"));
        assert!(text.contains("show_advanced_controls = true"));
        assert!(text.contains("window_controls_left = true"));

        let reloaded = AppConfig::load(&path).unwrap();
        assert_eq!(reloaded.ui.font_size, 18.0);
        assert_eq!(reloaded.ui.interface_size, 24.0);
        assert!(reloaded.ui.show_advanced_controls);
        assert!(reloaded.ui.window_controls_left);
    }

    #[test]
    fn retrieval_persistence_round_trips_advanced_controls() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "# retained\n[ui]\nfont_size = 17.0\n").unwrap();
        let config = AppConfig {
            source_path: Some(path.clone()),
            ..AppConfig::default()
        };

        config.persist_retrieval_settings(35, 45, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# retained"));
        assert!(text.contains("font_size = 17.0"));

        let reloaded = AppConfig::load(&path).unwrap();
        assert_eq!(reloaded.chat.fts_limit, 35);
        assert_eq!(reloaded.chat.semantic_limit, 45);
        assert!(!reloaded.chat.rerank);
    }

    #[test]
    fn full_settings_persistence_is_typed_lossless_and_removes_legacy_budgets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        std::fs::write(
            &path,
            "# retained header\ncustom_key = 42\n\n[chat]\nfts_limit = 7 # retained scalar comment\n\n[chat.agent]\ncumulative_request_tokens = 1234\ncumulative_tool_output_tokens = 567\n",
        )
        .unwrap();
        let current = AppConfig {
            source_path: Some(path.clone()),
            ..AppConfig::default()
        };
        let mut desired = current.clone();
        desired.chat.fts_limit = 37;
        desired.chat.semantic_limit = 29;
        desired.ui.font_size = 18.0;
        desired.embedding.high_quality_embedding = true;
        desired.lemonade.max_loaded_models = 4;

        let written = current.persist_settings(&desired).unwrap();
        assert_eq!(written, path);
        let text = std::fs::read_to_string(written).unwrap();
        assert!(text.contains("# retained header"));
        assert!(text.contains("custom_key = 42"));
        assert!(text.contains("fts_limit = 37 # retained scalar comment"));
        assert!(text.contains("semantic_limit = 29"));
        assert!(text.contains("font_size = 18.0"));
        assert!(text.contains("high_quality_embedding = true"));
        assert!(text.contains("max_loaded_models = 4"));
        assert!(!text.contains("cumulative_request_tokens"));
        assert!(!text.contains("cumulative_tool_output_tokens"));
    }

    #[test]
    fn test_default_model_load_params() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.models.ctx_size_for("embed-gemma-300m-FLM"), 2048);
        assert_eq!(
            cfg.models.ctx_size_for("ggml-org/embeddinggemma-300M-GGUF"),
            2048
        );
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

[lemonade]
max_loaded_models = 2

[storage]
db_path = "./tmp/kg"
embedding_dimensions = 1024
high_quality_embedding_dimensions = 2048
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
        assert_eq!(cfg.lemonade.max_loaded_models, 2);
        assert_eq!(cfg.storage.db_path, PathBuf::from("./tmp/kg"));
        assert_eq!(cfg.storage.embedding_dimensions, 1024);
        assert_eq!(cfg.storage.high_quality_embedding_dimensions, 2048);
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
"bge-reranker-v2-m3-GGUF" = {{ ctx_size = 8192, batch_size = 512, ubatch_size = 512, llamacpp_args = "--threads 6" }}
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
        assert_eq!(rerank_opts.llamacpp_args.as_deref(), Some("--threads 6"));
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

    #[test]
    fn test_unknown_keys_are_rejected_at_each_config_path() {
        let cases = [
            ("top level", "unexpected_root = true\n", "unexpected_root"),
            (
                "embedding",
                "[embedding]\nunexpected_embedding = true\n",
                "unexpected_embedding",
            ),
            (
                "models",
                "[models]\nunexpected_models = true\n",
                "unexpected_models",
            ),
            (
                "model load params",
                "[models.load_params]\ncustom = { ctx_sze = 12 }\n",
                "ctx_sze",
            ),
            ("chat", "[chat]\nsearch_limt = 4\n", "search_limt"),
            (
                "chat device",
                "[chat.gpu]\ntemprature = 0.5\n",
                "temprature",
            ),
            (
                "chat agent",
                "[chat.agent]\nrepeeted_call_limit = 2\n",
                "repeeted_call_limit",
            ),
            (
                "storage",
                "[storage]\nembedding_dimensons = 768\n",
                "embedding_dimensons",
            ),
            (
                "data",
                "[data]\nimport_flie = 'world.jsonl'\n",
                "import_flie",
            ),
            ("ui", "[ui]\nfont_sze = 14.0\n", "font_sze"),
        ];

        for (path, source, unknown_key) in cases {
            let mut file = NamedTempFile::new().unwrap();
            write!(file, "{source}").unwrap();
            let error = AppConfig::load(file.path()).unwrap_err().to_string();
            assert!(
                error.contains(unknown_key),
                "error for {path} did not identify {unknown_key:?}: {error}"
            );
        }
    }
}
