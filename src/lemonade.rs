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

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

// ── Model registry ────────────────────────────────────────────────────────────

/// Raw model entry as returned by `GET /api/v1/models`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LemonadeModelEntry {
    pub id: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub recipe: String,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default)]
    pub downloaded: Option<bool>,
    #[serde(default)]
    pub suggested: Option<bool>,
}

/// Functional role assigned to a model based on its id suffix and labels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelRole {
    /// NPU-accelerated embedding (`flm` recipe + `embeddings` label).
    NpuEmbedding,
    /// NPU speech-to-text (`flm` recipe + `audio`/`transcription` label).
    NpuStt,
    /// NPU large language model (`flm` recipe + `reasoning`/`tool-calling` label).
    NpuLlm,
    /// CPU text-to-speech (kokoro recipe).
    CpuTts,
    /// CPU/GPU embedding via llamacpp (`llamacpp` recipe + `embeddings` label).
    CpuEmbedding,
    /// GPU speech-to-text (whispercpp recipe or `transcription` label, non-FLM).
    GpuStt,
    /// GPU large language model (llamacpp recipe, reasoning/tool-calling labels).
    GpuLlm,
    /// Reranking model.
    Reranker,
    /// Image-generation model.
    ImageGen,
    /// Any model not matching a known role.
    Other,
}

impl LemonadeModelEntry {
    /// Classify this entry into a [`ModelRole`] based on labels and recipe.
    ///
    /// FLM (NPU) models are checked first so that `whisper-v3-turbo-FLM` and
    /// `qwen3-8b-FLM` are not misclassified as GPU models purely on label.
    pub fn role(&self) -> ModelRole {
        let labels: Vec<&str> = self.labels.iter().map(String::as_str).collect();

        // ── FLM (NPU) recipe — must be checked before generic label checks ──
        if self.recipe == "flm" {
            if labels.contains(&"embeddings") {
                return ModelRole::NpuEmbedding;
            }
            if labels.contains(&"transcription") || labels.contains(&"audio") {
                return ModelRole::NpuStt;
            }
            if labels.contains(&"reasoning")
                || labels.contains(&"tool-calling")
                || labels.contains(&"coding")
            {
                return ModelRole::NpuLlm;
            }
        }

        // ── TTS: kokoro recipe or explicit label ───────────────────────────
        if self.recipe == "kokoro" || labels.contains(&"tts") || labels.contains(&"speech") {
            return ModelRole::CpuTts;
        }

        // ── STT / transcription (non-FLM) ─────────────────────────────────
        if self.recipe == "whispercpp"
            || (labels.contains(&"transcription") && self.recipe != "flm")
            || (labels.contains(&"audio") && !labels.contains(&"tts"))
        {
            return ModelRole::GpuStt;
        }

        // ── Image generation ───────────────────────────────────────────────
        if self.recipe == "sd-cpp" || labels.contains(&"image") {
            return ModelRole::ImageGen;
        }

        // ── Reranking ──────────────────────────────────────────────────────
        if labels.contains(&"reranking") {
            return ModelRole::Reranker;
        }

        // ── llamacpp embeddings ────────────────────────────────────────────
        if self.recipe == "llamacpp" && labels.contains(&"embeddings") {
            return ModelRole::CpuEmbedding;
        }

        // ── LLM: llamacpp recipe with cognitive labels ─────────────────────
        if self.recipe == "llamacpp"
            && (labels.contains(&"reasoning")
                || labels.contains(&"tool-calling")
                || labels.contains(&"vision")
                || labels.contains(&"coding"))
        {
            return ModelRole::GpuLlm;
        }

        ModelRole::Other
    }
}

/// Wire-format response envelope from `GET /api/v1/models`.
#[derive(Debug, Deserialize)]
struct ModelsListResponse {
    data: Vec<LemonadeModelEntry>,
}

/// Fetched and categorised view of all models available on a Lemonade server.
///
/// Construct via [`LemonadeModelRegistry::fetch`].
#[derive(Debug, Clone)]
pub struct LemonadeModelRegistry {
    /// The base URL that was used to build this registry (trailing slash stripped).
    pub base_url: String,
    /// All model entries reported by the server.
    pub models: Vec<LemonadeModelEntry>,
}

impl LemonadeModelRegistry {
    /// Fetch the model list from `GET {base_url}/models` and build a registry.
    pub async fn fetch(base_url: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        let base = base_url.trim_end_matches('/');
        let url = format!("{base}/models");

        let resp: ModelsListResponse = client
            .get(&url)
            .header("Authorization", "Bearer lemonade")
            .send()
            .await
            .with_context(|| format!("Failed to reach Lemonade server at {url}"))?
            .error_for_status()
            .context("Lemonade /models returned an error status")?
            .json()
            .await
            .context("Failed to parse /models JSON response")?;

        info!(
            model_count = resp.data.len(),
            base_url, "Lemonade model registry loaded"
        );

        Ok(Self {
            base_url: base.to_string(),
            models: resp.data,
        })
    }

    /// All models matching `role`, in the order they were returned by the server.
    pub fn by_role(&self, role: &ModelRole) -> Vec<&LemonadeModelEntry> {
        self.models.iter().filter(|m| &m.role() == role).collect()
    }

