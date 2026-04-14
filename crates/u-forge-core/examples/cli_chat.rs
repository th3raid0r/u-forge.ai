//! u-forge.ai — Interactive RAG chat demo
//!
//! Loads the Foundation universe sample data and schemas, then starts an
//! interactive REPL where each user message triggers a hybrid knowledge-graph
//! search and an LLM response grounded in the retrieved context.
//!
//! Usage:
//!   cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_chat
//!   cargo run --example cli_chat [DATA_FILE] [SCHEMA_DIR]
//!   cargo run --example cli_chat [DATA_FILE] [SCHEMA_DIR] --config <CONFIG_FILE>
//!
//! Environment:
//!   LEMONADE_URL         Lemonade Server base URL (e.g. http://localhost:13305/api/v1)
//!   UFORGE_DATA_FILE     override DATA_FILE
//!   UFORGE_SCHEMA_DIR    override SCHEMA_DIR
//!   UFORGE_DEMO_CONFIG   path to demo config TOML file (for database overrides)
//!   RUST_LOG             log verbosity (error/warn/info/debug/trace)
//!
//! Chat options are read from `u-forge.toml` (the `[chat]` section).
//! See `AppConfig` / `ChatConfig` for the full list of knobs.
//!
//! REPL commands:
//!   /quit           — exit
//!   /clear          — reset conversation history
//!   /context        — toggle showing retrieved knowledge graph context
//!   /thinking       — toggle extended thinking
//!   /device [gpu|npu] — show or switch the active LLM device

#[path = "common/mod.rs"]
mod common;

use anyhow::Result;
use std::io::{BufRead, Write};
use std::sync::Arc;
use u_forge_core::{
    build_hq_embed_queue,
    config::AppConfig,
    embed_all_chunks,
    ingest::EmbeddingTarget,
    lemonade::{
        provider_factory::{Capability, ProviderFactory},
        selector::{ModelSelector, QualityTier, SelectedModel},
        resolve_lemonade_url, GpuResourceManager, LemonadeServerCatalog,
    },
    queue::InferenceQueueBuilder,
    rag::{build_rag_messages, format_search_context},
    search::{search_hybrid, HybridSearchConfig},
    setup_and_index, ChatDevice, ChatRequest, KnowledgeGraph, StreamToken,
    HIGH_QUALITY_EMBEDDING_DIMENSIONS,
};

