//! Opt-in, real-provider baseline. See docs/inference-fabric.md at the workspace root.

mod metrics;
mod search;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use futures::future::try_join_all;
use metrics::{Kind, MeterReading, Metered, Spread, Trace, ms, read_number};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::TempDir;
use u_forge_core::{
    EmbeddingProvider, KnowledgeGraph, ObjectBuilder,
    config::StorageConfig,
    embed_all_chunks,
    ingest::EmbeddingTarget,
    lemonade::{
        BuiltProvider, Capability, EmbeddedLemonade, LemonadeConnection, LemonadeHealth,
        LemonadeHttpClient, LemonadeManagement, LemonadeProvider, LemonadeServerCatalog,
        ModelLoadOptions, ProviderSlot, load_model_for_recipe_with_connection,
        unload_all_models_with_connection,
    },
    queue::InferenceQueueBuilder,
    types::ChunkType,
};

const CORPUS_VERSION: &str = "synthetic-archive-v1";
const QUERY: &str = "archive lantern map";
const BATCH: usize = 32; // Current production shape, verified against provider calls below.
type Connection = Arc<LemonadeConnection>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    runtime_version: String,
    hardware: String,
    fabric: String,
    power_profile: String,
    power_profile_path: Option<PathBuf>,
    ac_online_path: PathBuf,
    ac_online: bool,
    temperature_path: PathBuf,
    temperature_band_c: [f64; 2],
    cooldown_secs: u64,
    repetitions: usize,
    chunks: usize,
    interactive_period_ms: u64,
    max_loaded_models: usize,
    whole_system_joules_path: Option<PathBuf>,
    meter_max_age_ms: Option<u64>,
    embeddings: BTreeMap<String, Model>,
    #[serde(default)]
    concurrent: Vec<String>,
    search: Option<search::Config>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Model {
    id: String,
    checkpoint: String,
    // Operator-pinned immutable artifact/revision or digest, not a model family name.
    artifact: String,
    precision: String,
    recipe: String,
    backend: String,
    backend_version: String,
    ctx: usize,
    threads: Option<usize>,
    parallel: Option<usize>,
    device: Option<String>,
    #[serde(default = "default_weight")]
    weight: u32,
}

fn default_weight() -> u32 {
    10
}

impl Model {
    fn options(&self) -> ModelLoadOptions {
        let llama = self.recipe == "llamacpp";
        let args = llama.then(|| {
            format!(
                "--threads {} --threads-batch {} --parallel {} --batch-size {} --ubatch-size {}",
                self.threads.unwrap(),
                self.threads.unwrap(),
                self.parallel.unwrap(),
                self.ctx,
                self.ctx
            )
        });
        ModelLoadOptions {
            ctx_size: Some(self.ctx),
            llamacpp_backend: llama.then(|| self.backend.clone()),
            llamacpp_args: args,
            ..Default::default()
        }
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            !self.artifact.is_empty()
                && !self.precision.is_empty()
                && !self.checkpoint.is_empty()
                && !self.backend_version.is_empty()
                && self.ctx > 0,
            "model pins are required for {}",
            self.id
        );
        match self.recipe.as_str() {
            "llamacpp" => ensure!(
                self.threads.unwrap_or(0) > 0 && self.parallel.unwrap_or(0) > 0,
                "threads and parallel are required for {}",
                self.id
            ),
            "flm" => ensure!(
                self.backend == "npu"
                    && self.threads.is_none()
                    && self.parallel.is_none()
                    && self.device.is_none(),
                "FLM owns engine settings"
            ),
            _ => anyhow::bail!("unsupported benchmark recipe"),
        }
        ensure!(
            self.device.as_ref().is_none_or(|device| !device.is_empty()
                && device
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_:-".contains(c))),
            "device must be one engine device identifier"
        );
        Ok(())
    }

