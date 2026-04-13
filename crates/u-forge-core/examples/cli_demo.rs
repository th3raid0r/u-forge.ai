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
//!   LEMONADE_URL         Lemonade Server base URL (e.g. http://localhost:13305/api/v1)
//!   UFORGE_DATA_FILE     override DATA_FILE
//!   UFORGE_SCHEMA_DIR    override SCHEMA_DIR
//!   UFORGE_DEMO_CONFIG   path to demo config TOML file
//!   RUST_LOG             log verbosity (error/warn/info/debug/trace)
//!
//! Config file format (TOML):
//! ```toml
//! [fts]
//! [[fts.queries]]
//! query = "empire"
//! limit = 3
//!
//! [semantic]
//! [[semantic.queries]]
//! query = "collapse of civilization"
//! limit = 3
//!
//! [rerank]
//! [[rerank.queries]]
//! query = "Who founded the Foundation?"
//! semantic_limit = 6
//!
//! [hybrid]
//! queries = ["Who founded the Foundation and why?"]
//! alpha_sweep_query = "the collapse of an interstellar civilization"
//! alpha_sweep_values = [0.0, 0.5, 1.0]
//!
//! [hybrid.config]
//! alpha = 0.5
//! fts_limit = 15
//! semantic_limit = 15
//! rerank = true
//! limit = 3
//! ```

#[path = "common/mod.rs"]
mod common;

use anyhow::Result;
use std::sync::Arc;
use u_forge_core::{
    ai::embeddings::LemonadeProvider,
    config::AppConfig,
    hardware::npu::NpuDevice,
    lemonade::{
        resolve_lemonade_url, LemonadeModelRegistry, LemonadeRerankProvider, ModelRole, SystemInfo,
    },
    queue::{InferenceQueue, InferenceQueueBuilder},
    search::{search_hybrid, HybridSearchConfig},
    types::ObjectMetadata,
    EmbeddingProvider, KnowledgeGraph, ObjectBuilder,
    EMBEDDING_DIMENSIONS, HIGH_QUALITY_EMBEDDING_DIMENSIONS,
};