// ── Demo config (database overrides only) ────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct DemoConfig {
    database: Option<common::DatabaseConfig>,
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
        None => DemoConfig::default(),
        Some(ref path) => match common::load_toml_config::<DemoConfig>(path) {
            Ok(c) => {
                println!("   Config    : {path} (loaded)");
                c
            }
            Err(e) => return Err(e),
        },
    };

    let app_cfg = AppConfig::load_default();
    let chat_cfg = &app_cfg.chat;

    // ── Lemonade discovery ────────────────────────────────────────────────────

    let lemonade_url = resolve_lemonade_url().await;

    println!("🌟 u-forge.ai — RAG Chat Demo 🌟");
    println!("   Data    : {}", args.data_file);
    println!("   Schemas : {}", args.schema_dir);
    match &lemonade_url {
        Some(url) => println!("   Lemonade: {url} (auto-discovered)"),
        None => {
            println!("   Lemonade: not reachable");
            println!();
            println!("   To use the chat demo, start Lemonade Server and load an LLM model:");
            println!("     sudo snap install lemonade-server");
            println!("     lemonade-server serve");
            println!();
            println!("   Then pull a language model, for example:");
            println!("     lemonade-server pull GLM-4.7-Flash-GGUF");
            return Err(anyhow::anyhow!("Lemonade Server not reachable"));
        }
    }
    let url = lemonade_url.as_deref().unwrap();
    println!();

    // ── Catalog discovery + provider construction ─────────────────────────────

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🔌 Lemonade — detecting capabilities");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let catalog = match LemonadeServerCatalog::discover(url).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("   ❌ Could not discover server catalog: {e}");
            eprintln!("   Is Lemonade Server running at {url}?");
            return Err(e);
        }
    };

    let selector = ModelSelector::new(&catalog, &app_cfg.models, &app_cfg.embedding);

    // Determine the desired LLM model IDs: config override → selector fallback.
    // `select_llm_models` is used only to provide the fallback name; the actual
    // SelectedModel is resolved via `model_by_id` so that config-specified
    // models are used even when they are not in the preference list.
    let llm_selections = selector.select_llm_models();
    let desired_gpu_llm: String = chat_cfg
        .gpu
        .model
        .clone()
        .or_else(|| {
            llm_selections
                .iter()
                .find(|s| s.recipe != "flm")
                .map(|s| s.model_id.clone())
        })
        .unwrap_or_else(|| "Qwen3.5-9B-GGUF".to_string());
    let desired_npu_llm: String = chat_cfg
        .npu
        .model
        .clone()
        .or_else(|| {
            llm_selections
                .iter()
                .find(|s| s.recipe == "flm")
                .map(|s| s.model_id.clone())
        })
        .unwrap_or_else(|| "qwen3.5-9b-FLM".to_string());

    // Resolve the exact SelectedModel for each slot from the live catalog.
    // This bypasses preference-list filtering so config overrides are always
    // honoured, even for models that are not in llm_model_preferences.
    let gpu_llm_sel = selector.model_by_id(&desired_gpu_llm, QualityTier::NotApplicable);
    let npu_llm_sel = selector.model_by_id(&desired_npu_llm, QualityTier::NotApplicable);
    let npu_available = npu_llm_sel.is_some();

    print_llm_line("GPU", &desired_gpu_llm, chat_cfg.gpu.model.is_none());
    if npu_available {
        print_llm_line("NPU", &desired_npu_llm, chat_cfg.npu.model.is_none());
    }
    println!(
        "   Active device: {}",
        match chat_cfg.preferred_device {
            ChatDevice::Auto => "auto (→ gpu)",
            ChatDevice::Gpu => "gpu",
            ChatDevice::Npu => "npu",
            ChatDevice::Cpu => "cpu",
        }
    );

    // ── Build InferenceQueue (embedding + reranker + LLM) ────────────────────
    // cli_chat is text-only — STT and TTS are not loaded here.

    let gpu_mgr = Arc::new(GpuResourceManager::new());

    let embed_models = selector.select_embedding_models();
    let reranker_sel = selector.select_reranker();

    // Collect the model IDs already running on the server so load_model can
    // skip the round-trip for those.
    let already_loaded: Vec<String> = catalog.loaded.iter().map(|m| m.model_name.clone()).collect();

    if reranker_sel.is_none() {
        println!("   Reranker  : not available (results ordered by RRF score)");
    }

    // Collect all build specs, then launch them in parallel.
    struct BuildSpec {
        sel: SelectedModel,
        capability: Capability,
        weight: u32,
        gpu_mgr: Option<Arc<GpuResourceManager>>,
    }

    let mut specs: Vec<BuildSpec> = Vec::new();

    for sel in embed_models.iter().filter(|s| s.quality_tier == QualityTier::Standard) {
        let weight = match sel.recipe.as_str() {
            "flm" => app_cfg.embedding.npu_weight,
            "llamacpp" => match sel.backend.as_deref() {
                Some("rocm") | Some("vulkan") | Some("metal") => app_cfg.embedding.gpu_weight,
                _ => app_cfg.embedding.cpu_weight,
            },
            _ => app_cfg.embedding.cpu_weight,
        };
        specs.push(BuildSpec { sel: sel.clone(), capability: Capability::Embedding, weight, gpu_mgr: None });
    }
    if let Some(r_sel) = reranker_sel {
        specs.push(BuildSpec { sel: r_sel, capability: Capability::Reranking, weight: 100, gpu_mgr: None });
    }
    for sel in gpu_llm_sel.iter().chain(npu_llm_sel.iter()) {
        specs.push(BuildSpec { sel: sel.clone(), capability: Capability::TextGeneration, weight: 100, gpu_mgr: Some(Arc::clone(&gpu_mgr)) });
    }

    let url_owned = url.to_string();
    let build_futs = specs.iter().map(|s| {
        let sel = s.sel.clone();
        let cap = s.capability;
        let weight = s.weight;
        let gpu = s.gpu_mgr.clone();
        let base_url = url_owned.clone();
        let loaded = already_loaded.clone();
        async move { ProviderFactory::build(&sel, cap, &base_url, weight, gpu, &loaded).await }
    });
    let build_results = futures::future::join_all(build_futs).await;

    let mut providers = Vec::new();
    let mut embedding_available = false;

    for (spec, result) in specs.iter().zip(build_results) {
        match result {
            Ok(p) => {
                match spec.capability {
                    Capability::Embedding => {
                        println!("   Embedding : ✅ {} (weight={})", p.name, p.weight);
                        embedding_available = true;
                    }
                    Capability::Reranking => println!("   Reranker  : ✅ {}", p.name),
                    Capability::TextGeneration => println!("   LLM       : ✅ {}", p.name),
                    _ => {}
                }
                providers.push(p);
            }
            Err(e) => match spec.capability {
                Capability::Reranking => println!("   Reranker  : ⚠️  unavailable: {e}"),
                Capability::Embedding => println!("   Embedding : ⚠️  {} unavailable: {e}", spec.sel.model_id),
                _ => println!("   LLM       : ⚠️  {} unavailable: {e}", spec.sel.model_id),
            },
        }
    }

    let queue = InferenceQueueBuilder::new()
        .with_providers(providers)
        .with_config(app_cfg.clone())
        .build();

    // High-quality embedding queue (optional — only when HQ embedding is enabled).
    // Derive the model name from embed_models before the queue consumes it.
    let hq_model_name: Option<String> = embed_models
        .iter()
        .find(|s| s.quality_tier == QualityTier::High)
        .map(|s| match &s.backend {
            Some(b) => format!("{b}/{}", s.model_id),
            None => format!("{}/{}", s.recipe, s.model_id),
        });
    let hq_embed_queue = build_hq_embed_queue(&catalog, &app_cfg).await;
    match &hq_embed_queue {
        Some(_) => println!(
            "   HQ Embed  : ✅ {} ({HIGH_QUALITY_EMBEDDING_DIMENSIONS}-dim)",
            hq_model_name.as_deref().unwrap_or("unknown")
        ),
        None if app_cfg.embedding.high_quality_embedding => {
            println!("   HQ Embed  : ⚠️  enabled in config but no compatible model found");
        }
        None => {}
    }

    if !queue.has_text_generation() {
        eprintln!();
        eprintln!("   ❌ No LLM worker could be started.");
        eprintln!("   Make sure an LLM model is loaded in Lemonade Server.");
        return Err(anyhow::anyhow!("No LLM-capable worker in InferenceQueue"));
    }

    if !embedding_available {
        println!();
        println!("   ⚠️  No embedding model available — search will use FTS5 only.");
        println!("   For semantic search, add an embedding model via the Lemonade UI.");
        println!("   See README.md for instructions.");
    }

    println!();

    // ── Knowledge graph setup ─────────────────────────────────────────────────

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🗄️  Knowledge Graph");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let default_db_path = format!("{}/../../demo_data/kg", env!("CARGO_MANIFEST_DIR"));
    let db_cfg = demo_cfg.database.as_ref();
    let db_path_str = db_cfg
        .and_then(|c| c.path.as_deref())
        .unwrap_or(&default_db_path);
    let clear_db = db_cfg.map(|c| c.clear).unwrap_or(false);

    println!("   Opening knowledge graph at {db_path_str}…");
    let graph = KnowledgeGraph::new(db_path_str)?;
    if clear_db {
        println!("   Clearing existing data…");
        graph.clear_all()?;
    }
    let setup_result = setup_and_index(&graph, &args.schema_dir, &args.data_file).await?;
    if setup_result.fresh_import {
        println!(
            "   ✅ {} objects, {} edges imported, {} chunks indexed\n",
            setup_result.objects_created, setup_result.relationships_created, setup_result.chunks_indexed
        );
    } else {
        let s = graph.get_stats()?;
        println!(
            "   ✅ Loaded from disk ({} nodes, {} chunks)\n",
            s.node_count, s.chunk_count
        );
    }

    let emb = embed_all_chunks(&graph, &queue, EmbeddingTarget::Standard).await?;
    if emb.total > 0 {
        println!("   ✅ {}/{} chunks embedded\n", emb.stored, emb.total);
    }

    // ── REPL ─────────────────────────────────────────────────────────────────

    let search_config = HybridSearchConfig {
        alpha: if queue.has_embedding() {
            chat_cfg.alpha
        } else {
            0.0 // FTS-only when no embeddings
        },
        fts_limit: chat_cfg.search_limit * 4,
        semantic_limit: chat_cfg.search_limit * 4,
        rerank: queue.has_reranking(),
        limit: chat_cfg.search_limit,
        hq_semantic_boost: chat_cfg.hq_semantic_boost,
    };

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("hybrid search config: {:?}", search_config);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let gs = graph.get_stats()?;
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!(
        "💬 Chat ({} nodes, {} chunks indexed)",
        gs.node_count, gs.chunk_count
    );
    if !queue.has_embedding() {
        println!("   ⚠️  FTS-only search (no embedding model)");
    }
    let device_hint = if npu_available { "/quit  /clear  /context  /thinking  /device [gpu|npu]" }
                      else             { "/quit  /clear  /context  /thinking" };
    println!("   Commands: {device_hint}");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let mut history: Vec<u_forge_core::ChatMessage> = Vec::new();
    let mut show_context = false;
    let mut thinking_effort: Option<bool> = None;
    let mut active_device = chat_cfg.preferred_device.clone();
    let stdin = std::io::stdin();

    loop {
        // Print prompt and flush.
        print!("You: ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        match stdin.lock().read_line(&mut input) {
            Ok(0) => {
                // EOF — exit cleanly.
                println!();
                break;
            }
            Err(e) => {
                eprintln!("   ❌ Read error: {e}");
                break;
            }
            Ok(_) => {}
        }
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        // Handle REPL commands.
        match input.as_str() {
            "/quit" | "/exit" => {
                println!("Goodbye!");
                break;
            }
            "/clear" => {
                history.clear();
                println!("   ✅ Conversation history cleared.\n");
                continue;
            }
            "/context" => {
                show_context = !show_context;
                println!(
                    "   Context display: {}\n",
                    if show_context { "ON" } else { "OFF" }
                );
                continue;
            }
            "/thinking" => {
                thinking_effort = match thinking_effort {
                    None => Some(true),
                    Some(_) => None,
                };
                println!(
                    "   Thinking: {}\n",
                    if thinking_effort == Some(true) { "on" } else { "off" }
                );
                continue;
            }
            s if s == "/device" || s.starts_with("/device ") => {
                let arg = input.strip_prefix("/device").unwrap_or("").trim();
                match arg {
                    "" => {
                        // No argument: toggle if both available, otherwise show state.
                        if npu_available {
                            active_device = match active_device {
                                ChatDevice::Npu => ChatDevice::Gpu,
                                _ => ChatDevice::Npu,
                            };
                            let name = if active_device == ChatDevice::Npu { "npu" } else { "gpu" };
                            println!("   Active device: {name}\n");
                        } else {
                            println!("   Active device: gpu (NPU not loaded)\n");
                        }
                    }
                    "gpu" => {
                        active_device = ChatDevice::Gpu;
                        println!("   Active device: gpu\n");
                    }
                    "npu" => {
                        if npu_available {
                            active_device = ChatDevice::Npu;
                            println!("   Active device: npu\n");
                        } else {
                            println!("   ⚠️  NPU LLM not loaded — staying on gpu\n");
                        }
                    }
                    _ => println!("   Usage: /device [gpu|npu]\n"),
                }
                continue;
            }
            _ => {}
        }

        // Retrieve relevant context from the knowledge graph.
        let results =
            search_hybrid(&graph, &queue, hq_embed_queue.as_ref(), &input, &search_config).await?;
        let ctx = format_search_context(&results);

        if show_context {
            if ctx.source_count > 0 {
                println!(
                    "   [Context: {} node(s) retrieved]\n{}",
                    ctx.source_count,
                    indent_block(&ctx.formatted_context, "   | ")
                );
            } else {
                println!("   [Context: no matching nodes found]\n");
            }
        }

        // Build the message array and call the LLM.
        let messages = build_rag_messages(
            &chat_cfg.system_prompt,
            &ctx,
            &history,
            chat_cfg.max_history_turns,
            &input,
        );

        let device_cfg = match active_device {
            ChatDevice::Npu => &chat_cfg.npu,
            _ => &chat_cfg.gpu,
        };
        let effective_model = match active_device {
            ChatDevice::Npu => desired_npu_llm.as_str(),
            _ => desired_gpu_llm.as_str(),
        };
        let mut request = ChatRequest::new(messages).with_model(effective_model);
        if let Some(max_tokens) = device_cfg.max_tokens {
            request = request.with_max_tokens(max_tokens);
        }
        if let Some(temperature) = device_cfg.temperature {
            request = request.with_temperature(temperature);
        }
        if thinking_effort == Some(true) {
            request = request.with_thinking(true);
        }

        print!("{effective_model}: ");
        std::io::stdout().flush()?;

        let mut rx = match queue.generate_stream(request) {
            Err(e) => {
                eprintln!("❌ Stream init failed: {e}");
                continue;
            }
            Ok(rx) => rx,
        };

        let mut reply = String::new();
        let mut stream_err = false;
        let mut in_thinking = false;
        while let Some(chunk) = rx.recv().await {
            match chunk {
                Ok(StreamToken::Thinking(text)) => {
                    if !in_thinking {
                        // \x1b[90m = dark grey; \x1b[0m = reset
                        print!("\x1b[90m---think---\n");
                        in_thinking = true;
                    }
                    print!("{text}");
                    std::io::stdout().flush()?;
                }
                Ok(StreamToken::Content(text)) => {
                    if in_thinking {
                        print!("\n---think---\x1b[0m\n");
                        in_thinking = false;
                    }
                    print!("{text}");
                    std::io::stdout().flush()?;
                    reply.push_str(&text);
                }
                Err(e) => {
                    if in_thinking {
                        print!("\n---think---\x1b[0m\n");
                        in_thinking = false;
                    }
                    eprintln!("\n❌ Stream error: {e}");
                    stream_err = true;
                    break;
                }
            }
        }
        if in_thinking {
            print!("\n---think---\x1b[0m\n");
        }
        println!("\n");

        if !stream_err && !reply.is_empty() {
            history.push(u_forge_core::ChatMessage::user(&input));
            history.push(u_forge_core::ChatMessage::assistant(&reply));
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn print_usage() {
    let prog = std::env::args().next().unwrap_or_default();
    println!("Usage: {prog} [DATA_FILE] [SCHEMA_DIR] [--config <CONFIG_FILE>]");
    println!();
    println!("Interactive RAG chat demo backed by the Foundation universe knowledge graph.");
    println!();
    println!("REPL commands:");
    println!("  /quit     — exit");
    println!("  /clear    — reset conversation history");
    println!("  /context  — toggle display of retrieved knowledge graph context");
    println!("  /thinking — cycle reasoning effort: off → low → medium → high → off");
    println!();
    println!("Environment:");
    println!("  LEMONADE_URL       Override Lemonade Server URL");
    println!("  UFORGE_DATA_FILE   Override data file path");
    println!("  UFORGE_SCHEMA_DIR  Override schema directory");
    println!("  UFORGE_DEMO_CONFIG Override config file path");
    println!("  RUST_LOG           Log verbosity (error/warn/info/debug/trace)");
}

/// Print a single LLM capability line.
///
/// `is_default` is true when the model name came from the hardcoded fallback
/// rather than an explicit `[chat.gpu]` / `[chat.npu]` config entry.
fn print_llm_line(device: &str, model: &str, is_default: bool) {
    let suffix = if is_default { " (default)" } else { "" };
    println!("   LLM ({device:<3}) : ✅ {model}{suffix}");
}

/// Prefix every line of `text` with `prefix` for indented display.
fn indent_block(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| format!("{prefix}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}
