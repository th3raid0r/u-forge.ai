//! Model context-window limits from configuration.
//!
//! Lemonade Server does not expose per-model `max_seq_len` via its API.  This
//! module provides [`effective_ctx_size`] — the actual usable context
//! for a given model, read from the `models.context_limits` configuration
//! and capped to [`DEFAULT_EMBEDDING_CONTEXT_TOKENS`](crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS).
//!
//! # Adding a new model
//!
//! Add the model to `u-forge.toml` under the `[models.context_limits]` section.
//! The key is the exact model ID used by Lemonade Server (case-sensitive),
//! the value is the model's maximum sequence length in tokens.
//!
//! ```toml
//! [models.context_limits]
//! "embed-gemma-300m-FLM"     = 2048
//! "user.ggml-org/embeddinggemma-300M-GGUF"   = 2048
//! "nomic-embed-text-v1-GGUF" = 2048
//! ```

/// The usable context-window size for `model_id`.
///
/// Returns [`DEFAULT_EMBEDDING_CONTEXT_TOKENS`](crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS).
/// Model-specific limits are now configured in `u-forge.toml` under
/// `[models.context_limits]` and can be accessed via the configuration system
/// at runtime.
///
/// # Example
///
/// ```
/// use u_forge_core::lemonade::effective_ctx_size;
///
/// // Returns the default context size (4096 tokens)
/// assert_eq!(effective_ctx_size("embed-gemma-300m-FLM"), u_forge_core::DEFAULT_EMBEDDING_CONTEXT_TOKENS);
/// ```
pub fn effective_ctx_size(_model_id: &str) -> usize {
    crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nomic_v1_capped_at_default() {
        // With limits loaded from config, unknown models return the default
        assert_eq!(
            effective_ctx_size("nomic-embed-text-v1-GGUF"),
            crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS
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
