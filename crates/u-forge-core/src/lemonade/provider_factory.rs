//! Provider factory — builds live provider instances from [`SelectedModel`] values.
//!
//! [`ProviderFactory::build`] is the single entry point that replaces the 10
//! functions in `device_factory.rs` and the per-device constructors in
//! `hardware/`.  It branches on [`Capability`] and the model's recipe/backend
//! to construct the right provider, attach the GPU resource manager where
//! needed, and return a [`BuiltProvider`] ready for queue registration.
//!
//! # GPU resource manager attachment
//!
//! The factory attaches `gpu_manager` when the resolved backend targets shared
//! GPU memory:
//! - `llamacpp` + `rocm` / `vulkan` / `metal` → GPU
//! - `whispercpp` with a GPU manager provided → GPU (caller decides)
//! - `flm` (NPU) and `kokoro` (CPU) → never GPU-locked
//!
//! Pass `None` for `gpu_manager` when you know no GPU contention is possible
//! (e.g. CPU-only deployments).

use std::sync::Arc;

use anyhow::Result;
use tracing::info;

use crate::ai::embeddings::EmbeddingProvider;
use crate::ai::transcription::TranscriptionProvider;

use super::chat::LemonadeChatProvider;
use super::embedding::LemonadeProvider;
use super::gpu_manager::GpuResourceManager;
use super::load::{load_model, ModelLoadOptions};
use super::rerank::LemonadeRerankProvider;
use super::selector::SelectedModel;
use super::stt::LemonadeSttProvider;
use super::transcription::LemonadeTranscriptionProvider;
use super::tts::LemonadeTtsProvider;

// ── Capability ────────────────────────────────────────────────────────────────

/// The inference capability a provider supplies.
///
/// Passed to [`ProviderFactory::build`] to select the right provider type.
/// Stored on [`BuiltProvider`] so the queue builder can route to the correct
/// internal channel without inspecting the concrete type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Embedding,
    Transcription,
    TextGeneration,
    TextToSpeech,
    Reranking,
}

// ── ProviderSlot ──────────────────────────────────────────────────────────────

/// Type-safe union of the provider objects the queue builder accepts.
///
/// The variants mirror the five [`Capability`] values.  Trait-object variants
/// (`Embedding`, `Transcription`) allow multiple concrete implementations to
/// share a slot; the concrete-type variants (`Chat`, `Tts`, `Rerank`) avoid
/// an unnecessary allocation where there is only one implementation.
pub enum ProviderSlot {
    Embedding(Arc<dyn EmbeddingProvider>),
    Transcription(Arc<dyn TranscriptionProvider>),
    Chat(LemonadeChatProvider),
    Tts(Box<LemonadeTtsProvider>),
    Rerank(LemonadeRerankProvider),
}

// ── BuiltProvider ─────────────────────────────────────────────────────────────

/// A live provider instance ready for registration with [`InferenceQueueBuilder`].
///
/// Produced by [`ProviderFactory::build`].
pub struct BuiltProvider {
    /// Human-readable label for logging, e.g. `"rocm/Gemma-4-26B-A4B-it-GGUF"`.
    pub name: String,
    /// The inference capability this provider supplies.
    pub capability: Capability,
    /// The wrapped provider instance.
    pub provider: ProviderSlot,
    /// Dispatch weight used by [`WeightedEmbedDispatcher`] for embedding
    /// workers.  Ignored for non-embedding capabilities (use any value).
    ///
    /// [`WeightedEmbedDispatcher`]: crate::queue::weighted::WeightedEmbedDispatcher
    pub weight: u32,
}

// ── ProviderFactory ───────────────────────────────────────────────────────────

/// Builds live provider instances from [`SelectedModel`] descriptors.
pub struct ProviderFactory;

