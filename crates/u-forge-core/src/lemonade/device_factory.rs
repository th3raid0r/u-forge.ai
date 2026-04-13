//! Factory functions for constructing hardware device objects from Lemonade
//! model registries.
//!
//! All `from_registry` construction logic lives here so that the hardware
//! module files (`npu.rs`, `gpu.rs`, `cpu.rs`) do not import Lemonade-specific
//! provider types.  The device structs retain only lightweight constructors
//! that accept already-resolved provider trait objects.
//!
//! # Design
//!
//! Callers that already have a [`LemonadeModelRegistry`] should use these
//! factory functions (or the thin wrappers on the device types that delegate
//! to them) rather than constructing providers manually.

use std::sync::Arc;

use anyhow::Result;
use tracing::info;

use crate::ai::embeddings::EmbeddingProvider;
use crate::ai::transcription::TranscriptionProvider;
use super::embedding::LemonadeProvider;
use super::transcription::LemonadeTranscriptionProvider;
use crate::config::ModelConfig;
use crate::hardware::cpu::CpuDevice;
use crate::hardware::gpu::GpuDevice;
use crate::hardware::npu::{
    NpuDevice, DEFAULT_NPU_EMBEDDING_MODEL, DEFAULT_NPU_LLM_MODEL, DEFAULT_NPU_STT_MODEL,
};
use crate::hardware::DeviceCapability;

use super::gpu_manager::GpuResourceManager;
use super::registry::LemonadeModelRegistry;
use super::stt::LemonadeSttProvider;
use super::tts::LemonadeTtsProvider;
use super::{LemonadeChatProvider, ModelLoadOptions};

// ── NpuDevice factory ─────────────────────────────────────────────────────────

/// Build an [`NpuDevice`] with embedding + transcription + optional LLM from
/// explicit model identifiers.
///
/// `load_opts` — when `Some`, the embedding model is loaded via
/// `POST /api/v1/load` before the dimension probe.
pub async fn npu_from_url_with_load(
    base_url: &str,
    embedding_model: Option<&str>,
    stt_model: Option<&str>,
    llm_model: Option<&str>,
    load_opts: Option<&ModelLoadOptions>,
) -> Result<NpuDevice> {
    let emb_model = embedding_model.unwrap_or(DEFAULT_NPU_EMBEDDING_MODEL);
    let stt_model_id = stt_model.unwrap_or(DEFAULT_NPU_STT_MODEL);

    let embedding: Option<Arc<dyn EmbeddingProvider>> = Some(Arc::new(match load_opts {
        Some(opts) => LemonadeProvider::new_with_load(base_url, emb_model, opts).await?,
        None => LemonadeProvider::new(base_url, emb_model).await?,
    }));

    let transcription: Option<Arc<dyn TranscriptionProvider>> = Some(Arc::new(
        LemonadeTranscriptionProvider::new(base_url, stt_model_id),
    ));

    let mut capabilities = vec![DeviceCapability::Embedding, DeviceCapability::Transcription];

    let chat = llm_model.map(|m| {
        let model_id = if m.is_empty() {
            DEFAULT_NPU_LLM_MODEL
        } else {
            m
        };
        capabilities.push(DeviceCapability::TextGeneration);
        info!(model = model_id, "NpuDevice: LLM provider configured");
        LemonadeChatProvider::new_npu(base_url, model_id)
    });

    info!(
        embedding_model = emb_model,
        stt_model = stt_model_id,
        llm = chat
            .as_ref()
            .map(|c| c.model.as_str())
            .unwrap_or("disabled"),
        "NpuDevice initialised"
    );

    Ok(NpuDevice::from_parts(
        "AMD NPU (FLM)".to_string(),
        capabilities,
        embedding,
        transcription,
        chat,
    ))
}

