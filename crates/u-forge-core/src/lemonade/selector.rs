//! Model selection — replaces device_factory + role-based lookups.
//!
//! [`ModelSelector`] consumes a [`LemonadeServerCatalog`] and the application
//! config to produce ordered lists of [`SelectedModel`] values, ready for
//! `ProviderFactory::build` (Phase 4).
//!
//! No hardcoded model IDs appear here — all defaults live in `ModelConfig` as
//! configurable preference lists.  Selection methods filter to **downloaded**
//! models only; models that exist on the server but are not yet downloaded are
//! ignored.

use std::collections::HashSet;

use crate::config::{EmbeddingDeviceConfig, ModelConfig};
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
    /// Resolved llamacpp backend: `"rocm"`, `"vulkan"`, or `"cpu"`.
    ///
    /// `None` for non-llamacpp recipes where the backend is implicit in the
    /// recipe (FLM → NPU, whispercpp → Vulkan/CPU, kokoro → CPU).
    pub backend: Option<String>,
    /// Load options derived from [`ModelConfig`] for this model ID.
    pub load_opts: ModelLoadOptions,
    /// Embedding quality tier.  [`QualityTier::NotApplicable`] for all
    /// non-embedding capabilities.
    pub quality_tier: QualityTier,
}

// ── Private helpers (module-level) ───────────────────────────────────────────