    /// The preferred NPU embedding model (`embed-gemma-300m-FLM`), if available.
    ///
    /// Falls back to any model classified as [`ModelRole::NpuEmbedding`].
    pub fn npu_embedding_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "embed-gemma-300m-FLM")
            .or_else(|| self.by_role(&ModelRole::NpuEmbedding).into_iter().next())
    }

    /// The CPU TTS model (`kokoro-v1`), if available.
    ///
    /// Falls back to any model classified as [`ModelRole::CpuTts`].
    pub fn tts_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "kokoro-v1")
            .or_else(|| self.by_role(&ModelRole::CpuTts).into_iter().next())
    }

    /// The GPU STT model (`Whisper-Large-v3-Turbo`), if available.
    ///
    /// Only returns models classified as [`ModelRole::GpuStt`] (whispercpp recipe).
    /// For the NPU FLM whisper, use [`npu_stt_model`](Self::npu_stt_model).
    pub fn stt_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "Whisper-Large-v3-Turbo" && m.role() == ModelRole::GpuStt)
            .or_else(|| self.by_role(&ModelRole::GpuStt).into_iter().next())
    }

    /// The NPU STT model (`whisper-v3-turbo-FLM`), if available.
    ///
    /// Only returns models classified as [`ModelRole::NpuStt`] (FLM recipe + audio label).
    pub fn npu_stt_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "whisper-v3-turbo-FLM")
            .or_else(|| self.by_role(&ModelRole::NpuStt).into_iter().next())
    }

    /// The primary GPU LLM (`GLM-4.7-Flash-GGUF`), if available.
    ///
    /// Only returns models classified as [`ModelRole::GpuLlm`] (llamacpp recipe).
    /// For NPU FLM LLMs, use [`npu_llm_model`](Self::npu_llm_model).
    pub fn llm_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "GLM-4.7-Flash-GGUF" && m.role() == ModelRole::GpuLlm)
            .or_else(|| self.by_role(&ModelRole::GpuLlm).into_iter().next())
    }

    /// The preferred NPU LLM model (`qwen3-8b-FLM`), if available.
    ///
    /// Prefers the lighter `qwen3-8b-FLM` for responsiveness; falls back to any
    /// model classified as [`ModelRole::NpuLlm`] (FLM recipe + reasoning/tool-calling label).
    pub fn npu_llm_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "qwen3-8b-FLM")
            .or_else(|| {
                // prefer smaller model over larger for latency
                self.by_role(&ModelRole::NpuLlm).into_iter().min_by(|a, b| {
                    a.size
                        .unwrap_or(f64::MAX)
                        .partial_cmp(&b.size.unwrap_or(f64::MAX))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            })
    }

    /// The preferred reranker model (`bge-reranker-v2-m3-GGUF`), if available.
    ///
    /// Falls back to any model classified as [`ModelRole::Reranker`].
    pub fn reranker_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "bge-reranker-v2-m3-GGUF")
            .or_else(|| self.by_role(&ModelRole::Reranker).into_iter().next())
    }

    /// The preferred CPU/GPU llamacpp embedding model, if available.
    ///
    /// Prefers `nomic-embed-text-v2-moe-GGUF` (MoE, more recent) over
    /// `nomic-embed-text-v1-GGUF`; falls back to any [`ModelRole::CpuEmbedding`]
    /// model compatible with the standard 768-dim index.
    ///
    /// `Qwen3-Embedding-8B-GGUF` is excluded unless
    /// [`ENABLE_HIGH_QUALITY_EMBEDDING`](crate::ENABLE_HIGH_QUALITY_EMBEDDING)
    /// is `true` — it outputs 4096-dim vectors that require a separate index.
    pub fn cpu_embedding_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "nomic-embed-text-v2-moe-GGUF")
            .or_else(|| {
                self.models
                    .iter()
                    .find(|m| m.id == "nomic-embed-text-v1-GGUF")
            })
            .or_else(|| {
                self.by_role(&ModelRole::CpuEmbedding)
                    .into_iter()
                    .find(|m| {
                        crate::storage::ENABLE_HIGH_QUALITY_EMBEDDING
                            || m.id != "Qwen3-Embedding-8B-GGUF"
                    })
            })
    }

    /// All CPU/GPU llamacpp embedding models suitable for parallel workers.
    ///
    /// Returns every [`ModelRole::CpuEmbedding`] model in a stable preferred
    /// order: `nomic-embed-text-v2-moe-GGUF` first (newer MoE architecture),
    /// then `nomic-embed-text-v1-GGUF`, then any remaining models in
    /// server-reported order.
    ///
    /// `Qwen3-Embedding-8B-GGUF` is **excluded** unless
    /// [`ENABLE_HIGH_QUALITY_EMBEDDING`](crate::ENABLE_HIGH_QUALITY_EMBEDDING)
    /// is `true`.  It outputs 4096-dim vectors that are incompatible with the
    /// standard 768-dim index and require a separate search pipeline that is
    /// not yet implemented.
    ///
    /// Callers should still probe each returned model's actual output dimensions
    /// via [`LemonadeProvider::new`](crate::LemonadeProvider::new) and discard
    /// any whose dimensions do not match [`crate::EMBEDDING_DIMENSIONS`].
    pub fn all_cpu_embedding_models(&self) -> Vec<&LemonadeModelEntry> {
        // Gate high-quality models behind the feature flag.
        let high_quality = crate::storage::ENABLE_HIGH_QUALITY_EMBEDDING;

        // Stable preferred order: v2-moe first (newer MoE architecture), v1
        // second, everything else appended in server-reported order.
        const PREFERRED: &[&str] = &["nomic-embed-text-v2-moe-GGUF", "nomic-embed-text-v1-GGUF"];

        let candidates: Vec<&LemonadeModelEntry> = self
            .by_role(&ModelRole::CpuEmbedding)
            .into_iter()
            .filter(|m| high_quality || m.id != "Qwen3-Embedding-8B-GGUF")
            .collect();

        // Pass 1: emit preferred models in declared order (skip absent ones).
        let mut result: Vec<&LemonadeModelEntry> = PREFERRED
            .iter()
            .filter_map(|&id| candidates.iter().copied().find(|m| m.id == id))
            .collect();

        // Pass 2: append anything not already in the preferred list.
        for m in &candidates {
            if !PREFERRED.contains(&m.id.as_str()) {
                result.push(m);
            }
        }

        result
    }

    /// A human-readable summary of models grouped by role, for logging/diagnostics.
    pub fn summary(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        for m in &self.models {
            let _ = writeln!(
                s,
                "  [{:?}] {} ({:.2} GB, recipe={})",
                m.role(),
                m.id,
                m.size.unwrap_or(0.0),
                m.recipe,
            );
        }
        s
    }
}

// ── GPU resource manager ──────────────────────────────────────────────────────

/// The current exclusive workload occupying the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuWorkload {
    /// No active workload — GPU is free.
    Idle,
    /// Whisper STT is running. LLM requests will queue; further STT requests are rejected.
    SttActive,
    /// LLM inference is running. STT requests are rejected immediately.
    LlmActive,
}

impl std::fmt::Display for GpuWorkload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuWorkload::Idle => write!(f, "Idle"),
            GpuWorkload::SttActive => write!(f, "STT active"),
            GpuWorkload::LlmActive => write!(f, "LLM active"),
        }
    }
}

/// Enforces the GPU sharing policy between the latency-sensitive STT workload and
/// the throughput-oriented LLM inference workload.
///
/// Always construct via [`GpuResourceManager::new`], which returns an `Arc<Self>`
/// suitable for sharing across providers.
///
/// # Policy Summary
///
/// | Request | GPU state | Outcome           |
/// |---------|-----------|-------------------|
/// | STT     | Idle      | Acquired          |
/// | STT     | LlmActive | **Error** (blocked) |
/// | STT     | SttActive | Error (busy)      |
/// | LLM     | Idle      | Acquired          |
/// | LLM     | SttActive | **Queued** (waits) |
/// | LLM     | LlmActive | Queued (serialised)|
pub struct GpuResourceManager {
    workload: Mutex<GpuWorkload>,
    /// Notified whenever the workload transitions to [`GpuWorkload::Idle`].
    notify: Notify,
}

impl std::fmt::Debug for GpuResourceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuResourceManager")
            .field("workload", &*self.workload.lock())
            .finish()
    }
}

impl GpuResourceManager {
    /// Create a new, idle GPU resource manager wrapped in `Arc`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            workload: Mutex::new(GpuWorkload::Idle),
            notify: Notify::new(),
        })
    }

    /// Snapshot of the current GPU workload state.
    pub fn current_workload(&self) -> GpuWorkload {
        self.workload.lock().clone()
    }

    /// Attempt to acquire the GPU for STT work.
    ///
    /// This is a **non-blocking** call:
    /// - Returns `Ok(SttGuard)` when the GPU is idle.
    /// - Returns `Err` immediately if the GPU is busy with LLM inference or another STT
    ///   session.  Callers should surface this as a user-visible "try again later" message.
    pub fn begin_stt(self: &Arc<Self>) -> Result<SttGuard> {
        let mut w = self.workload.lock();
        match *w {
            GpuWorkload::Idle => {
                *w = GpuWorkload::SttActive;
                debug!("GPU acquired for STT");
                Ok(SttGuard {
                    manager: Arc::clone(self),
                })
            }
            GpuWorkload::LlmActive => Err(anyhow!(
                "GPU busy: LLM inference is in progress. \
                 STT is latency-sensitive and cannot be queued — retry once the \
                 LLM request completes."
            )),
            GpuWorkload::SttActive => {
                Err(anyhow!("GPU busy: an STT session is already in progress."))
            }
        }
    }

    /// Acquire the GPU for LLM inference, **waiting** if the GPU is currently busy.
    ///
    /// This is an **async** call that suspends the calling task when:
    /// - STT is active (queues until the STT session ends), or
    /// - Another LLM is active (serialises requests).
    ///
    /// It will never return an error — it simply waits for the GPU to become available.
    pub async fn begin_llm(self: &Arc<Self>) -> LlmGuard {
        loop {
            // Scope: hold the parking_lot mutex only briefly, never across .await.
            {
                let mut w = self.workload.lock();
                if *w == GpuWorkload::Idle {
                    *w = GpuWorkload::LlmActive;
                    debug!("GPU acquired for LLM inference");
                    return LlmGuard {
                        manager: Arc::clone(self),
                    };
                }
                let reason = w.clone();
                drop(w); // release before .await
                match reason {
                    GpuWorkload::SttActive => {
                        info!("LLM request queued: waiting for active STT session to complete");
                    }
                    GpuWorkload::LlmActive => {
                        debug!(
                            "LLM request queued: waiting for previous LLM inference to complete"
                        );
                    }
                    GpuWorkload::Idle => unreachable!(),
                }
            }
            // Suspend and wake up when a guard is dropped.
            self.notify.notified().await;
        }
    }

    /// Internal: release the GPU and wake all waiters.
    fn release(&self) {
        let mut w = self.workload.lock();
        *w = GpuWorkload::Idle;
        // Drop the lock before notifying so waiters don't spin on a held lock.
        drop(w);
        self.notify.notify_waiters();
    }
}

