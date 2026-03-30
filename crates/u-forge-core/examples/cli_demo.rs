//! u-forge.ai — CLI demo
//!
//! Loads the Foundation universe sample data and schemas, then demonstrates
//! graph queries, FTS5 full-text search, and — when a Lemonade Server is
//! reachable — prints detected hardware capabilities, available models, and
//! runs a rerank check against semantic search results.
//!
//! Usage:
//!   cargo run --example cli_demo
//!   cargo run --example cli_demo [DATA_FILE] [SCHEMA_DIR]
//!   cargo run --example cli_demo [DATA_FILE] [SCHEMA_DIR] --config <CONFIG_FILE>
//!
//! Environment:
//!   LEMONADE_URL         Lemonade Server base URL (e.g. http://localhost:8000/api/v1)
//!   UFORGE_DATA_FILE     override DATA_FILE
//!   UFORGE_SCHEMA_DIR    override SCHEMA_DIR
//!   UFORGE_DEMO_CONFIG   path to demo config JSON file
//!   RUST_LOG             log verbosity (error/warn/info/debug/trace)
//!
//! Config file format (JSON):
//! ```json
//! {
//!   "fts":      { "queries": [{"query": "empire", "limit": 3}] },
//!   "semantic": { "queries": [{"query": "collapse of civilization", "limit": 3}] },
//!   "rerank":   { "queries": [{"query": "Who founded the Foundation?", "semantic_limit": 6}] },
//!   "hybrid": {
//!     "config": {"alpha": 0.5, "fts_limit": 15, "semantic_limit": 15, "rerank": true, "limit": 3},
//!     "queries": ["Who founded the Foundation and why?"],
//!     "alpha_sweep_query": "the collapse of an interstellar civilization",
//!     "alpha_sweep_values": [0.0, 0.5, 1.0]
//!   }
//! }
//! ```

use anyhow::Result;
use std::env;
use std::sync::Arc;
use u_forge_core::{
    ai::embeddings::LemonadeProvider,
    hardware::npu::NpuDevice,
    ingest::DataIngestion,
    lemonade::{
        effective_ctx_size, resolve_lemonade_url, LemonadeModelRegistry, LemonadeRerankProvider,
        ModelLoadOptions, ModelRole, SystemInfo,
    },
    queue::{InferenceQueue, InferenceQueueBuilder},
    search::{search_hybrid, HybridSearchConfig},
    types::ObjectMetadata,
    ChunkType, EmbeddingProvider, KnowledgeGraph, ObjectBuilder, SchemaIngestion,
    EMBEDDING_DIMENSIONS,
};

// ── Demo config ───────────────────────────────────────────────────────────────

/// Optional per-section configuration loaded from a JSON file.
/// Every field is optional; missing sections fall back to built-in defaults.
#[derive(serde::Deserialize, Default)]
struct DemoConfig {
    fts: Option<FtsDemoConfig>,
    semantic: Option<SemanticDemoConfig>,
    rerank: Option<RerankDemoConfig>,
    hybrid: Option<HybridDemoConfig>,
}

#[derive(serde::Deserialize)]
struct FtsQuery {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(serde::Deserialize)]
struct FtsDemoConfig {
    queries: Vec<FtsQuery>,
}

#[derive(serde::Deserialize)]
struct SemanticQuery {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(serde::Deserialize)]
struct SemanticDemoConfig {
    queries: Vec<SemanticQuery>,
}

#[derive(serde::Deserialize)]
struct RerankQuery {
    query: String,
    #[serde(default = "default_rerank_semantic_limit")]
    semantic_limit: usize,
}

#[derive(serde::Deserialize)]
struct RerankDemoConfig {
    queries: Vec<RerankQuery>,
}

#[derive(serde::Deserialize)]
struct HybridSearchParams {
    #[serde(default = "default_alpha")]
    alpha: f32,
    #[serde(default = "default_hybrid_fts_limit")]
    fts_limit: usize,
    #[serde(default = "default_hybrid_sem_limit")]
    semantic_limit: usize,
    #[serde(default)]
    rerank: bool,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(serde::Deserialize)]
struct HybridDemoConfig {
    config: Option<HybridSearchParams>,
    #[serde(default)]
    queries: Vec<String>,
    alpha_sweep_query: Option<String>,
    alpha_sweep_values: Option<Vec<f32>>,
}

fn default_limit() -> usize {
    3
}
fn default_rerank_semantic_limit() -> usize {
    6
}
fn default_alpha() -> f32 {
    0.5
}
fn default_hybrid_fts_limit() -> usize {
    15
}
fn default_hybrid_sem_limit() -> usize {
    15
}

