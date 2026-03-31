//! Lemonade model registry and role classification.
//!
//! Fetches available models from a Lemonade Server and classifies them by
//! functional role (NPU embedding, GPU LLM, CPU TTS, etc.).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::client::LemonadeHttpClient;

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
    ///
    /// Renamed from `LlamacppEmbedding` — llamacpp models run on both GPU and CPU
    /// depending on Lemonade Server configuration; the old name was misleading.
    LlamacppEmbedding,
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
            return ModelRole::LlamacppEmbedding;
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
        let client = LemonadeHttpClient::new(base_url);

        let resp: ModelsListResponse = client
            .get_json("/models")
            .await
            .context("Failed to fetch Lemonade model registry")?;

        info!(
            model_count = resp.data.len(),
            base_url, "Lemonade model registry loaded"
        );

        Ok(Self {
            base_url: client.base_url,
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

    /// The preferred llamacpp embedding model (GPU or CPU), if available.
    ///
    /// Prefers `user.ggml-org/embeddinggemma-300M-GGUF` — the GGUF variant of
    /// embedding-gemma that produces 768-dim vectors compatible with the NPU
    /// `embed-gemma-300m-FLM` model.  This ensures all embedding workers share
    /// the same vector space regardless of which device produces the embedding.
    ///
    /// Falls back to any [`ModelRole::LlamacppEmbedding`] model whose dimensions
    /// match the standard 768-dim index.
    ///
    /// `Qwen3-Embedding-8B-GGUF` is excluded unless
    /// [`ENABLE_HIGH_QUALITY_EMBEDDING`](crate::ENABLE_HIGH_QUALITY_EMBEDDING)
    /// is `true` — it outputs 4096-dim vectors that require a separate index.
    pub fn llamacpp_embedding_model(&self) -> Option<&LemonadeModelEntry> {
        self.models
            .iter()
            .find(|m| m.id == "user.ggml-org/embeddinggemma-300M-GGUF")
            .or_else(|| {
                // Fall back to any LlamacppEmbedding that isn't Qwen3 (wrong dims)
                // and isn't a nomic model (different embedding space than gemma).
                self.by_role(&ModelRole::LlamacppEmbedding)
                    .into_iter()
                    .find(|m| {
                        let dominated = m.id == "Qwen3-Embedding-8B-GGUF"
                            && !crate::graph::ENABLE_HIGH_QUALITY_EMBEDDING;
                        let wrong_space = m.id.starts_with("nomic-");
                        !dominated && !wrong_space
                    })
            })
    }

    /// All llamacpp embedding models (GPU or CPU) suitable for parallel workers.
    ///
    /// Returns every [`ModelRole::LlamacppEmbedding`] model that lives in the same
    /// vector space as the NPU `embed-gemma-300m-FLM` model (768-dim gemma
    /// embeddings).  The preferred model is
    /// `user.ggml-org/embeddinggemma-300M-GGUF`; other gemma-compatible models
    /// are appended in server-reported order.
    ///
    /// Models from a **different embedding family** (e.g. nomic) are excluded
    /// because mixing embedding spaces produces meaningless distance scores.
    ///
    /// `Qwen3-Embedding-8B-GGUF` is also **excluded** unless
    /// [`ENABLE_HIGH_QUALITY_EMBEDDING`](crate::ENABLE_HIGH_QUALITY_EMBEDDING)
    /// is `true` — it outputs 4096-dim vectors incompatible with the 768-dim
    /// index.
    ///
    /// Callers should still probe each returned model's actual output dimensions
    /// via [`LemonadeProvider::new`](crate::LemonadeProvider::new) and discard
    /// any whose dimensions do not match [`crate::EMBEDDING_DIMENSIONS`].
    pub fn all_llamacpp_embedding_models(&self) -> Vec<&LemonadeModelEntry> {
        let high_quality = crate::graph::ENABLE_HIGH_QUALITY_EMBEDDING;

        const PREFERRED: &[&str] = &["user.ggml-org/embeddinggemma-300M-GGUF"];

        let candidates: Vec<&LemonadeModelEntry> = self
            .by_role(&ModelRole::LlamacppEmbedding)
            .into_iter()
            .filter(|m| {
                // Exclude Qwen3 unless high-quality flag is set.
                if m.id == "Qwen3-Embedding-8B-GGUF" && !high_quality {
                    return false;
                }
                // Exclude nomic models — different embedding space than gemma.
                if m.id.starts_with("nomic-") {
                    return false;
                }
                true
            })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::lemonade_url;

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
        // llamacpp recipe + embeddings label → LlamacppEmbedding
        let m = make_entry("nomic-embed-text-v1-GGUF", &["embeddings"], "llamacpp");
        assert_eq!(m.role(), ModelRole::LlamacppEmbedding);
    }

    #[test]
    fn test_role_cpu_embedding_gemma_gguf() {
        // User-added gemma GGUF: llamacpp recipe + custom + embeddings labels → LlamacppEmbedding
        let m = make_entry(
            "user.ggml-org/embeddinggemma-300M-GGUF",
            &["custom", "embeddings"],
            "llamacpp",
        );
        assert_eq!(m.role(), ModelRole::LlamacppEmbedding);
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
}