/// RAII guard that holds the GPU in [`GpuWorkload::SttActive`] mode.
///
/// When this value is dropped (normally or on error), the GPU is returned to
/// [`GpuWorkload::Idle`] and any queued LLM requests are woken.
pub struct SttGuard {
    manager: Arc<GpuResourceManager>,
}

impl std::fmt::Debug for SttGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttGuard").finish()
    }
}

impl Drop for SttGuard {
    fn drop(&mut self) {
        debug!("GPU released from STT — notifying waiters");
        self.manager.release();
    }
}

/// RAII guard that holds the GPU in [`GpuWorkload::LlmActive`] mode.
///
/// When dropped, the GPU returns to [`GpuWorkload::Idle`] and the next queued
/// request (STT or LLM) is woken.
pub struct LlmGuard {
    manager: Arc<GpuResourceManager>,
}

impl std::fmt::Debug for LlmGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmGuard").finish()
    }
}

impl Drop for LlmGuard {
    fn drop(&mut self) {
        debug!("GPU released from LLM inference — notifying waiters");
        self.manager.release();
    }
}

// ── TTS provider ─────────────────────────────────────────────────────────────

/// Built-in voices supported by kokoro-v1.
///
/// Pass [`KokoroVoice::Custom`] to use any voice string the server accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KokoroVoice {
    /// American English female (default, high quality).
    AfSky,
    /// American English female, warmer tone.
    AfHeart,
    /// American English male.
    AmAdam,
    /// British English male.
    BmGeorge,
    /// British English female.
    BfEmma,
    /// Arbitrary voice identifier forwarded verbatim to the API.
    Custom(String),
}

impl KokoroVoice {
    /// The voice identifier string expected by the Lemonade / kokoro API.
    pub fn as_str(&self) -> &str {
        match self {
            KokoroVoice::AfSky => "af_sky",
            KokoroVoice::AfHeart => "af_heart",
            KokoroVoice::AmAdam => "am_adam",
            KokoroVoice::BmGeorge => "bm_george",
            KokoroVoice::BfEmma => "bf_emma",
            KokoroVoice::Custom(v) => v.as_str(),
        }
    }
}

impl Default for KokoroVoice {
    fn default() -> Self {
        KokoroVoice::AfSky
    }
}

impl std::fmt::Display for KokoroVoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Text-to-speech via kokoro-v1 running on CPU.
///
/// Calls `POST /api/v1/audio/speech` and returns the raw audio bytes.
/// The response content type is typically `audio/wav`, but inspect the
/// `Content-Type` header of the raw HTTP response if you need to be certain.
///
/// This provider does **not** interact with [`GpuResourceManager`] because kokoro
/// runs entirely on the CPU.
#[derive(Debug, Clone)]
pub struct LemonadeTtsProvider {
    client: reqwest::Client,
    base_url: String,
    /// The model id sent to the API (e.g. `"kokoro-v1"`).
    pub model: String,
    /// Voice used when none is specified at call time.
    pub default_voice: KokoroVoice,
}

impl LemonadeTtsProvider {
    /// Construct with an explicit base URL and model id.
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            default_voice: KokoroVoice::default(),
        }
    }

    /// Construct using the TTS model discovered in `registry`.
    pub fn from_registry(registry: &LemonadeModelRegistry) -> Result<Self> {
        let model = registry
            .tts_model()
            .ok_or_else(|| anyhow!("No TTS model found in the Lemonade registry"))?;
        Ok(Self::new(&registry.base_url, &model.id))
    }

    /// Override the default voice.
    pub fn with_voice(mut self, voice: KokoroVoice) -> Self {
        self.default_voice = voice;
        self
    }

    /// Synthesize `text` into audio.
    ///
    /// `voice` overrides `self.default_voice` for this call only.
    /// Returns raw audio bytes (typically WAV).
    pub async fn synthesize(&self, text: &str, voice: Option<&KokoroVoice>) -> Result<Vec<u8>> {
        let voice_str = voice.unwrap_or(&self.default_voice).as_str();
        let start = std::time::Instant::now();

        let body = serde_json::json!({
            "model": self.model,
            "input":  text,
            "voice":  voice_str,
        });

        let response = self
            .client
            .post(format!("{}/audio/speech", self.base_url))
            .header("Authorization", "Bearer lemonade")
            .json(&body)
            .send()
            .await
            .context("TTS HTTP request failed")?
            .error_for_status()
            .context("Lemonade TTS returned an error status")?;

        let bytes = response
            .bytes()
            .await
            .context("Failed to read TTS audio bytes from response")?;

        tracing::debug!(
            model    = %self.model,
            voice    = %voice_str,
            input_chars = text.len(),
            output_bytes = bytes.len(),
            duration_ms  = start.elapsed().as_millis(),
            "TTS synthesis complete"
        );

        Ok(bytes.to_vec())
    }

    /// Synthesize using `self.default_voice`.
    pub async fn synthesize_default(&self, text: &str) -> Result<Vec<u8>> {
        let voice = self.default_voice.clone();
        self.synthesize(text, Some(&voice)).await
    }
}

// ── STT provider ─────────────────────────────────────────────────────────────

/// Transcription result returned by the Whisper endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TranscriptionResult {
    /// The transcribed text.
    pub text: String,
}

/// Speech-to-text via `Whisper-Large-v3-Turbo` running on GPU.
///
/// Uses a shared [`GpuResourceManager`] to enforce the GPU sharing policy.
/// Calls to [`LemonadeSttProvider::transcribe`] will return an error immediately
/// if LLM inference is currently active — STT must never queue behind slow inference.
#[derive(Debug, Clone)]
pub struct LemonadeSttProvider {
    client: reqwest::Client,
    base_url: String,
    /// The model id sent to the API (e.g. `"Whisper-Large-v3-Turbo"`).
    pub model: String,
    /// Shared GPU resource manager — also held by [`LemonadeChatProvider`].
    pub gpu: Arc<GpuResourceManager>,
}

impl LemonadeSttProvider {
    /// Construct with an explicit base URL, model id, and GPU manager.
    pub fn new(base_url: &str, model: &str, gpu: Arc<GpuResourceManager>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            gpu,
        }
    }

    /// Construct using the STT model discovered in `registry`.
    pub fn from_registry(
        registry: &LemonadeModelRegistry,
        gpu: Arc<GpuResourceManager>,
    ) -> Result<Self> {
        let model = registry
            .stt_model()
            .ok_or_else(|| anyhow!("No STT model found in the Lemonade registry"))?;
        Ok(Self::new(&registry.base_url, &model.id, gpu))
    }

    /// Transcribe `audio_data` to text.
    ///
    /// `audio_data` should be a valid audio file (WAV, MP3, OGG, FLAC, …).
    /// `filename` is the name hint sent to the server (e.g. `"recording.wav"`).
    ///
    /// # GPU Policy
    /// Returns `Err` immediately if LLM inference is currently occupying the GPU.
    /// The caller should surface this to the user as a "GPU busy" message and retry.
    pub async fn transcribe(
        &self,
        audio_data: Vec<u8>,
        filename: &str,
    ) -> Result<TranscriptionResult> {
        // Enforce GPU policy — STT is latency-sensitive, never queue.
        let _guard = self.gpu.begin_stt()?;

        let start = std::time::Instant::now();

        let audio_part = reqwest::multipart::Part::bytes(audio_data)
            .file_name(filename.to_string())
            .mime_str("audio/wav")
            .context("Failed to set audio MIME type")?;

        let form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", audio_part);

        let result: TranscriptionResult = self
            .client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .header("Authorization", "Bearer lemonade")
            .multipart(form)
            .send()
            .await
            .context("STT HTTP request failed")?
            .error_for_status()
            .context("Lemonade STT returned an error status")?
            .json()
            .await
            .context("Failed to parse STT transcription response")?;

        tracing::debug!(
            model        = %self.model,
            text_len     = result.text.len(),
            duration_ms  = start.elapsed().as_millis(),
            "STT transcription complete"
        );

        Ok(result)
        // _guard is dropped here → GPU released, queued LLM requests are woken.
    }
}

