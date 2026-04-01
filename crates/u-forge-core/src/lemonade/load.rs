//! Model loading via the Lemonade Server `POST /api/v1/load` endpoint.
//!
//! Explicitly loading a model before first use lets callers override server
//! defaults — most importantly `ctx_size`, which controls how many tokens the
//! model can process in one request.  Without an explicit load call the server
//! uses its built-in default (often 2 K or 512 tokens for embedding models),
//! which is too small for full-node embedding documents.
//!
//! # Usage
//!
//! ```no_run
//! # use u_forge_core::lemonade::load::{ModelLoadOptions, load_model};
//! # async fn example() -> anyhow::Result<()> {
//! let opts = ModelLoadOptions {
//!     ctx_size: Some(4096),
//!     ..Default::default()
//! };
//! load_model("http://localhost:8000/api/v1", "user.ggml-org/embeddinggemma-300M-GGUF", &opts).await?;
//! # Ok(()) }
//! ```

use anyhow::{Context, Result};
use serde::Serialize;

use super::client::LemonadeHttpClient;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns `true` when `model_name` uses the FLM (NPU) recipe.
///
/// FLM models do not use `llama-server` internally and will reject
/// `llamacpp_backend` / `llamacpp_args` parameters.
fn is_flm_model(model_name: &str) -> bool {
    model_name.ends_with("-FLM")
}

/// Build the effective `llamacpp_args` string for a non-FLM model.
///
/// If `ctx_size` is set and `--ubatch-size` is not already present in
/// `opts.llamacpp_args`, appends `--ubatch-size {ctx_size}` so the
/// micro-batch is kept in sync with the context window.  This prevents
/// llamacpp from rejecting prompts that fill the full context.
fn build_llamacpp_args(opts: &ModelLoadOptions) -> Option<String> {
    let base = opts.llamacpp_args.as_deref().unwrap_or("");

    if let Some(ctx) = opts.ctx_size {
        if !base.contains("--ubatch-size") {
            let extra = format!("--ubatch-size {ctx}");
            return Some(if base.is_empty() {
                extra
            } else {
                format!("{base} {extra}")
            });
        }
    }

    opts.llamacpp_args.clone()
}

// ── ModelLoadOptions ──────────────────────────────────────────────────────────

/// Options for the `POST /api/v1/load` Lemonade Server endpoint.
///
/// All fields are optional; unset fields are omitted from the request body and
/// the server uses its built-in defaults.  The mandatory `model_name` field is
/// passed separately to [`load_model`].
///
/// See the [Lemonade Server API docs](https://github.com/lemonade-sdk/lemonade)
/// for the full parameter reference.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelLoadOptions {
    /// Context window size in tokens.
    ///
    /// Applies to `llamacpp`, `flm`, and `ryzenai-llm` recipes.  Overrides
    /// the server default, which is often 512 or 2 K for embedding models.
    /// Use [`DEFAULT_EMBEDDING_CONTEXT_TOKENS`](crate::DEFAULT_EMBEDDING_CONTEXT_TOKENS)
    /// as a sensible starting value for embedding workloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx_size: Option<usize>,

    /// LlamaCpp backend to use: `"vulkan"`, `"rocm"`, `"metal"`, or `"cpu"`.
    ///
    /// Applies only to `llamacpp` recipes.  When `None` the server picks the
    /// best available backend automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llamacpp_backend: Option<String>,

    /// Extra arguments forwarded verbatim to `llama-server`.
    ///
    /// Useful for batch / micro-batch tuning, e.g.
    /// `"--batch-size 512 --ubatch-size 512"`.
    ///
    /// The following flags are **not** allowed here (the server rejects them):
    /// `-m`, `--port`, `--ctx-size`, `-ngl`, `--jinja`, `--mmproj`,
    /// `--embeddings`, `--reranking`.  Use [`ctx_size`](Self::ctx_size) instead
    /// of `--ctx-size`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llamacpp_args: Option<String>,
}

// ── Request body ──────────────────────────────────────────────────────────────

/// Serialised request body for `POST /api/v1/load`.
#[derive(Serialize)]
struct LoadRequest<'a> {
    model_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ctx_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    llamacpp_backend: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    llamacpp_args: Option<&'a str>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Explicitly load `model_name` into Lemonade Server with the given options.