    fn compute_class(&self) -> &'static str {
        match self.backend.as_str() {
            "cpu" => "cpu",
            "npu" => "npu",
            _ => "gpu",
        }
    }

    fn availability(&self, catalog: &LemonadeServerCatalog, label: &str) -> Option<&'static str> {
        let model = catalog
            .models
            .iter()
            .find(|m| m.id == self.id && m.downloaded);
        let Some(model) = model else {
            return Some("model_not_cached");
        };
        if model.recipe != self.recipe
            || model.checkpoint != self.checkpoint
            || !model.labels.contains(label)
        {
            return Some("catalog_model_pin_mismatch");
        }
        if !catalog.backends.iter().any(|b| {
            b.recipe == self.recipe
                && b.backend == self.backend
                && b.state == "installed"
                && b.version.as_deref() == Some(&self.backend_version)
        }) {
            return Some("backend_missing_or_version_mismatch");
        }
        None
    }

    async fn load(&self, connection: &Connection) -> Result<f64> {
        let start = Instant::now();
        let options = self.options();
        if let Some(device) = &self.device {
            // Device is a recipe option on /load, not a custom llama-server arg.
            // Keep the experiment-specific per-device binding out of app wiring.
            let _: Value = LemonadeHttpClient::from_connection(connection.clone())
                .post_json_load(
                    "/load",
                    &json!({
                        "model_name": self.id,
                        "ctx_size": options.ctx_size,
                        "llamacpp_backend": options.llamacpp_backend,
                        "llamacpp_args": options.llamacpp_args,
                        "llamacpp_device": device,
                    }),
                )
                .await?;
        } else {
            load_model_for_recipe_with_connection(
                connection.clone(),
                &self.id,
                &self.recipe,
                &options,
                &[],
            )
            .await?;
        }
        Ok(ms(start, Instant::now()))
    }
}

impl Config {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.repetitions > 0
                && self.chunks >= BATCH
                && self.chunks.is_multiple_of(BATCH)
                && self.interactive_period_ms > 0
                && self.max_loaded_models > 0,
            "invalid repetitions, chunk count, probe cadence, or capacity"
        );
        ensure!(
            !self.embeddings.is_empty()
                && self.cooldown_secs <= 300
                && self.temperature_band_c.iter().all(|t| t.is_finite())
                && self.temperature_band_c[0] <= self.temperature_band_c[1],
            "invalid routes or thermal limits"
        );
        ensure!(
            self.concurrent.iter().collect::<BTreeSet<_>>().len() == self.concurrent.len()
                && self
                    .concurrent
                    .iter()
                    .all(|key| self.embeddings.contains_key(key)),
            "concurrent routes must be distinct configured keys"
        );
        ensure!(
            self.whole_system_joules_path.is_some() == self.meter_max_age_ms.is_some()
                && self.meter_max_age_ms != Some(0),
            "set both meter path and positive meter_max_age_ms, or neither"
        );
        for model in self.embeddings.values() {
            model.validate()?;
        }
        if let Some(search) = &self.search {
            search.validate(self)?;
        }
        Ok(())
    }

    fn check_power(&self) -> Result<()> {
        let ac = read_number(&self.ac_online_path).context("read AC online state")?;
        ensure!(
            ac == f64::from(u8::from(self.ac_online)),
            "AC state changed"
        );
        if let Some(path) = &self.power_profile_path {
            let observed = std::fs::read_to_string(path).context("read OS power profile")?;
            ensure!(
                observed.trim() == self.power_profile,
                "OS power profile changed"
            );
        }
        Ok(())
    }

    async fn condition(&self) -> Result<f64> {
        tokio::time::sleep(Duration::from_secs(self.cooldown_secs)).await;
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            self.check_power()?;
            let temp = read_number(&self.temperature_path)
                .context("read starting temperature in millidegrees Celsius")?
                / 1000.0;
            if (self.temperature_band_c[0]..=self.temperature_band_c[1]).contains(&temp) {
                return Ok(temp);
            }
            ensure!(
                Instant::now() < deadline,
                "start temperature outside configured band ({temp:.1} C)"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    fn energy(&self) -> Option<MeterReading> {
        MeterReading::read(
            self.whole_system_joules_path.as_deref()?,
            self.meter_max_age_ms?,
        )
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Single,
    Weighted,
    Overlap,
}

fn concurrent_skip(config: &Config, routes: &[&Model]) -> Option<&'static str> {
    let Some(first) = routes.first().filter(|_| routes.len() > 1) else {
        return Some("fewer_than_two_configured_targets");
    };
    if routes.iter().any(|m| {
        m.artifact != first.artifact
            || m.precision != first.precision
            || m.checkpoint != first.checkpoint
            || m.recipe != first.recipe
    }) {
        return Some("incompatible_package_or_space");
    }
    if routes.iter().map(|m| &m.id).collect::<BTreeSet<_>>().len() != routes.len() {
        return Some("one_model_id_cannot_bind_two_backends");
    }
    let mut placements = BTreeSet::new();
    if routes.iter().any(|m| {
        (m.backend != "cpu" && m.device.is_none()) || !placements.insert((&m.backend, &m.device))
    }) {
        return Some("distinct_compute_targets_not_bound");
    }
    if routes.len() > config.max_loaded_models || routes.len() > config.chunks {
        return Some("insufficient_residency_or_work");
    }
    None
}

fn emit(record: Value) {
    println!("{record}");
}

struct ResidentModel {
    device: String,
    listener: Option<(u16, u64)>,
}

fn tcp_listeners(table: &str) -> impl Iterator<Item = (u16, u64)> + '_ {
    table.lines().skip(1).filter_map(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.get(3) != Some(&"0A") {
            return None;
        } // TCP_LISTEN
        let (_, port) = fields.get(1)?.rsplit_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let inode = fields.get(9)?.parse().ok()?;
        (inode != 0).then_some((port, inode))
    })
}