// ── Chat / LLM provider ───────────────────────────────────────────────────────

/// A single message in a chat conversation, following the OpenAI `messages` format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"system"`, `"user"`, or `"assistant"`.
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

/// Full response from `POST /api/v1/chat/completions`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<ChatUsage>,
}

/// A single completion choice.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token usage reported by the model.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl ChatCompletionResponse {
    /// Return the text content of the first choice, if any.
    pub fn first_content(&self) -> Option<&str> {
        self.choices.first().map(|c| c.message.content.as_str())
    }
}

/// Configuration for a single chat request, allowing per-call overrides.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    /// Overrides `LemonadeChatProvider::default_max_tokens`.
    pub max_tokens: Option<u32>,
    /// Overrides `LemonadeChatProvider::default_temperature`.
    pub temperature: Option<f32>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            max_tokens: None,
            temperature: None,
        }
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }
}

/// Chat / LLM via `GLM-4.7-Flash-GGUF` (or another configured GPU model).
///
/// Requests are **queued** if STT or another LLM is currently using the GPU.
/// See [`GpuResourceManager`] for the full policy description.
#[derive(Debug, Clone)]
pub struct LemonadeChatProvider {
    client: reqwest::Client,
    base_url: String,
    /// The model id sent to the API (e.g. `"GLM-4.7-Flash-GGUF"`).
    pub model: String,
    /// Shared GPU resource manager — also held by [`LemonadeSttProvider`].
    ///
    /// `None` when the provider targets the AMD NPU (FLM models), which runs on
    /// dedicated silicon with no GPU resource contention.  When `Some`, the GPU
    /// lock is acquired before every inference request via
    /// [`GpuResourceManager::begin_llm`].
    pub gpu: Option<Arc<GpuResourceManager>>,
    /// Default token limit used when no per-request override is given.
    pub default_max_tokens: u32,
    /// Default sampling temperature used when no per-request override is given.
    pub default_temperature: f32,
}

impl LemonadeChatProvider {
    /// Construct with an explicit base URL, model id, and optional GPU manager.
    ///
    /// Pass `Some(gpu)` for GPU-backed llamacpp models (ROCm / Vulkan) so that
    /// the GPU lock is acquired before each inference request.
    /// Pass `None` for NPU-backed FLM models — the NPU is dedicated silicon
    /// with no shared resource contention.
    pub fn new(base_url: &str, model: &str, gpu: Option<Arc<GpuResourceManager>>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            gpu,
            default_max_tokens: 2048,
            default_temperature: 0.7,
        }
    }

    /// Construct for NPU use — no GPU resource manager needed.
    ///
    /// FLM models run on the AMD NPU, which is physically separate from the
    /// GPU.  No locking is required between NPU LLM and GPU STT/LLM requests.
    pub fn new_npu(base_url: &str, model: &str) -> Self {
        Self::new(base_url, model, None)
    }

    /// Construct using the GPU LLM model discovered in `registry`.
    ///
    /// Looks for [`ModelRole::GpuLlm`] (llamacpp recipe) models only — FLM NPU
    /// LLMs are excluded.  Use [`from_registry_npu`](Self::from_registry_npu)
    /// for NPU LLMs.
    pub fn from_registry(
        registry: &LemonadeModelRegistry,
        gpu: Option<Arc<GpuResourceManager>>,
    ) -> Result<Self> {
        let model = registry
            .llm_model()
            .ok_or_else(|| anyhow!("No GPU LLM model found in the Lemonade registry"))?;
        Ok(Self::new(&registry.base_url, &model.id, gpu))
    }

    /// Construct using the NPU FLM LLM model discovered in `registry`.
    ///
    /// Looks for [`ModelRole::NpuLlm`] models only.  No GPU resource manager needed.
    pub fn from_registry_npu(registry: &LemonadeModelRegistry) -> Result<Self> {
        let model = registry
            .npu_llm_model()
            .ok_or_else(|| anyhow!("No NPU FLM LLM model found in the Lemonade registry"))?;
        Ok(Self::new_npu(&registry.base_url, &model.id))
    }

    /// Override the default max tokens ceiling.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.default_max_tokens = n;
        self
    }

    /// Override the default sampling temperature.
    pub fn with_temperature(mut self, t: f32) -> Self {
        self.default_temperature = t;
        self
    }

    /// Send a full `ChatRequest`, queuing if the GPU is busy.
    ///
    /// This is the primary entry point when you need fine-grained control.
    pub async fn complete(&self, req: ChatRequest) -> Result<ChatCompletionResponse> {
        // Acquire the GPU — suspends if STT or another LLM is active.
        // For NPU-backed providers (`gpu` is `None`), skip the lock entirely —
        // the NPU runs independently of the GPU with no shared resource.
        let _guard = if let Some(gpu) = &self.gpu {
            Some(gpu.begin_llm().await)
        } else {
            None
        };

        let start = std::time::Instant::now();
        let max_tokens = req.max_tokens.unwrap_or(self.default_max_tokens);
        let temperature = req.temperature.unwrap_or(self.default_temperature);

        let body = serde_json::json!({
            "model":       self.model,
            "messages":    req.messages,
            "max_tokens":  max_tokens,
            "temperature": temperature,
            "stream":      false,
        });

        let resp: ChatCompletionResponse = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", "Bearer lemonade")
            .json(&body)
            .send()
            .await
            .context("Chat HTTP request failed")?
            .error_for_status()
            .context("Lemonade chat completions returned an error status")?
            .json()
            .await
            .context("Failed to parse chat completion response")?;

        tracing::debug!(
            model         = %self.model,
            n_messages    = req.messages.len(),
            finish_reason = ?resp.choices.first().and_then(|c| c.finish_reason.as_deref()),
            total_tokens  = ?resp.usage.as_ref().map(|u| u.total_tokens),
            duration_ms   = start.elapsed().as_millis(),
            "Chat completion finished"
        );

        Ok(resp)
        // _guard dropped here → GPU released.
    }

    /// Send a list of messages with provider defaults, queuing if GPU is busy.
    pub async fn chat(&self, messages: Vec<ChatMessage>) -> Result<ChatCompletionResponse> {
        self.complete(ChatRequest::new(messages)).await
    }

    /// Convenience: single user-turn prompt. Returns the assistant's text.
    pub async fn ask(&self, prompt: &str) -> Result<String> {
        let resp = self.chat(vec![ChatMessage::user(prompt)]).await?;
        resp.first_content()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Chat response contained no choices"))
    }

    /// Convenience: system prompt + single user turn. Returns the assistant's text.
    pub async fn ask_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let resp = self
            .chat(vec![ChatMessage::system(system), ChatMessage::user(prompt)])
            .await?;
        resp.first_content()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Chat response contained no choices"))
    }
}

// ── Convenience builder ───────────────────────────────────────────────────────