fn load_demo_config(path: &str) -> Result<DemoConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Could not read config file '{path}': {e}"))?;
    serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("Could not parse config file '{path}': {e}"))
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage(&args[0]);
        return Ok(());
    }

    let data_file = args
        .get(1)
        .cloned()
        .or_else(|| env::var("UFORGE_DATA_FILE").ok())
        .unwrap_or_else(|| {
            format!(
                "{}/../../defaults/data/memory.json",
                env!("CARGO_MANIFEST_DIR")
            )
        });

    let schema_dir = args
        .get(2)
        .cloned()
        .or_else(|| env::var("UFORGE_SCHEMA_DIR").ok())
        .unwrap_or_else(|| format!("{}/../../defaults/schemas", env!("CARGO_MANIFEST_DIR")));

    // Optional demo config: --config <path>, UFORGE_DEMO_CONFIG env var, or
    // defaults/demo_config.json next to the data file / schema dir.
    let default_config_path = format!(
        "{}/../../defaults/demo_config.json",
        env!("CARGO_MANIFEST_DIR")
    );

    let config_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| w[1].clone())
        .or_else(|| env::var("UFORGE_DEMO_CONFIG").ok())
        .or_else(|| {
            if std::path::Path::new(&default_config_path).exists() {
                Some(default_config_path.clone())
            } else {
                None
            }
        });

    let demo_cfg: DemoConfig = match config_path {
        None => DemoConfig::default(),
        Some(ref path) => match load_demo_config(path) {
            Ok(c) => {
                println!("   Config    : {path} (loaded)");
                c
            }
            Err(e) => {
                eprintln!("   ⚠️  Config  : {e} — using built-in defaults");
                DemoConfig::default()
            }
        },
    };

    // Resolve the Lemonade Server URL: probe localhost first, then fall back to
    // the LEMONADE_URL env var.  An unset env var is never a hard error here —
    // the demo degrades gracefully when no server is reachable.
    let lemonade_url = resolve_lemonade_url().await;

    println!("🌟 u-forge.ai — Universe Forge 🌟");
    println!("   Data      : {data_file}");
    println!("   Schemas   : {schema_dir}");
    println!("   Storage   : SQLite (bundled, no system libs required)");
    match &lemonade_url {
        Some(url) => println!("   Lemonade  : {url} (auto-discovered)"),
        None => {
            println!("   Lemonade  : not reachable (start lemonade-server or set LEMONADE_URL)")
        }
    }
    println!();

    // ── Lemonade capabilities & model discovery ───────────────────────────────

    // Capture reachable providers for later sections; these are all Option so
    // the demo degrades gracefully when no server is present.
    let mut reranker: Option<LemonadeRerankProvider> = None;
    // Multi-worker embedding queue: NPU + all compatible llamacpp models.
    let mut embed_queue: Option<InferenceQueue> = None;

    if let Some(ref url) = lemonade_url {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🔌 Lemonade Server — capability detection");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        // ── System info ───────────────────────────────────────────────────────
        match SystemInfo::fetch(url).await {
            Err(e) => {
                println!("   ⚠️  Could not reach Lemonade Server: {e}");
                println!("      (continuing without AI features)\n");
            }
            Ok(info) => {
                println!("🖥️  System");
                println!("   Processor : {}", info.processor);
                println!("   Memory    : {}", info.physical_memory);
                println!("   OS        : {}", info.os_version);
                println!();

                // ── Device availability ───────────────────────────────────────
                println!("🔧 Devices");
                match &info.npu {
                    Some(d) if d.available => {
                        let name = if d.name.is_empty() {
                            "AMD NPU"
                        } else {
                            &d.name
                        };
                        println!("   NPU  : ✅ {name} (family: {})", d.family);
                    }
                    Some(_) => println!("   NPU  : ❌ present but unavailable"),
                    None => println!("   NPU  : — not detected"),
                }
                match &info.igpu {
                    Some(d) if d.available => {
                        let name = if d.name.is_empty() {
                            "AMD iGPU"
                        } else {
                            &d.name
                        };
                        println!("   iGPU : ✅ {name} (family: {})", d.family);
                    }
                    Some(_) => println!("   iGPU : ❌ present but unavailable"),
                    None => println!("   iGPU : — not detected"),
                }
                println!();

                // ── Derived capabilities ──────────────────────────────────────
                let caps = info.lemonade_capabilities();
                println!("🧠 Capabilities");

                // Backends installed
                println!("   Installed backends:");
                println!(
                    "     FLM (NPU)          : {}",
                    bool_icon(caps.flm_npu_installed)
                );
                println!(
                    "     llamacpp (ROCm)    : {}",
                    bool_icon(caps.llamacpp_rocm_installed)
                );
                println!(
                    "     llamacpp (Vulkan)  : {}",
                    bool_icon(caps.llamacpp_vulkan_installed)
                );
                println!(
                    "     whispercpp (Vulkan): {}",
                    bool_icon(caps.whispercpp_vulkan_installed)
                );
                println!(
                    "     whispercpp (CPU)   : {}",
                    bool_icon(caps.whispercpp_cpu_installed)
                );
                println!(
                    "     Kokoro TTS (CPU)   : {}",
                    bool_icon(caps.kokoro_cpu_installed)
                );
                println!();

                // Inference paths
                println!("   Inference paths:");
                println!(
                    "     Embed (NPU)        : {}",
                    capability_icon(caps.can_embed_npu)
                );
                println!(
                    "     LLM (NPU)          : {}",
                    capability_icon(caps.can_llm_npu)
                );
                println!(
                    "     LLM (GPU)          : {}",
                    capability_icon(caps.can_llm_gpu)
                );
                println!(
                    "     Rerank (GPU/CPU)   : {}",
                    capability_icon(caps.can_llm_gpu || caps.llamacpp_vulkan_installed)
                );
                // Audio paths omitted as requested
                println!();

                // ── Model registry ────────────────────────────────────────────
                println!("📋 Model Registry");
                match LemonadeModelRegistry::fetch(url).await {
                    Err(e) => println!("   ⚠️  Could not fetch model list: {e}\n"),
                    Ok(registry) => {
                        if registry.models.is_empty() {
                            println!("   (no models installed)\n");
                        } else {
                            // Group by role for a tidy display
                            let all_roles = [
                                ModelRole::NpuEmbedding,
                                ModelRole::CpuEmbedding,
                                ModelRole::NpuLlm,
                                ModelRole::GpuLlm,
                                ModelRole::Reranker,
                                ModelRole::ImageGen,
                                ModelRole::Other,
                            ];

                            for role in &all_roles {
                                let models = registry.by_role(role);
                                if models.is_empty() {
                                    continue;
                                }
                                println!("   {} [{} model(s)]", role_label(role), models.len());
                                for m in models {
                                    let status = if m.downloaded.unwrap_or(false) {
                                        "✅ downloaded"
                                    } else {
                                        "⬇️  not downloaded"
                                    };
                                    let suggested = if m.suggested.unwrap_or(false) {
                                        " ★"
                                    } else {
                                        ""
                                    };
                                    println!(
                                        "     • {}{} — {} | recipe: {}",
                                        m.id, suggested, status, m.recipe
                                    );
                                }
                            }
                            println!();

                            // Summarise which canonical models will be used
                            println!("   Active model selection:");
                            print_model_choice("   Embed (NPU)  ", registry.npu_embedding_model());
                            print_model_choice("   Embed (CPU)  ", registry.cpu_embedding_model());
                            print_model_choice("   LLM (NPU)   ", registry.npu_llm_model());
                            print_model_choice("   LLM (GPU)   ", registry.llm_model());
                            print_model_choice("   Reranker     ", registry.reranker_model());
                            println!();

                            // Build a reranker for the demo section below
                            match LemonadeRerankProvider::from_registry(&registry) {
                                Ok(r) => {
                                    println!("   ✅ Reranker ready: {}", r.model);
                                    reranker = Some(r);
                                }
                                Err(e) => println!("   ⚠️  No reranker available: {e}"),
                            }

                            // Build a multi-worker embedding InferenceQueue.
                            // Each compatible model (matching EMBEDDING_DIMENSIONS) becomes
                            // its own Tokio worker competing on the shared embed_queue so
                            // bulk embedding jobs are spread across all devices at once.
                            println!("   Building embedding workers…");
                            let mut eq_builder = InferenceQueueBuilder::new();
                            let mut worker_count = 0usize;

                            // NPU worker (FLM embed-gemma-300m-FLM)
                            // ctx_size capped to the model's actual max sequence length.
                            let npu_load_opts = ModelLoadOptions {
                                ctx_size: Some(effective_ctx_size("embed-gemma-300m-FLM")),
                                ..Default::default()
                            };
                            match NpuDevice::embedding_only(url, None, Some(&npu_load_opts)).await {
                                Ok(npu) => {
                                    let dims = npu.embedding.dimensions().unwrap_or(0);
                                    if dims == EMBEDDING_DIMENSIONS {
                                        println!("     ✅ NPU worker: {} ({dims}-dim)", npu.name);
                                        eq_builder = eq_builder.with_npu_device(npu);
                                        worker_count += 1;
                                    } else {
                                        println!(
                                            "     ⚠️  NPU skipped: returns {dims}-dim, \
                                             need {EMBEDDING_DIMENSIONS}-dim"
                                        );
                                    }
                                }
                                Err(e) => println!("     ⚠️  NPU embedding unavailable: {e}"),
                            }

                            // llamacpp workers — two instances of the preferred model
                            // (nomic-embed-text-v2-moe-GGUF), one served via ROCm and
                            // one via CPU, so both iGPU and CPU cores stay busy during
                            // bulk embedding.  The server allows up to 3 concurrent
                            // instances of the same model class.
                            if let Some(model) = registry.cpu_embedding_model() {
                                let model_id = model.id.clone();
                                // ctx_size capped to this model's actual max sequence length.
                                let cpu_load_opts = ModelLoadOptions {
                                    ctx_size: Some(effective_ctx_size(&model_id)),
                                    ..Default::default()
                                };
                                // Load instance 1 with ctx_size, then connect.
                                match LemonadeProvider::new_with_load(
                                    url,
                                    &model_id,
                                    &cpu_load_opts,
                                )
                                .await
                                {
                                    Err(e) => {
                                        println!("     ⚠️  llamacpp({model_id}) unavailable: {e}")
                                    }
                                    Ok(provider) => {
                                        let dims = provider.dimensions().unwrap_or(0);
                                        if dims != EMBEDDING_DIMENSIONS {
                                            println!(
                                                "     ⚠️  llamacpp({model_id}) skipped: \
                                                 {dims}-dim ≠ {EMBEDDING_DIMENSIONS}-dim"
                                            );
                                        } else {
                                            // Instance 1 — ROCm
                                            println!(
                                                "     ✅ llamacpp worker (ROCm): \
                                                 {model_id} ({dims}-dim)"
                                            );
                                            eq_builder = eq_builder.with_embedding_provider(
                                                Arc::new(provider),
                                                format!("llamacpp({model_id})/ROCm"),
                                            );
                                            worker_count += 1;

                                            // Instance 2 — CPU (second connection, model already
                                            // loaded so no second load call needed)
                                            match LemonadeProvider::new(url, &model_id).await {
                                                Err(e) => println!(
                                                    "     ⚠️  llamacpp({model_id})/CPU \
                                                     unavailable: {e}"
                                                ),
                                                Ok(provider2) => {
                                                    println!(
                                                        "     ✅ llamacpp worker (CPU): \
                                                         {model_id} ({dims}-dim)"
                                                    );
                                                    eq_builder = eq_builder
                                                        .with_embedding_provider(
                                                            Arc::new(provider2),
                                                            format!("llamacpp({model_id})/CPU"),
                                                        );
                                                    worker_count += 1;
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                println!("     ⚠️  No llamacpp embedding model found in registry");
                            }

                            if worker_count > 0 {
                                println!(
                                    "   ✅ {worker_count} embedding worker(s) ready \
                                     ({EMBEDDING_DIMENSIONS}-dim, cosine)"
                                );
                                embed_queue = Some(eq_builder.build());
                            } else {
                                println!(
                                    "   ⚠️  No compatible {EMBEDDING_DIMENSIONS}-dim \
                                     embedding models found — semantic search disabled."
                                );
                            }
                            println!();
                        }
                    }
                }
            }
        }
    }

    // ── Database ──────────────────────────────────────────────────────────────

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🗄️  Knowledge Graph");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let temp_dir = tempfile::TempDir::new()?;
    let db_path = temp_dir.path().join("kg");
    std::fs::create_dir_all(&db_path)?;

    println!("🗄️  Opening knowledge graph…");
    let graph = KnowledgeGraph::new(&db_path)?;
    println!("    ✅ Ready\n");

    // ── Empty DB proof ────────────────────────────────────────────────────────

    let empty = graph.get_stats()?;
    println!("🧪 Empty DB proof (before any load)");
    assert_eq!(
        empty.node_count, 0,
        "Expected 0 nodes, got {}",
        empty.node_count
    );
    assert_eq!(
        empty.edge_count, 0,
        "Expected 0 edges, got {}",
        empty.edge_count
    );
    assert_eq!(
        empty.chunk_count, 0,
        "Expected 0 chunks, got {}",
        empty.chunk_count
    );
    assert_eq!(
        empty.total_tokens, 0,
        "Expected 0 tokens, got {}",
        empty.total_tokens
    );
    println!("   Nodes   : {} ✅", empty.node_count);
    println!("   Edges   : {} ✅", empty.edge_count);
    println!("   Chunks  : {} ✅", empty.chunk_count);
    println!("   Tokens  : {} ✅", empty.total_tokens);
    println!();

    // ── Schemas ───────────────────────────────────────────────────────────────

    println!("📚 Loading schemas from {schema_dir}");
    match SchemaIngestion::load_schemas_from_directory(&schema_dir, "imported_schemas", "1.0.0") {
        Ok(schema_def) => {
            let mgr = graph.get_schema_manager();
            match mgr.save_schema(&schema_def).await {
                Ok(()) => {
                    println!(
                        "    ✅ Loaded {} object types:",
                        schema_def.object_types.len()
                    );
                    let mut names: Vec<_> = schema_def.object_types.keys().collect();
                    names.sort();
                    for name in names {
                        println!("       • {name}");
                    }
                }
                Err(e) => eprintln!("    ⚠️  Could not save schemas: {e}"),
            }
        }
        Err(e) => eprintln!("    ⚠️  Could not load schemas from {schema_dir}: {e}"),
    }
    println!();

    // ── Data import ───────────────────────────────────────────────────────────

    println!("📄 Importing data from {data_file}");
    let mut ingestion = DataIngestion::new(&graph);
    if let Err(e) = ingestion.import_json_data(&data_file).await {
        eprintln!("    ❌ Import failed: {e}");
        return Err(e);
    }

    let stats = ingestion.get_stats();
    println!("    ✅ Objects   : {}", stats.objects_created);
    println!("    ✅ Edges     : {}", stats.relationships_created);
    if stats.parse_errors > 0 {
        println!("    ⚠️  Parse errors: {}", stats.parse_errors);
    }
    println!();

    // ── Index text chunks for FTS5 ────────────────────────────────────────────

    println!("🔍 Indexing text for full-text search…");
    let all_objects = graph.get_all_objects()?;
    let mut indexed = 0usize;

    for obj in &all_objects {
        // Flatten the entire node (name, type, description, all properties, tags)
        // into one chunk so FTS5 and semantic search both see the full context.
        let text = obj.flatten_for_embedding();
        indexed += graph
            .add_text_chunk(obj.id, text, ChunkType::Imported)?
            .len();
    }

    println!("    ✅ {indexed} text chunks indexed\n");

    // ── Embed chunks for semantic (ANN) search ────────────────────────────────

    if let Some(ref eq) = embed_queue {
        println!("🧮 Embedding chunks for semantic search (sqlite-vec)…");
        println!("   Workers  : {}", eq.embedding_worker_count());

        // Collect all chunks that need embeddings.
        let chunks_to_embed = graph.get_all_objects().and_then(|objs| {
            let mut all = Vec::new();
            for obj in &objs {
                for chunk in graph.get_text_chunks(obj.id)? {
                    all.push(chunk);
                }
            }
            Ok(all)
        })?;

        let total = chunks_to_embed.len();
        println!("   Chunks   : {total}");

        // Fire all texts at once — embed_many() dispatches one job per chunk
        // and each worker races to claim jobs, so all workers run concurrently.
        let texts: Vec<String> = chunks_to_embed.iter().map(|c| c.content.clone()).collect();

        let t0 = std::time::Instant::now();
        match eq.embed_many(texts).await {
            Err(e) => {
                eprintln!("    ❌ embed_many failed: {e}");
            }
            Ok(vecs) => {
                let elapsed = t0.elapsed();
                let mut stored = 0usize;
                let mut skipped = 0usize;
                for (chunk, vec) in chunks_to_embed.iter().zip(vecs.iter()) {
                    match graph.upsert_chunk_embedding(chunk.id, vec) {
                        Ok(()) => stored += 1,
                        Err(e) => {
                            skipped += 1;
                            eprintln!(
                                "    ⚠️  Could not store embedding for chunk {}: {e}",
                                chunk.id
                            );
                        }
                    }
                }
                let rate = if elapsed.as_secs_f64() > 0.0 {
                    stored as f64 / elapsed.as_secs_f64()
                } else {
                    0.0
                };
                println!(
                    "    ✅ {stored}/{total} embedded in {:.1}s ({rate:.0} chunks/s, \
                     {skipped} skipped)\n",
                    elapsed.as_secs_f64()
                );
            }
        }
    } else if lemonade_url.is_some() {
        println!("ℹ️  Embedding skipped — no compatible embedding model available.\n");
    } else {
        println!("ℹ️  Embedding skipped — set LEMONADE_URL to enable semantic search.\n");
    }

    // ── Graph statistics ──────────────────────────────────────────────────────

    let gs = graph.get_stats()?;
    println!("📊 Graph statistics");
    println!("   Nodes   : {}", gs.node_count);
    println!("   Edges   : {}", gs.edge_count);
    println!("   Chunks  : {}", gs.chunk_count);
    println!("   Tokens  : {}", gs.total_tokens);
    println!();

    // ── FTS5 search demo ──────────────────────────────────────────────────────

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🔎 Full-text search demos (SQLite FTS5)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let default_fts_queries: &[(&str, usize)] = &[
        ("empire", 3),
        ("foundation", 3),
        ("terminus", 3),
        ("psychohistory", 3),
        ("robot", 3),
        ("galaxy", 3),
    ];

    let fts_queries: Vec<(String, usize)> = match &demo_cfg.fts {
        Some(cfg) => cfg.queries.iter().map(|q| (q.query.clone(), q.limit)).collect(),
        None => default_fts_queries
            .iter()
            .map(|(q, l)| (q.to_string(), *l))
            .collect(),
    };

    for (query, limit) in &fts_queries {
        println!("  Query: \"{query}\"");
        let results = graph.search_chunks_fts(query, *limit)?;
        if results.is_empty() {
            println!("    (no matches)\n");
            continue;
        }
        for (i, (_chunk_id, obj_id, snippet)) in results.iter().enumerate() {
            let node = graph.get_object(*obj_id)?;
            let label = node
                .as_ref()
                .map(|o| format!("{} [{}]", o.name, o.object_type))
                .unwrap_or_else(|| obj_id.to_string());
            println!("    {}. {}", i + 1, label);
            println!("       Matched: \"{}\"", snippet);
            if let Some(ref n) = node {
                print_node_full(n, "       ");
            }
        }
        println!();
    }

    // ── Semantic search demo ──────────────────────────────────────────────────

    if let Some(ref eq) = embed_queue {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🔭 Semantic search demos (sqlite-vec ANN)");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        println!("   Strategy: embed the query with the same model used to index");
        println!("   chunks, then find nearest neighbours by cosine distance.\n");

        let default_semantic: &[(&str, usize)] = &[
            ("mathematical prediction of human behaviour", 3),
            ("the collapse of a great interstellar civilization", 3),
            ("a planet on the periphery of known space", 3),
            ("a brilliant scientist and planner", 3),
        ];

        let semantic_queries: Vec<(String, usize)> = match &demo_cfg.semantic {
            Some(cfg) => cfg.queries.iter().map(|q| (q.query.clone(), q.limit)).collect(),
            None => default_semantic
                .iter()
                .map(|(q, l)| (q.to_string(), *l))
                .collect(),
        };

        for (query, limit) in &semantic_queries {
            println!("  Query: \"{query}\"");
            match eq.embed(query.as_str()).await {
                Err(e) => println!("    ⚠️  Embed failed: {e}\n"),
                Ok(query_vec) => match graph.search_chunks_semantic(&query_vec, *limit) {
                    Err(e) => println!("    ⚠️  Semantic search failed: {e}\n"),
                    Ok(results) if results.is_empty() => {
                        println!("    (no matches — are chunks embedded?)\n");
                    }
                    Ok(results) => {
                        for (i, (_chunk_id, obj_id, snippet, distance)) in
                            results.iter().enumerate()
                        {
                            let node = graph.get_object(*obj_id)?;
                            let label = node
                                .as_ref()
                                .map(|o| format!("{} [{}]", o.name, o.object_type))
                                .unwrap_or_else(|| obj_id.to_string());
                            println!("    {}. [dist {:.4}] {}", i + 1, distance, label);
                            println!("       Matched: \"{}\"", snippet);
                            if let Some(ref n) = node {
                                print_node_full(n, "       ");
                            }
                        }
                        println!();
                    }
                },
            }
        }
    } else if lemonade_url.is_some() {
        println!("ℹ️  Semantic search demo skipped — no compatible {EMBEDDING_DIMENSIONS}-dim embedding model available.\n");
    } else {
        println!("ℹ️  Semantic search demo skipped — set LEMONADE_URL to enable AI features.\n");
    }

    // ── Rerank demo ───────────────────────────────────────────────────────────

    if let (Some(ref rr), Some(ref eq)) = (&reranker, &embed_queue) {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🏆 Rerank demo (model: {})", rr.model);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        println!("   Strategy: embed the query, run a semantic ANN search to gather");
        println!("   candidate chunks, then ask the reranker to re-order them by");
        println!("   relevance to the original query.\n");

        let default_rerank: &[(&str, usize)] = &[
            ("Who founded the Foundation?", 6),
            ("mathematics and prediction of civilisation", 5),
            ("Galactic Empire collapse", 6),
        ];

        let rerank_queries: Vec<(String, usize)> = match &demo_cfg.rerank {
            Some(cfg) => cfg
                .queries
                .iter()
                .map(|q| (q.query.clone(), q.semantic_limit))
                .collect(),
            None => default_rerank
                .iter()
                .map(|(q, l)| (q.to_string(), *l))
                .collect(),
        };

        for (query, semantic_limit) in &rerank_queries {
            println!("  Query: \"{query}\"");

            let query_vec = match eq.embed(query.as_str()).await {
                Ok(v) => v,
                Err(e) => {
                    println!("    ⚠️  Embed failed: {e}\n");
                    continue;
                }
            };

            let sem_results = graph.search_chunks_semantic(&query_vec, *semantic_limit)?;

            if sem_results.is_empty() {
                println!("    ⚠️  Semantic search returned no candidates (are chunks embedded?)\n");
                continue;
            }

            // Collect candidate documents with their obj_id so reranked results
            // can load full node data.  Send the full flattened node (not just the
            // matched chunk) so the reranker scores on complete node context.
            let candidates: Vec<(u_forge_core::types::ObjectId, String, String)> = sem_results
                .iter()
                .map(|(_chunk_id, obj_id, _content, distance)| {
                    let node_opt = graph.get_object(*obj_id).ok().flatten();
                    let label = node_opt
                        .as_ref()
                        .map(|o| format!("{} [{}]", o.name, o.object_type))
                        .unwrap_or_else(|| obj_id.to_string());
                    let node_text = node_opt
                        .map(|o| format!("[dist:{distance:.4}]\n{}", o.flatten_for_embedding()))
                        .unwrap_or_else(|| format!("[dist:{distance:.4}] (node not found)"));
                    (*obj_id, label, node_text)
                })
                .collect();

            println!("   Semantic candidates:");
            for (i, (_id, label, snippet)) in candidates.iter().enumerate() {
                println!("     {i}. {label} — \"{}\"", snippet);
            }
            println!();

            // Rerank
            let documents: Vec<String> = candidates.iter().map(|(_, _, s)| s.clone()).collect();
            match rr.rerank(query, documents, Some(candidates.len())).await {
                Err(e) => println!("   ⚠️  Rerank request failed: {e}\n"),
                Ok(ranked) => {
                    println!("   Reranked (most relevant first):");
                    for (rank, doc) in ranked.iter().enumerate() {
                        let (obj_id, label, original_text) = &candidates[doc.index];
                        // Fall back to the original candidate text when the
                        // server doesn't echo the document text back.
                        let text = doc.document.as_deref().unwrap_or(original_text.as_str());
                        println!("     {}. [score {:.4}] {}", rank + 1, doc.score, label,);
                        println!("        Matched: \"{}\"", text);
                        if let Ok(Some(node)) = graph.get_object(*obj_id) {
                            print_node_full(&node, "        ");
                        }
                    }
                    println!();
                }
            }
        }
    } else if lemonade_url.is_some() {
        println!("ℹ️  Rerank demo skipped — requires both an embedding model and a reranker model.\n");
    } else {
        println!("ℹ️  Rerank demo skipped — set LEMONADE_URL to enable AI features.\n");
    }

    // ── Hybrid search demo ────────────────────────────────────────────────────
    //
    // Combines FTS5 + semantic ANN via Reciprocal Rank Fusion, then
    // optionally reranks the merged candidates with the cross-encoder.
    // Degrades gracefully: FTS-only when no embedding worker is available;
    // RRF-scored when no reranker is registered.

    if let Some(ref eq) = embed_queue {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🔀 Hybrid search demo (FTS5 + semantic ANN + rerank)");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        println!("   Strategy: use FTS5 + semantic ANN chunk matches as signal to");
        println!("   identify the most relevant knowledge graph NODES, then return");
        println!("   each winning node with full content, edges, and connected nodes.\n");

        let has_rr = reranker.is_some();

        // Config: from file if provided, otherwise balanced blend with top 3 nodes.
        let config = match demo_cfg.hybrid.as_ref().and_then(|h| h.config.as_ref()) {
            Some(p) => HybridSearchConfig {
                alpha: p.alpha,
                fts_limit: p.fts_limit,
                semantic_limit: p.semantic_limit,
                rerank: p.rerank && has_rr,
                limit: p.limit,
            },
            None => HybridSearchConfig {
                alpha: 0.5,
                fts_limit: 15,
                semantic_limit: 15,
                rerank: has_rr,
                limit: 3,
            },
        };

        println!(
            "   Config: alpha={} | fts_limit={} | semantic_limit={} | rerank={} | limit={} nodes\n",
            config.alpha, config.fts_limit, config.semantic_limit, config.rerank, config.limit,
        );

        let default_hybrid_queries: &[&str] = &[
            "Who founded the Foundation and why?",
            "What happened to the Galactic Empire?",
            "psychohistory and mathematical prediction",
            "robotic civilizations and machine intelligence",
        ];

        let hybrid_queries: Vec<String> = match &demo_cfg.hybrid {
            Some(h) if !h.queries.is_empty() => h.queries.clone(),
            _ => default_hybrid_queries.iter().map(|q| q.to_string()).collect(),
        };

        for query in &hybrid_queries {
            println!("  Query: \"{query}\"");

            match search_hybrid(&graph, eq, query.as_str(), &config).await {
                Err(e) => {
                    println!("    ⚠️  Hybrid search error: {e}\n");
                }
                Ok(results) if results.is_empty() => {
                    println!("    (no results — are chunks embedded?)\n");
                }
                Ok(results) => {
                    for (rank, result) in results.iter().enumerate() {
                        let src = result.sources.label();
                        println!(
                            "    {}. [score {:.4}] {src} {} [{}] — {} chunks, {} edges",
                            rank + 1,
                            result.score,
                            result.node.name,
                            result.node.object_type,
                            result.chunks.len(),
                            result.edges.len(),
                        );
                        print_node_full(&result.node, "       ");
                        if !result.connected_node_names.is_empty() {
                            let mut connected: Vec<String> = result
                                .connected_node_names
                                .values()
                                .map(|cn| format!("{} [{}]", cn.name, cn.object_type))
                                .collect();
                            connected.sort();
                            println!("       → {}", connected.join(", "));
                        }
                    }
                    println!();
                }
            }
        }

        // ── Per-alpha comparison ──────────────────────────────────────────────
        // Show one query at several alpha values so the blend effect is visible.

        let sweep_query = demo_cfg
            .hybrid
            .as_ref()
            .and_then(|h| h.alpha_sweep_query.as_deref())
            .unwrap_or("the collapse of an interstellar civilization");

        let default_sweep_alphas = [0.0f32, 0.5, 1.0];
        let sweep_alphas: &[f32] = demo_cfg
            .hybrid
            .as_ref()
            .and_then(|h| h.alpha_sweep_values.as_deref())
            .unwrap_or(&default_sweep_alphas);

        println!("  — Alpha sweep (query: \"{sweep_query}\") —\n");

        for &alpha in sweep_alphas {
            let label = match alpha {
                a if a == 0.0 => "pure FTS5 ",
                a if a == 1.0 => "pure SEM  ",
                _ => "blend",
            };
            let sweep_config = HybridSearchConfig {
                alpha,
                fts_limit: 10,
                semantic_limit: 10,
                rerank: false, // keep comparable — no reranker variance
                limit: 3,
            };
            print!("  alpha={alpha:.1} ({label}): ");
            match search_hybrid(&graph, eq, sweep_query, &sweep_config).await
            {
                Err(e) => println!("⚠️  {e}"),
                Ok(rs) if rs.is_empty() => println!("(no results)"),
                Ok(rs) => {
                    let names: Vec<String> = rs
                        .iter()
                        .map(|r| format!("{} [{}]", r.node.name, r.node.object_type))
                        .collect();
                    println!("{}", names.join(" | "));
                }
            }
        }
        println!();
    } else if lemonade_url.is_some() {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🔀 Hybrid search demo skipped — no compatible {EMBEDDING_DIMENSIONS}-dim embedding model available.\n");
    } else {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🔀 Hybrid search demo skipped — set LEMONADE_URL to enable AI features.\n");
    }

    // ── Relationship exploration ───────────────────────────────────────────────

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("👥 Relationship exploration");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let sample = all_objects.iter().find(|o| {
        o.object_type == "npc"
            || o.object_type == "character"
            || o.object_type == "player_character"
    });

    if let Some(character) = sample {
        println!("   Character : '{}'", character.name);
        println!("   Type      : {}", character.object_type);

        let neighbours = graph.get_neighbors(character.id)?;
        println!("   Neighbours: {}", neighbours.len());

        let edges = graph.get_relationships(character.id)?;
        println!("   Relationships ({} total):", edges.len());
        for edge in edges.iter().take(8) {
            let from_name = graph
                .get_object(edge.from)?
                .map(|o| o.name)
                .unwrap_or_else(|| "?".to_string());
            let to_name = graph
                .get_object(edge.to)?
                .map(|o| o.name)
                .unwrap_or_else(|| "?".to_string());
            println!(
                "      {} --[{}]--> {}",
                from_name,
                edge.edge_type.as_str(),
                to_name
            );
        }
        if edges.len() > 8 {
            println!("      … and {} more", edges.len() - 8);
        }
        println!();

        let subgraph = graph.query_subgraph(character.id, 2)?;
        println!(
            "   2-hop subgraph: {} objects, {} edges, {} chunks",
            subgraph.objects.len(),
            subgraph.edges.len(),
            subgraph.chunks.len(),
        );
        println!();
    }

    // ── ObjectBuilder demo ────────────────────────────────────────────────────

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🛠️  ObjectBuilder demo");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let custom_id = ObjectBuilder::character("Hari Seldon".to_string())
        .with_description("Mathematician and founder of psychohistory.".to_string())
        .with_property("affiliation".to_string(), "Galactic Empire".to_string())
        .with_tag("mathematician".to_string())
        .with_tag("founder".to_string())
        .add_to_graph(&graph)?;

    let retrieved = graph.get_object(custom_id)?.unwrap();
    println!(
        "   Created : {} [{}]",
        retrieved.name, retrieved.object_type
    );
    println!("   Tags    : {}", retrieved.tags.join(", "));
    println!(
        "   Property: affiliation = {}",
        retrieved.get_property("affiliation").unwrap_or_default()
    );
    println!();

    // ── Done ──────────────────────────────────────────────────────────────────

    println!("✨ Demo complete.");
    println!("   Storage   : SQLite, sqlite-vec");
    if lemonade_url.is_some() {
        println!("   AI        : Lemonade Server connected. Capabilities reported above.");
    } else {
        println!("   AI        : Set LEMONADE_URL to enable embeddings, LLM, and reranking.");
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns "✅ installed" / "❌ not installed" for a boolean backend flag.
fn bool_icon(installed: bool) -> &'static str {
    if installed {
        "✅ installed"
    } else {
        "❌ not installed"
    }
}

/// Returns "✅ available" / "❌ unavailable" for a derived capability flag.
fn capability_icon(available: bool) -> &'static str {
    if available {
        "✅ available"
    } else {
        "❌ unavailable"
    }
}

/// Human-readable label for a [`ModelRole`].
fn role_label(role: &ModelRole) -> &'static str {
    match role {
        ModelRole::NpuEmbedding => "Embedding (NPU / FLM)",
        ModelRole::CpuEmbedding => "Embedding (CPU / llamacpp)",
        ModelRole::NpuStt => "Speech-to-Text (NPU / FLM)",
        ModelRole::GpuStt => "Speech-to-Text (GPU / whispercpp)",
        ModelRole::NpuLlm => "LLM (NPU / FLM)",
        ModelRole::GpuLlm => "LLM (GPU / llamacpp)",
        ModelRole::CpuTts => "Text-to-Speech (CPU / Kokoro)",
        ModelRole::Reranker => "Reranker",
        ModelRole::ImageGen => "Image Generation",
        ModelRole::Other => "Other",
    }
}

/// Print which model the registry selects for a given slot, or a dash if none.
fn print_model_choice(label: &str, model: Option<&u_forge_core::lemonade::LemonadeModelEntry>) {
    match model {
        Some(m) => println!("{}  : {} (recipe: {})", label, m.id, m.recipe),
        None => println!("{}  : — (none available)", label),
    }
}

/// Print the full metadata for a node: description, properties, and tags.
/// `indent` is prepended to every output line.
fn print_node_full(node: &ObjectMetadata, indent: &str) {
    if let Some(desc) = &node.description {
        if !desc.is_empty() {
            println!("{indent}Description: {desc}");
        }
    }
    if let Some(props) = node.properties.as_object() {
        let mut pairs: Vec<(&String, &serde_json::Value)> = props.iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        for (key, val) in pairs {
            let display = match val {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !display.is_empty() {
                println!("{indent}{key}: {display}");
            }
        }
    }
    if !node.tags.is_empty() {
        println!("{indent}Tags: {}", node.tags.join(", "));
    }
}

// ── Usage ─────────────────────────────────────────────────────────────────────

fn print_usage(prog: &str) {
    println!("u-forge.ai CLI Demo");
    println!();
    println!("Usage:");
    println!("  {prog} [DATA_FILE] [SCHEMA_DIR] [--config CONFIG_FILE]");
    println!();
    println!("Arguments:");
    println!("  DATA_FILE      JSONL data file       (default: ./defaults/data/memory.json)");
    println!("  SCHEMA_DIR     schema directory      (default: ./defaults/schemas)");
    println!("  --config PATH  demo config JSON file (optional)");
    println!();
    println!("Environment:");
    println!("  UFORGE_DATA_FILE    override DATA_FILE");
    println!("  UFORGE_SCHEMA_DIR   override SCHEMA_DIR");
    println!("  UFORGE_DEMO_CONFIG  path to demo config JSON file");
    println!("  LEMONADE_URL        Lemonade Server base URL for AI features");
    println!("                      e.g. http://localhost:8000/api/v1");
    println!("  RUST_LOG            log level (error/warn/info/debug/trace)");
    println!();
    println!("Config file format (JSON) — all sections are optional:");
    println!(r#"  {{"fts": {{"queries": [{{"query": "empire", "limit": 3}}]}}}}"#);
    println!(r#"  {{"semantic": {{"queries": [{{"query": "collapse of empire", "limit": 3}}]}}}}"#);
    println!(r#"  {{"rerank": {{"queries": [{{"query": "Who founded it?", "fts_keyword": "foundation", "fts_limit": 6}}]}}}}"#);
    println!(r#"  {{"hybrid": {{"config": {{"alpha": 0.5, "fts_limit": 15, "semantic_limit": 15, "rerank": true, "limit": 3}},"#);
    println!(r#"              "queries": ["Who founded the Foundation?"],"#);
    println!(r#"              "alpha_sweep_query": "collapse of civilization","#);
    println!(r#"              "alpha_sweep_values": [0.0, 0.5, 1.0]}}}}"#);
    println!();
    println!("AI features (requires LEMONADE_URL):");
    println!("  • Hardware capability detection (NPU / iGPU / CPU)");
    println!("  • Model registry listing by role");
    println!("  • Rerank demo — FTS5 results re-scored by a cross-encoder");
}
