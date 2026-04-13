Plan: Interactive RAG Chat Demo (`cli_chat`)

## Context

The project needs an interactive chat demo that exercises the LLM + knowledge graph search pipeline end-to-end. This serves as a **stepping stone toward the TypeScript Agentic Sandbox** (`feature_TS-Agent-Sandbox.md`): the chat demo will use the same types (`ChatMessage`, `ChatRequest`, `InferenceQueue.generate()`) and search patterns (`search_hybrid`, `search_chunks_fts`) that the TS sandbox's `op_generate` and `op_search_hybrid` ops will expose. Building this now validates the RAG pipeline before the sandbox exists.

The existing `cli_demo.rs` (~1485 lines) is a linear walkthrough of individual capabilities. An interactive REPL is fundamentally different, so this will be a **separate example file**.

## Approach: New `cli_chat.rs` example + small `rag` module in u-forge-core

### Step 1: Add `src/rag.rs` module to u-forge-core

A small, reusable module that both the chat demo and the future TS sandbox can consume.

**File:** `crates/u-forge-core/src/rag.rs`

```rust
pub struct RagContext {
    pub formatted_context: String,
    pub source_count: usize,
}

/// Format NodeSearchResult items into an LLM-ready context block.
pub fn format_search_context(results: &[NodeSearchResult]) -> RagContext { ... }

/// Assemble the full ChatMessage array for a RAG turn.
pub fn build_rag_messages(
    system_base: &str,
    context: &RagContext,
    history: &[ChatMessage],
    max_history_turns: usize,
    user_query: &str,
) -> Vec<ChatMessage>
```

- `format_search_context` renders each node's name, type, chunk text, and connected node names into a structured text block
- `build_rag_messages` assembles: system message (base instructions + context) → truncated history (last N turn pairs) → current user message
- Wire into `lib.rs`: `pub mod rag;`

**Why this is a stepping stone:** The TS sandbox agent will do the exact same pattern — call `UForge.searchHybrid()`, format results, call `UForge.generate()`. Having the Rust reference implementation means the `.d.ts` contract can be designed to match.

### Step 2: Create `examples/cli_chat.rs`

**File:** `crates/u-forge-core/examples/cli_chat.rs`

Structure:
1. **Args/config** — Uses `common::resolve_demo_args()` and `common::load_toml_config()` from `examples/common/mod.rs`
2. **Lemonade discovery** — `resolve_lemonade_url()` → `LemonadeModelRegistry::fetch()` → `GpuResourceManager::new()`
3. **Build full InferenceQueue** — Unlike cli_demo (embedding-only), wire up both embedding AND LLM workers:
   - `NpuDevice::from_registry(&registry)` — gets embedding + STT + LLM if available
   - `GpuDevice::from_registry(&registry, gpu)` — gets STT + LLM + embedding if available
   - `InferenceQueueBuilder::new().with_npu_device(npu).with_gpu_device(gpu).build()`
   - This gives a queue with `has_text_generation() == true` AND `has_embedding() == true`
4. **KnowledgeGraph setup** — Uses `setup_and_index()` and `embed_all_chunks()` from `u_forge_core::ingest`
5. **Capability gate** — If `!queue.has_text_generation()`, print helpful message and exit
6. **REPL loop:**
   - Print `You: ` prompt, read line from stdin
   - Handle commands: `/quit`, `/clear` (reset history), `/context` (toggle showing retrieved context)
   - Run `search_hybrid(graph, queue, hq_queue, query, &config)` — falls back to FTS-only when `alpha=0.0` or no embeddings
   - `format_search_context(&results)` → `build_rag_messages(system, &ctx, &history, max_turns, &input)`
   - `queue.generate(ChatRequest::new(messages).with_max_tokens(max_tokens).with_temperature(temp))`
   - Print assistant response
   - Append user + assistant to history, loop

### Step 3: Add `[chat]` section to demo config

**File:** `defaults/demo_config.toml` — add:
```toml
[chat]
system_prompt = "You are a knowledgeable assistant..."
max_history_turns = 10
max_tokens = 1024
temperature = 0.7
alpha = 0.5
search_limit = 3
```

**File:** `cli_chat.rs` — add `ChatDemoConfig` struct with serde defaults for all fields.

### Step 4: Register the example

**File:** `crates/u-forge-core/Cargo.toml` — add `[[example]] name = "cli_chat"` if needed (Cargo auto-discovers examples, but explicit is clearer).

## Critical files to modify/create

| File | Action |
|------|--------|
| `crates/u-forge-core/src/rag.rs` | **Create** — RAG context formatting + message assembly |
| `crates/u-forge-core/src/lib.rs` | **Edit** — add `pub mod rag;` |
| `crates/u-forge-core/examples/cli_chat.rs` | **Create** — interactive REPL demo |
| `defaults/demo_config.toml` | **Edit** — add `[chat]` section |

## Reuse inventory (existing code to leverage, NOT rewrite)

| What | Where |
|------|-------|
| `ChatMessage`, `ChatRequest`, `ChatCompletionResponse` | `src/lemonade/chat.rs` |
| `InferenceQueue.generate()`, `.has_text_generation()` | `src/queue/dispatch.rs` |
| `search_hybrid()`, `NodeSearchResult`, `HybridSearchConfig` | `src/search/mod.rs` |
| `KnowledgeGraph` setup, schema/data loading | pattern from `cli_demo.rs` |
| `NpuDevice::from_registry()`, `GpuDevice::from_registry()` | `src/hardware/{npu,gpu}.rs` |
| `InferenceQueueBuilder` with `.with_npu_device()` / `.with_gpu_device()` | `src/queue/builder.rs` |
| `resolve_lemonade_url()` | `src/lemonade/mod.rs` |
| `LemonadeModelRegistry::fetch()`, `GpuResourceManager::new()` | `src/lemonade/` |

## Graceful degradation

- No Lemonade → print setup instructions, exit
- Lemonade but no LLM model → print which models exist, suggest loading one, exit
- LLM but no embedding → chat works with FTS-only search (note printed)
- Full capability → hybrid search + reranking + LLM

## TS Sandbox alignment

| Chat Demo (Rust) | TS Sandbox (future) |
|---|---|
| `search_hybrid(graph, queue, ...)` | `UForge.searchHybrid(config)` → `op_search_hybrid` |
| `graph.search_chunks_fts(query, limit)` | `UForge.searchFts(query, limit)` → `op_search_fts` |
| `queue.generate(ChatRequest::new(msgs))` | `UForge.generate(request)` → `op_generate` |
| `rag::format_search_context(&results)` | Agent-written TS that formats search results |
| `rag::build_rag_messages(...)` | Agent-written TS that constructs the messages array |
| `InferenceQueue` with LLM + embedding | `TsRuntime::new(graph, Some(queue))` |

## Verification

1. `cargo build --workspace` — compiles cleanly
2. `cargo test --workspace -- --test-threads=1` — all existing tests pass
3. `cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_chat` — without Lemonade: prints helpful message, exits cleanly
4. With Lemonade running + LLM model loaded: interactive chat works, retrieves relevant context from Foundation universe data, maintains conversation history
5. `/clear` resets history, `/quit` exits, `/context` toggles context display
