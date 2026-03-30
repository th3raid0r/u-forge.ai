//! Static registry of known model context-window limits.
//!
//! Lemonade Server does not expose per-model `max_seq_len` via its API.  This
//! module embeds a hand-maintained JSON registry (`assets/model_context_limits.json`)
//! at compile time and provides [`effective_ctx_size`] — the actual usable context
//! for a given model, capped to
//! [`DEFAULT_EMBEDDING_CONTEXT_TOKENS`](crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS).
//!
//! # Adding a new model
//!
//! Edit `crates/u-forge-core/assets/model_context_limits.json`.  The key is the
//! exact model ID used by Lemonade Server (case-sensitive), the value is the
//! model's published maximum sequence length in tokens.
//!
//! ```json
//! {
//!   "embed-gemma-300m-FLM": 2048,
//!   "embed-gemma-300M-GGUF": 2048,
//!   "user.ggml-org/embeddinggemma-300M-GGUF": 2048
//! }
//! ```

use std::collections::HashMap;
use std::sync::OnceLock;

static LIMITS: OnceLock<HashMap<String, usize>> = OnceLock::new();

fn limits() -> &'static HashMap<String, usize> {
    LIMITS.get_or_init(|| {
        let json = include_str!("../../assets/model_context_limits.json");
        serde_json::from_str(json).expect("assets/model_context_limits.json is malformed")
    })
}

/// The usable context-window size for `model_id`.
///
/// Returns the smaller of
/// [`DEFAULT_EMBEDDING_CONTEXT_TOKENS`](crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS)
/// and the model's known maximum sequence length from the bundled registry.
/// When a model is not listed in the registry,
/// [`DEFAULT_EMBEDDING_CONTEXT_TOKENS`](crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS)
/// is returned unchanged as a conservative default.
///
/// # Example
///
/// ```
/// use u_forge_core::lemonade::effective_ctx_size;
///
/// // embedding-gemma (both NPU and GGUF variants) supports 2048 tokens
/// assert_eq!(effective_ctx_size("embed-gemma-300m-FLM"), 2048);
/// assert_eq!(effective_ctx_size("user.ggml-org/embeddinggemma-300M-GGUF"), 2048);
/// ```
pub fn effective_ctx_size(model_id: &str) -> usize {
    let cap = crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS;
    let model_max = limits().get(model_id).copied().unwrap_or(cap);
    model_max.min(cap)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nomic_v2_moe_capped_at_512() {
        assert_eq!(effective_ctx_size("nomic-embed-text-v2-moe-GGUF"), 512);
    }

    #[test]
    fn test_nomic_v1_capped_at_default() {
        // nomic-v1 supports 8192 but DEFAULT_EMBEDDING_CONTEXT_TOKENS is 4096
        assert_eq!(
            effective_ctx_size("nomic-embed-text-v1-GGUF"),
            crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS
        );
    }

    #[test]
    fn test_embed_gemma_300m_flm() {
        assert_eq!(effective_ctx_size("embed-gemma-300m-FLM"), 2048);
    }

    #[test]
    fn test_embed_gemma_gguf_user_model() {
        assert_eq!(
            effective_ctx_size("user.ggml-org/embeddinggemma-300M-GGUF"),
            2048
        );
    }

    #[test]
    fn test_unknown_model_returns_default() {
        assert_eq!(
            effective_ctx_size("some-unknown-model-GGUF"),
            crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS
        );
    }
}