async fn snapshot(connection: &Connection, models: &[&Model]) -> Result<Vec<ResidentModel>> {
    let mut health = LemonadeHealth::fetch_with_connection(connection.clone()).await?;
    // Lemonade can pick the same port after restarting a worker; compare Linux
    // listener identities, not URL strings, outside the timed intervals.
    let tables = ["/proc/net/tcp", "/proc/net/tcp6"]
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect::<Vec<_>>();
    let listeners = tables
        .iter()
        .flat_map(|table| tcp_listeners(table))
        .collect::<Vec<_>>();
    models
        .iter()
        .map(|model| {
            let index = health
                .all_models_loaded
                .iter()
                .position(|m| m.model_name == model.id)
                .with_context(|| format!("{} not resident", model.id))?;
            let loaded = health.all_models_loaded.swap_remove(index);
            let expected_device = model.compute_class();
            ensure!(
                loaded
                    .device
                    .split_whitespace()
                    .any(|d| d == expected_device)
                    && loaded.recipe == model.recipe
                    && loaded.checkpoint == model.checkpoint,
                "loaded placement or checkpoint mismatch for {} ({})",
                model.id,
                loaded.device
            );
            ensure!(
                loaded
                    .recipe_options
                    .get("ctx_size")
                    .and_then(Value::as_u64)
                    == Some(model.ctx as u64),
                "context pin not confirmed for {}",
                model.id
            );
            if model.recipe == "llamacpp" {
                let opts = model.options();
                if let Some(ref device) = model.device {
                    ensure!(
                        loaded
                            .recipe_options
                            .get("llamacpp_device")
                            .and_then(Value::as_str)
                            == Some(device),
                        "engine device pin not confirmed for {}",
                        model.id
                    );
                }
                ensure!(
                    loaded
                        .recipe_options
                        .get("llamacpp_backend")
                        .and_then(Value::as_str)
                        == Some(&model.backend)
                        && loaded
                            .recipe_options
                            .get("llamacpp_args")
                            .and_then(Value::as_str)
                            == opts.llamacpp_args.as_deref(),
                    "load options not confirmed for {}",
                    model.id
                );
            }
            let listener = reqwest::Url::parse(&loaded.backend_url)
                .ok()
                .and_then(|url| {
                    let port = url.port()?;
                    let mut matches = listeners.iter().filter(|(p, _)| *p == port);
                    let first = *matches.next()?;
                    matches.next().is_none().then_some(first)
                });
            Ok(ResidentModel {
                device: loaded.device,
                listener,
            })
        })
        .collect()
}

