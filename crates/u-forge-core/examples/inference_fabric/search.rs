use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use u_forge_core::{
    KnowledgeGraph, embed_all_chunks,
    ingest::EmbeddingTarget,
    lemonade::{
        BuiltProvider, Capability, ChatEvent, ChatMessage, ChatRequest, ChatRunContext,
        LemonadeChatProvider, LemonadeRerankProvider, LemonadeRuntime, LemonadeRuntimeProfile,
        LemonadeServerCatalog, ProviderSlot, unload_all_models_with_connection,
    },
    queue::{CancellationToken, InferenceQueue, InferenceQueueBuilder},
    search::{HybridSearchConfig, SearchStageStatus, search_hybrid_response},
};

use super::metrics::{Call, Kind, Metered, Span, Spread, Trace, added_stall, ms};
use super::{
    Connection, Model, QUERY, corpus, embedding, emit, residency, snapshot, stable_residency,
};

const PROMPT: &str = "Print the sentence 'Lantern maps mark the river crossing.' on 128 consecutive lines. Repeat the exact same sentence on every line. Do not number the lines or add any other text.";
const SEED: u64 = 123;
const OP_BUDGET_MS: f64 = 25.0;
const SEARCH_BUDGET_MS: f64 = 50.0;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    embedding_route: String,
    chat: Model,
    rerank: Model,
    candidates_per_lane: usize,
    #[serde(default = "one_query")]
    query_repetitions: usize,
    max_tokens: u32,
    repeated_searches: usize,
    first_search_after_deltas: usize,
    deltas_between_searches: usize,
}

fn one_query() -> usize {
    1
}

impl Config {
    pub fn validate(&self, parent: &super::Config) -> Result<()> {
        let embed = parent
            .embeddings
            .get(&self.embedding_route)
            .context("unknown search embedding_route")?;
        ensure!(
            embed.id != self.chat.id
                && embed.id != self.rerank.id
                && self.chat.id != self.rerank.id,
            "search requires three distinct model IDs"
        );
        ensure!(
            self.candidates_per_lane > 0
                && self.candidates_per_lane <= 16
                && (1..=16).contains(&self.query_repetitions)
                && self.max_tokens > 0
                && self.repeated_searches > 1
                && self.first_search_after_deltas > 0
                && self.deltas_between_searches > 0,
            "invalid bounded chat-search workload"
        );
        self.chat.validate()?;
        self.rerank.validate()?;
        Ok(())
    }
}

struct Stream {
    start: Instant,
    delivery: Vec<Instant>,
    // Match ordinal AND framing/content, never emit text or hashes.
    fingerprints: Vec<u64>,
    completion_tokens: Option<u32>,
    profile_reload: bool,
}

impl Stream {
    fn gaps(&self) -> Vec<f64> {
        self.delivery
            .windows(2)
            .map(|gap| ms(gap[0], gap[1]))
            .collect()
    }

    fn summary(&self) -> Value {
        let decode_seconds = self
            .delivery
            .last()
            .zip(self.delivery.first())
            .map(|(end, start)| (*end - *start).as_secs_f64())
            .filter(|t| *t > 0.0);
        json!({
            "deltas": self.delivery.len(), "completion_tokens": self.completion_tokens,
            "tokens_per_second_after_first_delta": self.completion_tokens.zip(decode_seconds).map(|(n, t)| f64::from(n.saturating_sub(1)) / t),
            "first_delta_ms": self.delivery.first().map(|t| ms(self.start, *t)),
            "inter_delta_gap_ms": Spread::of(self.gaps()),
            "delivery_offsets_ms": self.delivery.iter().map(|t| ms(self.start, *t)).collect::<Vec<_>>(),
            "profile_reload_performed": self.profile_reload,
        })
    }
}

struct Chat {
    provider: LemonadeChatProvider,
    runtime: Arc<LemonadeRuntime>,
    profile: LemonadeRuntimeProfile,
    max_tokens: u32,
}

impl Chat {
    fn new(connection: &Connection, config: &Config) -> Self {
        let model = &config.chat;
        Self {
            provider: LemonadeChatProvider::from_connection(connection.clone(), &model.id, None),
            runtime: Arc::new(LemonadeRuntime::from_connection(connection.clone())),
            profile: LemonadeRuntimeProfile::new(&model.id, false, model.options())
                .with_backend_profile(
                    &model.recipe,
                    (model.recipe == "llamacpp").then(|| model.backend.clone()),
                    None,
                )
                .with_checkpoint(model.checkpoint.clone()),
            max_tokens: config.max_tokens,
        }
    }

