//! Lemonade discovery, setup management, capability activation, and model
//! selection for [`AppView`]. The root view continues to own lifecycle state
//! and every GPUI task; this module only makes that existing boundary visible.

use super::*;

#[derive(Clone)]
struct LemonadeMetadata {
    connection: Arc<LemonadeConnection>,
    embedded: Option<Arc<EmbeddedLemonade>>,
    catalog: LemonadeServerCatalog,
    downloads: Result<serde_json::Value, String>,
}

struct LemonadeActivation {
    queue: u_forge_core::queue::InferenceQueue,
    hq_queue: Option<u_forge_core::queue::InferenceQueue>,
    chat_provider: Option<LemonadeChatProvider>,
    llm_models: Vec<AvailableModel>,
    preferred_idx: usize,
    runtime: Arc<LemonadeRuntime>,
}

struct LemonadeChatActivation {
    provider: Option<LemonadeChatProvider>,
    models: Vec<AvailableModel>,
    preferred_idx: usize,
    runtime: Arc<LemonadeRuntime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LemonadeInitState {
    Offline,
    Discovering,
    CapabilitiesLoading,
    Ready,
    Degraded,
    Failed,
}

async fn discover_lemonade_metadata(
    existing_connection: Option<Arc<LemonadeConnection>>,
    existing_embedded: Option<Arc<EmbeddedLemonade>>,
    max_loaded_models: usize,
    startup: StartupTimeline,
) -> anyhow::Result<LemonadeMetadata> {
    let (connection, embedded) = {
        let _phase = startup.phase("lemonade_connection_resolve");
        match existing_connection {
            Some(connection) => (connection, existing_embedded),
            None => resolve_runtime_connection().await?,
        }
    };
    tracing::debug!(url = %connection.api_base(), "Lemonade server reachable");

    if connection.ownership() == LemonadeOwnership::Embedded {
        let changed = LemonadeManagement::new(connection.clone())
            .set_max_loaded_models(max_loaded_models, false)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "could not configure embedded Lemonade max_loaded_models={max_loaded_models}: {error}"
                )
            })?;
        tracing::debug!(
            max_loaded_models,
            changed,
            "Embedded Lemonade residency limit is configured"
        );
    }

    let catalog_connection = connection.clone();
    let downloads_connection = connection.clone();
    let catalog_timeline = startup.clone();
    let downloads_timeline = startup.clone();
    let (catalog, downloads) = tokio::join!(
        async move {
            let _phase = catalog_timeline.phase("lemonade_catalog_discovery");
            LemonadeServerCatalog::discover_with_connection(catalog_connection).await
        },
        async move {
            let _phase = downloads_timeline.phase("lemonade_downloads_query");
            LemonadeManagement::new(downloads_connection)
                .downloads()
                .await
                .map_err(|error| error.to_string())
        }
    );
    let catalog = catalog?;
    tracing::debug!(
        loaded = catalog.loaded.len(),
        models = catalog.models.len(),
        "Lemonade metadata fetched"
    );
    Ok(LemonadeMetadata {
        connection,
        embedded,
        catalog,
        downloads,
    })
}

async fn forward_management_events(
    mut receiver: u_forge_core::lemonade::ManagementProgressReceiver,
    events: &tokio::sync::broadcast::Sender<ManagementProgressEvent>,
) -> anyhow::Result<()> {
    let mut latest: Option<ManagementProgressEvent> = None;
    while let Some(event) = receiver.recv().await {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                if let Some(mut failed_event) = latest {
                    failed_event.kind = ManagementEventKind::Failed;
                    failed_event.message = Some(error.to_string());
                    let _ = events.send(failed_event);
                }
                return Err(error);
            }
        };
        let terminal = event.is_terminal();
        let failed = event.kind == u_forge_core::lemonade::ManagementEventKind::Failed;
        let message = event.message.clone();
        latest = Some(event.clone());
        let _ = events.send(event);
        if failed {
            anyhow::bail!(message.unwrap_or_else(|| "Lemonade management operation failed".into()));
        }
        if terminal {
            return Ok(());
        }
    }
    anyhow::bail!("Lemonade management stream closed without completion")
}

/// Ensure the small CPU retrieval baseline for the runtime owned by u-forge.
/// Optional accelerators and chat models remain explicit setup choices.
async fn provision_managed_baseline(
    connection: Arc<LemonadeConnection>,
    events: tokio::sync::broadcast::Sender<ManagementProgressEvent>,
    management_lock: Arc<tokio::sync::Mutex<()>>,
) -> anyhow::Result<bool> {
    if connection.ownership() != LemonadeOwnership::Embedded {
        return Ok(false);
    }
    let _guard = management_lock.lock().await;
    let catalog = LemonadeServerCatalog::discover_with_connection(connection.clone()).await?;
    let manager = LemonadeManagement::new(connection);
    let mut changed = false;
    let cpu = catalog
        .backends
        .iter()
        .find(|backend| backend.recipe == "llamacpp" && backend.backend == "cpu")
        .ok_or_else(|| {
            anyhow::anyhow!("Lemonade did not report an installable llama.cpp CPU backend")
        })?;
    if cpu.state != "installed" {
        let receiver = manager
            .install_backend_stream("llamacpp", "cpu", true)
            .await?;
        forward_management_events(receiver, &events).await?;
        changed = true;
    }

    for component in initial_setup_components()
        .into_iter()
        .filter(|component| component.role == SetupRole::StandardEmbedding)
    {
        let state = component_state(&catalog, &component);
        if let u_forge_core::lemonade::SetupComponentState::Conflict(message) = state {
            anyhow::bail!(message);
        }
        if state.needs_pull() {
            let pull = component.pull_spec();
            let receiver = manager
                .pull_stream(
                    pull.model_name,
                    pull.checkpoint,
                    pull.recipe,
                    pull.embedding,
                    true,
                )
                .await?;
            forward_management_events(receiver, &events).await?;
            changed = true;
        }
    }
    Ok(changed)
}

