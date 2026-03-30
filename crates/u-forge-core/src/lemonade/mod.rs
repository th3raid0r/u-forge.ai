//! Extended Lemonade Server integration.
//!
//! This module builds on the base [`LemonadeProvider`](crate::LemonadeProvider) to expose
//! the full breadth of the hardware-aware Lemonade stack:
//!
//! | Component                  | Hardware | Model                    |
//! |----------------------------|----------|--------------------------|
//! | [`LemonadeModelRegistry`]  | —        | Discovers all models     |
//! | [`LemonadeTtsProvider`]    | CPU      | `kokoro-v1`              |
//! | [`LemonadeSttProvider`]    | GPU      | `Whisper-Large-v3-Turbo` |
//! | [`LemonadeChatProvider`]   | GPU      | `GLM-4.7-Flash-GGUF`     |
//! | NPU embedding              | NPU      | `embed-gemma-300m-FLM`   |
//!
//! # GPU Sharing Policy
//!
//! Both [`LemonadeSttProvider`] and [`LemonadeChatProvider`] share the same GPU and use
//! a [`GpuResourceManager`] to enforce the following rules:
//!
//! * **STT invoked while LLM is active** → returns an error immediately.  STT is
//!   latency-sensitive and must not be made to wait for a long inference run.
//! * **LLM invoked while STT is active** → the future is suspended and resumes as soon as
//!   the STT session completes.
//! * **LLM invoked while another LLM is active** → same queuing behaviour.
//!
//! RAII guards ([`SttGuard`], [`LlmGuard`]) automatically release the GPU when dropped,
//! so callers cannot forget to unlock the resource.

pub mod chat;
pub(crate) mod client;
pub mod gpu_manager;
pub mod load;
pub mod registry;
pub mod rerank;
pub mod stt;
pub mod stack;
pub mod system_info;
pub mod tts;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use chat::{
    ChatChoice, ChatCompletionResponse, ChatMessage, ChatRequest, ChatUsage,
    LemonadeChatProvider,
};
pub use client::LemonadeHttpClient;
pub use gpu_manager::{GpuResourceManager, GpuWorkload, LlmGuard, SttGuard};
pub use load::{load_model, ModelLoadOptions};
pub use registry::{LemonadeModelEntry, LemonadeModelRegistry, ModelRole};
pub use rerank::{LemonadeRerankProvider, RerankDocument};
pub use stt::{LemonadeSttProvider, TranscriptionResult};
pub use stack::LemonadeStack;
pub use system_info::{LemonadeCapabilities, RecipeBackendInfo, SystemDeviceInfo, SystemInfo};
pub use tts::{KokoroVoice, LemonadeTtsProvider};

// ── URL resolution utilities ──────────────────────────────────────────────────

/// Resolve a Lemonade Server URL for a specific provider.
///
/// Shared helper for [`EmbeddingManager::try_new_auto`](crate::EmbeddingManager::try_new_auto) and
/// [`TranscriptionManager::try_new_auto`](crate::transcription::TranscriptionManager::try_new_auto)
/// to avoid duplicating the `arg → env var → [probe]` resolution pattern.
///
/// # Parameters
/// - `explicit`         — Caller-supplied URL (highest priority).
/// - `env_var`          — Name of the environment variable to check next.
/// - `probe_localhost`  — When `true`, falls back to probing localhost if
///   neither `explicit` nor the env var are set.  Pass `false` for providers
///   that should hard-error instead of probing (e.g. transcription).
///
/// Returns `None` when no URL could be found, indicating a hard error is
/// appropriate for the caller.
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
/// This is the canonical URL-discovery routine used both at application startup
/// and in integration tests.  Resolution order:
///
/// 1. `http://localhost:8000/api/v1` — probed via `GET /api/v1/health` with a
///    2-second timeout.  This is the default Lemonade Server port.
/// 2. `http://127.0.0.1:8000/api/v1` — same probe against the explicit IPv4
///    loopback address, in case `localhost` resolves to `::1` on the host.
/// 3. The `LEMONADE_URL` environment variable — accepted as-is with no liveness
///    check, allowing non-standard or remote servers to be configured.
///
/// Returns `None` when none of the above sources yield a reachable server.
pub async fn resolve_lemonade_url() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    for base in &["http://localhost:8000", "http://127.0.0.1:8000"] {
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

    // Fall back to an explicitly configured URL (e.g. a remote dev server).
    std::env::var("LEMONADE_URL").ok()
}
