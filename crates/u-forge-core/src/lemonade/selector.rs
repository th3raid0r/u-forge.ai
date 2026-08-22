//! Model selection — replaces device_factory + role-based lookups.
//!
//! [`ModelSelector`] consumes a [`LemonadeServerCatalog`] and the application
//! config to produce ordered lists of [`SelectedModel`] values, ready for
//! `ProviderFactory::build_with_connection`.
//!
//! No hardcoded model IDs appear here — all defaults live in `ModelConfig` as
//! configurable preference lists.  Selection methods filter to **downloaded**
//! models only; models that exist on the server but are not yet downloaded are
//! ignored.

use std::collections::HashSet;

use crate::config::{EmbeddingDeviceConfig, LlamaCppDevice, ModelConfig};
use crate::lemonade::catalog::{CatalogModel, LemonadeServerCatalog};
use crate::lemonade::load::ModelLoadOptions;

// ── Public types ──────────────────────────────────────────────────────────────

/// Embedding quality tier.
///
/// High-quality models produce larger embedding vectors (e.g. 4096-dim) and
/// use a separate queue and index.  All other models are Standard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualityTier {
    /// Standard 768-dim embedding workers.
    Standard,
    /// High-quality large-dim embedding workers (separate index).
    High,
    /// Not applicable — used for non-embedding capabilities.
    NotApplicable,
}

/// A model we've decided to use, with its resolved backend and load options.
///
/// Produced by [`ModelSelector`] methods and consumed by `ProviderFactory::build`.
#[derive(Debug, Clone)]
pub struct SelectedModel {
    pub model_id: String,
    /// Recipe name: `"llamacpp"`, `"flm"`, `"whispercpp"`, `"kokoro"`, etc.
    pub recipe: String,
    /// Resolved llamacpp backend: `"cuda"`, `"rocm"`, `"metal"`,
    /// `"vulkan"`, or `"cpu"`.
    ///
    /// `None` for non-llamacpp recipes where the backend is implicit in the
    /// recipe (FLM → NPU, whispercpp → Vulkan/CPU, kokoro → CPU).
    pub backend: Option<String>,
    /// Load options derived from [`ModelConfig`] for this model ID.
    pub load_opts: ModelLoadOptions,
    /// Embedding quality tier.  [`QualityTier::NotApplicable`] for all
    /// non-embedding capabilities.
    pub quality_tier: QualityTier,
    pub checkpoint: String,
    /// Maximum model-supported context advertised by Lemonade's catalog.
    pub max_context_window: Option<usize>,
    pub tool_capable: bool,
    pub reasoning_capable: bool,
    /// Visible selection changes such as preferred-device fallback.
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveChatLimits {
    pub load_context: Option<usize>,
    pub context: usize,
    pub direct_generation: usize,
    pub agent_generation: usize,
    pub diagnostics: Vec<String>,
}

impl SelectedModel {
    /// Resolve chat limits from the configured load context and the model's
    /// advertised capability. Catalog metadata is a quiet safety ceiling, not
    /// a user-facing global budget.
    pub fn reconcile_chat_limits(
        &self,
        direct_generation: usize,
        agent_generation: usize,
    ) -> anyhow::Result<EffectiveChatLimits> {
        let load_context = match (self.load_opts.ctx_size, self.max_context_window) {
            (Some(configured), Some(maximum)) => Some(configured.min(maximum)),
            (configured, _) => configured,
        };
        let context = load_context
            .or(self.max_context_window)
            .unwrap_or(usize::MAX);
        if context < 2 {
            return Err(anyhow::anyhow!(
                "effective model context is too small for a chat request"
            ));
        }
        Ok(EffectiveChatLimits {
            load_context,
            context,
            direct_generation: direct_generation.min(context),
            agent_generation: agent_generation.min(context),
            diagnostics: self.diagnostics.clone(),
        })
    }
}

// ── Private helpers (module-level) ───────────────────────────────────────────

/// Derive a canonical device slot string from a [`SelectedModel`].
///
/// Used by all selector methods to enforce at-most-one-worker-per-slot:
///
/// - `flm` recipe → `"npu"` (AMD NPU via FLM runtime)
/// - `llamacpp` + cuda/rocm/vulkan/metal → `"gpu"`
/// - `llamacpp` + cpu (or unresolved) → `"cpu"`
/// - Any other recipe (e.g. `"whispercpp"`, `"kokoro"`) → the recipe name
///   itself, giving each recipe its own shared slot.
pub fn is_gpu_backend(backend: Option<&str>) -> bool {
    matches!(backend, Some("cuda" | "rocm" | "vulkan" | "metal"))
}

fn model_device_slot(sel: &SelectedModel) -> String {
    match sel.recipe.as_str() {
        "flm" => "npu".to_string(),
        "llamacpp" if is_gpu_backend(sel.backend.as_deref()) => "gpu".to_string(),
        "llamacpp" => "cpu".to_string(),
        recipe => recipe.to_string(),
    }
}

// ── ModelSelector ─────────────────────────────────────────────────────────────

/// Selects models from a [`LemonadeServerCatalog`] based on configured
/// preference lists and device-enable flags.
pub struct ModelSelector<'a> {
    catalog: &'a LemonadeServerCatalog,
    config: &'a ModelConfig,
    embedding: &'a EmbeddingDeviceConfig,
}