/// Build chat-facing catalog/profile state without loading a model. This is
/// deliberately synchronous so the Assistant chrome is usable as soon as
/// metadata arrives, while embedding and reranking providers continue loading.
fn prepare_lemonade_chat(
    connection: Arc<LemonadeConnection>,
    catalog: &LemonadeServerCatalog,
    app_config: &AppConfig,
) -> LemonadeChatActivation {
    let selector = ModelSelector::new(catalog, &app_config.models, &app_config.embedding);
    let all_llm = selector.select_all_llm_models();
    let preferred_model_id = app_config.chat.active_device_config().model.clone();
    let (preferred_idx, selection_diagnostic) = select_preferred_llm_index(
        &all_llm,
        app_config.chat.preferred_device.clone(),
        preferred_model_id.as_deref(),
    );
    let models = all_llm
        .iter()
        .enumerate()
        .map(|(index, selected)| {
            let device_config = chat_device_config_for_model(&app_config.chat, selected).clone();
            let configured_generation = device_config
                .max_tokens
                .map(|value| value as usize)
                .unwrap_or(app_config.chat.response_reserve);
            let mut effective_limits = selected
                .reconcile_chat_limits(configured_generation, configured_generation)
                .map_err(|error| tracing::warn!(%error, "chat context is unusable"))
                .ok();
            let context = effective_limits
                .as_ref()
                .map_or(usize::MAX, |limits| limits.context);
            let (agent_budget, invalid_agent_budget) = match app_config
                .chat
                .agent
                .reconcile(context, app_config.chat.max_tool_turns)
            {
                Ok(budget) => (budget, None),
                Err(error) => {
                    tracing::warn!(%error, "agent budget configuration is unusable");
                    let fallback = u_forge_core::AgentBudgetConfig::default()
                        .reconcile(context, app_config.chat.max_tool_turns)
                        .expect("safe fallback agent budget reconciles");
                    (
                        fallback,
                        Some(format!(
                            "invalid agent budget configuration ({error}); safe defaults applied"
                        )),
                    )
                }
            };
            if let Some(limits) = &mut effective_limits {
                limits.diagnostics.extend(agent_budget.diagnostics.clone());
                limits.diagnostics.extend(invalid_agent_budget);
            }
            if index == preferred_idx
                && let (Some(limits), Some(diagnostic)) =
                    (&mut effective_limits, &selection_diagnostic)
            {
                limits.diagnostics.push(diagnostic.clone());
            }
            AvailableModel::from(selected).with_chat_profile(
                device_config,
                effective_limits,
                app_config.chat.max_tool_turns,
                agent_budget,
            )
        })
        .collect::<Vec<_>>();
    let gpu_manager = GpuResourceManager::new();
    let provider = all_llm.get(preferred_idx).map(|selected| {
        let gpu = (selected_model_device(selected) == "gpu").then(|| Arc::clone(&gpu_manager));
        LemonadeChatProvider::from_connection(connection.clone(), &selected.model_id, gpu)
    });
    LemonadeChatActivation {
        provider,
        models,
        preferred_idx,
        runtime: Arc::new(LemonadeRuntime::from_connection(connection)),
    }
}

async fn activate_lemonade_capabilities(
    connection: Arc<LemonadeConnection>,
    catalog: LemonadeServerCatalog,
    app_config: Arc<AppConfig>,
    startup: StartupTimeline,
) -> anyhow::Result<LemonadeActivation> {
    let selector = {
        let _phase = startup.phase("lemonade_model_selection");
        ModelSelector::new(&catalog, &app_config.models, &app_config.embedding)
    };
    let embed_models = selector.select_embedding_models();
    let reranker_sel = selector.select_reranker(app_config.reranking.llamacpp_device);
    let already_loaded: Vec<String> = catalog
        .loaded
        .iter()
        .map(|model| model.model_name.clone())
        .collect();

    let mut build_specs = Vec::new();
    // HQ is an additive retrieval lane, never a replacement for the standard
    // lane. Even a one-slot server must attempt the standard provider first;
    // HQ may then degrade away if the server cannot host both models.
    for selected in standard_embedding_models(&embed_models) {
        let weight = match selected.recipe.as_str() {
            "flm" => app_config.embedding.npu_weight,
            "llamacpp" => match selected.backend.as_deref() {
                Some("cuda" | "rocm" | "vulkan" | "metal") => app_config.embedding.gpu_weight,
                _ => app_config.embedding.cpu_weight,
            },
            _ => app_config.embedding.cpu_weight,
        };
        build_specs.push((selected.clone(), Capability::Embedding, weight));
    }
    if let Some(selected) = reranker_sel {
        build_specs.push((selected, Capability::Reranking, 100));
    }

    let gpu_manager = GpuResourceManager::new();
    let mut providers = Vec::new();
    // Lemonade's residency limit is per model type and may be one. Build every
    // standard embedding provider in selection order before HQ construction so
    // provider probes never race each other for that single embedding slot.
    for (selected, capability, weight) in build_specs {
        let result = {
            let _phase = startup.phase(format!(
                "provider_build.{capability:?}.{}",
                selected.model_id
            ));
            ProviderFactory::build_with_connection(
                &selected,
                capability,
                connection.clone(),
                weight,
                Some(gpu_manager.clone()),
                &already_loaded,
            )
            .await
        };
        match result {
            Ok(provider) => providers.push(provider),
            Err(error) => {
                tracing::warn!(%error, "Lemonade capability provider unavailable");
            }
        }
    }

    let queue = {
        let _phase = startup.phase("standard_inference_queue_build");
        InferenceQueueBuilder::new()
            .with_providers(providers)
            .with_config((*app_config).clone())
            .build()
    };
    tracing::debug!(
        embedding_workers = queue.embedding_worker_count(),
        "Standard inference queue ready"
    );

    let hq_queue = if queue.has_embedding() {
        let _phase = startup.phase("hq_inference_queue_build");
        build_hq_embed_queue_with_connection(&catalog, &app_config, connection.clone()).await
    } else {
        tracing::warn!(
            "HQ embedding lane skipped because no standard embedding provider is available"
        );
        None
    };

    let all_llm = selector.select_all_llm_models();
    let preferred_model_id = app_config.chat.active_device_config().model.clone();
    let (preferred_idx, selection_diagnostic) = select_preferred_llm_index(
        &all_llm,
        app_config.chat.preferred_device.clone(),
        preferred_model_id.as_deref(),
    );
    let llm_models = all_llm
        .iter()
        .enumerate()
        .map(|(index, selected)| {
            let device_config = chat_device_config_for_model(&app_config.chat, selected).clone();
            let configured_generation = device_config
                .max_tokens
                .map(|value| value as usize)
                .unwrap_or(app_config.chat.response_reserve);
            let mut effective_limits = selected
                .reconcile_chat_limits(configured_generation, configured_generation)
                .map_err(|error| tracing::warn!(%error, "chat context is unusable"))
                .ok();
            let context = effective_limits
                .as_ref()
                .map_or(usize::MAX, |limits| limits.context);
            let (agent_budget, invalid_agent_budget) = match app_config
                .chat
                .agent
                .reconcile(context, app_config.chat.max_tool_turns)
            {
                Ok(budget) => (budget, None),
                Err(error) => {
                    tracing::warn!(%error, "agent budget configuration is unusable");
                    let fallback = u_forge_core::AgentBudgetConfig::default()
                        .reconcile(context, app_config.chat.max_tool_turns)
                        .expect("safe fallback agent budget reconciles");
                    (
                        fallback,
                        Some(format!(
                            "invalid agent budget configuration ({error}); safe defaults applied"
                        )),
                    )
                }
            };
            if let Some(limits) = &mut effective_limits {
                limits.diagnostics.extend(agent_budget.diagnostics.clone());
                limits.diagnostics.extend(invalid_agent_budget);
            }
            if index == preferred_idx
                && let (Some(limits), Some(diagnostic)) =
                    (&mut effective_limits, &selection_diagnostic)
            {
                limits.diagnostics.push(diagnostic.clone());
            }
            AvailableModel::from(selected).with_chat_profile(
                device_config,
                effective_limits,
                app_config.chat.max_tool_turns,
                agent_budget,
            )
        })
        .collect::<Vec<_>>();

    let chat_provider = all_llm.get(preferred_idx).map(|selected| {
        let gpu = (selected_model_device(selected) == "gpu").then(|| Arc::clone(&gpu_manager));
        LemonadeChatProvider::from_connection(connection.clone(), &selected.model_id, gpu)
    });
    tracing::debug!(
        llm_count = all_llm.len(),
        preferred_idx,
        "Lemonade capability activation complete"
    );
    Ok(LemonadeActivation {
        queue,
        hq_queue,
        chat_provider,
        llm_models,
        preferred_idx,
        runtime: Arc::new(LemonadeRuntime::from_connection(connection)),
    })
}