fn residency(before: &[ResidentModel], after: Result<Vec<ResidentModel>>) -> Value {
    match after {
        Ok(after) => {
            let unknown = before
                .iter()
                .zip(&after)
                .filter(|(a, b)| a.listener.is_none() || b.listener.is_none())
                .count();
            let changed = before
                .iter()
                .zip(&after)
                .filter(|(a, b)| {
                    a.listener
                        .zip(b.listener)
                        .is_some_and(|(before, after)| before != after)
                })
                .count();
            json!({"resident_before_and_after": true, "backend_instance_changes_observed": changed, "backend_identity_unavailable": unknown, "devices": after.iter().map(|m| &m.device).collect::<Vec<_>>()})
        }
        Err(_) => {
            json!({"resident_before_and_after": false, "backend_instance_changes_observed": null})
        }
    }
}

fn stable_residency(record: &Value) -> bool {
    record["resident_before_and_after"] == true
        && record["backend_instance_changes_observed"] == 0
        && record["backend_identity_unavailable"] == 0
}

fn corpus(indices: impl Iterator<Item = usize>, dims: usize) -> Result<(TempDir, KnowledgeGraph)> {
    let temp = tempfile::tempdir()?;
    let graph = KnowledgeGraph::with_storage_config(&StorageConfig {
        db_path: temp.path().into(),
        embedding_dimensions: dims,
        ..Default::default()
    })?;
    for i in indices {
        let name = format!("Archive outpost {i:06}");
        let node = ObjectBuilder::custom("outpost".into(), name.clone()).add_to_graph(&graph)?;
        let description = format!(
            "{name} preserves a lantern map of the {} district. The surveyors annotate the river crossings and record which paths stay open during the winter. A caretaker reviews the archive before each expedition and replaces worn maps and supplies.",
            ["river", "harbor", "forest", "market"][i % 4]
        );
        ensure!(
            graph
                .add_text_chunk(node, description, ChunkType::Description)?
                .len()
                == 1,
            "fixture must produce one chunk per node"
        );
    }
    Ok((temp, graph))
}

async fn embedding(
    connection: &Connection,
    model: &Model,
    route: usize,
    trace: &Trace,
) -> Result<(BuiltProvider, usize, f64)> {
    let start = Instant::now();
    let inner = LemonadeProvider::from_connection(connection.clone(), &model.id).await?;
    let elapsed = ms(start, Instant::now());
    let dims = inner.dimensions()?;
    Ok((
        BuiltProvider {
            name: model.id.clone(),
            weight: model.weight,
            capability: Capability::Embedding,
            provider: ProviderSlot::Embedding(Arc::new(Metered {
                inner,
                route,
                trace: trace.clone(),
            })),
        },
        dims,
        elapsed,
    ))
}