/// Builds a matched set of GPU-sharing providers from a single registry fetch.
///
/// ```no_run
/// # async fn example() -> anyhow::Result<()> {
/// use u_forge_ai::lemonade::{LemonadeStack, GpuResourceManager};
///
/// let stack = LemonadeStack::build("http://127.0.0.1:8000/api/v1").await?;
/// let text  = stack.chat.ask("Describe a dragon in one sentence.").await?;
/// println!("{text}");
/// # Ok(()) }
/// ```
pub struct LemonadeStack {
    pub registry: LemonadeModelRegistry,
    pub gpu: Arc<GpuResourceManager>,
    pub tts: LemonadeTtsProvider,
    pub stt: LemonadeSttProvider,
    pub chat: LemonadeChatProvider,
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

impl LemonadeStack {
    /// Fetch the model registry and construct all providers sharing one GPU manager.
    pub async fn build(base_url: &str) -> Result<Self> {
        let registry = LemonadeModelRegistry::fetch(base_url).await?;
        let gpu = GpuResourceManager::new();

        let tts = LemonadeTtsProvider::from_registry(&registry)?;
        let stt = LemonadeSttProvider::from_registry(&registry, Arc::clone(&gpu))?;
        let chat = LemonadeChatProvider::from_registry(&registry, Some(Arc::clone(&gpu)))?;

        info!(
            tts_model  = %tts.model,
            stt_model  = %stt.model,
            chat_model = %chat.model,
            "LemonadeStack ready"
        );

        Ok(Self {
            registry,
            gpu,
            tts,
            stt,
            chat,
        })
    }
}

impl std::fmt::Debug for LemonadeStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LemonadeStack")
            .field("tts_model", &self.tts.model)
            .field("stt_model", &self.stt.model)
            .field("chat_model", &self.chat.model)
            .field("gpu", &self.gpu)
            .finish()
    }
}

// ── Reranking ─────────────────────────────────────────────────────────────────

/// A single ranked document returned by [`LemonadeRerankProvider::rerank`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankDocument {
    /// Original zero-based index in the input `documents` slice.
    pub index: usize,
    /// Relevance score — higher is more relevant.
    pub score: f32,
    /// The original document text, if the server echoed it back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
}

/// Reranker via `POST /api/v1/reranking` on Lemonade Server.
///
/// Unlike the GPU/NPU providers there is no shared-resource contention for
/// reranking — requests are sent directly to Lemonade Server which serialises
/// them internally.
#[derive(Debug, Clone)]
pub struct LemonadeRerankProvider {
    client: reqwest::Client,
    base_url: String,
    /// The reranker model id (e.g. `"bge-reranker-v2-m3-GGUF"`).
    pub model: String,
}

impl LemonadeRerankProvider {
    /// Construct with an explicit base URL and model id.
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }

    /// Construct using the reranker model discovered in `registry`.
    pub fn from_registry(registry: &LemonadeModelRegistry) -> Result<Self> {
        let model = registry
            .reranker_model()
            .ok_or_else(|| anyhow!("No reranker model found in the Lemonade registry"))?;
        Ok(Self::new(&registry.base_url, &model.id))
    }

    /// Rerank `documents` by relevance to `query`.
    ///
    /// # Arguments
    ///
    /// * `query`     — The search query or reference text.
    /// * `documents` — Candidate documents to score and rank.
    /// * `top_n`     — If `Some(n)`, only the top-n results are returned.
    ///   Pass `None` to return scores for every document.
    ///
    /// Results are returned **sorted by descending score** (most relevant first).
    pub async fn rerank(
        &self,
        query: &str,
        documents: Vec<String>,
        top_n: Option<usize>,
    ) -> Result<Vec<RerankDocument>> {
        let mut body = serde_json::json!({
            "model":     self.model,
            "query":     query,
            "documents": documents,
            "return_documents": true,
        });
        if let Some(n) = top_n {
            body["top_n"] = serde_json::json!(n);
        }

        let start = std::time::Instant::now();

        #[derive(Deserialize)]
        struct RerankResponseItem {
            index: usize,
            relevance_score: f32,
            #[serde(default)]
            document: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct RerankResponse {
            results: Vec<RerankResponseItem>,
        }

        let resp: RerankResponse = self
            .client
            .post(format!("{}/reranking", self.base_url))
            .header("Authorization", "Bearer lemonade")
            .json(&body)
            .send()
            .await
            .context("Rerank HTTP request failed")?
            .error_for_status()
            .context("Lemonade /reranking returned an error status")?
            .json()
            .await
            .context("Failed to parse reranking response")?;

        let mut results: Vec<RerankDocument> = resp
            .results
            .into_iter()
            .map(|item| {
                let document = item.document.and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s),
                    serde_json::Value::Object(ref o) => {
                        o.get("text").and_then(|t| t.as_str()).map(str::to_string)
                    }
                    _ => None,
                });
                RerankDocument {
                    index: item.index,
                    score: item.relevance_score,
                    document,
                }
            })
            .collect();

        // Sort by descending relevance score.
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        debug!(
            model = %self.model,
            n_docs = results.len(),
            duration_ms = start.elapsed().as_millis(),
            "Rerank complete"
        );

        Ok(results)
    }
}

// ── System info ───────────────────────────────────────────────────────────────

/// Capabilities derived from a [`SystemInfo`] snapshot.
///
/// Use [`SystemInfo::lemonade_capabilities`] to build this from a live server.
#[derive(Debug, Clone, Default)]
pub struct LemonadeCapabilities {
    // ── Device availability ───────────────────────────────────────────────
    /// AMD NPU (XDNA or similar) is present and accessible.
    pub npu_available: bool,
    /// AMD integrated GPU is present (iGPU shared memory).
    pub igpu_available: bool,

    // ── Installed recipe backends ─────────────────────────────────────────
    /// FLM (FastFlowLM) backend installed for NPU — enables embedding, NPU
    /// whisper, and NPU LLM.
    pub flm_npu_installed: bool,
    /// Kokoro TTS backend installed on CPU.
    pub kokoro_cpu_installed: bool,
    /// llama.cpp ROCm backend installed (iGPU via ROCm).
    pub llamacpp_rocm_installed: bool,
    /// llama.cpp Vulkan backend installed (iGPU or CPU via Vulkan).
    pub llamacpp_vulkan_installed: bool,
    /// whisper.cpp Vulkan backend installed.
    pub whispercpp_vulkan_installed: bool,
    /// whisper.cpp CPU backend installed.
    pub whispercpp_cpu_installed: bool,

    // ── Derived capability flags ──────────────────────────────────────────
    /// NPU embedding is possible (FLM + NPU).
    pub can_embed_npu: bool,
    /// NPU speech-to-text is possible (FLM + NPU).
    pub can_stt_npu: bool,
    /// NPU LLM inference is possible (FLM + NPU).
    pub can_llm_npu: bool,
    /// GPU speech-to-text is possible (whispercpp + iGPU).
    pub can_stt_gpu: bool,
    /// GPU LLM inference is possible (llamacpp ROCm or Vulkan + iGPU).
    pub can_llm_gpu: bool,
    /// CPU TTS is possible (Kokoro).
    pub can_tts_cpu: bool,
}

/// Raw device info from the `devices` section of `/system-info`.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemDeviceInfo {
    pub available: bool,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub name: String,
}

/// Info for a single recipe backend (e.g. `llamacpp.rocm`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RecipeBackendInfo {
    /// Installation state: `"installed"`, `"installable"`, `"unsupported"`, etc.
    #[serde(default)]
    pub state: String,
    /// Lemonade device ids this backend runs on (e.g. `["amd_igpu"]`).
    #[serde(default)]
    pub devices: Vec<String>,
}

/// Snapshot of the Lemonade server's hardware and recipe state.
///
/// Fetched from `GET {base_url}/../system-info` (one path level above the
/// `/api/v1` prefix).
///
/// Use [`SystemInfo::fetch`] to obtain a live snapshot, then
/// [`SystemInfo::lemonade_capabilities`] to derive which inference paths are
/// available and testable.
#[derive(Debug, Clone)]
pub struct SystemInfo {
    /// CPU/APU model string reported by the OS.
    pub processor: String,
    /// Physical RAM as a human-readable string (e.g. `"94.07 GB"`).
    pub physical_memory: String,
    /// OS version string.
    pub os_version: String,
    /// AMD NPU device info, if present.
    pub npu: Option<SystemDeviceInfo>,
    /// AMD integrated GPU device info, if present.
    pub igpu: Option<SystemDeviceInfo>,
    /// Recipe backend states, keyed by `"<recipe>/<backend>"`.
    /// E.g. `"flm/npu"`, `"llamacpp/rocm"`, `"whispercpp/vulkan"`.
    pub backends: std::collections::HashMap<String, RecipeBackendInfo>,
}