    async fn sample(&self, progress: watch::Sender<usize>) -> Result<Stream> {
        let start = Instant::now();
        let lease = self.runtime.acquire(&self.profile).await?;
        let profile_reload = lease.reload_performed;
        let mut request = ChatRequest::new(vec![ChatMessage::user(PROMPT)])
            .with_temperature(0.0)
            .with_max_tokens(self.max_tokens)
            .with_thinking(false);
        request.seed = Some(SEED);
        request.top_k = Some(1);
        let mut run = self.provider.start_run(
            request,
            ChatRunContext {
                gpu: None,
                profile: None,
                cancellation: CancellationToken::new(),
            },
        );
        let mut stream = Stream {
            start,
            delivery: vec![],
            fingerprints: vec![],
            completion_tokens: None,
            profile_reload,
        };
        while let Some(event) = run.events.recv().await {
            match event {
                ChatEvent::TextDelta(text) | ChatEvent::ReasoningDelta(text)
                    if !text.is_empty() =>
                {
                    stream.delivery.push(Instant::now());
                    let mut hash = DefaultHasher::new();
                    text.hash(&mut hash);
                    stream.fingerprints.push(hash.finish());
                    progress.send_replace(stream.delivery.len());
                }
                ChatEvent::Usage(usage) => stream.completion_tokens = Some(usage.completion_tokens),
                _ => {}
            }
        }
        run.completion
            .await
            .map_err(|error| anyhow::anyhow!("chat driver: {error:?}"))?;
        drop(lease);
        ensure!(
            stream.delivery.len() > 1,
            "chat did not produce a usable stream"
        );
        Ok(stream)
    }
}

struct Search {
    graph: KnowledgeGraph,
    queue: InferenceQueue,
    config: HybridSearchConfig,
    query: String,
    trace: Trace,
}

struct Observation {
    window: Span,
    calls: Vec<Call>,
    applied: bool,
}

impl Search {
    async fn one(&self) -> Result<Observation> {
        self.trace.lock().clear();
        let start = Instant::now();
        let response =
            search_hybrid_response(&self.graph, &self.queue, None, &self.query, &self.config)
                .await?;
        let end = Instant::now();
        let calls = std::mem::take(&mut *self.trace.lock());
        let applied = response.outcomes.standard_semantic.status == SearchStageStatus::Applied
            && response.outcomes.reranking.status == SearchStageStatus::Applied;
        Ok(Observation {
            window: Span { start, end },
            calls,
            applied,
        })
    }

    async fn during(
        &self,
        mut progress: watch::Receiver<usize>,
        config: &Config,
        count: usize,
    ) -> Result<Vec<Observation>> {
        let mut next = config.first_search_after_deltas;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            if progress.wait_for(|&i| i >= next).await.is_err() || progress.has_changed().is_err() {
                break;
            }
            out.push(self.one().await?);
            next = *progress.borrow() + config.deltas_between_searches;
        }
        Ok(out)
    }
}

impl Observation {
    fn report(&self, stream: &Stream, baseline: Option<&[f64]>, gaps: &[f64]) -> Value {
        // Missing, failed, or retried operations are not admissible calibration
        // samples. Report each actual call even when it did not fully overlap.
        let stall = |windows: &[Span]| {
            baseline.and_then(|base| added_stall(base, gaps, &stream.delivery, windows))
        };
        let ops = self.calls.iter().map(|call| {
            let added = call.ok.then(|| stall(&[call.span])).flatten();
            json!({"operation": if call.kind == Kind::Embed { "semantic" } else { "rerank" }, "inputs": call.items, "text_bytes": call.text_bytes, "provider_ms": call.span.ms(), "start_offset_ms": ms(stream.start, call.span.start), "added_stall_ms": added, "violated_25_ms": added.map(|t| t > OP_BUDGET_MS)})
        }).collect::<Vec<_>>();
        let e2e = self.valid().then(|| stall(&[self.window])).flatten();
        json!({"search_wall_ms": self.window.ms(), "agent_tool_wait_ms": null, "stages_applied": self.applied, "start_offset_ms": ms(stream.start, self.window.start), "operations": ops, "end_to_end_added_interruption_ms": e2e, "violated_50_ms": e2e.map(|t| t > SEARCH_BUDGET_MS)})
    }

    fn valid(&self) -> bool {
        self.applied
            && self.calls.len() == 2
            && self.calls[0].kind == Kind::Embed
            && self.calls[1].kind == Kind::Rerank
            && self.calls.iter().all(|call| call.ok)
    }
}

