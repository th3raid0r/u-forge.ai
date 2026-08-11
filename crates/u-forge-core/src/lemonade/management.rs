//! Durable model/backend setup operations for owned and confirmed external runtimes.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures::StreamExt;
use serde::Serialize;
use tokio::sync::mpsc;

use super::{
    CatalogModel, InstalledBackend, LemonadeConnection, LemonadeHttpClient, LemonadeOwnership,
    LemonadeServerCatalog,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupRole {
    StandardEmbedding,
    NpuEmbedding,
    Reranking,
    HighQualityEmbedding,
    Chat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupComponent {
    pub role: SetupRole,
    /// Canonical ID used for catalog matching, selection, and inference.
    pub model_id: &'static str,
    /// Additional catalog IDs accepted as the same managed component.
    pub catalog_aliases: &'static [&'static str],
    /// Custom-registration name required by `/pull` when the model is not in
    /// Lemonade's built-in registry. `None` means pull the built-in model by
    /// canonical ID without forwarding registration metadata.
    pull_registration_id: Option<&'static str>,
    pub checkpoint: Option<&'static str>,
    pub recipe: Option<&'static str>,
    pub required: bool,
    pub selected_by_default: bool,
    pub required_label: Option<&'static str>,
}

/// Registration inputs for `/pull`. Built-in models intentionally omit all
/// optional registration fields; adding any of them changes the operation into
/// a user-model registration in Lemonade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupPullSpec {
    pub model_name: &'static str,
    pub checkpoint: Option<&'static str>,
    pub recipe: Option<&'static str>,
    pub embedding: Option<bool>,
}

impl SetupComponent {
    pub fn matches_model_id(&self, model_id: &str) -> bool {
        self.model_id == model_id || self.catalog_aliases.contains(&model_id)
    }

    pub fn pull_spec(&self) -> SetupPullSpec {
        if let Some(model_name) = self.pull_registration_id {
            SetupPullSpec {
                model_name,
                checkpoint: self.checkpoint,
                recipe: self.recipe,
                embedding: Some(self.is_embedding()),
            }
        } else {
            SetupPullSpec {
                model_name: self.model_id,
                checkpoint: None,
                recipe: None,
                embedding: None,
            }
        }
    }

    pub fn is_embedding(&self) -> bool {
        matches!(
            self.role,
            SetupRole::StandardEmbedding
                | SetupRole::NpuEmbedding
                | SetupRole::HighQualityEmbedding
        )
    }
}

/// Live state of a setup component. A conflicting user registration is kept
/// distinct from a missing/downloadable model so setup never overwrites it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupComponentState {
    Ready,
    Missing,
    NeedsDownload,
    Conflict(String),
}

impl SetupComponentState {
    pub fn needs_pull(&self) -> bool {
        matches!(self, Self::Missing | Self::NeedsDownload)
    }
}

/// Backend selected using the configured preference order and current
/// `/system-info` lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupBackendChoice {
    pub recipe: String,
    pub backend: String,
    pub state: String,
    pub devices: Vec<String>,
}

impl SetupBackendChoice {
    pub fn needs_install(&self) -> bool {
        self.state != "installed"
    }
}

pub fn initial_setup_components() -> Vec<SetupComponent> {
    vec![
        SetupComponent {
            role: SetupRole::StandardEmbedding,
            model_id: "ggml-org/embeddinggemma-300M-GGUF",
            catalog_aliases: &["user.ggml-org/embeddinggemma-300M-GGUF"],
            pull_registration_id: Some("user.ggml-org/embeddinggemma-300M-GGUF"),
            checkpoint: Some("ggml-org/embeddinggemma-300M-GGUF:Q8_0"),
            recipe: Some("llamacpp"),
            required: true,
            selected_by_default: true,
            required_label: Some("embeddings"),
        },
        SetupComponent {
            role: SetupRole::NpuEmbedding,
            model_id: "embed-gemma-300m-FLM",
            catalog_aliases: &["user.embed-gemma-300m-FLM"],
            pull_registration_id: None,
            checkpoint: Some("embed-gemma:300m"),
            recipe: Some("flm"),
            required: false,
            selected_by_default: true,
            required_label: Some("embeddings"),
        },
        SetupComponent {
            role: SetupRole::Reranking,
            model_id: "bge-reranker-v2-m3-GGUF",
            catalog_aliases: &[],
            pull_registration_id: None,
            checkpoint: None,
            recipe: None,
            required: true,
            selected_by_default: true,
            required_label: Some("reranking"),
        },
        SetupComponent {
            role: SetupRole::HighQualityEmbedding,
            model_id: "Qwen3-Embedding-8B-GGUF",
            catalog_aliases: &[],
            pull_registration_id: None,
            checkpoint: None,
            recipe: None,
            required: false,
            selected_by_default: true,
            required_label: Some("embeddings"),
        },
    ]
}