async fn backfill(
    config: &Config,
    connection: &Connection,
    mode: Mode,
    models: &[&Model],
) -> Result<Value> {
    unload_all_models_with_connection(connection.clone()).await?;
    let cold_temp = config.condition().await?;
    let trace = Trace::default();
    let mut load_ms = vec![];
    let mut probe_ms = vec![];
    let mut providers = vec![];
    let mut dims = None;
    for (route, model) in models.iter().enumerate() {
        load_ms.push(model.load(connection).await?);
        let (provider, width, probe) = embedding(connection, model, route, &trace).await?;
        probe_ms.push(probe);
        ensure!(
            *dims.get_or_insert(width) == width,
            "concurrent embedding widths differ"
        );
        providers.push(provider);
    }
    let dims = dims.context("no embedding provider")?;
    let queues = if matches!(mode, Mode::Overlap) {
        providers
            .into_iter()
            .map(|p| InferenceQueueBuilder::new().with_provider(p).build())
            .collect::<Vec<_>>()
    } else {
        vec![
            InferenceQueueBuilder::new()
                .with_providers(providers)
                .build(),
        ]
    };
    let graphs = (0..queues.len())
        .map(|i| corpus((i..config.chunks).step_by(queues.len()), dims))
        .collect::<Result<Vec<_>>>()?;
    let warm_temp = config.condition().await?;
    let before = snapshot(connection, models).await?;
    let energy_start = config.energy();
    let start = Instant::now();
    let sweep = async {
        let results =
            try_join_all(graphs.iter().zip(&queues).map(|((_, graph), queue)| {
                embed_all_chunks(graph, queue, EmbeddingTarget::Standard)
            }))
            .await?;
        let elapsed = start.elapsed();
        let joules = energy_start
            .zip(config.energy())
            .zip(config.meter_max_age_ms)
            .and_then(|((start, end), max_age)| start.energy_to(&end, elapsed, max_age));
        let stored = results.iter().map(|r| r.stored).sum::<usize>();
        ensure!(
            stored == config.chunks,
            "backfill stored {stored} of {} chunks",
            config.chunks
        );
        Ok((elapsed, joules, stored))
    };
    // Do not enqueue after the sweep ends. Drain any previously submitted probe
    // before taking the trace; samples only count if submitted during backfill.
    let probes = async {
        let mut submitted = vec![];
        let mut next = 0;
        let mut sweep = std::pin::pin!(sweep);
        let result = loop {
            tokio::select! { biased;
                result = &mut sweep => break result,
                _ = tokio::time::sleep(Duration::from_millis(config.interactive_period_ms)) => {},
            }
            let t = Instant::now();
            let mut query = queues[next % queues.len()].submit_embed(QUERY);
            next += 1;
            let finished = tokio::select! { biased;
                result = &mut sweep => Some(result),
                result = &mut query => { result?; None }
            };
            submitted.push(t);
            if let Some(result) = finished {
                query.await?;
                break result;
            }
        };
        Result::<_>::Ok((result?, submitted))
    };
    let ((elapsed, joules, stored), submitted) = probes.await?;
    let calls = trace.lock().clone();
    let mut batches = calls
        .iter()
        .filter(|c| c.kind == Kind::Batch && c.ok)
        .collect::<Vec<_>>();
    batches.sort_unstable_by_key(|c| c.span.end);
    let first = batches.first().context("no completed provider batch")?;
    let last = batches.last().unwrap();
    let expected_sizes = graphs
        .iter()
        .map(|(_, g)| g.get_stats().map(|s| s.chunk_count))
        .collect::<Result<Vec<_>>>()?;
    for (i, n) in expected_sizes.into_iter().enumerate() {
        let mut expected = vec![BATCH; n / BATCH];
        if n % BATCH != 0 {
            expected.insert(0, n % BATCH);
        }
        let mut actual = batches
            .iter()
            .filter(|b| !matches!(mode, Mode::Overlap) || b.route == i)
            .map(|b| b.items)
            .collect::<Vec<_>>();
        actual.sort_unstable();
        ensure!(actual == expected, "provider batch shape changed");
    }
    let waits = calls
        .iter()
        .filter(|c| c.kind == Kind::Embed && c.ok)
        .collect::<Vec<_>>();
    ensure!(
        submitted.len() == waits.len(),
        "unmatched interactive probes"
    );
    let interactive = submitted
        .into_iter()
        .zip(waits)
        .map(|(t, c)| ms(t, c.span.start))
        .collect();
    let overlap_pairs = batches
        .iter()
        .enumerate()
        .flat_map(|(i, a)| {
            batches[..i].iter().filter_map(|b| {
                (a.route != b.route && a.span.overlaps(b.span))
                    .then_some([a.route.min(b.route), a.route.max(b.route)])
            })
        })
        .collect::<BTreeSet<_>>();
    let per_route = (0..models.len())
        .map(|i| {
            batches
                .iter()
                .filter(|b| b.route == i)
                .map(|b| b.items)
                .sum::<usize>()
        })
        .collect::<Vec<_>>();
    let provider_window =
        (last.span.end - batches.iter().map(|b| b.span.start).min().unwrap()).as_secs_f64();
    let retries: u64 = queues.iter().map(|q| q.stats().counters.retries).sum();
    config.check_power()?;
    let after = residency(&before, snapshot(connection, models).await);
    let admissible = stable_residency(&after) && retries == 0;
    Ok(json!({
        "admissible_sample": admissible,
        "status": "measured", "cold_start_c": cold_temp, "warm_start_c": warm_temp,
        "cold_load_ms": load_ms, "dimension_probe_ms": probe_ms,
        "warm_first_batch_ms": ms(start, first.span.end), "wall_ms": elapsed.as_secs_f64() * 1000.0,
        "chunks_per_second": stored as f64 / elapsed.as_secs_f64(),
        "warm_provider_window_chunks_per_second": (provider_window > 0.0).then(|| stored as f64 / provider_window),
        "completed_chunks": stored, "completed_by_route": per_route, "provider_request_overlap_pairs": overlap_pairs,
        "batch_items": Spread::of(batches.iter().map(|b| b.items as f64).collect()),
        "interactive_queue_wait_ms": Spread::of(interactive), "queue_retries": retries,
        "whole_system_joules": joules, "joules_per_completed_chunk": joules.map(|j| j / stored as f64),
        "energy_status": if joules.is_some() { "whole_system_meter" } else if config.whole_system_joules_path.is_some() { "meter_invalid_stale_or_window_too_short" } else { "meter_not_configured" },
        "residency": after,
    }))
}