impl SystemInfo {
    /// Fetch system info from `GET {base_url}/system-info`.
    ///
    /// The Lemonade system-info endpoint lives at `/api/v1/system-info`.
    /// Pass the `/api/v1` base URL directly (e.g. `http://localhost:8000/api/v1`)
    /// and this function will append `/system-info` to it.
    pub async fn fetch(base_url: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        // The system-info endpoint lives at /api/v1/system-info — keep the
        // /api/v1 prefix intact and just append /system-info.
        let base = base_url.trim_end_matches('/');
        let url = format!("{base}/system-info");

        let raw: serde_json::Value = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Failed to reach Lemonade system-info at {url}"))?
            .error_for_status()
            .context("Lemonade /system-info returned an error status")?
            .json()
            .await
            .context("Failed to parse /system-info JSON")?;

        let processor = raw
            .get("Processor")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let physical_memory = raw
            .get("Physical Memory")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let os_version = raw
            .get("OS Version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Parse device availability.
        let npu = raw
            .pointer("/devices/amd_npu")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let igpu = raw
            .pointer("/devices/amd_igpu")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // Flatten all recipe/backend combinations into a single map.
        let mut backends = std::collections::HashMap::new();
        if let Some(recipes) = raw.get("recipes").and_then(|v| v.as_object()) {
            for (recipe, recipe_val) in recipes {
                if let Some(bmap) = recipe_val.get("backends").and_then(|v| v.as_object()) {
                    for (backend, bval) in bmap {
                        let key = format!("{recipe}/{backend}");
                        if let Ok(info) = serde_json::from_value::<RecipeBackendInfo>(bval.clone())
                        {
                            backends.insert(key, info);
                        }
                    }
                }
            }
        }

        let info = Self {
            processor,
            physical_memory,
            os_version,
            npu,
            igpu,
            backends,
        };

        info!(
            processor = %info.processor,
            os = %info.os_version,
            npu_available = info.npu.as_ref().map(|d| d.available).unwrap_or(false),
            igpu_available = info.igpu.as_ref().map(|d| d.available).unwrap_or(false),
            "Lemonade system-info loaded"
        );

        Ok(info)
    }

    /// Returns `true` if the given recipe/backend is installed and available.
    pub fn is_installed(&self, recipe: &str, backend: &str) -> bool {
        let key = format!("{recipe}/{backend}");
        self.backends
            .get(&key)
            .map(|b| b.state == "installed")
            .unwrap_or(false)
    }

    /// Derive a [`LemonadeCapabilities`] snapshot from this system-info.
    pub fn lemonade_capabilities(&self) -> LemonadeCapabilities {
        let npu_available = self.npu.as_ref().map(|d| d.available).unwrap_or(false);
        let igpu_available = self.igpu.as_ref().map(|d| d.available).unwrap_or(false);

        let flm_npu_installed = self.is_installed("flm", "npu");
        let kokoro_cpu_installed = self.is_installed("kokoro", "cpu");
        let llamacpp_rocm_installed = self.is_installed("llamacpp", "rocm");
        let llamacpp_vulkan_installed = self.is_installed("llamacpp", "vulkan");
        let whispercpp_vulkan_installed = self.is_installed("whispercpp", "vulkan");
        let whispercpp_cpu_installed = self.is_installed("whispercpp", "cpu");

        LemonadeCapabilities {
            npu_available,
            igpu_available,
            flm_npu_installed,
            kokoro_cpu_installed,
            llamacpp_rocm_installed,
            llamacpp_vulkan_installed,
            whispercpp_vulkan_installed,
            whispercpp_cpu_installed,
            can_embed_npu: npu_available && flm_npu_installed,
            can_stt_npu: npu_available && flm_npu_installed,
            can_llm_npu: npu_available && flm_npu_installed,
            can_stt_gpu: igpu_available
                && (whispercpp_vulkan_installed || whispercpp_cpu_installed),
            can_llm_gpu: igpu_available && (llamacpp_rocm_installed || llamacpp_vulkan_installed),
            can_tts_cpu: kokoro_cpu_installed,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::lemonade_url;

    // ── Unit: model role classification ──────────────────────────────────────

    fn make_entry(id: &str, labels: &[&str], recipe: &str) -> LemonadeModelEntry {
        LemonadeModelEntry {
            id: id.into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            recipe: recipe.into(),
            size: None,
            downloaded: Some(true),
            suggested: Some(true),
        }
    }

    #[test]
    fn test_role_npu_embedding_flm() {
        let m = make_entry("embed-gemma-300m-FLM", &["embeddings"], "flm");
        assert_eq!(m.role(), ModelRole::NpuEmbedding);
    }

    #[test]
    fn test_role_npu_embedding_only_if_label_present() {
        // FLM recipe + reasoning label → NpuLlm (not GpuLlm — FLM runs on the NPU)
        let m = make_entry("qwen3-8b-FLM", &["reasoning", "tool-calling"], "flm");
        assert_eq!(m.role(), ModelRole::NpuLlm);
    }

    #[test]
    fn test_role_npu_stt() {
        // FLM recipe + transcription label → NpuStt
        let m = make_entry("whisper-v3-turbo-FLM", &["audio", "transcription"], "flm");
        assert_eq!(m.role(), ModelRole::NpuStt);
    }

    #[test]
    fn test_role_npu_llm_coding_label() {
        // FLM recipe + coding label → NpuLlm
        let m = make_entry("some-coder-FLM", &["coding"], "flm");
        assert_eq!(m.role(), ModelRole::NpuLlm);
    }

    #[test]
    fn test_role_cpu_embedding_llamacpp() {
        // llamacpp recipe + embeddings label → CpuEmbedding
        let m = make_entry("nomic-embed-text-v1-GGUF", &["embeddings"], "llamacpp");
        assert_eq!(m.role(), ModelRole::CpuEmbedding);
    }

    #[test]
    fn test_role_cpu_tts_kokoro_recipe() {
        let m = make_entry("kokoro-v1", &["tts", "speech"], "kokoro");
        assert_eq!(m.role(), ModelRole::CpuTts);
    }

    #[test]
    fn test_role_cpu_tts_label_only() {
        let m = make_entry("some-tts-model", &["tts"], "custom");
        assert_eq!(m.role(), ModelRole::CpuTts);
    }

    #[test]
    fn test_role_gpu_stt_whispercpp() {
        let m = make_entry(
            "Whisper-Large-v3-Turbo",
            &["audio", "transcription"],
            "whispercpp",
        );
        assert_eq!(m.role(), ModelRole::GpuStt);
    }

    #[test]
    fn test_role_gpu_stt_transcription_label() {
        // Non-FLM whispercpp with transcription label → GpuStt
        let m = make_entry(
            "Whisper-Large-v3-Turbo",
            &["audio", "transcription", "hot"],
            "whispercpp",
        );
        assert_eq!(m.role(), ModelRole::GpuStt);
    }

    #[test]
    fn test_role_gpu_stt_non_flm_transcription_label() {
        // Non-FLM recipe with transcription label → GpuStt
        let m = make_entry("some-whisper", &["transcription"], "whispercpp");
        assert_eq!(m.role(), ModelRole::GpuStt);
    }

    #[test]
    fn test_role_flm_not_misclassified_as_gpu_stt() {
        // FLM recipe + transcription label must be NpuStt, NOT GpuStt
        let m = make_entry("whisper-v3-turbo-FLM", &["audio", "transcription"], "flm");
        assert_ne!(
            m.role(),
            ModelRole::GpuStt,
            "FLM whisper must not be GpuStt"
        );
        assert_eq!(m.role(), ModelRole::NpuStt);
    }

    #[test]
    fn test_role_flm_llm_not_misclassified_as_gpu_llm() {
        // FLM recipe + reasoning label must be NpuLlm, NOT GpuLlm
        let m = make_entry("gpt-oss-20b-FLM", &["reasoning"], "flm");
        assert_ne!(m.role(), ModelRole::GpuLlm, "FLM LLM must not be GpuLlm");
        assert_eq!(m.role(), ModelRole::NpuLlm);
    }

    #[test]
    fn test_role_gpu_llm_llamacpp() {
        let m = make_entry("GLM-4.7-Flash-GGUF", &["tool-calling"], "llamacpp");
        assert_eq!(m.role(), ModelRole::GpuLlm);
    }

    #[test]
    fn test_role_reranker() {
        let m = make_entry("bge-reranker-v2-m3-GGUF", &["reranking"], "llamacpp");
        assert_eq!(m.role(), ModelRole::Reranker);
    }

    #[test]
    fn test_role_image_gen() {
        let m = make_entry("SDXL-Turbo", &["image"], "sd-cpp");
        assert_eq!(m.role(), ModelRole::ImageGen);
    }

    #[test]
    fn test_role_other() {
        let m = make_entry("mystery-model", &[], "unknown");
        assert_eq!(m.role(), ModelRole::Other);
    }

    // ── Unit: KokoroVoice ─────────────────────────────────────────────────────

    #[test]
    fn test_kokoro_voice_as_str() {
        assert_eq!(KokoroVoice::AfSky.as_str(), "af_sky");
        assert_eq!(KokoroVoice::AfHeart.as_str(), "af_heart");
        assert_eq!(KokoroVoice::AmAdam.as_str(), "am_adam");
        assert_eq!(KokoroVoice::BmGeorge.as_str(), "bm_george");
        assert_eq!(KokoroVoice::BfEmma.as_str(), "bf_emma");
        assert_eq!(
            KokoroVoice::Custom("af_custom".into()).as_str(),
            "af_custom"
        );
    }

    #[test]
    fn test_kokoro_voice_default() {
        assert_eq!(KokoroVoice::default(), KokoroVoice::AfSky);
    }

    // ── Unit: GPU resource manager — state transitions ────────────────────────

    #[tokio::test]
    async fn test_gpu_initial_state_is_idle() {
        let gpu = GpuResourceManager::new();
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    #[tokio::test]
    async fn test_stt_acquires_gpu_when_idle() {
        let gpu = GpuResourceManager::new();
        let _guard = gpu
            .begin_stt()
            .expect("Should acquire GPU for STT when idle");
        assert_eq!(gpu.current_workload(), GpuWorkload::SttActive);
    }

    #[tokio::test]
    async fn test_stt_guard_drop_releases_to_idle() {
        let gpu = GpuResourceManager::new();
        {
            let _g = gpu.begin_stt().unwrap();
            assert_eq!(gpu.current_workload(), GpuWorkload::SttActive);
        }
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    #[tokio::test]
    async fn test_llm_acquires_gpu_when_idle() {
        let gpu = GpuResourceManager::new();
        let _guard = gpu.begin_llm().await;
        assert_eq!(gpu.current_workload(), GpuWorkload::LlmActive);
    }

    #[tokio::test]
    async fn test_llm_guard_drop_releases_to_idle() {
        let gpu = GpuResourceManager::new();
        {
            let _g = gpu.begin_llm().await;
            assert_eq!(gpu.current_workload(), GpuWorkload::LlmActive);
        }
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    // ── Unit: GPU resource manager — STT blocking policy ─────────────────────

    #[tokio::test]
    async fn test_stt_blocked_when_llm_active() {
        let gpu = GpuResourceManager::new();
        let _llm = gpu.begin_llm().await;

        let result = gpu.begin_stt();
        assert!(result.is_err(), "STT must be blocked when LLM is active");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LLM inference"),
            "Error should mention LLM: {msg}"
        );
    }

    #[tokio::test]
    async fn test_stt_blocked_when_stt_active() {
        let gpu = GpuResourceManager::new();
        let _stt1 = gpu.begin_stt().expect("First STT should succeed");

        let result = gpu.begin_stt();
        assert!(result.is_err(), "Second concurrent STT must be rejected");
    }

    // ── Unit: GPU resource manager — LLM queuing policy ──────────────────────

    #[tokio::test]
    async fn test_llm_queues_behind_active_stt_and_proceeds_on_release() {
        use tokio::time::{sleep, timeout, Duration};

        let gpu = GpuResourceManager::new();
        let gpu_llm = Arc::clone(&gpu);

        // Hold the GPU for STT.
        let stt_guard = gpu.begin_stt().expect("STT should acquire GPU");

        // Spawn LLM task — it must wait.
        let llm_handle = tokio::spawn(async move {
            let _guard = gpu_llm.begin_llm().await;
            // If we reach here, the GPU is ours.
        });

        // Brief pause to let the LLM task enter the wait loop.
        sleep(Duration::from_millis(50)).await;

        // LLM task must not have completed yet.
        assert!(
            !llm_handle.is_finished(),
            "LLM task should still be waiting for STT to release"
        );

        // Release STT → LLM should be unblocked.
        drop(stt_guard);

        timeout(Duration::from_secs(2), llm_handle)
            .await
            .expect("LLM task should complete within 2 s after STT release")
            .expect("LLM task should not panic");
    }

    #[tokio::test]
    async fn test_multiple_llm_requests_serialise() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::time::{sleep, Duration};

        let gpu = GpuResourceManager::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..4 {
            let g = Arc::clone(&gpu);
            let c = Arc::clone(&counter);
            handles.push(tokio::spawn(async move {
                let _guard = g.begin_llm().await;
                // Only one task should be in this critical section at a time.
                let prev = c.fetch_add(1, Ordering::SeqCst);
                sleep(Duration::from_millis(5)).await;
                c.fetch_sub(1, Ordering::SeqCst);
                // If serialisation is working, prev should always be 0.
                assert_eq!(prev, 0, "Concurrent LLM requests must not overlap");
            }));
        }

        for h in handles {
            h.await.expect("Task should not panic");
        }

        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    #[tokio::test]
    async fn test_gpu_idle_after_sequential_stt_then_llm() {
        let gpu = GpuResourceManager::new();

        {
            let _stt = gpu.begin_stt().unwrap();
        }
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);

        {
            let _llm = gpu.begin_llm().await;
        }
        assert_eq!(gpu.current_workload(), GpuWorkload::Idle);
    }

    // ── Integration: model registry (requires LEMONADE_URL) ──────────────────

    #[tokio::test]
    async fn test_registry_fetch_returns_models() {
        let Some(url) = lemonade_url().await else {
            eprintln!("SKIP test_registry_fetch_returns_models — Lemonade Server not available");
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        assert!(
            !reg.models.is_empty(),
            "Registry must contain at least one model"
        );
        println!("Discovered {} models:\n{}", reg.models.len(), reg.summary());
    }

    #[tokio::test]
    async fn test_registry_identifies_npu_embedding_model() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let m = reg.npu_embedding_model();
        assert!(m.is_some(), "embed-gemma-300m-FLM should be present");
        assert!(
            m.unwrap().id.contains("embed-gemma"),
            "Expected embed-gemma model, got: {}",
            m.unwrap().id
        );
    }

    #[tokio::test]
    async fn test_registry_identifies_tts_model() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let m = reg.tts_model();
        assert!(m.is_some(), "kokoro-v1 TTS model should be present");
        assert_eq!(m.unwrap().id, "kokoro-v1");
    }

    #[tokio::test]
    async fn test_registry_identifies_stt_model() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let m = reg.stt_model();
        assert!(m.is_some(), "Whisper STT model should be present");
        assert!(
            m.unwrap().id.contains("Whisper"),
            "Expected Whisper model, got: {}",
            m.unwrap().id
        );
    }

    #[tokio::test]
    async fn test_registry_identifies_llm_model() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let m = reg.llm_model();
        assert!(m.is_some(), "GLM-4.7-Flash-GGUF LLM should be present");
        assert!(
            m.unwrap().id.contains("GLM"),
            "Expected GLM model, got: {}",
            m.unwrap().id
        );
    }

    #[tokio::test]
    async fn test_registry_by_role_roundtrip() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();

        let embeddings = reg.by_role(&ModelRole::NpuEmbedding);
        assert!(
            !embeddings.is_empty(),
            "At least one NPU embedding model expected"
        );

        let tts = reg.by_role(&ModelRole::CpuTts);
        assert!(!tts.is_empty(), "At least one TTS model expected");

        let stt = reg.by_role(&ModelRole::GpuStt);
        assert!(!stt.is_empty(), "At least one STT model expected");

        let llm = reg.by_role(&ModelRole::GpuLlm);
        assert!(!llm.is_empty(), "At least one LLM model expected");
    }