impl ProviderFactory {
    /// Construct a provider for `selected` and return it wrapped in a
    /// [`BuiltProvider`].
    ///
    /// # Parameters
    /// - `selected`    — Model descriptor from [`ModelSelector`].
    /// - `capability`  — Which inference capability to wire up.
    /// - `base_url`    — Lemonade Server API base URL (e.g. `http://localhost:13305/api/v1`).
    /// - `weight`      — Dispatch weight (relevant only for `Embedding` workers).
    /// - `gpu_manager` — Shared GPU lock; attach when the model will share GPU memory
    ///   with STT or LLM workloads.
    ///
    /// # Errors
    ///
    /// Returns an error if the server is unreachable, the model fails to load,
    /// or (for embedding) the dimension probe fails.
    ///
    /// [`ModelSelector`]: super::selector::ModelSelector
    pub async fn build(
        selected: &SelectedModel,
        capability: Capability,
        base_url: &str,
        weight: u32,
        gpu_manager: Option<Arc<GpuResourceManager>>,
    ) -> Result<BuiltProvider> {
        let model_id = &selected.model_id;
        let load_opts = Self::merge_backend(&selected.load_opts, selected.backend.as_deref());
        let name = Self::provider_name(selected);

        let provider = match capability {
            Capability::Embedding => Self::build_embedding(base_url, model_id, &load_opts, &name).await?,
            Capability::Reranking => Self::build_reranker(base_url, model_id, &load_opts, &name).await?,
            Capability::Transcription => Self::build_stt(base_url, model_id, &load_opts, &selected.recipe, gpu_manager, &name).await?,
            Capability::TextGeneration => Self::build_llm(base_url, model_id, &load_opts, selected, gpu_manager, &name).await?,
            Capability::TextToSpeech => Self::build_tts(base_url, model_id, &name),
        };

        Ok(BuiltProvider { name, capability, provider, weight })
    }

    // ── Private build helpers ─────────────────────────────────────────────────

    async fn build_embedding(
        base_url: &str,
        model_id: &str,
        load_opts: &ModelLoadOptions,
        name: &str,
    ) -> Result<ProviderSlot> {
        let provider = LemonadeProvider::new_with_load(base_url, model_id, load_opts).await?;
        info!(model = model_id, name, dimensions = ?provider.dimensions(), "Embedding provider built");
        Ok(ProviderSlot::Embedding(Arc::new(provider)))
    }

    async fn build_reranker(
        base_url: &str,
        model_id: &str,
        load_opts: &ModelLoadOptions,
        name: &str,
    ) -> Result<ProviderSlot> {
        let provider = LemonadeRerankProvider::new(base_url, model_id);
        provider.load(load_opts).await?;
        info!(model = model_id, name, "Reranker provider built");
        Ok(ProviderSlot::Rerank(provider))
    }

    async fn build_stt(
        base_url: &str,
        model_id: &str,
        load_opts: &ModelLoadOptions,
        recipe: &str,
        gpu_manager: Option<Arc<GpuResourceManager>>,
        name: &str,
    ) -> Result<ProviderSlot> {
        load_model(base_url, model_id, load_opts).await?;

        // whispercpp with a GPU manager → GPU-locked STT (enforces sharing policy).
        // flm (NPU) or whispercpp without a GPU manager → simple provider, no lock.
        let provider: Arc<dyn TranscriptionProvider> = if recipe == "whispercpp" {
            if let Some(gpu) = gpu_manager {
                info!(model = model_id, name, "STT provider built (GPU-locked)");
                Arc::new(LemonadeSttProvider::new(base_url, model_id, gpu))
            } else {
                info!(model = model_id, name, "STT provider built (no GPU lock)");
                Arc::new(LemonadeTranscriptionProvider::new(base_url, model_id))
            }
        } else {
            // FLM (NPU) or any other recipe — no GPU contention.
            info!(model = model_id, name, "STT provider built (NPU/CPU, no GPU lock)");
            Arc::new(LemonadeTranscriptionProvider::new(base_url, model_id))
        };

        Ok(ProviderSlot::Transcription(provider))
    }

    async fn build_llm(
        base_url: &str,
        model_id: &str,
        load_opts: &ModelLoadOptions,
        selected: &SelectedModel,
        gpu_manager: Option<Arc<GpuResourceManager>>,
        name: &str,
    ) -> Result<ProviderSlot> {
        load_model(base_url, model_id, load_opts).await?;

        let gpu = if Self::backend_uses_gpu(selected) { gpu_manager } else { None };
        let provider = LemonadeChatProvider::new(base_url, model_id, gpu);
        info!(model = model_id, name, gpu_locked = provider.gpu.is_some(), "LLM provider built");
        Ok(ProviderSlot::Chat(provider))
    }

