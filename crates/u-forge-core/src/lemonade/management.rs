//! Durable model/backend setup operations for owned and confirmed external runtimes.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use serde::Serialize;

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
    pub model_id: &'static str,
    /// Older IDs accepted as the same managed component. New pulls always use
    /// `model_id`, so compatibility aliases cannot create new stale names.
    pub legacy_model_ids: &'static [&'static str],
    pub checkpoint: Option<&'static str>,
    pub recipe: Option<&'static str>,
    pub required: bool,
    pub selected_by_default: bool,
    pub required_label: Option<&'static str>,
}

impl SetupComponent {
    pub fn matches_model_id(&self, model_id: &str) -> bool {
        self.model_id == model_id || self.legacy_model_ids.contains(&model_id)
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
            legacy_model_ids: &["user.ggml-org/embeddinggemma-300M-GGUF"],
            checkpoint: Some("ggml-org/embeddinggemma-300M-GGUF:Q8_0"),
            recipe: Some("llamacpp"),
            required: true,
            selected_by_default: true,
            required_label: Some("embeddings"),
        },
        SetupComponent {
            role: SetupRole::NpuEmbedding,
            model_id: "embed-gemma-300m-FLM",
            legacy_model_ids: &[],
            checkpoint: Some("embed-gemma:300m"),
            recipe: Some("flm"),
            required: false,
            selected_by_default: true,
            required_label: Some("embeddings"),
        },
        SetupComponent {
            role: SetupRole::Reranking,
            model_id: "bge-reranker-v2-m3-GGUF",
            legacy_model_ids: &[],
            checkpoint: None,
            recipe: None,
            required: true,
            selected_by_default: true,
            required_label: Some("reranking"),
        },
        SetupComponent {
            role: SetupRole::HighQualityEmbedding,
            model_id: "Qwen3-Embedding-8B-GGUF",
            legacy_model_ids: &[],
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
        .find(|model| component.matches_model_id(&model.id))
    else {
        tracing::debug!(
            model_id = component.model_id,
            legacy_model_ids = ?component.legacy_model_ids,
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
        embedding: bool,
        confirmed_external: bool,
    ) -> Result<serde_json::Value> {
        self.authorize_mutation(confirmed_external).await?;
        let body = pull_body(model_name, checkpoint, recipe, embedding);
        self.client.post_json_load("/pull", &body).await
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
}

fn pull_body(
    model_name: &str,
    checkpoint: Option<&str>,
    recipe: Option<&str>,
    embedding: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model_name": model_name,
        "embedding": embedding,
        "stream": true,
        "subscribe": false,
    });
    if let Some(checkpoint) = checkpoint {
        body["checkpoint"] = serde_json::Value::String(checkpoint.to_string());
    }
    if let Some(recipe) = recipe {
        body["recipe"] = serde_json::Value::String(recipe.to_string());
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
    fn current_and_legacy_standard_embedding_ids_are_detected() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::StandardEmbedding)
            .unwrap();
        assert_eq!(component.model_id, "ggml-org/embeddinggemma-300M-GGUF");

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
    fn downloaded_flm_embedding_is_detected_as_ready() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::NpuEmbedding)
            .unwrap();
        let catalog = LemonadeServerCatalog {
            models: vec![catalog_model(&component, true)],
            ..Default::default()
        };
        assert_eq!(component.model_id, "embed-gemma-300m-FLM");
        assert_eq!(
            component_state(&catalog, &component),
            SetupComponentState::Ready
        );
    }

    #[test]
    fn exact_standard_pull_body_has_durable_job_controls() {
        let component = initial_setup_components()
            .into_iter()
            .find(|component| component.role == SetupRole::StandardEmbedding)
            .unwrap();
        assert_eq!(
            pull_body(
                component.model_id,
                component.checkpoint,
                component.recipe,
                true
            ),
            serde_json::json!({
                "model_name": "ggml-org/embeddinggemma-300M-GGUF",
                "checkpoint": "ggml-org/embeddinggemma-300M-GGUF:Q8_0",
                "recipe": "llamacpp",
                "embedding": true,
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