fn standard_embedding_models(
    models: &[u_forge_core::lemonade::SelectedModel],
) -> impl Iterator<Item = &u_forge_core::lemonade::SelectedModel> {
    models
        .iter()
        .filter(|selected| selected.quality_tier == QualityTier::Standard)
}

impl AppView {
    /// Asynchronously discover Lemonade Server and build the InferenceQueue + ChatProvider.
    /// FTS5 search works immediately even if this fails.
    pub(crate) fn do_refresh_lemonade_setup(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.state.lemonade_connection.clone() else {
            self.setup_panel.update(cx, |panel, cx| {
                panel.set_busy(
                    false,
                    "Lemonade is not connected; retry the connection first.",
                );
                cx.notify();
            });
            return;
        };
        let tokio_rt = self.state.tokio_rt.clone();
        self.setup_panel.update(cx, |panel, cx| {
            panel.set_busy(true, "Refreshing catalog and durable downloads…");
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        let catalog = LemonadeServerCatalog::discover_with_connection(
                            connection.clone(),
                        )
                        .await?;
                        let downloads = LemonadeManagement::new(connection).downloads().await;
                        Ok::<_, anyhow::Error>((catalog, downloads))
                    })
                })
                .await;
            this.update(cx, |view, cx| match result {
                Ok((catalog, downloads)) => {
                    view.state.lemonade_catalog = Some(catalog.clone());
                    let mut complete = false;
                    view.setup_panel.update(cx, |panel, cx| {
                        panel.refresh_catalog(&catalog);
                        match downloads {
                            Ok(value) => panel.set_downloads(&value),
                            Err(error) => panel.set_busy(
                                false,
                                format!(
                                    "Catalog refreshed, but durable downloads are unavailable: {error}"
                                ),
                            ),
                        }
                        complete = panel.is_complete();
                        if complete {
                            panel.set_busy(false, "Setup is complete. AI providers are ready.");
                        } else {
                            panel.set_busy(false, "Setup still has components to provision.");
                        }
                        cx.notify();
                    });
                    if complete {
                        view.do_init_lemonade(cx);
                    }
                }
                Err(error) => view.setup_panel.update(cx, |panel, cx| {
                    panel.set_busy(false, format!("Setup refresh failed: {error}"));
                    cx.notify();
                }),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_install_lemonade_backend(
        &mut self,
        request: SetupBackendInstallRequested,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.state.lemonade_connection.clone() else {
            self.setup_panel.update(cx, |panel, cx| {
                panel.set_busy(
                    false,
                    "Lemonade is not connected; retry the connection first.",
                );
                cx.notify();
            });
            return;
        };
        let tokio_rt = self.state.tokio_rt.clone();
        let events = self.state.management_events.clone();
        let label = format!("{}:{}", request.recipe, request.backend);
        self.setup_panel.update(cx, |panel, cx| {
            panel.set_busy(true, format!("Installing backend {label}…"));
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        let manager = LemonadeManagement::new(connection.clone());
                        let receiver = manager
                            .install_backend_stream(
                                &request.recipe,
                                &request.backend,
                                request.confirmed_external,
                            )
                            .await?;
                        forward_management_events(receiver, &events).await?;
                        let catalog = LemonadeServerCatalog::discover_with_connection(
                            connection.clone(),
                        )
                        .await?;
                        let downloads = manager.downloads().await;
                        Ok::<_, anyhow::Error>((catalog, downloads))
                    })
                })
                .await;
            this.update(cx, |view, cx| {
                view.setup_panel.update(cx, |panel, cx| {
                    match result {
                        Ok((catalog, downloads)) => {
                            view.state.lemonade_catalog = Some(catalog.clone());
                            panel.refresh_catalog(&catalog);
                            if let Ok(downloads) = downloads {
                                panel.set_downloads(&downloads);
                            }
                            panel.set_busy(
                                false,
                                format!(
                                    "Backend {label} installation completed. Review its refreshed state below."
                                ),
                            );
                        }
                        Err(error) => panel.set_busy(
                            false,
                            format!("Backend {label} installation failed: {error}"),
                        ),
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_control_lemonade_download(
        &mut self,
        request: SetupDownloadRequested,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.state.lemonade_connection.clone() else {
            return;
        };
        let tokio_rt = self.state.tokio_rt.clone();
        let events = self.state.management_events.clone();
        self.setup_panel.update(cx, |panel, cx| {
            panel.set_busy(
                true,
                format!("Applying {:?} to download…", request.operation),
            );
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async move {
                        let manager = LemonadeManagement::new(connection);
                        match request.operation {
                            SetupDownloadOperation::Control(action) => {
                                manager
                                    .control_download(
                                        &request.job_id,
                                        action,
                                        request.confirmed_external,
                                    )
                                    .await?;
                            }
                            SetupDownloadOperation::Retry => {
                                // Current Lemonade exposes pause/cancel/remove
                                // controls. Retry/resume is a stopped-job remove
                                // followed by the same durable pull; partial files
                                // are reused by the server downloader.
                                manager
                                    .control_download(
                                        &request.job_id,
                                        u_forge_core::lemonade::DownloadAction::Remove,
                                        request.confirmed_external,
                                    )
                                    .await?;
                                let component =
                                    initial_setup_components().into_iter().find(|component| {
                                        component.matches_model_id(&request.model_name)
                                    });
                                let (model_name, checkpoint, recipe, embedding) = component
                                    .as_ref()
                                    .map(|component| {
                                        let pull = component.pull_spec();
                                        (
                                            pull.model_name,
                                            pull.checkpoint,
                                            pull.recipe,
                                            pull.embedding,
                                        )
                                    })
                                    .unwrap_or((request.model_name.as_str(), None, None, None));
                                let receiver = manager
                                    .pull_stream(
                                        model_name,
                                        checkpoint,
                                        recipe,
                                        embedding,
                                        request.confirmed_external,
                                    )
                                    .await?;
                                forward_management_events(receiver, &events).await?;
                            }
                        }
                        manager.downloads().await
                    })
                })
                .await;
            this.update(cx, |view, cx| {
                view.setup_panel.update(cx, |panel, cx| {
                    match result {
                        Ok(downloads) => {
                            panel.set_downloads(&downloads);
                            panel.set_busy(false, "Download action accepted by Lemonade.");
                        }
                        Err(error) => {
                            panel.set_busy(false, format!("Download action failed: {error}"));
                        }
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_provision_lemonade(
        &mut self,
        request: SetupRequested,
        cx: &mut Context<Self>,
    ) {
        let (Some(connection), Some(_catalog)) = (
            self.state.lemonade_connection.clone(),
            self.state.lemonade_catalog.clone(),
        ) else {
            self.setup_panel.update(cx, |panel, cx| {
                panel.set_busy(
                    false,
                    "A live Lemonade catalog is required before provisioning.",
                );
                cx.notify();
            });
            return;
        };

        let continue_to_world_setup = !self.state.schema_loaded;
        let mut next_config = (*self.state.app_config).clone();
        if let Err(error) = next_config.persist_lemonade_setup(
            request.high_quality_embedding,
            request.preferred_device.clone(),
            &request.chat_model,
            request.reasoning_control,
        ) {
            self.setup_panel.update(cx, |panel, cx| {
                panel.set_busy(false, format!("Could not save setup choices: {error}"));
                cx.notify();
            });
            return;
        }
        next_config.embedding.high_quality_embedding = request.high_quality_embedding;
        next_config.chat.preferred_device = request.preferred_device.clone();
        next_config.chat.reasoning_control = request.reasoning_control;
        match request.preferred_device {
            u_forge_core::ChatDevice::Auto | u_forge_core::ChatDevice::Gpu => {
                next_config.chat.gpu.model = Some(request.chat_model.clone())
            }
            u_forge_core::ChatDevice::Npu => {
                next_config.chat.npu.model = Some(request.chat_model.clone())
            }
            u_forge_core::ChatDevice::Cpu => {
                next_config.chat.cpu.model = Some(request.chat_model.clone())
            }
        }
        self.state.app_config = Arc::new(next_config.clone());

        let tokio_rt = self.state.tokio_rt.clone();
        let events = self.state.management_events.clone();
        let management_lock = self.state.management_lock.clone();
        let (embedding_ready_tx, embedding_ready_rx) = tokio::sync::oneshot::channel();
        self.setup_panel.update(cx, |panel, cx| {
            panel.set_busy(true, "Starting server-owned provisioning jobs…");
            cx.notify();
        });
        self.setup_open = false;
        if continue_to_world_setup {
            self.open_world_setup(cx);
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(provision_lemonade(
                        connection,
                        next_config,
                        request,
                        events,
                        management_lock,
                        embedding_ready_tx,
                    ))
                })
                .await;
            this.update(cx, |view, cx| {
                let succeeded = result.is_ok();
                view.setup_panel.update(cx, |panel, cx| {
                    match &result {
                        Ok((catalog, downloads, message)) => {
                            view.state.lemonade_catalog = Some(catalog.clone());
                            panel.refresh_catalog(catalog);
                            panel.set_downloads(downloads);
                            panel.set_busy(false, message.clone());
                        }
                        Err(error) => {
                            panel.set_busy(false, format!("Provisioning failed: {error}"));
                        }
                    }
                    cx.notify();
                });
                if succeeded {
                    view.state.management_status = Some("Provisioning complete".to_string());
                    view.reconfigure_lemonade(cx);
                } else if let Err(error) = result {
                    view.state.management_status = Some(format!("Provisioning failed: {error}"));
                }
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            if embedding_ready_rx.await.is_err() {
                return;
            }
            this.update(cx, |view, cx| {
                view.state.management_status =
                    Some("Embedding prerequisites downloaded; activating…".to_string());
                view.activate_world_embedding_prerequisites(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn do_init_lemonade(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.lemonade_init_state,
            LemonadeInitState::Discovering | LemonadeInitState::CapabilitiesLoading
        ) {
            tracing::debug!("Lemonade initialization is already in flight");
            return;
        }
        self.lemonade_init_generation = self.lemonade_init_generation.wrapping_add(1);
        let generation = self.lemonade_init_generation;
        self.lemonade_init_state = LemonadeInitState::Discovering;
        self.chat_panel.update(cx, |panel, _cx| {
            panel.set_connecting(true);
        });
        cx.notify();

        let app_config = self.state.app_config.clone();
        let max_loaded_models = app_config.lemonade.max_loaded_models;
        let tokio_rt = self.state.tokio_rt.clone();
        let existing_connection = self.state.lemonade_connection.clone();
        let existing_embedded = self.state.embedded_lemonade.clone();
        let startup = self.startup.clone();

        cx.spawn(async move |this, cx| {
            let metadata_timeline = startup.clone();
            let metadata_runtime = tokio_rt.clone();
            let metadata_result = cx
                .background_executor()
                .spawn(
                    async move {
                        metadata_runtime.block_on(discover_lemonade_metadata(
                            existing_connection,
                            existing_embedded,
                            max_loaded_models,
                            metadata_timeline,
                        ))
                    }
                    .instrument(tracing::info_span!("lemonade_metadata_init")),
                )
                .await;
            let metadata = match metadata_result {
                Ok(metadata) => metadata,
                Err(error) => {
                    this.update(cx, |view: &mut AppView, cx| {
                        if view.lemonade_init_generation != generation {
                            return;
                        }
                        eprintln!("Lemonade init skipped: {error}");
                        view.lemonade_init_state = LemonadeInitState::Failed;
                        view.chat_panel.update(cx, |panel, _cx| {
                            panel.set_connect_failed(&error.to_string());
                        });
                        if !view.state.schema_loaded {
                            view.open_world_setup(cx);
                        }
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let mut effective_config = (*app_config).clone();
            if effective_config.initialize_hardware_profile(&metadata.catalog) {
                if let Err(error) = app_config.persist_settings(&effective_config) {
                    tracing::warn!(%error, "Detected hardware profile could not be persisted");
                } else {
                    tracing::info!(
                        npu = effective_config.embedding.standard.npu_enabled,
                        hq_device = ?effective_config.embedding.high_quality.llamacpp_device,
                        gpu_runtime = ?effective_config.models.default_gpu_runtime,
                        "Initialized hardware-aware defaults"
                    );
                }
            }
            let effective_config = Arc::new(effective_config);
            let metadata_for_ui = metadata.clone();
            let config_for_ui = effective_config.clone();
            if this
                .update(cx, move |view: &mut AppView, cx| {
                    if view.lemonade_init_generation != generation {
                        return;
                    }
                    view.state.app_config = config_for_ui.clone();
                    view.search_panel
                        .update(cx, |panel, _cx| panel.set_app_config(config_for_ui.clone()));
                    view.apply_lemonade_metadata(metadata_for_ui, cx);
                    view.lemonade_init_state = LemonadeInitState::CapabilitiesLoading;
                })
                .is_err()
            {
                return;
            }
            if startup.should_exit_after(StartupMilestone::LemonadeMetadataReady) {
                return;
            }

            let activation_runtime = tokio_rt;
            let activation_config = effective_config;
            let activation_timeline = startup.clone();
            let activation_connection = metadata.connection.clone();
            let activation_catalog = metadata.catalog.clone();
            let activation_result = cx
                .background_executor()
                .spawn(
                    async move {
                        activation_runtime.block_on(activate_lemonade_capabilities(
                            activation_connection,
                            activation_catalog,
                            activation_config,
                            activation_timeline,
                        ))
                    }
                    .instrument(tracing::info_span!("lemonade_capability_activation")),
                )
                .await;

            this.update(cx, move |view: &mut AppView, cx| {
                if view.lemonade_init_generation != generation {
                    return;
                }
                match activation_result {
                    Ok(activation) => {
                        view.apply_lemonade_activation(activation, cx);
                        view.lemonade_init_state = LemonadeInitState::Ready;
                    }
                    Err(error) => {
                        eprintln!("Lemonade capability activation failed: {error}");
                        view.lemonade_init_state = LemonadeInitState::Degraded;
                        view.state.embedding_status = Some(format!(
                            "Assistant connected; retrieval AI is unavailable: {error}"
                        ));
                        view.chat_panel.update(cx, |panel, cx| {
                            panel.finish_capability_initialization(cx);
                        });
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Supersede any activation based on old settings and immediately discover
    /// capabilities again with the newly persisted configuration.
    pub(super) fn reconfigure_lemonade(&mut self, cx: &mut Context<Self>) {
        self.lemonade_init_generation = self.lemonade_init_generation.wrapping_add(1);
        self.lemonade_init_state = LemonadeInitState::Offline;
        self.do_init_lemonade(cx);
    }

    /// Activate newly downloaded embedding models without reopening setup while
    /// lower-priority chat and reranking provisioning continues.
    fn activate_world_embedding_prerequisites(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.state.lemonade_connection.clone() else {
            return;
        };
        let app_config = self.state.app_config.clone();
        let tokio_rt = self.state.tokio_rt.clone();
        let startup = self.startup.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(async {
                        let catalog =
                            LemonadeServerCatalog::discover_with_connection(connection.clone())
                                .await?;
                        activate_lemonade_capabilities(connection, catalog, app_config, startup)
                            .await
                    })
                })
                .await;
            this.update(cx, |view, cx| match result {
                Ok(activation) => view.apply_lemonade_activation(activation, cx),
                Err(error) => {
                    view.state.management_status =
                        Some(format!("Embedding activation failed: {error}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn start_managed_baseline_provisioning(
        &mut self,
        connection: Arc<LemonadeConnection>,
        cx: &mut Context<Self>,
    ) {
        if connection.ownership() != LemonadeOwnership::Embedded {
            return;
        }
        let tokio_rt = self.state.tokio_rt.clone();
        let events = self.state.management_events.clone();
        let management_lock = self.state.management_lock.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    tokio_rt.block_on(provision_managed_baseline(
                        connection,
                        events,
                        management_lock,
                    ))
                })
                .await;
            this.update(cx, |view: &mut AppView, cx| match result {
                Ok(true) => {
                    view.state.management_status =
                        Some("Standard embedding model downloaded".to_string());
                    cx.notify();
                }
                Ok(false) => {}
                Err(error) => {
                    view.state.embedding_status = Some(format!(
                        "Default retrieval model provisioning failed: {error}"
                    ));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_lemonade_metadata(&mut self, metadata: LemonadeMetadata, cx: &mut Context<Self>) {
        let _phase = self.startup.phase("lemonade_metadata_ui_apply");
        self.state.embedded_lemonade = metadata.embedded;
        if self._lemonade_signal_task.is_none()
            && let Some(embedded) = self.state.embedded_lemonade.clone()
        {
            self._lemonade_signal_task = Some(self.state.tokio_rt.spawn(async move {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => {
                        tracing::info!("Ctrl-C received; shutting down embedded Lemonade");
                        embedded.shutdown().await;
                        std::process::exit(130);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Could not install the Ctrl-C shutdown handler");
                    }
                }
            }));
        }
        self.state.lemonade_connection = Some(metadata.connection.clone());
        self.state.lemonade_catalog = Some(metadata.catalog.clone());
        let chat = prepare_lemonade_chat(
            metadata.connection.clone(),
            &metadata.catalog,
            &self.state.app_config,
        );
        if let Some(provider) = chat.provider {
            self.chat_panel.update(cx, |panel, _cx| {
                panel.set_provider(
                    provider,
                    chat.models,
                    chat.preferred_idx,
                    chat.runtime,
                    self.state.app_config.chat.reasoning_control,
                );
                panel.begin_capability_initialization();
            });
        } else {
            self.chat_panel.update(cx, |panel, _cx| {
                panel.set_connect_failed("No downloaded LLM models available");
            });
        }
        let setup_hq_default = if self
            .state
            .app_config
            .source_path
            .as_ref()
            .is_some_and(|path| path.exists())
        {
            self.state.app_config.embedding.high_quality_embedding
        } else {
            true
        };
        let mut setup = SetupPanel::new(
            metadata.connection.ownership(),
            &metadata.catalog,
            self.state
                .app_config
                .chat
                .active_device_config()
                .model
                .as_deref(),
            setup_hq_default,
            self.state.app_config.embedding.standard.npu_enabled,
            self.state.app_config.chat.preferred_device.clone(),
            self.state.app_config.chat.reasoning_control,
        )
        .with_schema_loaded(self.state.schema_loaded)
        .with_startup_timeline(self.startup.clone());
        match metadata.downloads {
            Ok(downloads) => {
                setup.set_downloads(&downloads);
                let degraded = [
                    metadata.catalog.diagnostics.health.as_deref(),
                    metadata.catalog.diagnostics.system_info.as_deref(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                if !degraded.is_empty() {
                    setup.set_busy(
                        false,
                        format!("Discovery is degraded: {}", degraded.join("; ")),
                    );
                }
            }
            Err(error) => setup.set_busy(
                false,
                format!("Inference is available, but managed downloads are unavailable: {error}"),
            ),
        }
        let setup_incomplete = !setup.is_complete();
        self.setup_panel.update(cx, |panel, cx| {
            *panel = setup;
            cx.notify();
        });
        self.setup_open |= setup_incomplete;
        if !setup_incomplete && !self.state.schema_loaded {
            self.open_world_setup(cx);
        }
        self.start_managed_baseline_provisioning(metadata.connection.clone(), cx);
        eprintln!("{LEMONADE_METADATA_READY_MESSAGE}");
        self.startup
            .milestone(StartupMilestone::LemonadeMetadataReady);
        cx.notify();
        if self
            .startup
            .should_exit_after(StartupMilestone::LemonadeMetadataReady)
        {
            cx.quit();
        }
    }

    fn apply_lemonade_activation(
        &mut self,
        activation: LemonadeActivation,
        cx: &mut Context<Self>,
    ) {
        let _phase = self.startup.phase("lemonade_activation_ui_apply");
        let LemonadeActivation {
            queue,
            hq_queue,
            chat_provider,
            llm_models,
            preferred_idx,
            runtime,
        } = activation;
        let has_embedding =
            queue.has_embedding() || hq_queue.as_ref().is_some_and(|queue| queue.has_embedding());
        let has_chat = chat_provider.is_some();
        self.state.inference_queue = Some(queue.clone());
        self.state.hq_queue = hq_queue.clone();
        let hq_arc = hq_queue.clone().map(Arc::new);
        self.search_panel.update(cx, |panel, _cx| {
            panel.set_queues(Some(queue.clone()), hq_queue);
        });

        let agent_gpu = chat_provider
            .as_ref()
            .and_then(|provider| provider.gpu.clone());

        if has_chat {
            match GraphAgent::new_with_connection_and_gpu(
                runtime.connection().clone(),
                self.state.graph.clone(),
                Arc::new(queue),
                hq_arc,
                self.state.app_config.chat.system_prompt.clone(),
                agent_gpu,
            ) {
                Ok(agent) => self
                    .chat_panel
                    .update(cx, |panel, _cx| panel.set_agent(agent)),
                Err(error) => eprintln!("GraphAgent init failed: {error}"),
            }
        }

        if let Some(provider) = chat_provider {
            self.chat_panel.update(cx, |panel, _cx| {
                panel.set_provider(
                    provider,
                    llm_models,
                    preferred_idx,
                    runtime,
                    self.state.app_config.chat.reasoning_control,
                );
            });
        } else {
            self.chat_panel.update(cx, |panel, _cx| {
                panel.set_connect_failed("No downloaded LLM models available");
            });
        }
        self.chat_panel.update(cx, |panel, cx| {
            panel.finish_capability_initialization(cx);
        });
        if has_embedding {
            self.run_embedding_plan(EmbeddingPlan::embed_all(), cx);
        }
        self.sync_world_setup_readiness(cx);
        self.startup.milestone(StartupMilestone::StartupReady);
        cx.notify();
        if self
            .startup
            .should_exit_after(StartupMilestone::StartupReady)
        {
            cx.quit();
        }
    }
}

fn select_preferred_llm_index(
    models: &[u_forge_core::lemonade::SelectedModel],
    preferred_device: u_forge_core::ChatDevice,
    explicit_model: Option<&str>,
) -> (usize, Option<String>) {
    if let Some(explicit_model) = explicit_model
        && let Some(index) = models
            .iter()
            .position(|model| model.model_id == explicit_model)
    {
        return (index, None);
    }

    let requested = match preferred_device {
        u_forge_core::ChatDevice::Auto => None,
        u_forge_core::ChatDevice::Gpu => Some("gpu"),
        u_forge_core::ChatDevice::Npu => Some("npu"),
        u_forge_core::ChatDevice::Cpu => Some("cpu"),
    };
    let index = requested
        .and_then(|device| {
            models
                .iter()
                .position(|model| selected_model_device(model) == device)
        })
        .or_else(|| {
            ["gpu", "npu", "cpu"].into_iter().find_map(|device| {
                models
                    .iter()
                    .position(|model| selected_model_device(model) == device)
            })
        })
        .unwrap_or(0);
    let selected_device = models.get(index).map(selected_model_device);
    let diagnostic = if let Some(explicit) = explicit_model {
        Some(format!(
            "configured model {explicit} is unavailable; rebuilt the complete profile for {}",
            selected_device.unwrap_or("the available device")
        ))
    } else if let (Some(requested), Some(selected)) = (requested, selected_device) {
        (requested != selected).then(|| {
            format!(
                "preferred device {requested} is unavailable; rebuilt the complete profile for {selected}"
            )
        })
    } else {
        None
    };
    (index, diagnostic)
}

fn selected_model_device(model: &u_forge_core::lemonade::SelectedModel) -> &'static str {
    match model.recipe.as_str() {
        "flm" => "npu",
        "llamacpp"
            if u_forge_core::lemonade::selector::is_gpu_backend(model.backend.as_deref()) =>
        {
            "gpu"
        }
        _ => "cpu",
    }
}

fn chat_device_config_for_model<'a>(
    chat: &'a u_forge_core::ChatConfig,
    model: &u_forge_core::lemonade::SelectedModel,
) -> &'a u_forge_core::ChatDeviceConfig {
    match selected_model_device(model) {
        "gpu" => &chat.gpu,
        "npu" => &chat.npu,
        _ => &chat.cpu,
    }
}

async fn provision_lemonade(
    connection: Arc<u_forge_core::lemonade::LemonadeConnection>,
    config: AppConfig,
    request: SetupRequested,
    events: tokio::sync::broadcast::Sender<ManagementProgressEvent>,
    management_lock: Arc<tokio::sync::Mutex<()>>,
    embedding_ready: tokio::sync::oneshot::Sender<()>,
) -> anyhow::Result<(LemonadeServerCatalog, serde_json::Value, String)> {
    let _guard = management_lock.lock().await;
    let catalog = LemonadeServerCatalog::discover_with_connection(connection.clone()).await?;
    let manager = LemonadeManagement::new(connection.clone());
    // Verify that the management plane is available before mutating an
    // external or embedded runtime. Each mutation below remains subscribed to
    // its SSE stream until a terminal event arrives.
    manager.downloads().await?;
    if let Some(error) = &catalog.diagnostics.system_info {
        anyhow::bail!(
            "backend discovery is unavailable, so managed setup cannot safely install a compatible backend: {error}"
        );
    }

    let mut installed = HashSet::new();
    let mut jobs_started = 0usize;
    let mut components = initial_setup_components()
        .into_iter()
        .filter(|component| match component.role {
            SetupRole::StandardEmbedding => {
                config.embedding.standard.llamacpp_device != u_forge_core::LlamaCppDevice::Disabled
            }
            SetupRole::NpuEmbedding => config.embedding.standard.npu_enabled,
            SetupRole::Reranking => {
                config.chat.rerank
                    && config.reranking.llamacpp_device != u_forge_core::LlamaCppDevice::Disabled
            }
            SetupRole::HighQualityEmbedding => {
                request.high_quality_embedding
                    && config.embedding.high_quality.llamacpp_device
                        != u_forge_core::LlamaCppDevice::Disabled
            }
            SetupRole::Chat => false,
        })
        .collect::<Vec<_>>();
    components.sort_by_key(|component| match component.role {
        SetupRole::StandardEmbedding => 0,
        SetupRole::HighQualityEmbedding => 1,
        SetupRole::NpuEmbedding => 2,
        SetupRole::Reranking => 3,
        SetupRole::Chat => 4,
    });
    let readiness_index = components
        .iter()
        .rposition(|component| {
            matches!(
                component.role,
                SetupRole::StandardEmbedding
                    | SetupRole::NpuEmbedding
                    | SetupRole::HighQualityEmbedding
            )
        })
        .ok_or_else(|| anyhow::anyhow!("standard embedding setup component is missing"))?;
    let mut embedding_ready = Some(embedding_ready);
    for (index, component) in components.iter().enumerate() {
        let state = component_state(&catalog, component);
        if let u_forge_core::lemonade::SetupComponentState::Conflict(message) = &state {
            anyhow::bail!(message.clone());
        }
        let recipe = component.recipe.or_else(|| {
            catalog
                .models
                .iter()
                .find(|model| component.matches_model_id(&model.id))
                .map(|model| model.recipe.as_str())
        });
        if let Some(recipe) = recipe.filter(|recipe| !recipe.is_empty()) {
            let lane_device = match component.role {
                SetupRole::StandardEmbedding => Some(config.embedding.standard.llamacpp_device),
                SetupRole::HighQualityEmbedding => {
                    Some(config.embedding.high_quality.llamacpp_device)
                }
                SetupRole::Reranking => Some(config.reranking.llamacpp_device),
                SetupRole::NpuEmbedding | SetupRole::Chat => None,
            };
            let preference = match lane_device {
                Some(u_forge_core::LlamaCppDevice::Cpu) => vec!["cpu".to_string()],
                Some(u_forge_core::LlamaCppDevice::Gpu) => {
                    let mut preference = config.models.gpu_backend_preference(&catalog);
                    preference.push("cpu".to_string());
                    preference
                }
                Some(u_forge_core::LlamaCppDevice::Disabled) => Vec::new(),
                None => Vec::new(),
            };
            let choice = select_setup_backend(&catalog, recipe, &preference).ok_or_else(|| {
                anyhow::anyhow!(
                    "no installed or installable {recipe} backend was reported for {}",
                    component.model_id
                )
            })?;
            if choice.needs_install()
                && installed.insert((choice.recipe.clone(), choice.backend.clone()))
            {
                let receiver = manager
                    .install_backend_stream(
                        &choice.recipe,
                        &choice.backend,
                        request.confirmed_external,
                    )
                    .await?;
                forward_management_events(receiver, &events).await?;
            }
        }
        if state.needs_pull() {
            let pull = component.pull_spec();
            let receiver = manager
                .pull_stream(
                    pull.model_name,
                    pull.checkpoint,
                    pull.recipe,
                    pull.embedding,
                    request.confirmed_external,
                )
                .await?;
            forward_management_events(receiver, &events).await?;
            jobs_started += 1;
        }
        if index == readiness_index
            && let Some(sender) = embedding_ready.take()
        {
            let _ = sender.send(());
        }
    }

    let chat_state = chat_component_state(&catalog, &request.chat_model);
    if let u_forge_core::lemonade::SetupComponentState::Conflict(message) = &chat_state {
        anyhow::bail!(message.clone());
    }
    let chat_model = catalog
        .models
        .iter()
        .find(|model| model.id == request.chat_model)
        .ok_or_else(|| anyhow::anyhow!("selected chat model is no longer in the live catalog"))?;
    let chat_preference = if chat_model.recipe != "llamacpp" {
        Vec::new()
    } else {
        match config.chat.preferred_device {
            u_forge_core::ChatDevice::Cpu => vec!["cpu".to_string()],
            u_forge_core::ChatDevice::Gpu | u_forge_core::ChatDevice::Auto => {
                let mut preference = config.models.gpu_backend_preference(&catalog);
                preference.push("cpu".to_string());
                preference
            }
            u_forge_core::ChatDevice::Npu => Vec::new(),
        }
    };
    let choice =
        select_setup_backend(&catalog, &chat_model.recipe, &chat_preference).ok_or_else(|| {
            anyhow::anyhow!(
                "no installed or installable {} backend was reported for {}",
                chat_model.recipe,
                request.chat_model
            )
        })?;
    if choice.needs_install() && installed.insert((choice.recipe.clone(), choice.backend.clone())) {
        let receiver = manager
            .install_backend_stream(&choice.recipe, &choice.backend, request.confirmed_external)
            .await?;
        forward_management_events(receiver, &events).await?;
    }
    if chat_state.needs_pull() {
        let receiver = manager
            .pull_stream(
                &request.chat_model,
                None,
                None,
                None,
                request.confirmed_external,
            )
            .await?;
        forward_management_events(receiver, &events).await?;
        jobs_started += 1;
    }

    let downloads = manager.downloads().await?;
    let refreshed = LemonadeServerCatalog::discover_with_connection(connection).await?;
    let message = if jobs_started == 0 {
        "Selections saved; all selected models are already downloaded.".to_string()
    } else {
        format!("Completed {jobs_started} model download(s). The setup catalog is current.")
    };
    Ok((refreshed, downloads, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(
        id: &str,
        recipe: &str,
        backend: Option<&str>,
    ) -> u_forge_core::lemonade::SelectedModel {
        u_forge_core::lemonade::SelectedModel {
            model_id: id.to_string(),
            recipe: recipe.to_string(),
            backend: backend.map(ToString::to_string),
            load_opts: u_forge_core::ModelLoadOptions::default(),
            quality_tier: u_forge_core::lemonade::QualityTier::NotApplicable,
            checkpoint: id.to_string(),
            max_context_window: None,
            tool_capable: true,
            reasoning_capable: false,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn preferred_device_selects_a_coherent_profile_and_reports_fallback() {
        let models = vec![
            selected("gpu", "llamacpp", Some("vulkan")),
            selected("npu", "flm", None),
            selected("cpu", "llamacpp", Some("cpu")),
        ];
        assert_eq!(
            select_preferred_llm_index(&models, u_forge_core::ChatDevice::Npu, None).0,
            1
        );
        let (index, diagnostic) =
            select_preferred_llm_index(&models[..1], u_forge_core::ChatDevice::Npu, None);
        assert_eq!(index, 0);
        assert!(diagnostic.unwrap().contains("rebuilt the complete profile"));
    }

    #[test]
    fn model_picker_profiles_use_their_own_device_sampling() {
        let mut chat = u_forge_core::ChatConfig::default();
        chat.gpu.temperature = Some(0.1);
        chat.npu.temperature = Some(0.2);
        chat.cpu.temperature = Some(0.3);
        assert_eq!(
            chat_device_config_for_model(&chat, &selected("gpu", "llamacpp", Some("vulkan")))
                .temperature,
            Some(0.1)
        );
        assert_eq!(
            chat_device_config_for_model(&chat, &selected("npu", "flm", None)).temperature,
            Some(0.2)
        );
        assert_eq!(
            chat_device_config_for_model(&chat, &selected("cpu", "llamacpp", Some("cpu")))
                .temperature,
            Some(0.3)
        );
    }

    #[test]
    fn hq_selection_never_suppresses_the_standard_embedding_provider() {
        let mut standard = selected("standard", "llamacpp", Some("vulkan"));
        standard.quality_tier = QualityTier::Standard;
        let mut hq = selected("hq", "llamacpp", Some("vulkan"));
        hq.quality_tier = QualityTier::High;

        let models = [hq, standard];
        let selected = standard_embedding_models(&models)
            .map(|model| model.model_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(selected, ["standard"]);
    }
}