/// Compare a fixed setup descriptor with the live catalog.
pub fn component_state(
    catalog: &LemonadeServerCatalog,
    component: &SetupComponent,
) -> SetupComponentState {
    let Some(model) = catalog
        .models
        .iter()
        .find(|model| model.id == component.model_id)
        .or_else(|| {
            catalog
                .models
                .iter()
                .find(|model| component.catalog_aliases.contains(&model.id.as_str()))
        })
    else {
        tracing::debug!(
            model_id = component.model_id,
            catalog_aliases = ?component.catalog_aliases,
            candidate_model_ids = ?catalog
                .models
                .iter()
                .filter(|model| {
                    model.recipe == "flm"
                        || model.labels.iter().any(|label| label == "embeddings")
                })
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            "setup component was not present in the live Lemonade catalog"
        );
        return SetupComponentState::Missing;
    };

    tracing::debug!(
        model_id = component.model_id,
        catalog_model_id = %model.id,
        expected_checkpoint = ?component.checkpoint,
        catalog_checkpoint = ?model.checkpoint,
        expected_recipe = ?component.recipe,
        catalog_recipe = %model.recipe,
        catalog_labels = ?model.labels,
        downloaded = model.downloaded,
        "matched setup component against the live Lemonade catalog"
    );

    if let Some(expected) = component.checkpoint
        && model.checkpoint != expected
    {
        return SetupComponentState::Conflict(format!(
            "{} is registered with checkpoint {:?}; u-forge requires {expected:?}",
            component.model_id, model.checkpoint
        ));
    }
    if let Some(expected) = component.recipe
        && model.recipe != expected
    {
        return SetupComponentState::Conflict(format!(
            "{} is registered with recipe {:?}; u-forge requires {expected:?}",
            component.model_id, model.recipe
        ));
    }
    if let Some(label) = component.required_label
        && !model.labels.contains(label)
    {
        return SetupComponentState::Conflict(format!(
            "{} does not advertise the required {label:?} capability",
            component.model_id
        ));
    }

    if model.downloaded {
        SetupComponentState::Ready
    } else {
        SetupComponentState::NeedsDownload
    }
}

/// Validate a user-selected chat model without imposing a particular recipe.
pub fn chat_component_state(
    catalog: &LemonadeServerCatalog,
    model_id: &str,
) -> SetupComponentState {
    let Some(model) = catalog.models.iter().find(|model| model.id == model_id) else {
        return SetupComponentState::Missing;
    };
    if !matches!(model.recipe.as_str(), "llamacpp" | "flm")
        || model.labels.iter().any(|label| {
            matches!(
                label.as_str(),
                "embeddings" | "reranking" | "audio" | "transcription" | "tts"
            )
        })
    {
        return SetupComponentState::Conflict(format!("{model_id} is not a chat model"));
    }
    if model.downloaded {
        SetupComponentState::Ready
    } else {
        SetupComponentState::NeedsDownload
    }
}

/// Choose the first compatible backend in preference order. Lifecycle states
/// that Lemonade can install or update are returned so the caller can enqueue
/// the idempotent install task.
pub fn select_setup_backend(
    catalog: &LemonadeServerCatalog,
    recipe: &str,
    preference: &[String],
) -> Option<SetupBackendChoice> {
    let candidates: Vec<&InstalledBackend> = catalog
        .backends
        .iter()
        .filter(|backend| {
            backend.recipe == recipe
                && matches!(
                    backend.state.as_str(),
                    "installed" | "installable" | "update_required"
                )
        })
        .collect();

    let selected = preference
        .iter()
        .find_map(|name| {
            candidates
                .iter()
                .find(|backend| backend.backend == *name)
                .copied()
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|backend| backend.state == "installed")
                .copied()
        })
        .or_else(|| candidates.first().copied())?;
    Some(SetupBackendChoice {
        recipe: selected.recipe.clone(),
        backend: selected.backend.clone(),
        state: selected.state.clone(),
        devices: selected.devices.clone(),
    })
}