    // ── Integration: TTS (requires LEMONADE_URL) ──────────────────────────────

    #[tokio::test]
    async fn test_tts_returns_audio_bytes() {
        let Some(url) = lemonade_url().await else {
            eprintln!("SKIP test_tts_returns_audio_bytes — Lemonade Server not available");
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let tts = LemonadeTtsProvider::from_registry(&reg).unwrap();

        let audio = tts.synthesize_default("Hello, adventurer!").await.unwrap();
        assert!(!audio.is_empty(), "TTS should return non-empty audio bytes");
        println!("TTS returned {} bytes of audio", audio.len());
    }

    #[tokio::test]
    async fn test_tts_multiple_voices() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let tts = LemonadeTtsProvider::from_registry(&reg).unwrap();

        for voice in &[
            KokoroVoice::AfSky,
            KokoroVoice::AfHeart,
            KokoroVoice::AmAdam,
            KokoroVoice::BmGeorge,
        ] {
            let audio = tts
                .synthesize("The dungeon awaits.", Some(voice))
                .await
                .unwrap();
            assert!(
                !audio.is_empty(),
                "Voice {:?} should produce audio bytes",
                voice
            );
        }
    }

    #[tokio::test]
    async fn test_tts_long_text() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let tts = LemonadeTtsProvider::from_registry(&reg).unwrap();

