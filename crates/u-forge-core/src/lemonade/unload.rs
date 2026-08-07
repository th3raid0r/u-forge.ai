//! Explicit model unloading via Lemonade Server's `POST /v1/unload` endpoint.
//!
//! A model-specific request frees one runtime, while an empty request unloads
//! every loaded model. The latter is also used by the owned embedded-process
//! shutdown path so backend children (notably `llama-server`) exit before
//! `lemond` is reaped.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;

use super::{LemonadeConnection, LemonadeHttpClient};

#[derive(Serialize)]
struct UnloadRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    model_name: Option<&'a str>,
}

/// Unload one named model while leaving Lemonade Server running.
pub async fn unload_model(base_url: &str, model_name: &str) -> Result<()> {
    let connection = Arc::new(LemonadeConnection::external(base_url)?);
    unload_model_with_connection(connection, model_name).await
}

/// Unload one named model through an existing shared connection.
pub async fn unload_model_with_connection(
    connection: Arc<LemonadeConnection>,
    model_name: &str,
) -> Result<()> {
    unload_with_connection(connection, Some(model_name)).await
}

/// Unload every model while leaving Lemonade Server running.
pub async fn unload_all_models(base_url: &str) -> Result<()> {
    let connection = Arc::new(LemonadeConnection::external(base_url)?);
    unload_all_models_with_connection(connection).await
}

/// Unload every model through an existing shared connection.
pub async fn unload_all_models_with_connection(connection: Arc<LemonadeConnection>) -> Result<()> {
    unload_with_connection(connection, None).await
}

async fn unload_with_connection(
    connection: Arc<LemonadeConnection>,
    model_name: Option<&str>,
) -> Result<()> {
    let started = std::time::Instant::now();
    let client = LemonadeHttpClient::from_connection(connection);
    let body = UnloadRequest { model_name };
    let _: serde_json::Value =
        client
            .post_json_load("/unload", &body)
            .await
            .with_context(|| match model_name {
                Some(model_name) => {
                    format!("Failed to unload model '{model_name}' via Lemonade Server")
                }
                None => "Failed to unload all models via Lemonade Server".to_string(),
            })?;

    tracing::info!(
        model = model_name.unwrap_or("<all>"),
        duration_us = started.elapsed().as_micros() as u64,
        "Model unload completed via Lemonade Server"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unload_request_omits_model_name_for_unload_all() {
        let one = serde_json::to_value(UnloadRequest {
            model_name: Some("model-one"),
        })
        .unwrap();
        let all = serde_json::to_value(UnloadRequest { model_name: None }).unwrap();

        assert_eq!(one, serde_json::json!({ "model_name": "model-one" }));
        assert_eq!(all, serde_json::json!({}));
    }
}