/// Derive a canonical device slot string from a [`SelectedModel`].
///
/// Used by all selector methods to enforce at-most-one-worker-per-slot:
///
/// - `flm` recipe → `"npu"` (AMD NPU via FLM runtime)
/// - `llamacpp` + rocm/vulkan/metal → `"gpu"`
/// - `llamacpp` + cpu (or unresolved) → `"cpu"`
/// - Any other recipe (e.g. `"whispercpp"`, `"kokoro"`) → the recipe name
///   itself, giving each recipe its own shared slot.
fn model_device_slot(sel: &SelectedModel) -> String {
    match sel.recipe.as_str() {
        "flm" => "npu".to_string(),
        "llamacpp" => match sel.backend.as_deref() {
            Some("rocm") | Some("vulkan") | Some("metal") => "gpu".to_string(),
            _ => "cpu".to_string(),
        },
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
        Self { catalog, config, embedding }
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
    ///   are `"npu"` (FLM), `"gpu"` (llamacpp + rocm/vulkan/metal), and `"cpu"`
    ///   (llamacpp + cpu).  The first (highest-preference) model wins each slot;
    ///   all later candidates for the same slot are dropped.  This prevents
    ///   spawning multiple NPU workers or mixing incompatible model families
    ///   (e.g. embedgemma + nomic) in the same embedding index.
    pub fn select_embedding_models(&self) -> Vec<SelectedModel> {
        let candidates = self.catalog.downloaded_models_with_label("embeddings");
        let ordered = self.apply_preference_order(&candidates, &self.config.embedding_model_preferences);

        let mut result: Vec<SelectedModel> = ordered
            .into_iter()
            .filter_map(|m| {
                let backend = self.resolve_llamacpp_backend(&m.recipe);
                if !self.is_embedding_backend_enabled(&m.recipe, backend.as_deref()) {
                    return None;
                }
                let quality_tier = if self.config.high_quality_embedding_models.contains(&m.id) {
                    QualityTier::High
                } else {
                    QualityTier::Standard
                };
                Some(SelectedModel {
                    model_id: m.id.clone(),
                    recipe: m.recipe.clone(),
                    backend,
                    load_opts: self.config.load_options_for(&m.id),
                    quality_tier,
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
            load_opts: self.config.load_options_for(&m.id),
            quality_tier,
        })
    }

    /// Returns the best available reranker (label `"reranking"`).
    pub fn select_reranker(&self) -> Option<SelectedModel> {
        let candidates = self.catalog.downloaded_models_with_label("reranking");
        let ordered = self.apply_preference_order(&candidates, &self.config.reranker_model_preferences);

        ordered.into_iter().next().map(|m| SelectedModel {
            model_id: m.id.clone(),
            recipe: m.recipe.clone(),
            backend: self.resolve_llamacpp_backend(&m.recipe),
            load_opts: self.config.load_options_for(&m.id),
            quality_tier: QualityTier::NotApplicable,
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
                load_opts: self.config.load_options_for(&m.id),
                quality_tier: QualityTier::NotApplicable,
            })
            .collect();

        let mut seen_slots = HashSet::<String>::new();
        result.retain(|s| seen_slots.insert(model_device_slot(s)));
        result
    }

    /// Returns LLM models (recipe `"llamacpp"` or `"flm"`, without
    /// `"embeddings"`, `"reranking"`, `"audio"`, `"transcription"`, or `"tts"`
    /// labels).
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
                    && !m.labels.contains("embeddings")
                    && !m.labels.contains("reranking")
                    && !m.labels.contains("audio")
                    && !m.labels.contains("transcription")
                    && !m.labels.contains("tts")
            })
            .collect();

        let ordered = self.apply_preference_order(&candidates, &self.config.llm_model_preferences);

        let mut result: Vec<SelectedModel> = ordered
            .into_iter()
            .map(|m| SelectedModel {
                model_id: m.id.clone(),
                recipe: m.recipe.clone(),
                backend: self.resolve_llamacpp_backend(&m.recipe),
                load_opts: self.config.load_options_for(&m.id),
                quality_tier: QualityTier::NotApplicable,
            })
            .collect();

        let mut seen_slots = HashSet::<String>::new();
        result.retain(|s| seen_slots.insert(model_device_slot(s)));
        result
    }

    /// Returns the TTS model (recipe `"kokoro"` or label `"tts"`).
    ///
    /// TTS has no backend parameter — the kokoro recipe is always CPU.
    pub fn select_tts(&self) -> Option<SelectedModel> {
        let by_recipe = self.catalog.downloaded_models_with_recipe("kokoro");
        let by_label = self.catalog.downloaded_models_with_label("tts");

        let mut seen: HashSet<&str> = HashSet::new();
        let mut candidates: Vec<&CatalogModel> = Vec::new();
        for m in by_recipe.into_iter().chain(by_label.into_iter()) {
            if seen.insert(m.id.as_str()) {
                candidates.push(m);
            }
        }

        let ordered = self.apply_preference_order(&candidates, &self.config.tts_model_preferences);

        ordered.into_iter().next().map(|m| SelectedModel {
            model_id: m.id.clone(),
            recipe: m.recipe.clone(),
            backend: None, // TTS is always CPU via kokoro; no backend param needed
            load_opts: self.config.load_options_for(&m.id),
            quality_tier: QualityTier::NotApplicable,
        })
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Resolve the llamacpp backend for a model.
    ///
    /// For `llamacpp` models, iterates `ModelConfig::llamacpp_backend_preference`
    /// and returns the first entry that is actually installed on the server.
    /// Falls back to `"cpu"` when no preference matches (always available).
    ///
    /// Returns `None` for non-llamacpp recipes (FLM, whispercpp, kokoro) where
    /// the backend is implicit in the recipe.
    fn resolve_llamacpp_backend(&self, recipe: &str) -> Option<String> {
        if recipe != "llamacpp" {
            return None;
        }
        for backend in &self.config.llamacpp_backend_preference {
            if self.catalog.has_installed_backend("llamacpp", backend) {
                return Some(backend.clone());
            }
        }
        Some("cpu".to_string())
    }

    /// Returns `true` if the resolved device path is enabled in the embedding config.
    ///
    /// - FLM recipe → `npu_enabled`
    /// - llamacpp with rocm/vulkan backend → `gpu_enabled`
    /// - llamacpp with cpu backend (or unresolved) → `cpu_enabled`
    /// - Any other recipe → always enabled
    fn is_embedding_backend_enabled(&self, recipe: &str, backend: Option<&str>) -> bool {
        match recipe {
            "flm" => self.embedding.npu_enabled,
            "llamacpp" => match backend {
                Some("rocm") | Some("vulkan") => self.embedding.gpu_enabled,
                _ => self.embedding.cpu_enabled,
            },
            _ => true,
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
    use crate::config::{EmbeddingDeviceConfig, ModelConfig};
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
        }
    }

    fn catalog_with(models: Vec<CatalogModel>, backends: Vec<InstalledBackend>) -> LemonadeServerCatalog {
        LemonadeServerCatalog {
            base_url: String::new(),
            models,
            backends,
            loaded: vec![],
            processor: String::new(),
            memory_gb: 0.0,
        }
    }

    fn default_embedding_cfg() -> EmbeddingDeviceConfig {
        EmbeddingDeviceConfig::default()
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
        let emb = EmbeddingDeviceConfig::default();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_embedding_models();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model_id, "embed-flm");
    }

    #[test]
    fn test_select_embedding_respects_npu_disabled() {
        let catalog = catalog_with(
            vec![model("embed-flm", "flm", &["embeddings"])],
            vec![installed_backend("flm", "npu", &["amd_npu"])],
        );
        let cfg = ModelConfig::default();
        let emb = EmbeddingDeviceConfig { npu_enabled: false, ..Default::default() };
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        assert!(selector.select_embedding_models().is_empty());
    }

    #[test]
    fn test_select_embedding_respects_gpu_disabled() {
        let catalog = catalog_with(
            vec![model("embed-gguf", "llamacpp", &["embeddings"])],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let emb = EmbeddingDeviceConfig { gpu_enabled: false, ..Default::default() };
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
        let qwen = results.iter().find(|m| m.model_id == "Qwen3-Embedding-8B-GGUF").unwrap();
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
        let mut cfg = ModelConfig::default();
        cfg.embedding_model_preferences = vec!["model-a".to_string()];
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
        let mut cfg = ModelConfig::default();
        cfg.embedding_model_preferences = vec!["embed-gemma-FLM".to_string()];
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
        let hq = results.iter().find(|m| m.quality_tier == QualityTier::High).unwrap();
        let std = results.iter().find(|m| m.quality_tier == QualityTier::Standard).unwrap();
        assert_eq!(hq.model_id, "Qwen3-Embedding-8B-GGUF");
        assert_eq!(std.model_id, "embed-std-GGUF");
    }

    // ── Backend resolution ────────────────────────────────────────────────────

    #[test]
    fn test_llamacpp_backend_prefers_rocm_over_vulkan() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["reasoning"])],
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
            vec![model("llm-gguf", "llamacpp", &["reasoning"])],
            vec![installed_backend("llamacpp", "vulkan", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results[0].backend.as_deref(), Some("vulkan"));
    }

    #[test]
    fn test_llamacpp_backend_falls_back_to_cpu_when_nothing_installed() {
        let catalog = catalog_with(
            vec![model("llm-gguf", "llamacpp", &["reasoning"])],
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
            vec![model("qwen3.5-4B-FLM", "flm", &["reasoning"])],
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

        let result = selector.select_reranker().unwrap();
        assert_eq!(result.model_id, "bge-reranker-v2-m3-GGUF");
        assert_eq!(result.quality_tier, QualityTier::NotApplicable);
    }

    #[test]
    fn test_select_reranker_none_when_missing() {
        let catalog = catalog_with(vec![], vec![]);
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        assert!(ModelSelector::new(&catalog, &cfg, &emb).select_reranker().is_none());
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
            vec![model("Whisper-Large-v3-Turbo", "whispercpp", &["audio", "transcription"])],
            vec![],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_stt_models();
        assert_eq!(results.len(), 1);
    }

    // ── LLM selection ─────────────────────────────────────────────────────────

    #[test]
    fn test_select_llm_excludes_embedding_and_reranking() {
        let catalog = catalog_with(
            vec![
                model("Gemma-4-26B-A4B-it-GGUF", "llamacpp", &["tool-calling"]),
                model("embed-gguf", "llamacpp", &["embeddings"]),     // excluded
                model("reranker", "llamacpp", &["reranking"]),         // excluded
                model("qwen3.5-4B-FLM", "flm", &["reasoning"]),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let cfg = ModelConfig::default();
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        let ids: Vec<&str> = results.iter().map(|m| m.model_id.as_str()).collect();
        assert!(ids.contains(&"qwen3.5-4B-FLM"), "FLM LLM should be included");
        assert!(ids.contains(&"Gemma-4-26B-A4B-it-GGUF"), "GPU LLM should be included");
        assert!(!ids.contains(&"embed-gguf"), "Embedding must be excluded");
        assert!(!ids.contains(&"reranker"), "Reranker must be excluded");
    }

    #[test]
    fn test_select_llm_preference_order() {
        let catalog = catalog_with(
            vec![
                model("Gemma-4-26B-A4B-it-GGUF", "llamacpp", &["tool-calling"]),
                model("qwen3.5-4B-FLM", "flm", &["reasoning"]),
            ],
            vec![],
        );
        let cfg = ModelConfig::default(); // default prefs: qwen3.5-4B-FLM first
        let emb = default_embedding_cfg();
        let selector = ModelSelector::new(&catalog, &cfg, &emb);

        let results = selector.select_llm_models();
        assert_eq!(results[0].model_id, "qwen3.5-4B-FLM");
        assert_eq!(results[1].model_id, "Gemma-4-26B-A4B-it-GGUF");
    }

    #[test]
    fn test_select_llm_limit_one_per_gpu_slot() {
        // Two GPU (llamacpp + rocm) LLMs — only the preferred one wins the slot.
        let catalog = catalog_with(
            vec![
                model("Gemma-4-26B-A4B-it-GGUF", "llamacpp", &["tool-calling"]),
                model("Qwen3-30B-A3B-GGUF", "llamacpp", &["reasoning"]),
            ],
            vec![installed_backend("llamacpp", "rocm", &["amd_igpu"])],
        );
        let mut cfg = ModelConfig::default();
        cfg.llm_model_preferences = vec!["Gemma-4-26B-A4B-it-GGUF".to_string()];
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
                model("qwen3.5-4B-FLM", "flm", &["reasoning"]),
                model("Gemma-4-26B-A4B-it-GGUF", "llamacpp", &["tool-calling"]),
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
                model("Whisper-Large-v3-Turbo", "whispercpp", &["audio", "transcription"]),
                model("Whisper-Small", "whispercpp", &["audio"]),
            ],
            vec![installed_backend("whispercpp", "cpu", &["cpu"])],
        );
        let mut cfg = ModelConfig::default();
        cfg.stt_model_preferences = vec!["Whisper-Large-v3-Turbo".to_string()];
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
                model("Whisper-Large-v3-Turbo", "whispercpp", &["audio", "transcription"]),
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
        assert_eq!(results.len(), 2, "NPU and whispercpp STT slots are independent");
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
        assert!(ModelSelector::new(&catalog, &cfg, &emb).select_tts().is_none());
    }
}