    fn build_tts(base_url: &str, model_id: &str, name: &str) -> ProviderSlot {
        let provider = LemonadeTtsProvider::new(base_url, model_id);
        info!(model = model_id, name, "TTS provider built");
        ProviderSlot::Tts(Box::new(provider))
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Copy `opts` and inject the resolved llamacpp backend.
    ///
    /// The selector stores the resolved backend on `SelectedModel.backend`; the
    /// `ModelLoadOptions` returned by `ModelConfig::load_options_for` does not
    /// include it.  This merge ensures the `/api/v1/load` call specifies the
    /// correct backend so the server doesn't auto-select a different one.
    fn merge_backend(opts: &ModelLoadOptions, backend: Option<&str>) -> ModelLoadOptions {
        ModelLoadOptions {
            ctx_size: opts.ctx_size,
            batch_size: opts.batch_size,
            ubatch_size: opts.ubatch_size,
            llamacpp_backend: backend.map(String::from),
            llamacpp_args: opts.llamacpp_args.clone(),
        }
    }

    /// Returns `true` when the model's resolved backend occupies shared GPU memory.
    ///
    /// Used to decide whether to attach the `GpuResourceManager`.  Only
    /// llamacpp models with a GPU backend (rocm / vulkan / metal) need the lock;
    /// FLM (NPU) and CPU llamacpp run on dedicated or non-contended resources.
    fn backend_uses_gpu(selected: &SelectedModel) -> bool {
        matches!(
            (selected.recipe.as_str(), selected.backend.as_deref()),
            ("llamacpp", Some("rocm")) | ("llamacpp", Some("vulkan")) | ("llamacpp", Some("metal"))
        )
    }

    /// Build a human-readable worker name for logging.
    ///
    /// Prefers `backend/model_id` for llamacpp models (e.g. `"rocm/llm.gguf"`),
    /// falls back to `recipe/model_id` for all others (e.g. `"flm/embed-gemma-FLM"`).
    fn provider_name(selected: &SelectedModel) -> String {
        match &selected.backend {
            Some(backend) => format!("{}/{}", backend, selected.model_id),
            None => format!("{}/{}", selected.recipe, selected.model_id),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lemonade::load::ModelLoadOptions;
    use crate::lemonade::selector::{QualityTier, SelectedModel};

    fn sel(model_id: &str, recipe: &str, backend: Option<&str>) -> SelectedModel {
        SelectedModel {
            model_id: model_id.to_string(),
            recipe: recipe.to_string(),
            backend: backend.map(String::from),
            load_opts: ModelLoadOptions::default(),
            quality_tier: QualityTier::NotApplicable,
        }
    }

    // ── provider_name ─────────────────────────────────────────────────────────

    #[test]
    fn test_name_llamacpp_uses_backend() {
        let s = sel("embed.gguf", "llamacpp", Some("rocm"));
        assert_eq!(ProviderFactory::provider_name(&s), "rocm/embed.gguf");
    }

    #[test]
    fn test_name_llamacpp_vulkan() {
        let s = sel("llm.gguf", "llamacpp", Some("vulkan"));
        assert_eq!(ProviderFactory::provider_name(&s), "vulkan/llm.gguf");
    }

    #[test]
    fn test_name_llamacpp_cpu() {
        let s = sel("embed.gguf", "llamacpp", Some("cpu"));
        assert_eq!(ProviderFactory::provider_name(&s), "cpu/embed.gguf");
    }

    #[test]
    fn test_name_flm_uses_recipe() {
        let s = sel("embed-gemma-FLM", "flm", None);
        assert_eq!(ProviderFactory::provider_name(&s), "flm/embed-gemma-FLM");
    }

    #[test]
    fn test_name_kokoro_uses_recipe() {
        let s = sel("kokoro-v1", "kokoro", None);
        assert_eq!(ProviderFactory::provider_name(&s), "kokoro/kokoro-v1");
    }

    #[test]
    fn test_name_whispercpp_uses_recipe() {
        let s = sel("Whisper-Large-v3-Turbo", "whispercpp", None);
        assert_eq!(ProviderFactory::provider_name(&s), "whispercpp/Whisper-Large-v3-Turbo");
    }

    // ── backend_uses_gpu ──────────────────────────────────────────────────────

    #[test]
    fn test_gpu_llamacpp_rocm() {
        assert!(ProviderFactory::backend_uses_gpu(&sel("m", "llamacpp", Some("rocm"))));
    }

    #[test]
    fn test_gpu_llamacpp_vulkan() {
        assert!(ProviderFactory::backend_uses_gpu(&sel("m", "llamacpp", Some("vulkan"))));
    }

    #[test]
    fn test_gpu_llamacpp_metal() {
        assert!(ProviderFactory::backend_uses_gpu(&sel("m", "llamacpp", Some("metal"))));
    }

    #[test]
    fn test_no_gpu_llamacpp_cpu() {
        assert!(!ProviderFactory::backend_uses_gpu(&sel("m", "llamacpp", Some("cpu"))));
    }

    #[test]
    fn test_no_gpu_flm() {
        assert!(!ProviderFactory::backend_uses_gpu(&sel("m", "flm", None)));
    }

    #[test]
    fn test_no_gpu_whispercpp() {
        assert!(!ProviderFactory::backend_uses_gpu(&sel("m", "whispercpp", None)));
    }

    #[test]
    fn test_no_gpu_kokoro() {
        assert!(!ProviderFactory::backend_uses_gpu(&sel("m", "kokoro", None)));
    }

    // ── merge_backend ─────────────────────────────────────────────────────────

    #[test]
    fn test_merge_backend_injects_backend() {
        let opts = ModelLoadOptions { ctx_size: Some(4096), ..Default::default() };
        let merged = ProviderFactory::merge_backend(&opts, Some("rocm"));
        assert_eq!(merged.ctx_size, Some(4096));
        assert_eq!(merged.llamacpp_backend.as_deref(), Some("rocm"));
    }

    #[test]
    fn test_merge_backend_none_leaves_unset() {
        let opts = ModelLoadOptions { ctx_size: Some(2048), ..Default::default() };
        let merged = ProviderFactory::merge_backend(&opts, None);
        assert_eq!(merged.ctx_size, Some(2048));
        assert!(merged.llamacpp_backend.is_none());
    }

    #[test]
    fn test_merge_backend_preserves_all_fields() {
        let opts = ModelLoadOptions {
            ctx_size: Some(8192),
            batch_size: Some(512),
            ubatch_size: Some(256),
            llamacpp_args: Some("--some-flag".to_string()),
            llamacpp_backend: None,
        };
        let merged = ProviderFactory::merge_backend(&opts, Some("vulkan"));
        assert_eq!(merged.ctx_size, Some(8192));
        assert_eq!(merged.batch_size, Some(512));
        assert_eq!(merged.ubatch_size, Some(256));
        assert_eq!(merged.llamacpp_args.as_deref(), Some("--some-flag"));
        assert_eq!(merged.llamacpp_backend.as_deref(), Some("vulkan"));
    }

    // ── Integration (requires running Lemonade Server) ────────────────────────

    #[tokio::test]
    async fn test_build_embedding_provider() {
        let url = crate::test_helpers::require_integration_url!();
        let catalog = crate::lemonade::LemonadeServerCatalog::discover(&url).await.unwrap();

        let Some(model) = catalog.downloaded_models_with_label("embeddings").into_iter().next() else {
            eprintln!("SKIP: no downloaded embedding model");
            return;
        };

        let selected = SelectedModel {
            model_id: model.id.clone(),
            recipe: model.recipe.clone(),
            backend: None,
            load_opts: ModelLoadOptions::default(),
            quality_tier: QualityTier::Standard,
        };

        let built = ProviderFactory::build(&selected, Capability::Embedding, &url, 100, None)
            .await
            .expect("build embedding provider");

        assert_eq!(built.capability, Capability::Embedding);
        assert_eq!(built.weight, 100);
        assert!(matches!(built.provider, ProviderSlot::Embedding(_)));
    }
}
