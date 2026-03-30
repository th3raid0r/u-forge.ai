//! u-forge.ai — CLI demo
//!
//! Loads the Foundation universe sample data and schemas, then demonstrates
//! graph queries, FTS5 full-text search, and — when a Lemonade Server is
//! reachable — prints detected hardware capabilities, available models, and
//! runs a rerank check against FTS5 search results.
//!
//! Usage:
//!   cargo run --example cli_demo
//!   cargo run --example cli_demo [DATA_FILE] [SCHEMA_DIR]
//!
//! Environment:
//!   LEMONADE_URL       Lemonade Server base URL (e.g. http://localhost:8000/api/v1)
//!   UFORGE_DATA_FILE   override DATA_FILE
//!   UFORGE_SCHEMA_DIR  override SCHEMA_DIR
//!   RUST_LOG           log verbosity (error/warn/info/debug/trace)

use anyhow::Result;
use std::env;
use std::sync::Arc;
use u_forge_core::{
    ai::embeddings::LemonadeProvider,
    hardware::npu::NpuDevice,
    ingest::DataIngestion,
    lemonade::{
        resolve_lemonade_url, LemonadeModelRegistry, LemonadeRerankProvider, ModelRole, SystemInfo,
    },
    queue::{InferenceQueue, InferenceQueueBuilder},
    search::{search_hybrid, HybridSearchConfig},
    types::ObjectMetadata,
    ChunkType, EmbeddingProvider, KnowledgeGraph, ObjectBuilder, SchemaIngestion,
    EMBEDDING_DIMENSIONS,
};

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
                            match NpuDevice::embedding_only(url, None).await {
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
                                match LemonadeProvider::new(url, &model_id).await {
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

                                            // Instance 2 — CPU (second connection to same model)
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
        indexed += graph
            .add_text_chunk(obj.id, obj.name.clone(), ChunkType::Description)?
            .len();

        if let Some(desc) = &obj.description {
            if !desc.is_empty() {
                indexed += graph
                    .add_text_chunk(obj.id, desc.clone(), ChunkType::Description)?
                    .len();
            }
        }

        if let Some(props) = obj.properties.as_object() {
            for (_key, val) in props {
                if let Some(s) = val.as_str() {
                    if !s.is_empty() {
                        indexed += graph
                            .add_text_chunk(obj.id, s.to_string(), ChunkType::Imported)?
                            .len();
                    }
                }
            }
        }
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

    let queries = [
        "empire",
        "foundation",
        "terminus",
        "psychohistory",
        "robot",
        "galaxy",
    ];

    for query in &queries {
        println!("  Query: \"{query}\"");
        let results = graph.search_chunks_fts(query, 3)?;
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

        let semantic_queries = [
            "mathematical prediction of human behaviour",
            "the collapse of a great interstellar civilization",
            "a planet on the periphery of known space",
            "a brilliant scientist and planner",
        ];

        for query in &semantic_queries {
            println!("  Query: \"{query}\"");
            match eq.embed(*query).await {
                Err(e) => println!("    ⚠️  Embed failed: {e}\n"),
                Ok(query_vec) => match graph.search_chunks_semantic(&query_vec, 3) {
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

    if let Some(ref rr) = reranker {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("🏆 Rerank demo (model: {})", rr.model);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        println!("   Strategy: run an FTS5 search, collect the top snippets as");
        println!("   candidate documents, then ask the reranker to re-order them");
        println!("   by relevance to the original query.\n");

        let rerank_queries: &[(&str, &str, usize)] = &[
            ("Who founded the Foundation?", "foundation", 6),
            (
                "mathematics and prediction of civilisation",
                "mathematics",
                5,
            ),
            ("Galactic Empire collapse", "empire", 6),
        ];

        for (query, fts_keyword, fts_limit) in rerank_queries {
            println!("  Query: \"{query}\"");

            let fts_results = graph.search_chunks_fts(fts_keyword, *fts_limit)?;

            if fts_results.is_empty() {
                println!("    ⚠️  FTS returned no candidates for keyword \"{fts_keyword}\"\n");
                continue;
            }

            // Collect candidate documents (snippet text) with their FTS rank.
            // Store obj_id alongside so reranked results can load full node data.
            let candidates: Vec<(u_forge_core::types::ObjectId, String, String)> = fts_results
                .iter()
                .map(|(_chunk_id, obj_id, snippet)| {
                    let label = graph
                        .get_object(*obj_id)
                        .ok()
                        .flatten()
                        .map(|o| format!("{} [{}]", o.name, o.object_type))
                        .unwrap_or_else(|| obj_id.to_string());
                    (*obj_id, label, snippet.clone())
                })
                .collect();

            println!("   FTS candidates (keyword: \"{fts_keyword}\"):");
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
                        // Fall back to the original candidate snippet when the
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
        println!("ℹ️  Rerank demo skipped — no reranker model available on this server.\n");
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

        // Config: balanced blend, return top 3 nodes with full context.
        let config = HybridSearchConfig {
            alpha: 0.5,
            fts_limit: 15,
            semantic_limit: 15,
            rerank: has_rr,
            limit: 3,
        };

        println!(
            "   Config: alpha={} | fts_limit={} | semantic_limit={} | rerank={} | limit={} nodes\n",
            config.alpha, config.fts_limit, config.semantic_limit, config.rerank, config.limit,
        );

        let hybrid_queries = [
            "Who founded the Foundation and why?",
            "What happened to the Galactic Empire?",
            "psychohistory and mathematical prediction",
            "robotic civilizations and machine intelligence",
        ];

        for query in &hybrid_queries {
            println!("  Query: \"{query}\"");

            match search_hybrid(&graph, eq, query, &config).await {
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
        // Show one query at three alpha values so the blend effect is visible.

        println!("  — Alpha sweep (query: \"the collapse of an interstellar civilization\") —\n");

        for &alpha in &[0.0f32, 0.5, 1.0] {
            let label = match alpha {
                a if a == 0.0 => "pure FTS5 ",
                a if a == 1.0 => "pure SEM  ",
                _ => "blend 50/50",
            };
            let sweep_config = HybridSearchConfig {
                alpha,
                fts_limit: 10,
                semantic_limit: 10,
                rerank: false, // keep comparable — no reranker variance
                limit: 3,
            };
            print!("  alpha={alpha:.1} ({label}): ");
            match search_hybrid(
                &graph,
                eq,
                "the collapse of an interstellar civilization",
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
    println!("  {prog} [DATA_FILE] [SCHEMA_DIR]");
    println!();
    println!("Arguments:");
    println!("  DATA_FILE   JSONL data file  (default: ./defaults/data/memory.json)");
    println!("  SCHEMA_DIR  schema directory (default: ./defaults/schemas)");
    println!();
    println!("Environment:");
    println!("  UFORGE_DATA_FILE   override DATA_FILE");
    println!("  UFORGE_SCHEMA_DIR  override SCHEMA_DIR");
    println!("  LEMONADE_URL       Lemonade Server base URL for AI features");
    println!("                     e.g. http://localhost:8000/api/v1");
    println!("  RUST_LOG           log level (error/warn/info/debug/trace)");
    println!();
    println!("AI features (requires LEMONADE_URL):");
    println!("  • Hardware capability detection (NPU / iGPU / CPU)");
    println!("  • Model registry listing by role");
    println!("  • Rerank demo — FTS5 results re-scored by a cross-encoder");
}
