//! Serialized Lemonade loaded-profile coordination.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::config::ReasoningControl;

use super::{
    LemonadeConnection, LemonadeHealth, ModelLoadOptions, reload_model_for_recipe_with_connection,
    unload_model_with_connection,
};

/// Three-state reasoning policy used by both direct and agent requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReasoningPolicy {
    #[default]
    Default,
    Enabled,
    Disabled,
}

impl ReasoningPolicy {
    pub fn request_hint(self) -> Option<bool> {
        match self {
            Self::Default => None,
            Self::Enabled => Some(true),
            Self::Disabled => Some(false),
        }
    }
}

/// State resolved together for one LLM request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LemonadeRuntimeProfile {
    pub model_id: String,
    pub checkpoint: Option<String>,
    pub recipe: String,
    pub backend: Option<String>,
    pub device: Option<String>,
    pub reasoning: ReasoningPolicy,
    pub reasoning_control: ReasoningControl,
    pub reasoning_capable: bool,
    pub load_options: ModelLoadOptions,
}

impl LemonadeRuntimeProfile {
    /// Compatibility constructor. New selection code should additionally set
    /// recipe/backend/capability using the builder methods below.
    pub fn new(
        model_id: impl Into<String>,
        reasoning_enabled: bool,
        load_options: ModelLoadOptions,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            checkpoint: None,
            recipe: String::new(),
            backend: None,
            device: None,
            reasoning: if reasoning_enabled {
                ReasoningPolicy::Enabled
            } else {
                ReasoningPolicy::Disabled
            },
            reasoning_control: ReasoningControl::Request,
            reasoning_capable: false,
            load_options,
        }
    }

    pub fn with_backend_profile(
        mut self,
        recipe: impl Into<String>,
        backend: Option<String>,
        device: Option<String>,
    ) -> Self {
        self.recipe = recipe.into();
        self.backend = backend;
        self.device = device;
        self
    }

    pub fn with_checkpoint(mut self, checkpoint: impl Into<String>) -> Self {
        let checkpoint = checkpoint.into();
        self.checkpoint = (!checkpoint.is_empty()).then_some(checkpoint);
        self
    }

    pub fn with_reasoning(
        mut self,
        policy: ReasoningPolicy,
        control: ReasoningControl,
        capable: bool,
    ) -> Self {
        self.reasoning = policy;
        self.reasoning_control = control;
        self.reasoning_capable = capable;
        self
    }

    fn loaded_key_and_options(&self) -> Result<(LoadedProfileKey, ModelLoadOptions)> {
        let mut options = self.load_options.clone();
        let reload_reasoning = self.reasoning_control == ReasoningControl::Reload
            && self.reasoning_capable
            && self.recipe == "llamacpp";

        if self.recipe != "llamacpp" && !self.recipe.is_empty() {
            options.llamacpp_backend = None;
            options.llamacpp_args = None;
        } else if reload_reasoning {
            let existing = options.llamacpp_args.as_deref().unwrap_or_default();
            if existing.contains("--chat-template-kwargs") {
                return Err(anyhow!(
                    "llamacpp_args contains u-forge-owned flag --chat-template-kwargs"
                ));
            }
            if let Some(enabled) = self.reasoning.request_hint() {
                let managed = format!("--chat-template-kwargs '{{\"enable_thinking\":{enabled}}}'");
                options.llamacpp_args = Some(if existing.is_empty() {
                    managed
                } else {
                    format!("{existing} {managed}")
                });
            }
        }

        let key = LoadedProfileKey {
            model_id: self.model_id.clone(),
            checkpoint: self.checkpoint.clone(),
            recipe: self.recipe.clone(),
            backend: self.backend.clone(),
            device: self.device.clone(),
            load_options: options.clone(),
            reasoning: reload_reasoning.then_some(self.reasoning),
        };
        Ok((key, options))
    }
}

/// Only fields that can require backend reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProfileKey {
    pub model_id: String,
    pub checkpoint: Option<String>,
    pub recipe: String,
    pub backend: Option<String>,
    pub device: Option<String>,
    pub load_options: ModelLoadOptions,
    pub reasoning: Option<ReasoningPolicy>,
}

/// RAII proof that one loaded-profile conflict domain is reserved.
pub struct LemonadeRuntimeLease {
    _guard: OwnedMutexGuard<()>,
    pub reload_performed: bool,
    pub degraded_authority: Option<String>,
}

/// Coordinates health comparison, reload, request startup, and response life.
pub struct LemonadeRuntime {
    connection: Arc<LemonadeConnection>,
    gate: Arc<AsyncMutex<()>>,
    active: Mutex<Option<LoadedProfileKey>>,
}