impl<'a> ModelSelector<'a> {
    /// Create a new selector.
    ///
    /// # Parameters
    /// - `catalog`   — Live server snapshot from [`LemonadeServerCatalog::discover`].
    /// - `config`    — Model config (preference lists, load params).
    /// - `embedding` — Embedding device config (enabled flags, weights).
    pub fn new(
        catalog: &'a LemonadeServerCatalog,
        config: &'a ModelConfig,
        embedding: &'a EmbeddingDeviceConfig,
    ) -> Self {
        Self {
            catalog,
            config,
            embedding,
        }
    }

    /// Returns embedding models to register as workers, ordered by priority.
    ///
    /// - Filters to downloaded models with the `"embeddings"` label.
    /// - Respects `EmbeddingDeviceConfig.{npu,gpu,cpu}_enabled` flags.
    /// - Assigns [`QualityTier::High`] to models in
    ///   `ModelConfig::high_quality_embedding_models`.
    /// - Ordered by `ModelConfig::embedding_model_preferences`, then any
    ///   remaining downloaded embedding models.
    /// - **At most one worker per (device slot, quality tier).**  Device slots
    ///   are `"npu"` (FLM), `"gpu"` (llamacpp + cuda/rocm/vulkan/metal), and `"cpu"`
    ///   (llamacpp + cpu).  The first (highest-preference) model wins each slot;
    ///   all later candidates for the same slot are dropped.  This prevents
    ///   spawning multiple NPU workers or mixing incompatible model families
    ///   (e.g. embedgemma + nomic) in the same embedding index.
    pub fn select_embedding_models(&self) -> Vec<SelectedModel> {
        let candidates = self.catalog.downloaded_models_with_label("embeddings");
        let ordered =
            self.apply_preference_order(&candidates, &self.config.embedding_model_preferences);

        let mut result: Vec<SelectedModel> = ordered
            .into_iter()
            .filter_map(|m| {
                let quality_tier = if self.config.high_quality_embedding_models.contains(&m.id) {
                    QualityTier::High
                } else {
                    QualityTier::Standard
                };
                let lane = match quality_tier {
                    QualityTier::High => &self.embedding.high_quality,
                    _ => &self.embedding.standard,
                };
                let backend = match m.recipe.as_str() {
                    "flm" if lane.npu_enabled => None,
                    "flm" => return None,
                    "llamacpp" => self.resolve_llamacpp_device(lane.llamacpp_device)?,
                    _ => self.resolve_llamacpp_backend(&m.recipe),
                };
                Some(SelectedModel {
                    model_id: m.id.clone(),
                    recipe: m.recipe.clone(),
                    backend: backend.clone(),
                    load_opts: self.load_options_for(m),
                    quality_tier,
                    checkpoint: m.checkpoint.clone(),
                    max_context_window: m.max_context_window,
                    tool_capable: m.supports_tool_calling(),
                    reasoning_capable: m.supports_reasoning(),
                    diagnostics: if lane.llamacpp_device == LlamaCppDevice::Gpu {
                        self.backend_diagnostics(&m.recipe, backend.as_deref())
                    } else {
                        Vec::new()
                    },
                })
            })
            .collect();

        // Enforce at most one worker per (device_slot, quality_tier).
        // First model in preference order wins each slot.
        let mut seen = HashSet::<(String, bool)>::new();
        result.retain(|s| seen.insert((model_device_slot(s), s.quality_tier == QualityTier::High)));

        result
    }

    /// Look up a single downloaded model by its exact ID.
    ///
    /// Unlike the `select_*` methods this bypasses preference lists and
    /// device-slot deduplication — it returns the first downloaded catalog
    /// entry whose `id` matches exactly, with the backend and load options
    /// resolved the same way as all other selection methods.
    ///
    /// Returns `None` when no downloaded model with `model_id` exists in the
    /// catalog.
    ///
    /// # Use case
    ///
    /// Honouring explicit per-device config overrides (e.g. `chat_cfg.gpu.model`
    /// or `chat_cfg.npu.model`) where the caller already knows which model it
    /// wants and only needs the resolved [`SelectedModel`].
    pub fn model_by_id(&self, model_id: &str, quality_tier: QualityTier) -> Option<SelectedModel> {
        let m = self
            .catalog
            .models
            .iter()
            .find(|m| m.id == model_id && m.downloaded)?;
        let backend = self.resolve_llamacpp_backend(&m.recipe);
        Some(SelectedModel {
            model_id: m.id.clone(),
            recipe: m.recipe.clone(),
            backend,
            load_opts: self.load_options_for(m),
            quality_tier,
            checkpoint: m.checkpoint.clone(),
            max_context_window: m.max_context_window,
            tool_capable: m.supports_tool_calling(),
            reasoning_capable: m.supports_reasoning(),
            diagnostics: self.backend_diagnostics(
                &m.recipe,
                self.resolve_llamacpp_backend(&m.recipe).as_deref(),
            ),
        })
    }