/// Build an [`NpuDevice`] from a model registry with optional load options.
pub async fn npu_from_registry_with_load(
    registry: &LemonadeModelRegistry,
    load_opts: Option<&ModelLoadOptions>,
) -> Result<NpuDevice> {
    let emb_entry = registry.npu_embedding_model().ok_or_else(|| {
        anyhow::anyhow!("No NPU embedding model found in the Lemonade registry")
    })?;

    let embedding: Option<Arc<dyn EmbeddingProvider>> = Some(Arc::new(match load_opts {
        Some(opts) => {
            LemonadeProvider::new_with_load(&registry.base_url, &emb_entry.id, opts).await?
        }
        None => LemonadeProvider::new(&registry.base_url, &emb_entry.id).await?,
    }));

    let mut capabilities = vec![DeviceCapability::Embedding];

    let transcription: Option<Arc<dyn TranscriptionProvider>> =
        registry.npu_stt_model().map(|m| {
            capabilities.push(DeviceCapability::Transcription);
            info!(model = %m.id, "NpuDevice: STT provider ready");
            Arc::new(LemonadeTranscriptionProvider::new(
                &registry.base_url,
                &m.id,
            )) as Arc<dyn TranscriptionProvider>
        });

    let chat = registry.npu_llm_model().map(|m| {
        capabilities.push(DeviceCapability::TextGeneration);
        info!(model = %m.id, "NpuDevice: LLM provider ready");
        LemonadeChatProvider::new_npu(&registry.base_url, &m.id)
    });

    if capabilities.is_empty() {
        tracing::warn!(
            "npu_from_registry_with_load: no capabilities found — \
             device will advertise nothing"
        );
    }

    info!(
        embedding = %emb_entry.id,
        stt = transcription.is_some(),
        llm = chat.as_ref().map(|c| c.model.as_str()).unwrap_or("none"),
        "NpuDevice from registry"
    );

    Ok(NpuDevice::from_parts(
        "AMD NPU (FLM)".to_string(),
        capabilities,
        embedding,
        transcription,
        chat,
    ))
}

/// Build an [`NpuDevice`] from a model registry using per-model load params
/// from `config`.
pub async fn npu_from_registry_with_config(
    registry: &LemonadeModelRegistry,
    config: &ModelConfig,
) -> Result<NpuDevice> {
    let emb_entry = registry.npu_embedding_model().ok_or_else(|| {
        anyhow::anyhow!("No NPU embedding model found in the Lemonade registry")
    })?;
    let emb_opts = config.load_options_for(&emb_entry.id);
    npu_from_registry_with_load(registry, Some(&emb_opts)).await
}

/// Build an [`NpuDevice`] with embedding only (no STT or LLM).
pub async fn npu_embedding_only(
    base_url: &str,
    model: Option<&str>,
    load_opts: Option<&ModelLoadOptions>,
) -> Result<NpuDevice> {
    let emb_model = model.unwrap_or(DEFAULT_NPU_EMBEDDING_MODEL);

    let embedding: Option<Arc<dyn EmbeddingProvider>> = Some(Arc::new(match load_opts {
        Some(opts) => LemonadeProvider::new_with_load(base_url, emb_model, opts).await?,
        None => LemonadeProvider::new(base_url, emb_model).await?,
    }));

    info!(model = emb_model, "NpuDevice initialised (embedding only)");

    Ok(NpuDevice::from_parts(
        "AMD NPU Embedding".to_string(),
        vec![DeviceCapability::Embedding],
        embedding,
        None,
        None,
    ))
}

/// Build an [`NpuDevice`] with LLM only (no embedding or STT).
pub fn npu_llm_only(base_url: &str, model: Option<&str>) -> NpuDevice {
    let llm_model = model.unwrap_or(DEFAULT_NPU_LLM_MODEL);
    info!(model = llm_model, "NpuDevice initialised (LLM only)");
    let chat = Some(LemonadeChatProvider::new_npu(base_url, llm_model));
    NpuDevice::from_parts(
        "AMD NPU LLM".to_string(),
        vec![DeviceCapability::TextGeneration],
        None,
        None,
        chat,
    )
}

/// Build an [`NpuDevice`] with transcription only (no embedding or LLM).
pub fn npu_transcription_only(base_url: &str, model: Option<&str>) -> NpuDevice {
    let stt_model = model.unwrap_or(DEFAULT_NPU_STT_MODEL);
    let transcription: Arc<dyn TranscriptionProvider> =
        Arc::new(LemonadeTranscriptionProvider::new(base_url, stt_model));
    info!(model = stt_model, "NpuDevice initialised (transcription only)");
    NpuDevice::from_parts(
        "AMD NPU STT".to_string(),
        vec![DeviceCapability::Transcription],
        None,
        Some(transcription),
        None,
    )
}

// ── GpuDevice factory ─────────────────────────────────────────────────────────