pub async fn run(
    parent: &super::Config,
    config: &Config,
    connection: &Connection,
    catalog: &LemonadeServerCatalog,
) -> Result<()> {
    let embed = &parent.embeddings[&config.embedding_route];
    if let Some(reason) = [
        (&config.chat, "chat"),
        (embed, "embeddings"),
        (&config.rerank, "reranking"),
    ]
    .iter()
    .find_map(|(model, label)| model.availability(catalog, label))
    {
        emit(json!({"kind": "chat_search", "status": "skipped", "reason": reason}));
        return Ok(());
    }
    let chat = Chat::new(connection, config);
    let profiles = [&config.chat, embed, &config.rerank];
    for repeat in 0..parent.repetitions {
        unload_all_models_with_connection(connection.clone()).await?;
        let cold_temp = parent.condition().await?;
        let trace = Trace::default();
        let chat_load = config.chat.load(connection).await?;
        let chat_cold = chat.sample(watch::channel(0).0).await?;
        let embed_load = embed.load(connection).await?;
        let (provider, dims, probe) = embedding(connection, embed, 0, &trace).await?;
        let rerank_load = config.rerank.load(connection).await?;
        let rerank = BuiltProvider {
            name: config.rerank.id.clone(),
            capability: Capability::Reranking,
            provider: ProviderSlot::Rerank(Arc::new(Metered {
                inner: LemonadeRerankProvider::from_connection(
                    connection.clone(),
                    &config.rerank.id,
                ),
                route: 0,
                trace: trace.clone(),
            })),
            weight: 1,
        };
        let queue = InferenceQueueBuilder::new()
            .with_providers(vec![provider, rerank])
            .build();
        let (_tmp, graph) = corpus(0..64, dims)?;
        ensure!(
            embed_all_chunks(&graph, &queue, EmbeddingTarget::Standard)
                .await?
                .stored
                == 64,
            "search fixture indexing failed"
        );
        let search = Search {
            graph,
            queue,
            trace,
            query: std::iter::repeat_n(QUERY, config.query_repetitions)
                .collect::<Vec<_>>()
                .join(" "),
            config: HybridSearchConfig {
                fts_limit: config.candidates_per_lane,
                semantic_limit: config.candidates_per_lane,
                limit: config.candidates_per_lane,
                ..Default::default()
            },
        };
        let cold = search.one().await?;
        ensure!(
            cold.valid(),
            "search fixture did not run both inference stages"
        );
        emit(
            json!({"kind": "chat_search_cold", "repeat": repeat + 1, "start_c": cold_temp, "chat_load_ms": chat_load, "chat_first_request_delta_ms": ms(chat_cold.start, chat_cold.delivery[0]), "chat_profile_reload_performed": chat_cold.profile_reload, "embedding_load_ms": embed_load, "dimension_probe_ms": probe, "rerank_load_ms": rerank_load, "first_search_wall_ms": cold.window.ms()}),
        );
        for count in [1, config.repeated_searches] {
            let (chat, search) = (&chat, &search);
            let trial = |searches| async move {
                let temperature = parent.condition().await?;
                let before = snapshot(connection, &profiles).await?;
                let (tx, rx) = watch::channel(0);
                let (stream, operations) =
                    tokio::try_join!(chat.sample(tx), search.during(rx, config, searches))?;
                parent.check_power()?;
                let resident = residency(&before, snapshot(connection, &profiles).await);
                Ok::<_, anyhow::Error>((stream, operations, resident, temperature))
            };
            // Interleave paired no-search controls and alternate their order.
            let (
                (base, _, base_resident, base_temp),
                (treated, observations, treated_resident, treated_temp),
            ) = if repeat % 2 == 0 {
                (trial(0).await?, trial(count).await?)
            } else {
                let treated = trial(count).await?;
                (trial(0).await?, treated)
            };
            let matched = base.fingerprints == treated.fingerprints
                && base.completion_tokens == treated.completion_tokens;
            let base_gaps = base.gaps();
            let gaps = treated.gaps();
            let complete = observations.len() == count
                && observations.iter().all(|obs| {
                    obs.valid()
                        && obs.window.start >= treated.delivery[0]
                        && obs.window.end <= *treated.delivery.last().unwrap()
                });
            let reports = observations
                .iter()
                .map(|obs| obs.report(&treated, matched.then_some(&base_gaps), &gaps))
                .collect::<Vec<_>>();
            let stalls = reports
                .iter()
                .flat_map(|s| s["operations"].as_array().unwrap())
                .filter_map(|op| op["added_stall_ms"].as_f64())
                .collect::<Vec<_>>();
            let admissible = matched
                && complete
                && stable_residency(&base_resident)
                && stable_residency(&treated_resident)
                && !base.profile_reload
                && !treated.profile_reload;
            emit(
                json!({"kind": "chat_search_pair", "repeat": repeat + 1, "requested_searches": count, "matched_delivery": matched, "complete_overlap": complete, "admissible_sample": admissible, "query_bytes": search.query.len(),
                    "control": base.summary(), "treatment": treated.summary(), "control_start_c": base_temp, "treatment_start_c": treated_temp,
                    "control_residency": base_resident, "treatment_residency": treated_resident,
                    "searches": reports, "worst_operation_added_stall_ms": stalls.iter().copied().reduce(f64::max), "operation_violations": (matched && complete).then(|| stalls.iter().filter(|t| **t > OP_BUDGET_MS).count()),
                    "search_violations": (matched && complete).then(|| reports.iter().filter(|r| r["violated_50_ms"] == true).count()),
                    "total_added_interruption_ms": (matched && complete).then(|| added_stall(&base_gaps, &gaps, &treated.delivery, &observations.iter().map(|o| o.window).collect::<Vec<_>>())).flatten(),
                }),
            );
        }
    }
    Ok(())
}