/// Models eligible for the setup chat picker, including not-yet-downloaded
/// entries because setup itself owns provisioning.
pub fn setup_chat_models(catalog: &LemonadeServerCatalog) -> Vec<&CatalogModel> {
    catalog
        .models
        .iter()
        .filter(|model| {
            matches!(model.recipe.as_str(), "llamacpp" | "flm")
                && !model.labels.iter().any(|label| {
                    matches!(
                        label.as_str(),
                        "embeddings" | "reranking" | "audio" | "transcription" | "tts"
                    )
                })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadAction {
    Pause,
    Cancel,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementOperationKind {
    ModelPull,
    BackendInstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagementEventKind {
    Progress,
    Complete,
    Failed,
}

/// Normalized event emitted by Lemonade's management SSE endpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagementProgressEvent {
    pub operation: ManagementOperationKind,
    pub target: String,
    pub kind: ManagementEventKind,
    pub progress_percent: Option<f32>,
    pub transferred_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub message: Option<String>,
}

impl ManagementProgressEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.kind,
            ManagementEventKind::Complete | ManagementEventKind::Failed
        )
    }
}

pub type ManagementProgressReceiver = mpsc::UnboundedReceiver<Result<ManagementProgressEvent>>;

#[derive(Clone)]
pub struct LemonadeManagement {
    connection: Arc<LemonadeConnection>,
    client: LemonadeHttpClient,
}

impl LemonadeManagement {
    pub fn new(connection: Arc<LemonadeConnection>) -> Self {
        Self {
            client: LemonadeHttpClient::from_connection(connection.clone()),
            connection,
        }
    }

    /// Verify that mutations are allowed. External changes require distinct
    /// credentials, a successful admin probe, and confirmation for this action.
    pub async fn authorize_mutation(&self, confirmed_external: bool) -> Result<()> {
        if self.connection.ownership() == LemonadeOwnership::Embedded {
            return Ok(());
        }
        if !self.connection.has_api_key() || !self.connection.has_admin_api_key() {
            return Err(anyhow!(
                "external Lemonade management requires LEMONADE_API_KEY and LEMONADE_ADMIN_API_KEY"
            ));
        }
        if !confirmed_external {
            return Err(anyhow!("external Lemonade mutation was not confirmed"));
        }
        let _: serde_json::Value = self.client.get_admin_json("/config").await?;
        Ok(())
    }

    /// Start a server-owned durable model pull and return its job response.
    pub async fn pull(
        &self,
        model_name: &str,
        checkpoint: Option<&str>,
        recipe: Option<&str>,
        embedding: Option<bool>,
        confirmed_external: bool,
    ) -> Result<serde_json::Value> {
        self.authorize_mutation(confirmed_external).await?;
        let body = pull_body(model_name, checkpoint, recipe, embedding);
        self.client.post_json_load("/pull", &body).await
    }

    /// Pull a model with a client-owned SSE subscription. The receiver owns
    /// progress until a terminal event or transport failure; dropping it ends
    /// observation and allows the request task to stop naturally.
    pub async fn pull_stream(
        &self,
        model_name: &str,
        checkpoint: Option<&str>,
        recipe: Option<&str>,
        embedding: Option<bool>,
        confirmed_external: bool,
    ) -> Result<ManagementProgressReceiver> {
        self.authorize_mutation(confirmed_external).await?;
        let mut body = pull_body(model_name, checkpoint, recipe, embedding);
        body["subscribe"] = serde_json::Value::Bool(true);
        let response = self.client.post_stream("/pull", &body).await?;
        Ok(management_event_stream(
            response,
            ManagementOperationKind::ModelPull,
            model_name.to_string(),
        ))
    }

    pub async fn downloads(&self) -> Result<serde_json::Value> {
        self.client.get_json("/downloads").await
    }

    pub async fn control_download(
        &self,
        job_id: &str,
        action: DownloadAction,
        confirmed_external: bool,
    ) -> Result<serde_json::Value> {
        self.authorize_mutation(confirmed_external).await?;
        self.client
            .post_json_load("/downloads/control", &download_control_body(job_id, action))
            .await
    }

    pub async fn install_backend(
        &self,
        recipe: &str,
        backend: &str,
        confirmed_external: bool,
    ) -> Result<serde_json::Value> {
        self.authorize_mutation(confirmed_external).await?;
        self.client
            .post_json_load(
                "/install",
                &serde_json::json!({
                    "recipe": recipe,
                    "backend": backend,
                    "stream": false,
                }),
            )
            .await
    }

    pub async fn install_backend_stream(
        &self,
        recipe: &str,
        backend: &str,
        confirmed_external: bool,
    ) -> Result<ManagementProgressReceiver> {
        self.authorize_mutation(confirmed_external).await?;
        let response = self
            .client
            .post_stream(
                "/install",
                &serde_json::json!({
                    "recipe": recipe,
                    "backend": backend,
                    "stream": true,
                }),
            )
            .await?;
        Ok(management_event_stream(
            response,
            ManagementOperationKind::BackendInstall,
            format!("{recipe}:{backend}"),
        ))
    }
}

fn management_event_stream(
    response: reqwest::Response,
    operation: ManagementOperationKind,
    target: String,
) -> ManagementProgressReceiver {
    let (tx, rx) = mpsc::unbounded_channel();
    let _ = tx.send(Ok(ManagementProgressEvent {
        operation,
        target: target.clone(),
        kind: ManagementEventKind::Progress,
        progress_percent: None,
        transferred_bytes: None,
        total_bytes: None,
        message: Some("Starting…".to_string()),
    }));
    tokio::spawn(async move {
        let mut bytes = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut terminal = false;
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = tx.send(Err(anyhow!("management SSE transport failed: {error}")));
                    return;
                }
            };
            buffer.extend_from_slice(&chunk);
            while let Some(end) = find_sse_boundary(&buffer) {
                let frame = buffer.drain(..end).collect::<Vec<_>>();
                let boundary_len = if buffer.starts_with(b"\r\n\r\n") {
                    4
                } else {
                    2
                };
                buffer.drain(..boundary_len);
                match decode_management_event(&frame, operation, &target) {
                    Ok(Some(event)) => {
                        terminal |= event.is_terminal();
                        if tx.send(Ok(event)).is_err() || terminal {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        return;
                    }
                }
            }
        }
        if !terminal {
            let _ = tx.send(Err(anyhow!("management SSE ended before a terminal event")));
        }
    });
    rx
}