/// Build a [`GpuDevice`] from a model registry with optional load options for
/// the embedding model.
pub async fn gpu_from_registry(
    registry: &LemonadeModelRegistry,
    gpu: Arc<GpuResourceManager>,
) -> GpuDevice {
    let mut capabilities = Vec::new();

    let stt: Option<Arc<dyn TranscriptionProvider>> =
        LemonadeSttProvider::from_registry(registry, Arc::clone(&gpu))
            .ok()
            .inspect(|p| {
                capabilities.push(DeviceCapability::Transcription);
                info!(model = %p.model, "GpuDevice: STT provider ready");
            })
            .map(|p| Arc::new(p) as Arc<dyn TranscriptionProvider>);

    let chat = LemonadeChatProvider::from_registry(registry, Some(Arc::clone(&gpu)))
        .ok()
        .inspect(|p| {
            capabilities.push(DeviceCapability::TextGeneration);
            info!(model = %p.model, "GpuDevice: Chat/LLM provider ready");
        });

    let embedding =
        crate::hardware::init_llamacpp_embedding(registry, None, &mut capabilities, "GpuDevice")
            .await;

    if capabilities.is_empty() {
        tracing::warn!(
            "gpu_from_registry: no STT, LLM, or embedding models found — \
             device will advertise no capabilities"
        );
    }

    GpuDevice::from_parts(
        "AMD GPU (ROCm)".to_string(),
        capabilities,
        gpu,
        stt,
        chat,
        embedding,
    )
}

/// Build a [`GpuDevice`] from a model registry with per-model load params from
/// `config`.
pub async fn gpu_from_registry_with_config(
    registry: &LemonadeModelRegistry,
    gpu: Arc<GpuResourceManager>,
    config: &ModelConfig,
) -> GpuDevice {
    let mut capabilities = Vec::new();

    let stt: Option<Arc<dyn TranscriptionProvider>> =
        LemonadeSttProvider::from_registry(registry, Arc::clone(&gpu))
            .ok()
            .inspect(|p| {
                capabilities.push(DeviceCapability::Transcription);
                info!(model = %p.model, "GpuDevice: STT provider ready");
            })
            .map(|p| Arc::new(p) as Arc<dyn TranscriptionProvider>);

    let chat = LemonadeChatProvider::from_registry(registry, Some(Arc::clone(&gpu)))
        .ok()
        .inspect(|p| {
            capabilities.push(DeviceCapability::TextGeneration);
            info!(model = %p.model, "GpuDevice: Chat/LLM provider ready");
        });

    let load_opts_for_embed = registry
        .llamacpp_embedding_model()
        .map(|m| config.load_options_for(&m.id));
    let embedding = crate::hardware::init_llamacpp_embedding(
        registry,
        load_opts_for_embed.as_ref(),
        &mut capabilities,
        "GpuDevice",
    )
    .await;

    if capabilities.is_empty() {
        tracing::warn!(
            "gpu_from_registry_with_config: no STT, LLM, or embedding models found — \
             device will advertise no capabilities"
        );
    }

    GpuDevice::from_parts(
        "AMD GPU (ROCm)".to_string(),
        capabilities,
        gpu,
        stt,
        chat,
        embedding,
    )
}

// ── CpuDevice factory ─────────────────────────────────────────────────────────

/// Build a [`CpuDevice`] from a model registry.
pub async fn cpu_from_registry(registry: &LemonadeModelRegistry) -> CpuDevice {
    let tts = LemonadeTtsProvider::from_registry(registry).ok();

    let mut capabilities = Vec::new();
    if tts.is_some() {
        info!("CpuDevice: TTS provider ready (Kokoro)");
        capabilities.push(DeviceCapability::TextToSpeech);
    }

    let embedding =
        crate::hardware::init_llamacpp_embedding(registry, None, &mut capabilities, "CpuDevice")
            .await;

    if capabilities.is_empty() {
        tracing::warn!(
            "cpu_from_registry: no TTS or embedding models found — \
             device will advertise no capabilities"
        );
    }

    CpuDevice::from_parts(
        "CPU (Kokoro TTS)".to_string(),
        capabilities,
        tts,
        embedding,
    )
}

/// Build a [`CpuDevice`] from a model registry with per-model load params from
/// `config`.
pub async fn cpu_from_registry_with_config(
    registry: &LemonadeModelRegistry,
    config: &ModelConfig,
) -> CpuDevice {
    let tts = LemonadeTtsProvider::from_registry(registry).ok();

    let mut capabilities = Vec::new();
    if tts.is_some() {
        info!("CpuDevice: TTS provider ready (Kokoro)");
        capabilities.push(DeviceCapability::TextToSpeech);
    }

    let load_opts_for_embed = registry
        .llamacpp_embedding_model()
        .map(|m| config.load_options_for(&m.id));
    let embedding = crate::hardware::init_llamacpp_embedding(
        registry,
        load_opts_for_embed.as_ref(),
        &mut capabilities,
        "CpuDevice",
    )
    .await;

    if capabilities.is_empty() {
        tracing::warn!(
            "cpu_from_registry_with_config: no TTS or embedding models found — \
             device will advertise no capabilities"
        );
    }

    CpuDevice::from_parts(
        "CPU (Kokoro TTS)".to_string(),
        capabilities,
        tts,
        embedding,
    )
}