impl LemonadeRuntime {
    pub fn new(base_url: impl AsRef<str>) -> Self {
        let connection = LemonadeConnection::external(base_url.as_ref()).unwrap_or_else(|_| {
            LemonadeConnection::external("http://127.0.0.1:1/v1")
                .expect("static fallback Lemonade URL is valid")
        });
        Self::from_connection(Arc::new(connection))
    }

    pub fn from_connection(connection: Arc<LemonadeConnection>) -> Self {
        let gate = connection.llm_runtime_gate();
        Self {
            connection,
            gate,
            active: Mutex::new(None),
        }
    }

    pub fn connection(&self) -> &Arc<LemonadeConnection> {
        &self.connection
    }

    /// Acquire the runtime through response completion. If live health cannot
    /// establish the requested profile, explicitly reload rather than trusting
    /// the local cache or an `already_loaded` list.
    pub async fn acquire(
        self: &Arc<Self>,
        profile: &LemonadeRuntimeProfile,
    ) -> Result<LemonadeRuntimeLease> {
        let guard = self.gate.clone().lock_owned().await;
        let (key, load_options) = profile.loaded_key_and_options()?;

        let (live_matches, degraded_authority) =
            match LemonadeHealth::fetch_with_connection(self.connection.clone()).await {
                Ok(health) => (health_matches(&health, &key), None),
                Err(error) => (
                    false,
                    Some(format!(
                        "health unavailable; profile was explicitly loaded: {error:#}"
                    )),
                ),
            };

        // A runtime represents one feature's model slot. Release the model
        // previously owned by that slot before activating a different model so
        // Lemonade never has to evict an unrelated capability merely to make
        // room for the replacement. Profile-only changes for the same model
        // continue through the ordinary reload path below.
        let previous_model = self
            .active
            .lock()
            .expect("runtime state mutex poisoned")
            .as_ref()
            .filter(|active| active.model_id != key.model_id)
            .map(|active| active.model_id.clone());
        if let Some(previous_model) = previous_model {
            unload_model_with_connection(self.connection.clone(), &previous_model)
                .await
                .with_context(|| {
                    format!(
                        "failed to release previous Lemonade model {previous_model} before activating {}",
                        profile.model_id
                    )
                })?;
            *self.active.lock().expect("runtime state mutex poisoned") = None;
        }

        let reload_performed = if live_matches {
            false
        } else {
            reload_model_for_recipe_with_connection(
                self.connection.clone(),
                &profile.model_id,
                &profile.recipe,
                &load_options,
            )
            .await
            .with_context(|| format!("failed to activate Lemonade profile {}", profile.model_id))?;
            true
        };

        *self.active.lock().expect("runtime state mutex poisoned") = Some(key);
        Ok(LemonadeRuntimeLease {
            _guard: guard,
            reload_performed,
            degraded_authority,
        })
    }

    /// Acquire or activate a profile while allowing the owning operation to
    /// interrupt gate wait, health discovery, and the model load request.
    pub async fn acquire_with_cancellation(
        self: &Arc<Self>,
        profile: &LemonadeRuntimeProfile,
        cancellation: &crate::queue::CancellationToken,
    ) -> crate::queue::InferenceResult<LemonadeRuntimeLease> {
        tokio::select! {
            _ = cancellation.cancelled() => Err(cancellation.error()),
            result = self.acquire(profile) => result.map_err(|error| {
                crate::queue::InferenceError::classify_timeout(
                    error,
                    crate::queue::TimeoutClass::ModelActivation,
                )
            }),
        }
    }

    /// Compatibility activation. Prefer `acquire` and retain its lease through
    /// the complete request/stream.
    pub async fn activate(self: &Arc<Self>, profile: &LemonadeRuntimeProfile) -> Result<bool> {
        let lease = self.acquire(profile).await?;
        Ok(lease.reload_performed)
    }

    pub fn active_profile(&self) -> Option<LoadedProfileKey> {
        self.active
            .lock()
            .expect("runtime state mutex poisoned")
            .clone()
    }
}

fn health_matches(health: &LemonadeHealth, key: &LoadedProfileKey) -> bool {
    health.all_models_loaded.iter().any(|loaded| {
        loaded.model_name == key.model_id
            && key
                .checkpoint
                .as_deref()
                .is_none_or(|checkpoint| loaded.checkpoint == checkpoint)
            && (key.recipe.is_empty() || loaded.recipe == key.recipe)
            && key
                .device
                .as_deref()
                .is_none_or(|device| loaded.device.split_whitespace().any(|live| live == device))
            && option_matches(
                key.load_options.ctx_size,
                &loaded.recipe_options,
                &["ctx_size", "context_size", "n_ctx"],
            )
            && string_option_matches(
                key.backend.as_deref(),
                &loaded.recipe_options,
                &["llamacpp_backend", "backend"],
            )
            && string_option_matches(
                key.load_options.llamacpp_args.as_deref(),
                &loaded.recipe_options,
                &["llamacpp_args"],
            )
    })
}