fn find_sse_boundary(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| bytes.windows(2).position(|window| window == b"\n\n"))
}

fn decode_management_event(
    frame: &[u8],
    operation: ManagementOperationKind,
    target: &str,
) -> Result<Option<ManagementProgressEvent>> {
    let text = std::str::from_utf8(frame)?;
    let mut event_name = None;
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') || line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start());
        }
    }
    if data.is_empty() {
        return Ok(None);
    }
    let payload_text = data.join("\n");
    let payload = serde_json::from_str::<serde_json::Value>(&payload_text)
        .unwrap_or_else(|_| serde_json::Value::String(payload_text.clone()));
    let normalized_name = event_name.unwrap_or("progress").to_ascii_lowercase();
    let kind = if matches!(normalized_name.as_str(), "complete" | "completed" | "done") {
        ManagementEventKind::Complete
    } else if matches!(normalized_name.as_str(), "error" | "failed" | "failure") {
        ManagementEventKind::Failed
    } else {
        ManagementEventKind::Progress
    };
    let number = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| payload.get(*name).and_then(serde_json::Value::as_f64))
    };
    let integer = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| payload.get(*name).and_then(serde_json::Value::as_u64))
    };
    let mut progress_percent = number(&["progress", "percent", "percentage"]).map(|value| {
        let value = if value <= 1.0 { value * 100.0 } else { value };
        value.clamp(0.0, 100.0) as f32
    });
    let transferred_bytes = integer(&[
        "transferred_bytes",
        "downloaded_bytes",
        "downloaded",
        "completed",
    ]);
    let total_bytes = integer(&["total_bytes", "total", "size"]);
    if progress_percent.is_none()
        && let (Some(transferred), Some(total)) = (transferred_bytes, total_bytes)
        && total > 0
    {
        progress_percent = Some((transferred as f64 * 100.0 / total as f64) as f32);
    }
    let message = ["message", "status", "detail", "error"]
        .into_iter()
        .find_map(|name| payload.get(name).and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .or_else(|| payload.as_str().map(ToString::to_string));
    Ok(Some(ManagementProgressEvent {
        operation,
        target: target.to_string(),
        kind,
        progress_percent,
        transferred_bytes,
        total_bytes,
        message,
    }))
}