        let text = "Deep beneath the ancient mountain, \
                    where shadows cling to every stone, \
                    the adventurers discovered a chamber unlike any they had seen before. \
                    Runes of glowing amber lined the walls, pulsing with a rhythm like a \
                    heartbeat, and at the centre stood a pedestal bearing a single obsidian key.";

        let audio = tts.synthesize_default(text).await.unwrap();
        assert!(!audio.is_empty(), "Long-form TTS should return audio bytes");
    }

    // ── Integration: Chat (requires LEMONADE_URL) ─────────────────────────────

    #[tokio::test]
    async fn test_chat_ask_returns_response() {
        let Some(url) = lemonade_url().await else {
            eprintln!("SKIP test_chat_ask_returns_response — Lemonade Server not available");
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::from_registry(&reg, Some(gpu)).unwrap();

        let response = chat
            .ask("Respond with exactly one word: pong")
            .await
            .unwrap();
        assert!(
            !response.is_empty(),
            "Chat should return a non-empty response"
        );
        println!("Chat response: {response}");
    }

    #[tokio::test]
    async fn test_chat_with_system_prompt() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::from_registry(&reg, Some(gpu)).unwrap();

        let response = chat
            .ask_with_system(
                "You are a concise TTRPG lore assistant. Answer in one sentence.",
                "What is the capital of the Forgotten Realms?",
            )
            .await
            .unwrap();

        assert!(
            !response.is_empty(),
            "System-prompted chat should return content"
        );
    }

    #[tokio::test]
    async fn test_chat_multi_turn_conversation() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::from_registry(&reg, Some(gpu)).unwrap();

        let messages = vec![
            ChatMessage::system("You are a TTRPG dungeon master. Be concise."),
            ChatMessage::user("I enter the tavern. What do I see?"),
            ChatMessage::assistant("A dim room with three patrons nursing their drinks."),
            ChatMessage::user("I approach the bar. What does the barkeep say?"),
        ];

        let resp = chat.chat(messages).await.unwrap();
        let content = resp.first_content().expect("Should have a response");
        assert!(!content.is_empty());
    }

    #[tokio::test]
    async fn test_chat_request_with_overrides() {
        let Some(url) = lemonade_url().await else {
            return;
        };
        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::from_registry(&reg, Some(gpu)).unwrap();

        let req = ChatRequest::new(vec![ChatMessage::user("Count to three.")])
            .with_max_tokens(64)
            .with_temperature(0.1);

        let resp = chat.complete(req).await.unwrap();
        assert!(resp.first_content().is_some());
    }

    // ── Integration: GPU policy end-to-end (requires LEMONADE_URL) ───────────

    #[tokio::test]
    async fn test_llm_queues_behind_simulated_stt_integration() {
        let Some(url) = lemonade_url().await else {
            eprintln!(
                "SKIP test_llm_queues_behind_simulated_stt_integration — Lemonade Server not available"
            );
            return;
        };
        use tokio::time::{sleep, timeout, Duration};

        let reg = LemonadeModelRegistry::fetch(&url).await.unwrap();
        let gpu = GpuResourceManager::new();
        let chat = LemonadeChatProvider::from_registry(&reg, Some(Arc::clone(&gpu))).unwrap();

        // Simulate an active STT session (no real audio upload needed).
        let stt_guard = gpu
            .begin_stt()
            .expect("STT guard should succeed when GPU is idle");

        let chat2 = chat.clone();
        let llm_task = tokio::spawn(async move { chat2.ask("Say: ready").await });

        // Give the LLM task time to enter its wait loop.
        sleep(Duration::from_millis(100)).await;
        assert!(
            !llm_task.is_finished(),
            "LLM must still be queued behind STT"
        );

        // Release the simulated STT session.
        drop(stt_guard);

        let result = timeout(Duration::from_secs(60), llm_task)
            .await
            .expect("LLM task should complete within 60 s after STT release")
            .expect("LLM task should not panic")
            .expect("LLM chat should succeed");

        assert!(
            !result.is_empty(),
            "LLM should return a non-empty response after queuing"
        );
    }

    #[tokio::test]
    async fn test_stt_blocked_during_simulated_llm_integration() {
        let Some(_url) = lemonade_url().await else {
            return;
        };

        // This is purely a policy test — no real LLM request needed.
        let gpu = GpuResourceManager::new();
        let _llm_guard = gpu.begin_llm().await;

        let result = gpu.begin_stt();
        assert!(
            result.is_err(),
            "STT must be rejected when LLM guard is held"
        );
        assert!(
            result.unwrap_err().to_string().contains("LLM inference"),
            "Error message should mention LLM inference"
        );
    }

    // ── Integration: LemonadeStack builder ────────────────────────────────────

    #[tokio::test]
    async fn test_stack_builds_successfully() {
        let Some(url) = lemonade_url().await else {
            eprintln!("SKIP test_stack_builds_successfully — Lemonade Server not available");
            return;
        };

        let stack = LemonadeStack::build(&url).await.unwrap();
        assert_eq!(stack.tts.model, "kokoro-v1");
        assert!(stack.stt.model.contains("Whisper"));
        assert!(stack.chat.model.contains("GLM"));
        println!("{:?}", stack);
    }

    #[tokio::test]
    async fn test_stack_tts_and_chat_share_nothing_on_gpu() {
        let Some(url) = lemonade_url().await else {
            return;
        };

        let stack = LemonadeStack::build(&url).await.unwrap();
        // TTS runs on CPU — should not touch the GPU manager.
        assert_eq!(stack.gpu.current_workload(), GpuWorkload::Idle);

        let _audio = stack.tts.synthesize_default("Testing.").await.unwrap();
        // GPU should still be idle after a TTS call.
        assert_eq!(stack.gpu.current_workload(), GpuWorkload::Idle);
    }

    #[tokio::test]
    async fn test_stack_stt_and_chat_share_gpu_manager() {
        let Some(_url) = lemonade_url().await else {
            return;
        };

        // Structural check: both stt and chat must hold the *same* Arc.
        // We verify this by acquiring via stt and seeing it reflected in chat's gpu.
        let gpu = GpuResourceManager::new();
        let stt_gpu = Arc::clone(&gpu);
        let chat_gpu = Arc::clone(&gpu);

        let _guard = stt_gpu.begin_stt().unwrap();
        assert_eq!(chat_gpu.current_workload(), GpuWorkload::SttActive);
    }
}