    /// Returns the best available reranker (label `"reranking"`).
    pub fn select_reranker(&self, device: LlamaCppDevice) -> Option<SelectedModel> {
        let candidates = self.catalog.downloaded_models_with_label("reranking");
        let ordered =
            self.apply_preference_order(&candidates, &self.config.reranker_model_preferences);

        ordered.into_iter().find_map(|m| {
            let backend = match m.recipe.as_str() {
                "llamacpp" => self.resolve_llamacpp_device(device)?,
                _ => self.resolve_llamacpp_backend(&m.recipe),
            };
            Some(SelectedModel {
                model_id: m.id.clone(),
                recipe: m.recipe.clone(),
                backend: backend.clone(),
                load_opts: self.load_options_for(m),
                quality_tier: QualityTier::NotApplicable,
                checkpoint: m.checkpoint.clone(),
                max_context_window: m.max_context_window,
                tool_capable: m.supports_tool_calling(),
                reasoning_capable: m.supports_reasoning(),
                diagnostics: if device == LlamaCppDevice::Gpu {
                    self.backend_diagnostics(&m.recipe, backend.as_deref())
                } else {
                    Vec::new()
                },
            })
        })
    }

    /// Returns STT models (label `"audio"` or `"transcription"`).
    ///
    /// TTS models that incidentally carry `"audio"` (e.g. kokoro) are excluded.
    ///
    /// **At most one worker per device slot** (FLM → NPU, whispercpp → its own
    /// slot).  The first (highest-preference) model wins each slot.
    pub fn select_stt_models(&self) -> Vec<SelectedModel> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut candidates: Vec<&CatalogModel> = Vec::new();
        for label in &["audio", "transcription"] {
            for m in self.catalog.downloaded_models_with_label(label) {
                if seen.insert(m.id.as_str()) {
                    candidates.push(m);
                }
            }
        }
        // Exclude TTS models that carry "audio" (kokoro recipe or "tts" label).
        candidates.retain(|m| m.recipe != "kokoro" && !m.labels.contains("tts"));

        let ordered = self.apply_preference_order(&candidates, &self.config.stt_model_preferences);

        let mut result: Vec<SelectedModel> = ordered
            .into_iter()
            .map(|m| SelectedModel {
                model_id: m.id.clone(),
                recipe: m.recipe.clone(),
                backend: self.resolve_llamacpp_backend(&m.recipe),
                load_opts: self.load_options_for(m),
                quality_tier: QualityTier::NotApplicable,
                checkpoint: m.checkpoint.clone(),
                max_context_window: m.max_context_window,
                tool_capable: m.supports_tool_calling(),
                reasoning_capable: m.supports_reasoning(),
                diagnostics: self.backend_diagnostics(
                    &m.recipe,
                    self.resolve_llamacpp_backend(&m.recipe).as_deref(),
                ),
            })
            .collect();

