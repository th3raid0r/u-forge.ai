# Feature: Deduplicate Example Code into Shared Module

## Context

The two example binaries (`cli_chat.rs` at 690 lines and `cli_demo.rs` at 1507 lines) share ~300 lines of duplicated orchestration code. The library already exports the building blocks — the duplication is in the setup/glue layer that both examples repeat verbatim. A secondary opportunity exists in the hardware modules where `cpu.rs` and `gpu.rs` each duplicate embedding provider initialization across their `from_registry` / `from_registry_with_config` method pairs.

## Approach

Create `crates/u-forge-core/examples/common/mod.rs` as a shared module included via `#[path = "common/mod.rs"] mod common;` in each example. Extract 6 focused helpers. Separately, add a private `init_llamacpp_embedding()` helper in both `cpu.rs` and `gpu.rs`. Update docs to reflect the new structure at each step.

---

## Step 0: Write feature file ✅

This file.

---

## Step 1: Create `examples/common/mod.rs` (~170 lines)

**New file:** `crates/u-forge-core/examples/common/mod.rs`

Contents (6 items):

### 1a. `DatabaseConfig` struct
Replaces `DatabaseChatConfig` (cli_chat:50-55) and `DatabaseDemoConfig` (cli_demo:73-80).
```rust
pub struct DatabaseConfig { pub path: Option<String>, pub clear: bool }
```

### 1b. `load_toml_config<T: DeserializeOwned>(path: &str) -> Result<T>`
Replaces `load_demo_config()` in both files (cli_chat:62-66, cli_demo:171-175).

### 1c. `DemoArgs` struct + `resolve_demo_args() -> DemoArgs`
Replaces argument parsing in both files (cli_chat:74-114, cli_demo:183-225).
Returns `{ data_file, schema_dir, config_path, help_requested }`.

### 1d. `setup_knowledge_graph(db_path, clear, schema_dir, data_file) -> Result<(KnowledgeGraph, bool)>`
Replaces the KG open + clear + schema load + data import + FTS indexing sequence (cli_chat:340-422, cli_demo:626-732). Returns `(graph, fresh_import)`.

### 1e. `build_hq_embed_queue(registry, app_cfg) -> Option<InferenceQueue>`
Replaces HQ embedding queue construction (cli_chat:287-316, cli_demo:577-617).

### 1f. `embed_all_chunks(graph, queue) -> Result<()>`
Replaces chunk embedding loop (cli_chat:425-458, cli_demo:740-800).

---

## Step 2: Update `cli_chat.rs`

1. Add `#[path = "common/mod.rs"] mod common;` after doc comment
2. Replace `DatabaseChatConfig` with `common::DatabaseConfig` in local `DemoConfig`
3. Remove `load_demo_config()`, use `common::load_toml_config::<DemoConfig>(path)`
4. Replace arg parsing with `let args = common::resolve_demo_args();`
5. Replace KG setup block with `common::setup_knowledge_graph(...)` call
6. Replace HQ embed queue block with `common::build_hq_embed_queue(...)` call
7. Replace chunk embedding block with `common::embed_all_chunks(...)` call
8. Keep all chat-specific code: REPL loop, stream handling, LLM model loading, `print_llm_line`, `indent_block`, NPU/GPU device setup for inference queue (includes LLM workers, not just embedding)

**Note:** cli_chat's inference queue builder includes LLM + reranker workers alongside embedding, so the full builder setup stays in cli_chat. Only the HQ embed queue and chunk embedding helpers apply.

---

## Step 3: Update `cli_demo.rs`

1. Add `#[path = "common/mod.rs"] mod common;` after doc comment
2. Replace `DatabaseDemoConfig` with `common::DatabaseConfig` in local `DemoConfig`
3. Remove `load_demo_config()`, use `common::load_toml_config::<DemoConfig>(path)`
4. Replace arg parsing with `let args = common::resolve_demo_args();`
5. Replace KG setup block with `common::setup_knowledge_graph(...)` call
6. Replace HQ embed queue block with `common::build_hq_embed_queue(...)` call
7. Replace standard embedding block with `common::embed_all_chunks(...)` call
8. Keep all demo-specific code: SystemInfo display, model registry display, FTS/semantic/rerank/hybrid demos, alpha sweep, relationship exploration, ObjectBuilder demo, all display helpers (`bool_icon`, `capability_icon`, `role_label`, `print_model_choice`, `print_node_full`)
9. Keep HQ embedding block inline (uses `upsert_chunk_embedding_hq` — different from standard embedding)

