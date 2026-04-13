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

pub mod catalog;
pub mod chat;
pub mod duplicate_guard;
pub mod embedding;
pub mod gpu_manager;
pub mod health;
pub mod load;
pub mod provider_factory;
pub mod rerank;
pub mod selector;
pub mod stt;
pub mod system_info;
pub mod transcription;
pub mod tts;
pub(crate) mod client;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use catalog::{CatalogModel, InstalledBackend, LemonadeServerCatalog, LoadedModel};
pub use duplicate_guard::DuplicateGuard;
pub use provider_factory::{BuiltProvider, Capability, ProviderFactory, ProviderSlot};
pub use selector::{ModelSelector, QualityTier, SelectedModel};
pub use chat::{
    ChatChoice, ChatCompletionResponse, ChatMessage, ChatRequest,
    ChatUsage, LemonadeChatProvider, StreamToken,
};
pub use client::{make_lemonade_openai_client, LemonadeHttpClient};
pub use embedding::LemonadeProvider;
pub use transcription::LemonadeTranscriptionProvider;
pub use health::{LemonadeHealth, LoadedModelEntry};
pub use gpu_manager::{GpuResourceManager, GpuWorkload, LlmGuard, SttGuard};
pub use load::{load_model, ModelLoadOptions};
pub use rerank::{LemonadeRerankProvider, RerankDocument};
pub use stt::{LemonadeSttProvider, TranscriptionResult};
pub use system_info::{RecipeBackendInfo, SystemDeviceInfo, SystemInfo};
pub use tts::{KokoroVoice, LemonadeTtsProvider};

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
/// 1. `http://localhost:13305/api/v1` — probed via `GET /api/v1/health`.
/// 2. `http://127.0.0.1:13305/api/v1` — same probe against explicit IPv4 loopback.
/// 3. The `LEMONADE_URL` environment variable — accepted as-is with no liveness check.
///
/// Returns `None` when none of the above sources yield a reachable server.
pub async fn resolve_lemonade_url() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    for base in &["http://localhost:13305", "http://127.0.0.1:13305"] {
        if client
            .get(format!("{}/api/v1/health", base))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return Some(format!("{}/api/v1", base));
        }
    }

    std::env::var("LEMONADE_URL").ok()
}