fn option_matches(expected: Option<usize>, object: &serde_json::Value, names: &[&str]) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    names
        .iter()
        .find_map(|name| object.get(name).and_then(serde_json::Value::as_u64))
        .is_some_and(|actual| actual == expected as u64)
}

fn string_option_matches(
    expected: Option<&str>,
    object: &serde_json::Value,
    names: &[&str],
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    names
        .iter()
        .find_map(|name| object.get(name).and_then(serde_json::Value::as_str))
        == Some(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn request_reasoning_is_not_loaded_identity() {
        let normal = LemonadeRuntimeProfile::new("model", false, ModelLoadOptions::default());
        let reasoning = LemonadeRuntimeProfile::new("model", true, ModelLoadOptions::default());
        assert_eq!(
            normal.loaded_key_and_options().unwrap().0,
            reasoning.loaded_key_and_options().unwrap().0
        );
    }

    #[test]
    fn reload_reasoning_is_managed_loaded_identity() {
        let profile = LemonadeRuntimeProfile::new("model", true, ModelLoadOptions::default())
            .with_backend_profile("llamacpp", Some("rocm".into()), Some("gpu".into()))
            .with_reasoning(ReasoningPolicy::Enabled, ReasoningControl::Reload, true);
        let (key, options) = profile.loaded_key_and_options().unwrap();
        assert_eq!(key.reasoning, Some(ReasoningPolicy::Enabled));
        assert!(options.llamacpp_args.unwrap().contains("enable_thinking"));
    }

    #[test]
    fn rejects_user_owned_template_flag_and_drops_llama_args_for_flm() {
        let conflict = LemonadeRuntimeProfile::new(
            "model",
            true,
            ModelLoadOptions {
                llamacpp_args: Some("--chat-template-kwargs x".into()),
                ..Default::default()
            },
        )
        .with_backend_profile("llamacpp", None, None)
        .with_reasoning(ReasoningPolicy::Enabled, ReasoningControl::Reload, true);
        assert!(conflict.loaded_key_and_options().is_err());

        let flm = LemonadeRuntimeProfile::new(
            "model-FLM",
            true,
            ModelLoadOptions {
                llamacpp_backend: Some("rocm".into()),
                llamacpp_args: Some("--foo".into()),
                ..Default::default()
            },
        )
        .with_backend_profile("flm", None, Some("npu".into()));
        let (_, options) = flm.loaded_key_and_options().unwrap();
        assert!(options.llamacpp_backend.is_none());
        assert!(options.llamacpp_args.is_none());
    }

    #[tokio::test]
    async fn live_profile_drives_reload_external_detection_and_serialization() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let loaded = Arc::new(tokio::sync::Mutex::new(None::<serde_json::Value>));
        let reloads = Arc::new(AtomicUsize::new(0));
        let lifecycle = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let server_loaded = loaded.clone();
        let server_reloads = reloads.clone();
        let server_lifecycle = lifecycle.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let loaded = server_loaded.clone();
                let reloads = server_reloads.clone();
                let lifecycle = server_lifecycle.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 2048];
                    let header_end;
                    loop {
                        let read = socket.read(&mut buffer).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = end + 4;
                            break;
                        }
                    }
                    let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(str::trim)
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    while request.len() < header_end + content_length {
                        let read = socket.read(&mut buffer).await.unwrap();
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let path = headers.split_whitespace().nth(1).unwrap().to_string();
                    let body = if path == "/v1/health" {
                        let live = loaded.lock().await.clone();
                        serde_json::json!({ "status": "ok", "all_models_loaded": live.into_iter().collect::<Vec<_>>() })
                    } else if path == "/v1/load" {
                        let request_body: serde_json::Value =
                            serde_json::from_slice(&request[header_end..]).unwrap();
                        let model_name = request_body["model_name"].as_str().unwrap();
                        lifecycle.lock().await.push(format!("load:{model_name}"));
                        reloads.fetch_add(1, Ordering::SeqCst);
                        *loaded.lock().await = Some(serde_json::json!({
                            "model_name": request_body["model_name"],
                            "recipe": "llamacpp",
                            "device": "gpu",
                            "recipe_options": {
                                "ctx_size": request_body["ctx_size"],
                                "llamacpp_backend": request_body["llamacpp_backend"],
                            }
                        }));
                        serde_json::json!({"status":"success"})
                    } else if path == "/v1/unload" {
                        let request_body: serde_json::Value =
                            serde_json::from_slice(&request[header_end..]).unwrap();
                        let model_name = request_body["model_name"].as_str().unwrap();
                        lifecycle.lock().await.push(format!("unload:{model_name}"));
                        *loaded.lock().await = None;
                        serde_json::json!({"status":"success"})
                    } else {
                        panic!("unexpected {path}")
                    };
                    let body = body.to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });

        let connection = Arc::new(
            LemonadeConnection::with_credentials(
                &format!("http://{address}"),
                super::super::LemonadeOwnership::External,
                None,
                None,
                super::super::LemonadeTimeouts::default(),
            )
            .unwrap(),
        );
        let runtime = Arc::new(LemonadeRuntime::from_connection(connection));
        let profile = LemonadeRuntimeProfile::new(
            "model",
            true,
            ModelLoadOptions {
                ctx_size: Some(4096),
                llamacpp_backend: Some("rocm".into()),
                ..Default::default()
            },
        )
        .with_backend_profile("llamacpp", Some("rocm".into()), Some("gpu".into()));

        let first = runtime.acquire(&profile).await.unwrap();
        assert!(first.reload_performed);
        drop(first);
        let unchanged = runtime.acquire(&profile).await.unwrap();
        assert!(!unchanged.reload_performed);

        let waiting_runtime = runtime.clone();
        let waiting_profile = profile.clone();
        let waiter = tokio::spawn(async move { waiting_runtime.acquire(&waiting_profile).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), async {
                while !waiter.is_finished() {
                    tokio::task::yield_now().await
                }
            })
            .await
            .is_err()
        );
        drop(unchanged);
        let acquired = waiter.await.unwrap().unwrap();
        drop(acquired);

        let replacement_profile = LemonadeRuntimeProfile::new(
            "replacement-model",
            true,
            ModelLoadOptions {
                ctx_size: Some(4096),
                llamacpp_backend: Some("rocm".into()),
                ..Default::default()
            },
        )
        .with_backend_profile("llamacpp", Some("rocm".into()), Some("gpu".into()));
        let replacement = runtime.acquire(&replacement_profile).await.unwrap();
        assert!(replacement.reload_performed);
        drop(replacement);
        assert_eq!(
            lifecycle.lock().await.as_slice(),
            ["load:model", "unload:model", "load:replacement-model"]
        );

        *loaded.lock().await = Some(serde_json::json!({
            "model_name": "changed-by-other-client", "recipe": "llamacpp", "device": "gpu"
        }));
        let external_change = runtime.acquire(&replacement_profile).await.unwrap();
        assert!(external_change.reload_performed);
        drop(external_change);
        assert_eq!(reloads.load(Ordering::SeqCst), 3);
        assert_eq!(
            lifecycle.lock().await.as_slice(),
            [
                "load:model",
                "unload:model",
                "load:replacement-model",
                "load:replacement-model",
            ]
        );
        server.abort();
    }

    #[tokio::test]
    async fn cancellation_during_model_load_releases_runtime_gate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let load_started = Arc::new(tokio::sync::Semaphore::new(0));
        let server_started = load_started.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let load_started = server_started.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let read = socket.read(&mut buffer).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let headers = String::from_utf8_lossy(&request);
                    let path = headers.split_whitespace().nth(1).unwrap_or_default();
                    if path == "/v1/health" {
                        let body = r#"{"status":"ok","all_models_loaded":[]}"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        socket.write_all(response.as_bytes()).await.unwrap();
                    } else if path == "/v1/load" {
                        load_started.add_permits(1);
                        // Keep the load request active until cancellation drops
                        // the HTTP future and closes the client connection.
                        let _ = socket.read(&mut buffer).await;
                    }
                });
            }
        });
        let connection =
            Arc::new(LemonadeConnection::external(&format!("http://{address}/v1")).unwrap());
        let runtime = Arc::new(LemonadeRuntime::from_connection(connection));
        let profile = LemonadeRuntimeProfile::new("model", false, ModelLoadOptions::default())
            .with_backend_profile("llamacpp", Some("cpu".into()), Some("cpu".into()));
        let cancellation = crate::queue::CancellationToken::new();
        let acquire = tokio::spawn({
            let runtime = runtime.clone();
            let profile = profile.clone();
            let cancellation = cancellation.clone();
            async move {
                runtime
                    .acquire_with_cancellation(&profile, &cancellation)
                    .await
            }
        });
        load_started.acquire().await.unwrap().forget();
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), acquire)
            .await
            .expect("model load did not react to cancellation")
            .unwrap();
        assert!(matches!(
            result,
            Err(crate::queue::InferenceError::Cancelled)
        ));
        assert!(
            runtime.gate.try_lock().is_ok(),
            "runtime gate remained held"
        );
        server.abort();
    }
}