async fn run(config: &Config, connection: &Connection) -> Result<()> {
    let catalog = LemonadeServerCatalog::discover_with_connection(connection.clone()).await?;
    ensure!(
        catalog.server_version.as_deref() == Some(&config.runtime_version),
        "expected Lemonade {}, got {:?}",
        config.runtime_version,
        catalog.server_version
    );
    LemonadeManagement::new(connection.clone())
        .set_max_loaded_models(config.max_loaded_models, false)
        .await?;
    emit(
        json!({"kind": "manifest", "schema": 1, "corpus": CORPUS_VERSION, "u_forge_core_version": env!("CARGO_PKG_VERSION"), "runtime": config.runtime_version, "hardware": config.hardware, "fabric": config.fabric, "power_profile": config.power_profile, "power_profile_probe_configured": config.power_profile_path.is_some(), "ac_online": config.ac_online, "temperature_band_c": config.temperature_band_c, "cooldown_secs": config.cooldown_secs, "repetitions": config.repetitions, "chunks": config.chunks, "backfill_batch_items": BATCH, "interactive_period_ms": config.interactive_period_ms, "meter_max_age_ms": config.meter_max_age_ms, "max_loaded_models": config.max_loaded_models, "models": config.embeddings, "concurrent": config.concurrent, "search": config.search }),
    );
    for class in ["cpu", "gpu", "npu"] {
        if !config
            .embeddings
            .values()
            .any(|model| model.compute_class() == class)
        {
            emit(
                json!({"kind": "coverage", "target_class": class, "status": "skipped", "reason": "no_pinned_route_configured"}),
            );
        }
    }
    let mut cases = config
        .embeddings
        .keys()
        .map(|name| (Mode::Single, vec![name.as_str()]))
        .collect::<Vec<_>>();
    let concurrent = config
        .concurrent
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    cases.push((Mode::Weighted, concurrent.clone()));
    cases.push((Mode::Overlap, concurrent));
    let mut measured = 0;
    for repeat in 0..config.repetitions {
        for (mode, names) in cases
            .iter()
            .cycle()
            .skip(repeat % cases.len())
            .take(cases.len())
        {
            let models = names
                .iter()
                .map(|key| &config.embeddings[*key])
                .collect::<Vec<_>>();
            let skip = if matches!(mode, Mode::Single) {
                None
            } else {
                concurrent_skip(config, &models)
            }
            .or_else(|| {
                models
                    .iter()
                    .find_map(|model| model.availability(&catalog, "embeddings"))
            });
            let sample = if let Some(reason) = skip {
                json!({"status": "skipped", "reason": reason})
            } else {
                match backfill(config, connection, *mode, &models).await {
                    Ok(sample) => {
                        measured += 1;
                        sample
                    }
                    Err(error) => {
                        eprintln!("backfill {names:?}: {error:#}");
                        json!({"status": "failed", "reason": "backfill_failed_see_stderr"})
                    }
                }
            };
            emit(
                json!({"kind": "backfill", "repeat": repeat + 1, "mode": mode, "routes": names, "sample": sample}),
            );
        }
    }
    if let Some(search) = &config.search {
        if let Err(error) = search::run(config, search, connection, &catalog).await {
            emit(
                json!({"kind": "chat_search", "status": "failed", "reason": "chat_search_failed_see_stderr"}),
            );
            return Err(error);
        }
    } else {
        emit(
            json!({"kind": "chat_search", "status": "skipped", "reason": "search_not_configured"}),
        );
    }
    ensure!(measured > 0, "no successful real backfill samples");
    Ok(())
}