---

## Step 4: Update docs after example refactor

### `ARCHITECTURE.md`
- Line 53: Update module map — replace `examples/cli_demo.rs | Only runnable entry point` with three rows:
  ```
  | `examples/common/mod.rs` | Shared example helpers (config, args, KG setup, embedding) | `DatabaseConfig`, `DemoArgs` |
  | `examples/cli_demo.rs`   | Demo: hardware caps, FTS5, semantic, rerank, hybrid search  | — |
  | `examples/cli_chat.rs`   | Interactive RAG chat REPL                                   | — |
  ```
- Line 16: Update path resolution note to mention `common/mod.rs` also uses `CARGO_MANIFEST_DIR`.

### `.rulesdir/project-structure.mdc`
- Line 106: Update from `cli_demo.rs — The only runnable entry point` to:
  ```
  - examples/common/mod.rs — Shared helpers (config loading, arg parsing, KG setup, embedding)
  - examples/cli_demo.rs — Demo entry point (hardware caps, search demos)
  - examples/cli_chat.rs — Interactive RAG chat REPL
  ```

### `README.md`
- Lines 251-253: Update directory tree to include `common/`:
  ```
  │   │   └── examples/
  │   │       ├── common/              # Shared helpers (config, args, KG setup)
  │   │       ├── cli_demo.rs          # Demo: hardware caps, FTS5, reranking
  │   │       └── cli_chat.rs          # Interactive RAG chat REPL
  ```

### `feature_ChatDemo.md`
- Note that shared code patterns now live in `examples/common/mod.rs` rather than being "reused from cli_demo".

---

## Step 5: Hardware module dedup

### `crates/u-forge-core/src/hardware/gpu.rs`
Add private helper:
```rust
async fn init_llamacpp_embedding(
    registry: &LemonadeModelRegistry,
    load_opts: Option<&ModelLoadOptions>,
) -> Option<Arc<dyn EmbeddingProvider>>
```
Both `from_registry` and `from_registry_with_config` call this instead of duplicating the 20-line embedding init block. Reduces ~40 duplicated lines to ~10.

### `crates/u-forge-core/src/hardware/cpu.rs`
Same pattern. Both `from_registry` and `from_registry_with_config` call a shared `init_llamacpp_embedding` helper.

**No changes to `npu.rs`** — it already delegates via `from_registry_with_load`.

---

## Step 6: Update docs after hardware refactor

### `ARCHITECTURE.md`
- GpuDevice section: note that both constructors share `init_llamacpp_embedding()` internally.
- CpuDevice section: same note.

---

## Files Modified

| File | Action |
|---|---|
| `feature_ExampleDedup.md` | **Create** — this file |
| `crates/u-forge-core/examples/common/mod.rs` | **Create** (~170 lines) |
| `crates/u-forge-core/examples/cli_chat.rs` | Edit — replace ~200 lines with shared calls |
| `crates/u-forge-core/examples/cli_demo.rs` | Edit — replace ~230 lines with shared calls |
| `ARCHITECTURE.md` | Edit — update module map for examples + hardware notes |
| `.rulesdir/project-structure.mdc` | Edit — update Important Files section |
| `README.md` | Edit — update directory tree |
| `feature_ChatDemo.md` | Edit — note shared module |
| `crates/u-forge-core/src/hardware/gpu.rs` | Edit — extract `init_llamacpp_embedding` helper |
| `crates/u-forge-core/src/hardware/cpu.rs` | Edit — extract `init_llamacpp_embedding` helper |

## Verification

1. `cargo build --workspace` — compilation check
2. `cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_demo` — with no env vars, should degrade gracefully
3. `cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_chat` — with no env vars (will fail at Lemonade check, that's expected)
4. `cargo test --workspace -- --test-threads=1` — existing tests pass
5. If Lemonade Server is running: full end-to-end run of both examples
6. `grep -r "DatabaseChatConfig\|DatabaseDemoConfig\|load_demo_config" crates/` — should return zero hits