///
/// Blocks until the server confirms the model is loaded (or returns an error).
/// Safe to call even if the model is already loaded — the server treats
/// repeated load calls as a no-op or a reconfiguration.
///
/// # Errors
///
/// Returns an error if the server is unreachable, returns a non-2xx status, or
/// the `model_name` is not registered in the server's model registry.
pub async fn load_model(base_url: &str, model_name: &str, opts: &ModelLoadOptions) -> Result<()> {
    let client = LemonadeHttpClient::new(base_url);

    // FLM (NPU) models do not use llama-server and will reject llamacpp params.
    let flm = is_flm_model(model_name);
    let effective_args: Option<String> = if flm {
        None
    } else {
        build_llamacpp_args(opts)
    };

    let body = LoadRequest {
        model_name,
        ctx_size: opts.ctx_size,
        llamacpp_backend: if flm { None } else { opts.llamacpp_backend.as_deref() },
        llamacpp_args: effective_args.as_deref(),
    };

    // The /load endpoint returns a JSON object; we don't need its contents —
    // error_for_status() inside post_json() is the signal we care about.
    let _: serde_json::Value = client
        .post_json("/load", &body)
        .await
        .with_context(|| format!("Failed to load model '{model_name}' via Lemonade Server"))?;

    tracing::info!(
        model = model_name,
        ctx_size = ?opts.ctx_size,
        flm,
        effective_llamacpp_args = ?effective_args,
        "Model loaded via Lemonade Server"
    );

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper unit tests ─────────────────────────────────────────────────────

    #[test]
    fn test_is_flm_model() {
        assert!(is_flm_model("embed-gemma-300m-FLM"));
        assert!(is_flm_model("qwen3-8b-FLM"));
        assert!(!is_flm_model("nomic-embed-text-v1-GGUF"));
        assert!(!is_flm_model("bge-reranker-v2-m3-GGUF"));
    }

    #[test]
    fn test_build_llamacpp_args_injects_ubatch_when_ctx_set() {
        let opts = ModelLoadOptions {
            ctx_size: Some(4096),
            ..Default::default()
        };
        let args = build_llamacpp_args(&opts).unwrap();
        assert_eq!(args, "--ubatch-size 4096");
    }

    #[test]
    fn test_build_llamacpp_args_appends_to_existing() {
        let opts = ModelLoadOptions {
            ctx_size: Some(2048),
            llamacpp_args: Some("--batch-size 512".to_string()),
            ..Default::default()
        };
        let args = build_llamacpp_args(&opts).unwrap();
        assert_eq!(args, "--batch-size 512 --ubatch-size 2048");
    }

    #[test]
    fn test_build_llamacpp_args_no_duplicate_ubatch() {
        let opts = ModelLoadOptions {
            ctx_size: Some(4096),
            llamacpp_args: Some("--ubatch-size 512".to_string()),
            ..Default::default()
        };
        let args = build_llamacpp_args(&opts).unwrap();
        // Already present — must not be added again
        assert_eq!(args, "--ubatch-size 512");
        assert_eq!(args.matches("--ubatch-size").count(), 1);
    }

    #[test]
    fn test_build_llamacpp_args_no_ctx_returns_existing_args() {
        let opts = ModelLoadOptions {
            ctx_size: None,
            llamacpp_args: Some("--batch-size 512".to_string()),
            ..Default::default()
        };
        assert_eq!(
            build_llamacpp_args(&opts).as_deref(),
            Some("--batch-size 512")
        );
    }

    #[test]
    fn test_build_llamacpp_args_all_none_returns_none() {
        let opts = ModelLoadOptions::default();
        assert!(build_llamacpp_args(&opts).is_none());
    }

    #[test]
    fn test_model_load_options_default_is_all_none() {
        let opts = ModelLoadOptions::default();
        assert!(opts.ctx_size.is_none());
        assert!(opts.llamacpp_backend.is_none());
        assert!(opts.llamacpp_args.is_none());
    }

    #[test]
    fn test_model_load_options_serializes_without_nulls() {
        let opts = ModelLoadOptions {
            ctx_size: Some(4096),
            ..Default::default()
        };
        let body = LoadRequest {
            model_name: "test-model",
            ctx_size: opts.ctx_size,
            llamacpp_backend: opts.llamacpp_backend.as_deref(),
            llamacpp_args: opts.llamacpp_args.as_deref(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["model_name"], "test-model");
        assert_eq!(json["ctx_size"], 4096);
        assert!(
            json.get("llamacpp_backend").is_none(),
            "unset fields must be omitted"
        );
        assert!(
            json.get("llamacpp_args").is_none(),
            "unset fields must be omitted"
        );
    }

    #[test]
    fn test_model_load_options_full_serialization() {
        let opts = ModelLoadOptions {
            ctx_size: Some(8192),
            llamacpp_backend: Some("rocm".to_string()),
            llamacpp_args: Some("--batch-size 512 --ubatch-size 256".to_string()),
        };
        let body = LoadRequest {
            model_name: "nomic-embed-text-v2-moe-GGUF",
            ctx_size: opts.ctx_size,
            llamacpp_backend: opts.llamacpp_backend.as_deref(),
            llamacpp_args: opts.llamacpp_args.as_deref(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["ctx_size"], 8192);
        assert_eq!(json["llamacpp_backend"], "rocm");
        assert_eq!(json["llamacpp_args"], "--batch-size 512 --ubatch-size 256");
    }

    #[tokio::test]
    async fn test_load_model_fails_on_unreachable_server() {
        let opts = ModelLoadOptions {
            ctx_size: Some(4096),
            ..Default::default()
        };
        let result = load_model(
            "http://127.0.0.1:19999/api/v1",
            "embed-gemma-300m-FLM",
            &opts,
        )
        .await;
        assert!(result.is_err(), "Expected error for unreachable server");
    }

    /// Integration test: explicitly load `nomic-embed-text-v1-GGUF` with
    /// `DEFAULT_EMBEDDING_CONTEXT_TOKENS` and verify the server accepts the request.
    ///
    /// Skips automatically when no Lemonade Server is reachable.
    #[tokio::test]
    async fn test_load_nomic_embed_v1_default_ctx() {
        let url = crate::test_helpers::require_integration_url!();

        let opts = ModelLoadOptions {
            ctx_size: Some(crate::lemonade::effective_ctx_size(
                "nomic-embed-text-v1-GGUF",
            )),
            ..Default::default()
        };

        let result = load_model(&url, "nomic-embed-text-v1-GGUF", &opts).await;
        assert!(
            result.is_ok(),
            "load_model failed: {:?}",
            result.unwrap_err()
        );
    }
}