fn pull_body(
    model_name: &str,
    checkpoint: Option<&str>,
    recipe: Option<&str>,
    embedding: Option<bool>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model_name": model_name,
        "stream": true,
        "subscribe": false,
    });
    if let Some(checkpoint) = checkpoint {
        body["checkpoint"] = serde_json::Value::String(checkpoint.to_string());
    }
    if let Some(recipe) = recipe {
        body["recipe"] = serde_json::Value::String(recipe.to_string());
    }
    if let Some(embedding) = embedding {
        body["embedding"] = serde_json::Value::Bool(embedding);
    }
    body
}

fn download_control_body(job_id: &str, action: DownloadAction) -> serde_json::Value {
    serde_json::json!({ "id": job_id, "action": action })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_model(component: &SetupComponent, downloaded: bool) -> CatalogModel {
        CatalogModel {
            id: component.model_id.to_string(),
            checkpoint: component.checkpoint.unwrap_or_default().to_string(),
            recipe: component.recipe.unwrap_or("llamacpp").to_string(),
            labels: component
                .required_label
                .into_iter()
                .map(ToString::to_string)
                .collect(),
            downloaded,
            ..Default::default()
        }
    }

    #[test]
    fn setup_components_keep_required_roles_explicit() {
        let components = initial_setup_components();
        assert!(
            components
                .iter()
                .any(|item| item.role == SetupRole::StandardEmbedding && item.required)
        );
        assert!(components.iter().any(|item| {
            item.role == SetupRole::NpuEmbedding && item.selected_by_default && !item.required
        }));
        assert!(
            components
                .iter()
                .any(|item| item.role == SetupRole::Reranking && item.required)
        );
        assert!(
            components
                .iter()
                .any(|item| item.role == SetupRole::HighQualityEmbedding && !item.required)
        );
    }

    #[test]
    fn standard_registration_is_exact_and_conflicts_are_actionable() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::StandardEmbedding)
            .unwrap();
        let mut catalog = LemonadeServerCatalog {
            models: vec![catalog_model(&component, false)],
            ..Default::default()
        };
        assert_eq!(
            component_state(&catalog, &component),
            SetupComponentState::NeedsDownload
        );
        catalog.models[0].checkpoint = "someone/else:Q4".to_string();
        let SetupComponentState::Conflict(message) = component_state(&catalog, &component) else {
            panic!("expected registration conflict")
        };
        assert!(message.contains("checkpoint"));
    }

    #[test]
    fn canonical_and_custom_registration_standard_embedding_ids_are_detected() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::StandardEmbedding)
            .unwrap();
        assert_eq!(component.model_id, "ggml-org/embeddinggemma-300M-GGUF");
        assert_eq!(
            component.pull_spec().model_name,
            "user.ggml-org/embeddinggemma-300M-GGUF"
        );

        let mut current = catalog_model(&component, true);
        let mut catalog = LemonadeServerCatalog {
            models: vec![current.clone()],
            ..Default::default()
        };
        assert_eq!(
            component_state(&catalog, &component),
            SetupComponentState::Ready
        );

        current.id = "user.ggml-org/embeddinggemma-300M-GGUF".to_string();
        catalog.models = vec![current];
        assert_eq!(
            component_state(&catalog, &component),
            SetupComponentState::Ready
        );
    }

    #[test]
    fn canonical_and_legacy_registration_flm_embedding_ids_are_detected() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::NpuEmbedding)
            .unwrap();
        assert_eq!(component.model_id, "embed-gemma-300m-FLM");
        assert_eq!(component.pull_spec().model_name, "embed-gemma-300m-FLM");

        let mut model = catalog_model(&component, true);
        let mut catalog = LemonadeServerCatalog {
            models: vec![model.clone()],
            ..Default::default()
        };
        assert_eq!(
            component_state(&catalog, &component),
            SetupComponentState::Ready
        );

        model.id = "user.embed-gemma-300m-FLM".to_string();
        catalog.models = vec![model];
        assert_eq!(
            component_state(&catalog, &component),
            SetupComponentState::Ready
        );
    }

    #[test]
    fn canonical_catalog_entry_wins_over_legacy_registration_alias() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::NpuEmbedding)
            .unwrap();
        let mut legacy = catalog_model(&component, true);
        legacy.id = "user.embed-gemma-300m-FLM".to_string();
        let canonical = catalog_model(&component, false);
        let catalog = LemonadeServerCatalog {
            models: vec![legacy, canonical],
            ..Default::default()
        };

        assert_eq!(
            component_state(&catalog, &component),
            SetupComponentState::NeedsDownload
        );
    }

    #[test]
    fn exact_standard_pull_body_has_durable_job_controls() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::StandardEmbedding)
            .unwrap();
        let pull = component.pull_spec();
        assert_eq!(
            pull_body(
                pull.model_name,
                pull.checkpoint,
                pull.recipe,
                pull.embedding
            ),
            serde_json::json!({
                "model_name": "user.ggml-org/embeddinggemma-300M-GGUF",
                "checkpoint": "ggml-org/embeddinggemma-300M-GGUF:Q8_0",
                "recipe": "llamacpp",
                "embedding": true,
                "stream": true,
                "subscribe": false,
            })
        );
    }

    #[test]
    fn exact_flm_pull_body_uses_builtin_model_name_only() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::NpuEmbedding)
            .unwrap();
        let pull = component.pull_spec();
        assert_eq!(
            pull_body(
                pull.model_name,
                pull.checkpoint,
                pull.recipe,
                pull.embedding
            ),
            serde_json::json!({
                "model_name": "embed-gemma-300m-FLM",
                "stream": true,
                "subscribe": false,
            })
        );
    }

    #[test]
    fn backend_selection_obeys_preference_before_lifecycle_state() {
        let catalog = LemonadeServerCatalog {
            backends: vec![
                InstalledBackend {
                    recipe: "llamacpp".to_string(),
                    backend: "cpu".to_string(),
                    state: "installed".to_string(),
                    ..Default::default()
                },
                InstalledBackend {
                    recipe: "llamacpp".to_string(),
                    backend: "vulkan".to_string(),
                    state: "installable".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let choice = select_setup_backend(
            &catalog,
            "llamacpp",
            &["vulkan".to_string(), "cpu".to_string()],
        )
        .unwrap();
        assert_eq!(choice.backend, "vulkan");
        assert!(choice.needs_install());
    }

    #[test]
    fn download_control_uses_current_id_and_action_shape() {
        assert_eq!(
            download_control_body("model:test", DownloadAction::Pause),
            serde_json::json!({"id": "model:test", "action": "pause"})
        );
    }

    #[test]
    fn management_sse_normalizes_progress_and_terminal_events() {
        let progress = decode_management_event(
            b"event: progress\ndata: {\"downloaded_bytes\":25,\"total_bytes\":100,\"message\":\"pulling\"}",
            ManagementOperationKind::ModelPull,
            "test-model",
        )
        .unwrap()
        .unwrap();
        assert_eq!(progress.kind, ManagementEventKind::Progress);
        assert_eq!(progress.progress_percent, Some(25.0));
        assert_eq!(progress.transferred_bytes, Some(25));
        assert!(!progress.is_terminal());

        let complete = decode_management_event(
            b"event: complete\r\ndata: {\"status\":\"ready\"}",
            ManagementOperationKind::BackendInstall,
            "llamacpp:cpu",
        )
        .unwrap()
        .unwrap();
        assert_eq!(complete.kind, ManagementEventKind::Complete);
        assert_eq!(complete.message.as_deref(), Some("ready"));
        assert!(complete.is_terminal());
    }

    #[test]
    fn management_sse_accepts_multiline_data_and_keepalives() {
        assert!(
            decode_management_event(b": keepalive", ManagementOperationKind::ModelPull, "model",)
                .unwrap()
                .is_none()
        );
        let failed = decode_management_event(
            b"event: error\ndata: download\ndata: interrupted",
            ManagementOperationKind::ModelPull,
            "model",
        )
        .unwrap()
        .unwrap();
        assert_eq!(failed.kind, ManagementEventKind::Failed);
        assert_eq!(failed.message.as_deref(), Some("download\ninterrupted"));
    }

    #[tokio::test]
    async fn external_mutations_require_both_credentials_and_confirmation() {
        let keyless = Arc::new(
            LemonadeConnection::with_credentials(
                "http://127.0.0.1:1/v1",
                LemonadeOwnership::External,
                None,
                None,
                super::super::LemonadeTimeouts::default(),
            )
            .unwrap(),
        );
        let error = LemonadeManagement::new(keyless)
            .authorize_mutation(true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("requires LEMONADE_API_KEY"));

        let credentialed = Arc::new(
            LemonadeConnection::with_credentials(
                "http://127.0.0.1:1/v1",
                LemonadeOwnership::External,
                Some("api".to_string()),
                Some("admin".to_string()),
                super::super::LemonadeTimeouts::default(),
            )
            .unwrap(),
        );
        let error = LemonadeManagement::new(credentialed)
            .authorize_mutation(false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not confirmed"));
    }
}
