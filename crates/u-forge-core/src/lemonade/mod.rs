//! Extended Lemonade Server integration.
//!
//! This module exposes the full Lemonade AI stack:
//!
//! | Component                    | Hardware | Model                       |
//! |------------------------------|----------|-----------------------------|
//! | [`LemonadeServerCatalog`]    | —        | Discovers all models        |
//! | [`ModelSelector`]            | —        | Selects models by capability|
//! | [`ProviderFactory`]          | —        | Builds live providers       |
//! | [`LemonadeTtsProvider`]      | CPU      | `kokoro-v1`                 |
//! | [`LemonadeSttProvider`]      | GPU      | `Whisper-Large-v3-Turbo`    |
//! | [`LemonadeChatProvider`]     | GPU/NPU  | llamacpp / FLM models       |
//!
//! # GPU Sharing Policy
//!
//! Both [`LemonadeSttProvider`] and [`LemonadeChatProvider`] share the same GPU and use
//! a [`GpuResourceManager`] to enforce the following rules:
//!
//! * **STT invoked while LLM is active** → returns an error immediately.
//! * **LLM invoked while STT is active** → the future is suspended and resumes when STT completes.
//! * **LLM invoked while another LLM is active** → same queuing behaviour.
//!
//! RAII guards ([`SttGuard`], [`LlmGuard`]) automatically release the GPU when dropped.

use std::sync::Arc;

pub mod catalog;
pub mod chat;
pub(crate) mod client;
pub mod duplicate_guard;
pub mod embedded;
pub mod embedding;
pub mod gpu_manager;
pub mod health;
pub mod load;
pub mod management;
pub mod provider_factory;
pub mod rerank;
pub mod runtime;
pub mod selector;
pub mod stt;
pub mod system_info;
pub mod transcription;
pub mod tts;
pub mod unload;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use catalog::{CatalogModel, InstalledBackend, LemonadeServerCatalog, LoadedModel};
pub use chat::{
    AgentBudgetDiagnostics, ChatChoice, ChatCompletionResponse, ChatEvent, ChatMessage,
    ChatRequest, ChatTerminalReason, ChatUsage, LemonadeChatProvider, StreamToken,
};
pub use client::{
    LemonadeConnection, LemonadeHttpClient, LemonadeOwnership, LemonadeSecret,
    LemonadeTelemetryLinks, LemonadeTimeouts, make_lemonade_openai_client,
    make_lemonade_openai_client_for,
};
pub use duplicate_guard::DuplicateGuard;
pub use embedded::{EmbeddedLemonade, EmbeddedRuntimeError};
pub use embedding::LemonadeProvider;
pub use gpu_manager::{GpuResourceManager, GpuWorkload, LlmGuard, SttGuard};
pub use health::{LemonadeHealth, LoadedModelEntry};
pub use load::{
    ModelLoadOptions, load_model, load_model_for_recipe_with_connection,
    load_model_with_connection, reload_model, reload_model_for_recipe_with_connection,
    reload_model_with_connection,
};
pub use management::{
    DownloadAction, LemonadeManagement, ManagementEventKind, ManagementOperationKind,
    ManagementProgressEvent, ManagementProgressReceiver, SetupBackendChoice, SetupComponent,
    SetupComponentState, SetupRole, chat_component_state, component_state,
    initial_setup_components, select_setup_backend, setup_chat_models,
};
pub use provider_factory::{
    BuiltProvider, Capability, CoordinatedChatProvider, ProviderFactory, ProviderSlot,
};
pub use rerank::{LemonadeRerankProvider, RerankDocument, RerankProvider};
pub use runtime::{
    LemonadeRuntime, LemonadeRuntimeLease, LemonadeRuntimeProfile, LoadedProfileKey,
    ReasoningPolicy,
};
pub use selector::{EffectiveChatLimits, ModelSelector, QualityTier, SelectedModel};
pub use stt::{LemonadeSttProvider, TranscriptionResult};
pub use system_info::{RecipeBackendInfo, SystemDeviceInfo, SystemInfo};
pub use transcription::LemonadeTranscriptionProvider;
pub use tts::{KokoroVoice, LemonadeTtsProvider};
pub use unload::{
    unload_all_models, unload_all_models_with_connection, unload_model,
    unload_model_with_connection,
};

// ── URL resolution utilities ──────────────────────────────────────────────────

/// Resolve a Lemonade Server URL for a specific provider.
///
/// Shared helper for provider auto-discovery to avoid duplicating the
/// `arg → env var → [probe]` resolution pattern.
///
/// # Parameters
/// - `explicit`         — Caller-supplied URL (highest priority).
/// - `env_var`          — Name of the environment variable to check next.
/// - `probe_localhost`  — When `true`, falls back to probing localhost if
///   neither `explicit` nor the env var are set.
///
/// Returns `None` when no URL could be found.
pub async fn resolve_provider_url(
    explicit: Option<&str>,
    env_var: &str,
    probe_localhost: bool,
) -> Option<String> {
    if let Some(url) = explicit {
        return Some(url.to_string());
    }
    if let Ok(url) = std::env::var(env_var) {
        return Some(url);
    }
    if probe_localhost {
        return resolve_lemonade_url().await;
    }
    None
}

/// Resolve a reachable Lemonade Server base URL.
///
/// Resolution order:
///
/// 1. `LEMONADE_URL`, normalized to a supported API prefix.
/// 2. `http://localhost:13305/v1` — probed via `GET /v1/health`.
/// 3. `http://127.0.0.1:13305/v1` — explicit IPv4 loopback fallback.
///
/// Returns `None` when none of the above sources yield a reachable server.
pub async fn resolve_lemonade_url() -> Option<String> {
    if let Ok(url) = std::env::var("LEMONADE_URL") {
        return LemonadeConnection::external(&url)
            .ok()
            .map(|connection| connection.api_base().to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    for base in &["http://localhost:13305", "http://127.0.0.1:13305"] {
        if client
            .get(format!("{}/v1/health", base))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return Some(format!("{}/v1", base));
        }
    }

    None
}

/// Select an explicit external connection or launch the private embedded
/// runtime. The embedded path never adopts an unrelated process already bound
/// to a candidate port.
pub async fn resolve_runtime_connection()
-> anyhow::Result<(Arc<LemonadeConnection>, Option<Arc<EmbeddedLemonade>>)> {
    if let Ok(url) = std::env::var("LEMONADE_URL") {
        return Ok((Arc::new(LemonadeConnection::external(&url)?), None));
    }
    let embedded = EmbeddedLemonade::launch().await?;
    Ok((embedded.connection(), Some(embedded)))
}