        let mut seen_slots = HashSet::<String>::new();
        result.retain(|s| seen_slots.insert(model_device_slot(s)));
        result
    }

    /// Returns downloaded chat models using a supported recipe (`"llamacpp"`
    /// or `"flm"`) and advertising Lemonade's explicit `"chat"` capability.
    ///
    /// **At most one worker per device slot** (FLM → NPU, llamacpp + GPU
    /// backend → GPU, llamacpp + cpu → CPU).  Both a GPU worker and an NPU
    /// worker may coexist; the chat layer picks between them via
    /// `ChatConfig::preferred_device`.
    pub fn select_llm_models(&self) -> Vec<SelectedModel> {
        let candidates: Vec<&CatalogModel> = self
            .catalog
            .models
            .iter()
            .filter(|m| {
                m.downloaded
                    && (m.recipe == "llamacpp" || m.recipe == "flm")
                    && m.labels.contains("chat")
            })
            .collect();

        let ordered = self.apply_preference_order(&candidates, &self.config.llm_model_preferences);

        let mut result: Vec<SelectedModel> = ordered
            .into_iter()
            .map(|m| SelectedModel {
                model_id: m.id.clone(),
                recipe: m.recipe.clone(),
                backend: self.resolve_llamacpp_backend(&m.recipe),
                load_opts: self.load_options_for(m),
                quality_tier: QualityTier::NotApplicable,
                checkpoint: m.checkpoint.clone(),
                max_context_window: m.max_context_window,
                tool_capable: m.supports_tool_calling(),
                reasoning_capable: m.supports_reasoning(),
                diagnostics: self.backend_diagnostics(
                    &m.recipe,
                    self.resolve_llamacpp_backend(&m.recipe).as_deref(),
                ),
            })
            .collect();

        let mut seen_slots = HashSet::<String>::new();
        result.retain(|s| seen_slots.insert(model_device_slot(s)));
        result
    }

    /// Returns **all** downloaded models advertising Lemonade's explicit `"chat"`
    /// capability through a supported recipe, without the one-per-device-slot
    /// deduplication applied by [`select_llm_models`]. Preference ordering is still
    /// applied. Intended for the chat UI model picker where the user should see
    /// every available model.
    pub fn select_all_llm_models(&self) -> Vec<SelectedModel> {
        let candidates: Vec<&CatalogModel> = self
            .catalog
            .models
            .iter()
            .filter(|m| {
                m.downloaded
                    && (m.recipe == "llamacpp" || m.recipe == "flm")
                    && m.labels.contains("chat")
            })
            .collect();

        let ordered = self.apply_preference_order(&candidates, &self.config.llm_model_preferences);

        ordered
            .into_iter()
            .map(|m| SelectedModel {
                model_id: m.id.clone(),
                recipe: m.recipe.clone(),
                backend: self.resolve_llamacpp_backend(&m.recipe),
                load_opts: self.load_options_for(m),
                quality_tier: QualityTier::NotApplicable,
                checkpoint: m.checkpoint.clone(),
                max_context_window: m.max_context_window,
                tool_capable: m.supports_tool_calling(),
                reasoning_capable: m.supports_reasoning(),
                diagnostics: self.backend_diagnostics(
                    &m.recipe,
                    self.resolve_llamacpp_backend(&m.recipe).as_deref(),
                ),
            })
            .collect()
    }

    /// Returns the TTS model (recipe `"kokoro"` or label `"tts"`).
    ///
    /// TTS has no backend parameter — the kokoro recipe is always CPU.
    pub fn select_tts(&self) -> Option<SelectedModel> {
        let by_recipe = self.catalog.downloaded_models_with_recipe("kokoro");
        let by_label = self.catalog.downloaded_models_with_label("tts");

        let mut seen: HashSet<&str> = HashSet::new();
        let mut candidates: Vec<&CatalogModel> = Vec::new();
        for m in by_recipe.into_iter().chain(by_label) {
            if seen.insert(m.id.as_str()) {
                candidates.push(m);
            }
        }

        let ordered = self.apply_preference_order(&candidates, &self.config.tts_model_preferences);

        ordered.into_iter().next().map(|m| SelectedModel {
            model_id: m.id.clone(),
            recipe: m.recipe.clone(),
            backend: None, // TTS is always CPU via kokoro; no backend param needed
            load_opts: self.load_options_for(m),
            quality_tier: QualityTier::NotApplicable,
            checkpoint: m.checkpoint.clone(),
            max_context_window: m.max_context_window,
            tool_capable: m.supports_tool_calling(),
            reasoning_capable: m.supports_reasoning(),
            diagnostics: self.backend_diagnostics(
                &m.recipe,
                self.resolve_llamacpp_backend(&m.recipe).as_deref(),
            ),
        })
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Resolve configured load options without asking Lemonade to exceed the
    /// selected model's advertised capability.
    fn load_options_for(&self, model: &CatalogModel) -> ModelLoadOptions {
        let mut options = self.config.load_options_for(&model.id);
        if let (Some(configured), Some(maximum)) = (options.ctx_size, model.max_context_window) {
            options.ctx_size = Some(configured.min(maximum));
        }
        options
    }

    /// Resolve the llamacpp backend for a model.
    ///
    /// For `llamacpp` models, applies the configured logical GPU policy and
    /// returns its first installed backend. Falls back to `"cpu"` when none
    /// matches (always available).
    ///
    /// Returns `None` for non-llamacpp recipes (FLM, whispercpp, kokoro) where
    /// the backend is implicit in the recipe.
    fn resolve_llamacpp_backend(&self, recipe: &str) -> Option<String> {
        if recipe != "llamacpp" {
            return None;
        }
        for backend in self.config.gpu_backend_preference(self.catalog) {
            if self.catalog.has_installed_backend("llamacpp", &backend) {
                return Some(backend);
            }
        }
        Some("cpu".to_string())
    }

    fn resolve_llamacpp_device(&self, device: LlamaCppDevice) -> Option<Option<String>> {
        match device {
            LlamaCppDevice::Disabled => None,
            LlamaCppDevice::Cpu => Some(Some("cpu".to_string())),
            LlamaCppDevice::Gpu => Some(self.resolve_llamacpp_backend("llamacpp")),
        }
    }

    fn backend_diagnostics(&self, recipe: &str, backend: Option<&str>) -> Vec<String> {
        if recipe != "llamacpp" {
            return Vec::new();
        }
        let preferred = self.config.gpu_backend_preference(self.catalog);
        match (preferred.first(), backend) {
            (Some(preferred), Some(selected)) if preferred != selected => vec![format!(
                "preferred backend {preferred} unavailable; selected {selected} and rebuilt the profile"
            )],
            _ => Vec::new(),
        }
    }

    /// Sort `candidates` by preference list: listed models appear first (in
    /// list order), then remaining models in original catalog order.
    fn apply_preference_order<'b>(
        &self,
        candidates: &[&'b CatalogModel],
        preferences: &[String],
    ) -> Vec<&'b CatalogModel> {
        let mut result: Vec<&CatalogModel> = preferences
            .iter()
            .filter_map(|id| candidates.iter().copied().find(|m| &m.id == id))
            .collect();
        for m in candidates {
            if !preferences.contains(&m.id) {
                result.push(m);
            }
        }
        result
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::config::{
        EmbeddingDeviceConfig, GpuRuntimePreference, LlamaCppDevice, ModelConfig, ModelLoadParams,
    };
    use crate::lemonade::catalog::{CatalogModel, InstalledBackend, LemonadeServerCatalog};

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn model(id: &str, recipe: &str, labels: &[&str]) -> CatalogModel {
        CatalogModel {
            id: id.to_string(),
            recipe: recipe.to_string(),
            labels: labels.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
            downloaded: true,
            size_gb: None,
            checkpoint: String::new(),
            ..Default::default()
        }
    }

    fn not_downloaded(id: &str, recipe: &str, labels: &[&str]) -> CatalogModel {
        CatalogModel {
            downloaded: false,
            ..model(id, recipe, labels)
        }
    }

    fn installed_backend(recipe: &str, backend: &str, devices: &[&str]) -> InstalledBackend {
        InstalledBackend {
            recipe: recipe.to_string(),
            backend: backend.to_string(),
            devices: devices.iter().map(|s| s.to_string()).collect(),
            state: "installed".to_string(),
            ..Default::default()
        }
    }

    fn catalog_with(
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

    fn default_embedding_cfg() -> EmbeddingDeviceConfig {
        let mut config = EmbeddingDeviceConfig::default();
        config.standard.npu_enabled = true;
        config.standard.llamacpp_device = LlamaCppDevice::Gpu;
        config.high_quality.llamacpp_device = LlamaCppDevice::Gpu;
        config
    }

    // ── Embedding selection ───────────────────────────────────────────────────

    #[test]
    fn test_select_embedding_skips_not_downloaded() {
        let catalog = catalog_with(
            vec![
                model("embed-flm", "flm", &["embeddings"]),
                not_downloaded("embed-gguf", "llamacpp", &["embeddings"]),
            ],
            vec![installed_backend("flm", "npu", &["amd_npu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_embedding_models();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model_id, "embed-flm");
    }

    #[test]
    fn catalog_context_quietly_caps_invalid_load_and_generation_sizes() {
        let selected = SelectedModel {
            model_id: "chat".into(),
            recipe: "llamacpp".into(),
            backend: Some("vulkan".into()),
            load_opts: ModelLoadOptions {
                ctx_size: Some(262_144),
                ..Default::default()
            },
            quality_tier: QualityTier::NotApplicable,
            checkpoint: String::new(),
            max_context_window: Some(131_072),
            tool_capable: true,
            reasoning_capable: true,
            diagnostics: Vec::new(),
        };
        let limits = selected.reconcile_chat_limits(262_144, 131_072).unwrap();
        assert_eq!(limits.load_context, Some(131_072));
        assert_eq!(limits.context, 131_072);
        assert_eq!(limits.direct_generation, 131_072);
        assert_eq!(limits.agent_generation, 131_072);
        assert!(limits.diagnostics.is_empty());
    }

    #[test]
    fn selector_caps_the_runtime_load_option_to_catalog_context() {
        let mut chat = model("chat", "llamacpp", &[]);
        chat.max_context_window = Some(131_072);
        let catalog = catalog_with(
            vec![chat],
            vec![installed_backend("llamacpp", "vulkan", &["gpu"])],
        );
        let mut config = ModelConfig::default();
        config.default_gpu_runtime = GpuRuntimePreference::Vulkan;
        config.load_params.insert(
            "chat".into(),
            ModelLoadParams {
                ctx_size: Some(262_144),
                ..Default::default()
            },
        );
        let embedding = default_embedding_cfg();
        let selected = ModelSelector::new(&catalog, &config, &embedding)
            .model_by_id("chat", QualityTier::NotApplicable)
            .unwrap();

        assert_eq!(selected.load_opts.ctx_size, Some(131_072));
        assert!(selected.diagnostics.is_empty());
    }

    #[test]
    fn automatic_server_context_has_no_u_forge_ceiling() {
        let selected = SelectedModel {
            model_id: "chat".into(),
            recipe: "llamacpp".into(),
            backend: Some("vulkan".into()),
            load_opts: ModelLoadOptions::default(),
            quality_tier: QualityTier::NotApplicable,
            checkpoint: String::new(),
            max_context_window: None,
            tool_capable: true,
            reasoning_capable: true,
            diagnostics: Vec::new(),
        };
        let limits = selected.reconcile_chat_limits(262_144, 131_072).unwrap();
        assert_eq!(limits.load_context, None);
        assert_eq!(limits.context, usize::MAX);
        assert!(limits.diagnostics.is_empty());
    }

    #[test]
    fn test_select_embedding_respects_npu_disabled() {
        let catalog = catalog_with(
            vec![model("embed-flm", "flm", &["embeddings"])],
            vec![installed_backend("flm", "npu", &["amd_npu"])],
        );
        let cfg = ModelConfig::default();
        let mut emb = default_embedding_cfg();
        emb.standard.npu_enabled = false;
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        assert!(selector.select_embedding_models().is_empty());
    }

    #[test]
    fn test_select_embedding_respects_llamacpp_disabled() {
        let catalog = catalog_with(
            vec![model("embed-gguf", "llamacpp", &["embeddings"])],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let mut emb = default_embedding_cfg();
        emb.standard.llamacpp_device = LlamaCppDevice::Disabled;
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        assert!(selector.select_embedding_models().is_empty());
    }

    #[test]
    fn test_select_embedding_assigns_hq_tier() {
        let catalog = catalog_with(
            vec![
                model("Qwen3-Embedding-8B-GGUF", "llamacpp", &["embeddings"]),
                model("embed-std", "llamacpp", &["embeddings"]),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default(); // Qwen3 is default HQ model
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_embedding_models();
        let qwen = results
            .iter()
            .find(|m| m.model_id == "Qwen3-Embedding-8B-GGUF")
            .unwrap();
        let std_m = results.iter().find(|m| m.model_id == "embed-std").unwrap();
        assert_eq!(qwen.quality_tier, QualityTier::High);
        assert_eq!(std_m.quality_tier, QualityTier::Standard);
    }

    #[test]
    fn test_select_embedding_preference_picks_winner_for_slot() {
        // Two models competing for the same GPU slot — only the preferred one wins.
        let catalog = catalog_with(
            vec![
                model("model-b", "llamacpp", &["embeddings"]),
                model("model-a", "llamacpp", &["embeddings"]),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig {
            embedding_model_preferences: vec!["model-a".to_string()],
            ..Default::default()
        };
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_embedding_models();
        // Only one GPU-slot worker; preference list picks model-a.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model_id, "model-a");
    }

    #[test]
    fn test_select_embedding_different_devices_both_selected() {
        // Same-preference models on different devices — one worker per device slot.
        let catalog = catalog_with(
            vec![
                model("embed-gemma-FLM", "flm", &["embeddings"]),
                model("embed-gemma-GGUF", "llamacpp", &["embeddings"]),
            ],
            vec![
                installed_backend("flm", "npu", &["amd_npu"]),
                installed_backend("llamacpp", "rocm", &["amd_igpu"]),
            ],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        assert_eq!(
            cfg.gpu_backend_preference(&catalog),
            vec!["rocm".to_string(), "vulkan".to_string()]
        );
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_embedding_models();
        // NPU slot → embed-gemma-FLM, GPU slot → embed-gemma-GGUF
        assert_eq!(results.len(), 2);
        let ids: Vec<&str> = results.iter().map(|m| m.model_id.as_str()).collect();
        assert!(ids.contains(&"embed-gemma-FLM"));
        assert!(ids.contains(&"embed-gemma-GGUF"));
    }

    #[test]
    fn test_select_embedding_limit_one_npu_worker() {
        // Two FLM models compete for the single NPU slot; first preference wins.
        let catalog = catalog_with(
            vec![
                model("embed-gemma-FLM", "flm", &["embeddings"]),
                model("nomic-embed-FLM", "flm", &["embeddings"]),
            ],
            vec![installed_backend("flm", "npu", &["amd_npu"])],
        );
        let cfg = ModelConfig {
            embedding_model_preferences: vec!["embed-gemma-FLM".to_string()],
            ..Default::default()
        };
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_embedding_models();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model_id, "embed-gemma-FLM");
    }

    #[test]
    fn test_select_embedding_hq_and_standard_same_device_both_kept() {
        // Standard + HQ on the same GPU device → different (device, tier) slots →
        // both survive deduplication.
        let catalog = catalog_with(
            vec![
                model("Qwen3-Embedding-8B-GGUF", "llamacpp", &["embeddings"]),
                model("embed-std-GGUF", "llamacpp", &["embeddings"]),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default(); // Qwen3 is default HQ
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_embedding_models();
        assert_eq!(results.len(), 2, "Standard and HQ occupy separate slots");
        let hq = results
            .iter()
            .find(|m| m.quality_tier == QualityTier::High)
            .unwrap();
        let std = results
            .iter()
            .find(|m| m.quality_tier == QualityTier::Standard)
            .unwrap();
        assert_eq!(hq.model_id, "Qwen3-Embedding-8B-GGUF");
        assert_eq!(std.model_id, "embed-std-GGUF");
    }

    // ── Backend resolution ────────────────────────────────────────────────────

    #[test]
    fn test_llamacpp_backend_prefers_rocm_over_vulkan() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["chat", "reasoning"])],
            vec![
                installed_backend("llamacpp", "rocm", &["amd_igpu"]),
                installed_backend("llamacpp", "vulkan", &["amd_igpu"]),
            ],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results[0].backend.as_deref(), Some("rocm"));
    }

    #[test]
    fn test_llamacpp_backend_falls_back_to_vulkan() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["chat", "reasoning"])],
            vec![installed_backend("llamacpp", "vulkan", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results[0].backend.as_deref(), Some("vulkan"));
    }

    #[test]
    fn test_amd_device_keeps_rocm_preference_when_only_vulkan_is_available() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["chat", "reasoning"])],
            vec![installed_backend("llamacpp", "vulkan", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();

        assert_eq!(
            cfg.gpu_backend_preference(&catalog),
            vec!["rocm".to_string(), "vulkan".to_string()]
        );
        let results = ModelSelector::new(&catalog, &cfg, &emb).select_llm_models();
        assert_eq!(results[0].backend.as_deref(), Some("vulkan"));
        assert!(results[0].diagnostics[0].contains("preferred backend rocm"));
    }

    #[test]
    fn test_llamacpp_backend_prefers_cuda_over_vulkan() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["chat", "reasoning"])],
            vec![
                installed_backend("llamacpp", "cuda", &["nvidia_gpu"]),
                installed_backend("llamacpp", "vulkan", &["nvidia_gpu"]),
            ],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        assert_eq!(
            cfg.gpu_backend_preference(&catalog),
            vec!["cuda".to_string(), "vulkan".to_string()]
        );
        let results = ModelSelector::new(&catalog, &cfg, &emb).select_llm_models();

        assert_eq!(results[0].backend.as_deref(), Some("cuda"));
        assert!(is_gpu_backend(results[0].backend.as_deref()));
    }

    #[test]
    fn test_nvidia_device_keeps_cuda_preference_when_only_vulkan_is_available() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["chat", "reasoning"])],
            vec![installed_backend("llamacpp", "vulkan", &["nvidia_gpu"])],
        );
        let cfg = ModelConfig::default();

        assert_eq!(
            cfg.gpu_backend_preference(&catalog),
            vec!["cuda".to_string(), "vulkan".to_string()]
        );
    }

    #[test]
    fn test_explicit_vulkan_runtime_overrides_cuda() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["chat", "reasoning"])],
            vec![
                installed_backend("llamacpp", "cuda", &["nvidia_gpu"]),
                installed_backend("llamacpp", "vulkan", &["nvidia_gpu"]),
            ],
        );
        let cfg = ModelConfig {
            default_gpu_runtime: GpuRuntimePreference::Vulkan,
            ..Default::default()
        };
        let emb = default_embedding_cfg();
        let results = ModelSelector::new(&catalog, &cfg, &emb).select_llm_models();

        assert_eq!(results[0].backend.as_deref(), Some("vulkan"));
    }

    #[test]
    fn test_llamacpp_backend_falls_back_to_cpu_when_nothing_installed() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["chat", "reasoning"])],
            vec![], // no backends installed
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results[0].backend.as_deref(), Some("cpu"));
    }

    #[test]
    fn test_flm_recipe_has_no_backend() {
        let catalog = catalog_with(
            vec![model("qwen3.5-4B-FLM", "flm", &["chat", "reasoning"])],
            vec![installed_backend("flm", "npu", &["amd_npu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results.len(), 1);
        assert!(results[0].backend.is_none(), "FLM backend must be None");
    }

    // ── Reranker selection ────────────────────────────────────────────────────

    #[test]
    fn test_select_reranker_picks_preference_first() {
        let catalog = catalog_with(
            vec![
                model("other-reranker", "llamacpp", &["reranking"]),
                model("bge-reranker-v2-m3-GGUF", "llamacpp", &["reranking"]),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let result = selector.select_reranker(LlamaCppDevice::Gpu).unwrap();
        assert_eq!(result.model_id, "bge-reranker-v2-m3-GGUF");
        assert_eq!(result.quality_tier, QualityTier::NotApplicable);
    }

    #[test]
    fn test_select_reranker_none_when_missing() {
        let catalog = catalog_with(vec![], vec![]);
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        assert!(
            ModelSelector::new(&catalog, &cfg, &emb)
                .select_reranker(LlamaCppDevice::Gpu)
                .is_none()
        );
    }

    // ── STT selection ─────────────────────────────────────────────────────────

    #[test]
    fn test_select_stt_excludes_tts_models() {
        let catalog = catalog_with(
            vec![
                model("whisper-v3-turbo-FLM", "flm", &["audio", "transcription"]),
                model("kokoro-v1", "kokoro", &["audio", "tts"]), // must be excluded
            ],
            vec![installed_backend("flm", "npu", &["amd_npu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_stt_models();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model_id, "whisper-v3-turbo-FLM");
    }

    #[test]
    fn test_select_stt_deduplicates_audio_and_transcription_labels() {
        // A model carrying both "audio" and "transcription" must appear only once.
        let catalog = catalog_with(
            vec![model(
                "Whisper-Large-v3-Turbo",
                "whispercpp",
                &["audio", "transcription"],
            )],
            vec![],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_stt_models();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_select_stt_keeps_flm_models_with_realtime_transcription_label() {
        let catalog = catalog_with(
            vec![
                model(
                    "whisper-v3-turbo-FLM",
                    "flm",
                    &["realtime-transcription", "transcription"],
                ),
                model(
                    "Whisper-Large-v3-Turbo",
                    "whispercpp",
                    &["audio", "transcription"],
                ),
            ],
            vec![installed_backend("flm", "npu", &["amd_npu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_stt_models();
        let ids: Vec<&str> = results
            .iter()
            .map(|model| model.model_id.as_str())
            .collect();
        assert!(ids.contains(&"whisper-v3-turbo-FLM"));
        assert!(ids.contains(&"Whisper-Large-v3-Turbo"));
    }

    // ── LLM selection ─────────────────────────────────────────────────────────

    #[test]
    fn test_select_llm_requires_explicit_chat_label() {
        let catalog = catalog_with(
            vec![
                model(
                    "Gemma-4-26B-A4B-it-GGUF",
                    "llamacpp",
                    &["chat", "tool-calling"],
                ),
                model("embed-gguf", "llamacpp", &["embeddings"]), // excluded
                model("reranker", "llamacpp", &["reranking"]),    // excluded
                model("untyped-llm", "llamacpp", &["reasoning"]), // excluded
                model("qwen3.5-4B-FLM", "flm", &["chat", "reasoning"]),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        let ids: Vec<&str> = results.iter().map(|m| m.model_id.as_str()).collect();
        assert!(
            ids.contains(&"qwen3.5-4B-FLM"),
            "FLM LLM should be included"
        );
        assert!(
            ids.contains(&"Gemma-4-26B-A4B-it-GGUF"),
            "GPU LLM should be included"
        );
        assert!(!ids.contains(&"embed-gguf"), "Embedding must be excluded");
        assert!(!ids.contains(&"reranker"), "Reranker must be excluded");
        assert!(
            !ids.contains(&"untyped-llm"),
            "Models without the chat label must be excluded"
        );
    }

    #[test]
    fn test_select_llm_preference_order() {
        let catalog = catalog_with(
            vec![
                model(
                    "Gemma-4-26B-A4B-it-GGUF",
                    "llamacpp",
                    &["chat", "tool-calling"],
                ),
                model("qwen3.5-4B-FLM", "flm", &["chat", "reasoning"]),
            ],
            vec![],
        );
        let cfg = ModelConfig::default(); // default prefs: Gemma (GPU) first
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results[0].model_id, "Gemma-4-26B-A4B-it-GGUF");
        assert_eq!(results[1].model_id, "qwen3.5-4B-FLM");
    }

    #[test]
    fn test_select_llm_limit_one_per_gpu_slot() {
        // Two GPU (llamacpp + rocm) LLMs — only the preferred one wins the slot.
        let catalog = catalog_with(
            vec![
                model(
                    "Gemma-4-26B-A4B-it-GGUF",
                    "llamacpp",
                    &["chat", "tool-calling"],
                ),
                model("Qwen3-30B-A3B-GGUF", "llamacpp", &["chat", "reasoning"]),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig {
            llm_model_preferences: vec!["Gemma-4-26B-A4B-it-GGUF".to_string()],
            ..Default::default()
        };
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model_id, "Gemma-4-26B-A4B-it-GGUF");
    }

    #[test]
    fn test_select_llm_npu_and_gpu_both_kept() {
        // One FLM (NPU) and one llamacpp (GPU) — separate slots, both survive.
        let catalog = catalog_with(
            vec![
                model("qwen3.5-4B-FLM", "flm", &["chat", "reasoning"]),
                model(
                    "Gemma-4-26B-A4B-it-GGUF",
                    "llamacpp",
                    &["chat", "tool-calling"],
                ),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results.len(), 2, "NPU and GPU LLM slots are independent");
        let ids: Vec<&str> = results.iter().map(|m| m.model_id.as_str()).collect();
        assert!(ids.contains(&"qwen3.5-4B-FLM"));
        assert!(ids.contains(&"Gemma-4-26B-A4B-it-GGUF"));
    }

    // ── STT selection (slot limits) ───────────────────────────────────────────

    #[test]
    fn test_select_stt_limit_one_per_recipe_slot() {
        // Two whispercpp models — only the preferred one wins the "whispercpp" slot.
        let catalog = catalog_with(
            vec![
                model(
                    "Whisper-Large-v3-Turbo",
                    "whispercpp",
                    &["audio", "transcription"],
                ),
                model("Whisper-Small", "whispercpp", &["audio"]),
            ],
            vec![installed_backend("whispercpp", "cpu", &["cpu"])],
        );
        let cfg = ModelConfig {
            stt_model_preferences: vec!["Whisper-Large-v3-Turbo".to_string()],
            ..Default::default()
        };
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_stt_models();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model_id, "Whisper-Large-v3-Turbo");
    }

    #[test]
    fn test_select_stt_npu_and_whispercpp_both_kept() {
        // FLM (NPU) and whispercpp occupy different slots — both survive.
        let catalog = catalog_with(
            vec![
                model("whisper-v3-turbo-FLM", "flm", &["audio", "transcription"]),
                model(
                    "Whisper-Large-v3-Turbo",
                    "whispercpp",
                    &["audio", "transcription"],
                ),
            ],
            vec![
                installed_backend("flm", "npu", &["amd_npu"]),
                installed_backend("whispercpp", "cpu", &["cpu"]),
            ],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_stt_models();
        assert_eq!(
            results.len(),
            2,
            "NPU and whispercpp STT slots are independent"
        );
        let ids: Vec<&str> = results.iter().map(|m| m.model_id.as_str()).collect();
        assert!(ids.contains(&"whisper-v3-turbo-FLM"));
        assert!(ids.contains(&"Whisper-Large-v3-Turbo"));
    }

    // ── TTS selection ─────────────────────────────────────────────────────────

    #[test]
    fn test_select_tts_picks_kokoro_recipe_first() {
        let catalog = catalog_with(
            vec![
                model("other-tts", "custom", &["tts"]),
                model("kokoro-v1", "kokoro", &["tts", "speech"]),
            ],
            vec![],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let result = selector.select_tts().unwrap();
        assert_eq!(result.model_id, "kokoro-v1");
        assert!(result.backend.is_none(), "TTS backend must be None");
    }

    #[test]
    fn test_select_tts_none_when_missing() {
        let catalog = catalog_with(vec![], vec![]);
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        assert!(
            ModelSelector::new(&catalog, &cfg, &emb)
                .select_tts()
                .is_none()
        );
    }
}