fn validate_fixture_paths(cache: &Path, data: &Path, binary: &Path) -> Result<()> {
    ensure!(
        [cache, data, binary].iter().all(|p| p.is_absolute()),
        "fixture paths must be absolute (see BUILD.md)"
    );
    let cache = cache.canonicalize().context("resolve fixture cache")?;
    let data = data
        .canonicalize()
        .context("resolve fixture data directory")?;
    let root = cache.parent().context("fixture cache has no parent")?;
    ensure!(
        cache == root.join("cache")
            && data == root.join("data")
            && root.join(".u-forge-inference-fabric").is_file(),
        "use cache/ and data/ in a marked disposable fixture (see BUILD.md)"
    );
    let private = cache.join("u-forge/lemonade");
    ensure!(
        private.canonicalize().ok().as_ref() == Some(&private),
        "stage the models/backend cache inside the disposable fixture, not a symlink"
    );
    ensure!(
        data.join("u-forge/lemonade").symlink_metadata().is_err(),
        "legacy cache must not be present in the fixture data directory"
    );
    for name in [
        "models",
        "bin",
        "resources",
        "config.json",
        "user_models.json",
    ] {
        let path = private.join(name);
        ensure!(
            !path.is_symlink(),
            "fixture path {name} must not be a symlink"
        );
    }
    let bin = binary.canonicalize().context("resolve pinned runtime")?;
    // Embedded launch migrates models found next to the executable. Never
    // invoke that path on an installation that still has a packaged model tree.
    for path in [binary, bin.as_path()] {
        let adjacent = path
            .parent()
            .context("runtime path has no parent")?
            .join("models");
        ensure!(
            !adjacent.exists() && adjacent.symlink_metadata().is_err(),
            "stage the pinned lemond and resources without its adjacent models/ tree"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("usage: inference_fabric <experiment.toml>")?;
    let config: Config = toml::from_str(&std::fs::read_to_string(Path::new(&path))?)?;
    config.validate()?;
    let path_env = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .with_context(|| format!("set {name} for the disposable fixture (see BUILD.md)"))
    };
    validate_fixture_paths(
        &path_env("XDG_CACHE_HOME")?,
        &path_env("XDG_DATA_HOME")?,
        &path_env("UFORGE_LEMOND_PATH")?,
    )?;
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let embedded = EmbeddedLemonade::launch().await?;
    let connection = embedded.connection();
    let shutdown = async {
        #[cfg(unix)]
        tokio::select! { signal = tokio::signal::ctrl_c() => signal, _ = terminate.recv() => Ok(()) }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await
    };
    let result = tokio::select! {
        result = run(&config, &connection) => result,
        signal = shutdown => signal.map_err(anyhow::Error::from).and(Err(anyhow::anyhow!("benchmark interrupted"))),
    };
    embedded.shutdown().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Config {
        let config: Config =
            toml::from_str(include_str!("../../../../benchmarks/inference-fabric.toml")).unwrap();
        config.validate().unwrap();
        config
    }

    #[test]
    fn concurrent_cases_require_matching_packages_and_distinct_ids_and_bindings() {
        let mut config = fixture();
        let check = |config: &Config| {
            concurrent_skip(
                config,
                &[&config.embeddings["cpu"], &config.embeddings["igpu"]],
            )
        };
        assert_eq!(check(&config), None);
        let gpu = config.embeddings.get_mut("igpu").unwrap();
        let artifact = std::mem::replace(&mut gpu.artifact, "different".into());
        assert_eq!(check(&config), Some("incompatible_package_or_space"));
        let cpu_id = config.embeddings["cpu"].id.clone();
        let gpu = config.embeddings.get_mut("igpu").unwrap();
        gpu.artifact = artifact;
        let id = std::mem::replace(&mut gpu.id, cpu_id);
        assert_eq!(
            check(&config),
            Some("one_model_id_cannot_bind_two_backends")
        );
        let gpu = config.embeddings.get_mut("igpu").unwrap();
        gpu.id = id;
        gpu.device = None;
        assert_eq!(check(&config), Some("distinct_compute_targets_not_bound"));
    }

    #[test]
    fn rejects_packaged_model_migration_and_unmarked_state() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cache = root.path().join("cache");
        let data = root.path().join("data");
        let binary = root.path().join("runtime/lemond");
        std::fs::create_dir_all(cache.join("u-forge/lemonade"))?;
        std::fs::create_dir_all(&data)?;
        std::fs::create_dir_all(binary.parent().unwrap())?;
        std::fs::write(&binary, b"stub")?;
        assert!(validate_fixture_paths(&cache, &data, &binary).is_err());
        std::fs::write(root.path().join(".u-forge-inference-fabric"), b"")?;
        validate_fixture_paths(&cache, &data, &binary)?;
        let models = binary.with_file_name("models");
        std::fs::create_dir(&models)?;
        let sentinel = models.join("untouched");
        std::fs::write(&sentinel, b"application model")?;
        assert!(validate_fixture_paths(&cache, &data, &binary).is_err());
        assert_eq!(std::fs::read(sentinel)?, b"application model");
        Ok(())
    }

    #[test]
    fn residency_requires_same_known_backend_instances() {
        let entry = |listener| ResidentModel {
            device: "gpu".into(),
            listener,
        };
        let before = [entry(Some((8001, 42)))];
        assert!(stable_residency(&residency(
            &before,
            Ok(vec![entry(Some((8001, 42)))])
        )));
        assert!(!stable_residency(&residency(
            &before,
            Ok(vec![entry(Some((8001, 43)))])
        )));
        assert!(!stable_residency(&residency(
            &before,
            Ok(vec![entry(None)])
        )));
        assert!(!stable_residency(&residency(
            &before,
            Err(anyhow::anyhow!("not resident"))
        )));
    }
}