// ── Demo config ───────────────────────────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct DemoConfig {
    database: Option<common::DatabaseConfig>,
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
    #[serde(default = "default_hq_semantic_boost")]
    hq_semantic_boost: f32,
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
fn default_hq_semantic_boost() -> f32 {
    3.0
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = common::resolve_demo_args();

    if args.help_requested {
        print_usage();
        return Ok(());
    }

    let demo_cfg: DemoConfig = match args.config_path {
        None => {
            eprintln!("❌ No demo config file found.");
            eprintln!("   Pass --config <path>, set UFORGE_DEMO_CONFIG, or place");
            eprintln!("   defaults/demo_config.toml relative to the project root.");
            return Err(anyhow::anyhow!("Demo config file required but not found"));
        }
        Some(ref path) => match common::load_toml_config::<DemoConfig>(path) {
            Ok(c) => {
                println!("   Config    : {path} (loaded)");
                c
            }
            Err(e) => return Err(e),
        },
    };

    // Resolve the Lemonade Server URL: probe localhost first, then fall back to
    // the LEMONADE_URL env var.  An unset env var is never a hard error here —
    // the demo degrades gracefully when no server is reachable.
    let lemonade_url = resolve_lemonade_url().await;

    println!("🌟 u-forge.ai — Universe Forge 🌟");
    println!("   Data      : {}", args.data_file);
    println!("   Schemas   : {}", args.schema_dir);
    println!("   Storage   : SQLite (bundled, no system libs required)");
    match &lemonade_url {
        Some(url) => println!("   Lemonade  : {url} (auto-discovered)"),
        None => {
            println!("   Lemonade  : not reachable (start lemonade-server or set LEMONADE_URL)")
        }
    }
    println!();

    // Resolve DB config upfront (needed for the Database section below)
    let default_db_path = format!("{}/../../demo_data/kg", env!("CARGO_MANIFEST_DIR"));
    let db_cfg = demo_cfg.database.as_ref();
    let db_path_str = db_cfg
        .and_then(|c| c.path.as_deref())
        .unwrap_or(&default_db_path);
    let clear_db = db_cfg.map(|c| c.clear).unwrap_or(false);

    // ── Lemonade capabilities & model discovery ───────────────────────────────

    // Capture reachable providers for later sections; these are all Option so
    // the demo degrades gracefully when no server is present.
    let mut reranker: Option<LemonadeRerankProvider> = None;
    // Multi-worker embedding queue: NPU + all compatible llamacpp models (768-dim).
    // Needed for query embedding in search demos.
    let mut embed_queue: Option<InferenceQueue> = None;
    // High-quality embedding queue: Qwen3-Embedding-8B-GGUF (4096-dim).
    // Needed for HQ query embedding if available.
    let mut hq_embed_queue: Option<InferenceQueue> = None;

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
                                ModelRole::LlamacppEmbedding,
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
                            let device_cfg = AppConfig::load_default();
                            println!("   Active model selection:");
                            print_model_choice("   Embed (NPU)  ", registry.npu_embedding_model());
                            print_model_choice(
                                "   Embed (llamacpp)",
                                registry.llamacpp_embedding_model(),
                            );
                            print_model_choice(
                                "   Embed (HQ)   ",
                                registry.hq_embedding_model(
                                    device_cfg.embedding.high_quality_embedding,
                                ),
                            );
                            print_model_choice("   LLM (NPU)   ", registry.npu_llm_model());
                            print_model_choice("   LLM (GPU)   ", registry.llm_model());
                            print_model_choice("   Reranker     ", registry.reranker_model());
                            println!();

                            // Build a reranker for the demo section below
                            match LemonadeRerankProvider::from_registry(&registry) {
                                Ok(r) => {
                                    let load_opts = device_cfg.models.load_options_for(&r.model);
                                    if let Err(e) = r.load(&load_opts).await {
                                        println!("   ⚠️  Reranker load failed ({e}), using server defaults");
                                    }
                                    println!("   ✅ Reranker ready: {}", r.model);
                                    reranker = Some(r);
                                }
                                Err(e) => println!("   ⚠️  No reranker available: {e}"),
                            }

                            // Build a multi-worker embedding InferenceQueue with weighted dispatch.
                            // device_cfg loaded above for model selection display.
                            println!("   Device config:");
                            println!(
                                "     NPU embed: {} (weight={})",
                                if device_cfg.embedding.npu_enabled {
                                    "enabled"
                                } else {
                                    "disabled"
                                },
                                device_cfg.embedding.npu_weight
                            );
                            println!(
                                "     GPU embed: {} (weight={})",
                                if device_cfg.embedding.gpu_enabled {
                                    "enabled"
                                } else {
                                    "disabled"
                                },
                                device_cfg.embedding.gpu_weight
                            );
                            println!(
                                "     CPU embed: {} (weight={})",
                                if device_cfg.embedding.cpu_enabled {
                                    "enabled"
                                } else {
                                    "disabled"
                                },
                                device_cfg.embedding.cpu_weight
                            );
                            println!(
                                "     HQ embed : {} ({HIGH_QUALITY_EMBEDDING_DIMENSIONS}-dim)",
                                if device_cfg.embedding.high_quality_embedding {
                                    "enabled"
                                } else {
                                    "disabled"
                                }
                            );

                            println!("   Building embedding workers…");
                            let mut eq_builder =
                                InferenceQueueBuilder::new().with_config(device_cfg.clone());
                            let mut worker_count = 0usize;

                            // NPU worker (FLM embed-gemma-300m-FLM)
                            // ctx_size capped to the model's actual max sequence length.
                            let npu_load_opts =
                                device_cfg.models.load_options_for("embed-gemma-300m-FLM");
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

                            // llamacpp worker — embedding-gemma GGUF variant (GPU or CPU).
                            // Must be the same model family as the NPU embed-gemma-300m-FLM
                            // so that all workers produce vectors in the same embedding
                            // space.  Mixing model families (e.g. nomic + gemma) causes
                            // meaningless distance scores.
                            if let Some(model) = registry.llamacpp_embedding_model() {
                                let model_id = model.id.clone();
                                let cpu_load_opts = device_cfg.models.load_options_for(&model_id);
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
                                            println!(
                                                "     ✅ llamacpp worker: \
                                                 {model_id} ({dims}-dim)"
                                            );
                                            eq_builder = eq_builder.with_embedding_provider(
                                                Arc::new(provider),
                                                format!("llamacpp({model_id})"),
                                            );
                                            worker_count += 1;
                                        }
                                    }
                                }
                            } else {
                                println!("     ⚠️  No llamacpp embedding model found in registry");
                                println!("        Add embedding-gemma GGUF via Lemonade UI —");
                                println!("        see README.md for instructions.");
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

                            // ── High-quality embedding worker (Qwen3-Embedding-8B-GGUF) ─
                            hq_embed_queue =
                                common::build_hq_embed_queue(&registry, &device_cfg).await;
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

    let (graph, _fresh) = common::setup_knowledge_graph(
        db_path_str,
        clear_db,
        &args.schema_dir,
        &args.data_file,
    )
    .await?;

    // ── Embed chunks for semantic (ANN) search ────────────────────────────────

    let post_load_stats = graph.get_stats()?;
    let needs_hq_embedding = post_load_stats.chunk_count > post_load_stats.embedded_hq_count;

    if let Some(ref eq) = embed_queue {
        common::embed_all_chunks(&graph, eq).await?;
    } else if lemonade_url.is_some() {
        println!("ℹ️  Embedding skipped — no compatible embedding model available.\n");
    } else {
        println!("ℹ️  Embedding skipped — set LEMONADE_URL to enable semantic search.\n");
    }

    // ── HQ embed chunks (4096-dim) ───────────────────────────────────────────

    if let Some(ref hq_eq) = hq_embed_queue {
        if !needs_hq_embedding {
            println!(
                "ℹ️  HQ embedding skipped — all {} chunks already HQ embedded.\n",
                post_load_stats.chunk_count
            );
        } else {
            println!("🧮 HQ embedding chunks ({HIGH_QUALITY_EMBEDDING_DIMENSIONS}-dim)…");
            println!("   Workers  : {}", hq_eq.embedding_worker_count());

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

            let texts: Vec<String> = chunks_to_embed.iter().map(|c| c.content.clone()).collect();

            let t0 = std::time::Instant::now();
            match hq_eq.embed_many(texts).await {
                Err(e) => {
                    eprintln!("    ❌ HQ embed_many failed: {e}");
                }
                Ok(vecs) => {
                    let elapsed = t0.elapsed();
                    let mut stored = 0usize;
                    let mut skipped = 0usize;
                    for (chunk, vec) in chunks_to_embed.iter().zip(vecs.iter()) {
                        match graph.upsert_chunk_embedding_hq(chunk.id, vec) {
                            Ok(()) => stored += 1,
                            Err(e) => {
                                skipped += 1;
                                eprintln!(
                                    "    ⚠️  Could not store HQ embedding for chunk {}: {e}",
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
                        "    ✅ {stored}/{total} HQ embedded in {:.1}s ({rate:.0} chunks/s, \
                     {skipped} skipped)\n",
                        elapsed.as_secs_f64()
                    );
                }
            }
        } // end needs_hq_embedding else
    }

    // ── Graph statistics ──────────────────────────────────────────────────────

    let gs = graph.get_stats()?;
    println!("📊 Graph statistics");
    println!("   Nodes     : {}", gs.node_count);
    println!("   Edges     : {}", gs.edge_count);
    println!("   Chunks    : {}", gs.chunk_count);
    println!("   Tokens    : {}", gs.total_tokens);
    println!("   Embedded  : {}/{}", gs.embedded_count, gs.chunk_count);
    println!(
        "   Embedded HQ: {}/{}",
        gs.embedded_hq_count, gs.chunk_count
    );
    println!();

    // ── FTS5 search demo ──────────────────────────────────────────────────────

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🔎 Full-text search demos (SQLite FTS5)");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let fts_queries: Vec<(String, usize)> = match &demo_cfg.fts {
        Some(cfg) => cfg
            .queries
            .iter()
            .map(|q| (q.query.clone(), q.limit))
            .collect(),
        None => Vec::new(),
    };

    if fts_queries.is_empty() {
        println!("ℹ️  FTS search demo skipped — add an [fts] section with queries to your config file.\n");
    }

    for (query, limit) in &fts_queries {
        println!("  Query: \"{query}\"");
        let results = graph.search_chunks_fts(query, *limit)?;
        if results.is_empty() {
            println!("    (no matches)\n");
            continue;
        }
        for (i, (_chunk_id, obj_id, _snippet)) in results.iter().enumerate() {
            let node = graph.get_object(*obj_id)?;
            let label = node
                .as_ref()
                .map(|o| format!("{} [{}]", o.name, o.object_type))
                .unwrap_or_else(|| obj_id.to_string());
            println!("    {}. {}", i + 1, label);
            if let Some(ref n) = node {
                print_node_full(n, &graph, "       ");
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

        let semantic_queries: Vec<(String, usize)> = match &demo_cfg.semantic {
            Some(cfg) => cfg
                .queries
                .iter()
                .map(|q| (q.query.clone(), q.limit))
                .collect(),
            None => Vec::new(),
        };

        if semantic_queries.is_empty() {
            println!("ℹ️  Semantic search demo skipped — add a [semantic] section with queries to your config file.\n");
        }

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
                        for (i, (_chunk_id, obj_id, _snippet, distance)) in
                            results.iter().enumerate()
                        {
                            let node = graph.get_object(*obj_id)?;
                            let label = node
                                .as_ref()
                                .map(|o| format!("{} [{}]", o.name, o.object_type))
                                .unwrap_or_else(|| obj_id.to_string());
                            println!("    {}. [dist {:.4}] {}", i + 1, distance, label);
                            if let Some(ref n) = node {
                                print_node_full(n, &graph, "       ");
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

    // ── HQ Semantic search demo ──────────────────────────────────────────────

    if let Some(ref hq_eq) = hq_embed_queue {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🔭 HQ Semantic search demos ({HIGH_QUALITY_EMBEDDING_DIMENSIONS}-dim ANN)");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

        let semantic_queries: Vec<(String, usize)> = match &demo_cfg.semantic {
            Some(cfg) => cfg
                .queries
                .iter()
                .map(|q| (q.query.clone(), q.limit))
                .collect(),
            None => Vec::new(),
        };

        if semantic_queries.is_empty() {
            println!("ℹ️  HQ semantic search demo skipped — add a [semantic] section with queries to your config file.\n");
        }

        for (query, limit) in &semantic_queries {
            println!("  Query: \"{query}\"");
            match hq_eq.embed(query.as_str()).await {
                Err(e) => println!("    ⚠️  HQ embed failed: {e}\n"),
                Ok(query_vec) => match graph.search_chunks_semantic_hq(&query_vec, *limit) {
                    Err(e) => println!("    ⚠️  HQ semantic search failed: {e}\n"),
                    Ok(results) if results.is_empty() => {
                        println!("    (no HQ matches — are chunks HQ-embedded?)\n");
                    }
                    Ok(results) => {
                        for (i, (_chunk_id, obj_id, _snippet, distance)) in
                            results.iter().enumerate()
                        {
                            let node = graph.get_object(*obj_id)?;
                            let label = node
                                .as_ref()
                                .map(|o| format!("{} [{}]", o.name, o.object_type))
                                .unwrap_or_else(|| obj_id.to_string());
                            println!("    {}. [dist {:.4}] {}", i + 1, distance, label);
                            if let Some(ref n) = node {
                                print_node_full(n, &graph, "       ");
                            }
                        }
                        println!();
                    }
                },
            }
        }
    }

    // ── Rerank demo ───────────────────────────────────────────────────────────

    if let (Some(ref rr), Some(ref eq)) = (&reranker, &embed_queue) {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🏆 Rerank demo (model: {})", rr.model);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        println!("   Strategy: embed the query, run a semantic ANN search to gather");
        println!("   candidate chunks, then ask the reranker to re-order them by");
        println!("   relevance to the original query.\n");

        let rerank_queries: Vec<(String, usize)> = match &demo_cfg.rerank {
            Some(cfg) => cfg
                .queries
                .iter()
                .map(|q| (q.query.clone(), q.semantic_limit))
                .collect(),
            None => Vec::new(),
        };

        if rerank_queries.is_empty() {
            println!("ℹ️  Rerank demo skipped — add a [rerank] section with queries to your config file.\n");
        }

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
                        .map(|o| {
                            let edges = graph.get_relationships(o.id).unwrap_or_default();
                            let edge_lines: Vec<String> = edges
                                .iter()
                                .filter_map(|e| {
                                    let from = if e.from == o.id {
                                        Some(o.name.clone())
                                    } else {
                                        graph.get_object(e.from).ok().flatten().map(|n| n.name)
                                    };
                                    let to = if e.to == o.id {
                                        Some(o.name.clone())
                                    } else {
                                        graph.get_object(e.to).ok().flatten().map(|n| n.name)
                                    };
                                    Some(format!("{} {} {}", from?, e.edge_type.as_str(), to?))
                                })
                                .collect();
                            format!(
                                "[dist:{distance:.4}]\n{}",
                                o.flatten_for_embedding(&edge_lines)
                            )
                        })
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
                        let _text = doc.document.as_deref().unwrap_or(original_text.as_str());
                        println!("     {}. [score {:.4}] {}", rank + 1, doc.score, label,);
                        if let Ok(Some(node)) = graph.get_object(*obj_id) {
                            print_node_full(&node, &graph, "        ");
                        }
                    }
                    println!();
                }
            }
        }
    } else if lemonade_url.is_some() {
        println!(
            "ℹ️  Rerank demo skipped — requires both an embedding model and a reranker model.\n"
        );
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
                hq_semantic_boost: p.hq_semantic_boost,
            },
            None => HybridSearchConfig {
                alpha: 0.5,
                fts_limit: 15,
                semantic_limit: 15,
                rerank: has_rr,
                limit: 3,
                hq_semantic_boost: 3.0,
            },
        };

        println!(
            "   Config: alpha={} | fts_limit={} | semantic_limit={} | rerank={} | semantic_boost={} | limit={} nodes\n",
            config.alpha, config.fts_limit, config.semantic_limit, config.rerank, config.hq_semantic_boost, config.limit,
        );

        let hybrid_queries: Vec<String> = match &demo_cfg.hybrid {
            Some(h) if !h.queries.is_empty() => h.queries.clone(),
            _ => Vec::new(),
        };

        if hybrid_queries.is_empty() {
            println!("ℹ️  Hybrid search demo skipped — add a [hybrid] section with queries to your config file.\n");
        }

        for query in &hybrid_queries {
            println!("  Query: \"{query}\"");

            match search_hybrid(&graph, eq, hq_embed_queue.as_ref(), query.as_str(), &config).await
            {
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
                        print_node_full(&result.node, &graph, "       ");
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
            .and_then(|h| h.alpha_sweep_query.as_deref());

        if let Some(sweep_query) = sweep_query {
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
                hq_semantic_boost: 3.0,
            };
            print!("  alpha={alpha:.1} ({label}): ");
            match search_hybrid(
                &graph,
                eq,
                hq_embed_queue.as_ref(),
                sweep_query,
                &sweep_config,
            )
            .await
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
        } // end if let Some(sweep_query)
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

    let all_objects = graph.get_all_objects()?;
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
        ModelRole::LlamacppEmbedding => "Embedding (GPU/CPU llamacpp)",
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

/// Print the full metadata for a node: description, properties, tags, and edges.
/// `indent` is prepended to every output line.
fn print_node_full(node: &ObjectMetadata, graph: &KnowledgeGraph, indent: &str) {
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
    for line in graph.edge_display_lines(node) {
        println!("{indent}{line}");
    }
}

// ── Usage ─────────────────────────────────────────────────────────────────────

fn print_usage() {
    let prog = std::env::args().next().unwrap_or_default();
    println!("u-forge.ai CLI Demo");
    println!();
    println!("Usage:");
    println!("  {prog} [DATA_FILE] [SCHEMA_DIR] [--config CONFIG_FILE]");
    println!();
    println!("Arguments:");
    println!("  DATA_FILE      JSONL data file       (default: ./defaults/data/memory.json)");
    println!("  SCHEMA_DIR     schema directory      (default: ./defaults/schemas)");
    println!("  --config PATH  demo config TOML file (optional)");
    println!();
    println!("Environment:");
    println!("  UFORGE_DATA_FILE    override DATA_FILE");
    println!("  UFORGE_SCHEMA_DIR   override SCHEMA_DIR");
    println!("  UFORGE_DEMO_CONFIG  path to demo config TOML file");
    println!("  LEMONADE_URL        Lemonade Server base URL for AI features");
    println!("                      e.g. http://localhost:13305/api/v1");
    println!("  RUST_LOG            log level (error/warn/info/debug/trace)");
    println!();
    println!("Config file format (TOML) — all sections are optional:");
    println!(r#"  [fts]"#);
    println!(r#"  [[fts.queries]]"#);
    println!(r#"  query = "empire""#);
    println!(r#"  limit = 3"#);
    println!();
    println!(r#"  [semantic]"#);
    println!(r#"  [[semantic.queries]]"#);
    println!(r#"  query = "collapse of empire""#);
    println!(r#"  limit = 3"#);
    println!();
    println!(r#"  [rerank]"#);
    println!(r#"  [[rerank.queries]]"#);
    println!(r#"  query = "Who founded it?"#);
    println!(r#"  semantic_limit = 6"#);
    println!();
    println!(r#"  [hybrid]"#);
    println!(r#"  [hybrid.config]"#);
    println!(r#"  alpha = 0.5"#);
    println!(r#"  fts_limit = 15"#);
    println!(r#"  semantic_limit = 15"#);
    println!(r#"  rerank = true"#);
    println!(r#"  limit = 3"#);
    println!(r#"  queries = ["Who founded the Foundation?"],"#);
    println!(r#"  alpha_sweep_query = "collapse of civilization","#);
    println!(r#"  alpha_sweep_values = [0.0, 0.5, 1.0]"#);
    println!();
    println!("AI features (requires LEMONADE_URL):");
    println!("  • Hardware capability detection (NPU / iGPU / CPU)");
    println!("  • Model registry listing by role");
    println!("  • Rerank demo — FTS5 results re-scored by a cross-encoder");
}
