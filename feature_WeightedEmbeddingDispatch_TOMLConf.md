# Plan: Weighted Embedding Dispatch + TOML Device Configuration

## Context

The project treats embedding as NPU-only at the device level, even though GPU and CPU can both run llamacpp embedding models with equivalent capability. The `GpuDevice` and `CpuDevice` structs don't declare `Embedding` capability. The registry has `ModelRole::CpuEmbedding` for llamacpp embedding models but no GPU equivalent, and the name is misleading since llamacpp runs on both GPU and CPU. The current queue uses pure work-stealing with no device priority.

The user wants:
- GPU and CPU recognized as embedding-capable devices
- A weighted dispatch system: NPU (highest) > GPU > CPU (lowest)
- TOML config to enable/disable devices (needed because Lemonade can't run the same model on GPU+CPU simultaneously, and disabling NPU is required for high-quality embedding models)

## Approach

### Step 1: Add `toml` dependency
- **File:** `crates/u-forge-core/Cargo.toml`
- Add `toml = "0.8"` to `[dependencies]`

### Step 2: Create `config.rs` — Device configuration with TOML loading
- **New file:** `crates/u-forge-core/src/config.rs`
- Define `DeviceConfig` (top-level) and `EmbeddingDeviceConfig` (nested `[embedding]` section)
- All fields have defaults via `impl Default`: NPU enabled weight=100, GPU enabled weight=50, CPU enabled weight=10
- `DeviceConfig::load(path: Option<&Path>) -> Result<Self>` — reads file or returns defaults if missing
- `DeviceConfig::load_default() -> Self` — checks `./u-forge-devices.toml`, then `$XDG_CONFIG_HOME/u-forge/devices.toml`, then defaults
- Register module in `lib.rs`

```toml
# u-forge-devices.toml
[embedding]
npu_enabled = true
gpu_enabled = true
cpu_enabled = true
npu_weight = 100
gpu_weight = 50
cpu_weight = 10
```

### Step 3: Rename `CpuEmbedding` to `LlamacppEmbedding` in registry
- **File:** `crates/u-forge-core/src/lemonade/registry.rs`
- Rename `ModelRole::CpuEmbedding` variant to `ModelRole::LlamacppEmbedding`
- Rename `cpu_embedding_model()` to `llamacpp_embedding_model()`
- Rename `all_cpu_embedding_models()` to `all_llamacpp_embedding_models()`
- Update all call sites (grep for `CpuEmbedding`, `cpu_embedding_model`, `all_cpu_embedding_models`)
- Update tests in registry.rs

### Step 4: Add embedding capability to `GpuDevice`
- **File:** `crates/u-forge-core/src/hardware/gpu.rs`
- Add field: `pub embedding: Option<Arc<dyn EmbeddingProvider>>`
- In `from_registry()`: make it `async`, check `registry.llamacpp_embedding_model()`, create `LemonadeProvider` if found, push `DeviceCapability::Embedding`
- Add `embedding_model: Option<&str>` param to `new()` (or add a `with_embedding()` builder method)
- Add `has_embedding(&self) -> bool`
- Embedding does NOT need `GpuResourceManager` locking — llamacpp embedding on Lemonade Server uses a different execution path from whispercpp STT
- Update tests

### Step 5: Add embedding capability to `CpuDevice`
- **File:** `crates/u-forge-core/src/hardware/cpu.rs`
- Add field: `pub embedding: Option<Arc<dyn EmbeddingProvider>>`
- In `from_registry()`: make it `async`, check `registry.llamacpp_embedding_model()`, create `LemonadeProvider` if found, push `DeviceCapability::Embedding`
- Add `has_embedding(&self) -> bool`
- Update tests

### Step 6: Create `WeightedEmbedDispatcher`
- **New file:** `crates/u-forge-core/src/queue/weighted.rs`
- Core structs:

```rust
pub(super) struct WeightedEmbedDispatcher {
    workers: Vec<WeightedWorkerSlot>,
}

struct WeightedWorkerSlot {
    queue: Arc<WorkQueue<EmbedJob>>,
    weight: u32,
    name: String,
    idle: Arc<AtomicBool>,
}
```

- `add_worker(weight, name) -> (Arc<WorkQueue<EmbedJob>>, Arc<AtomicBool>)` — returns queue + idle flag for the worker task
- `submit(job: EmbedJob)`:
  1. Find all workers with `idle == true`
  2. If any idle: push to highest-weight idle worker's queue
  3. If none idle: push to highest-weight worker's queue (natural backpressure)
  4. Call `notify_one()` on the target queue
- `pending() -> usize` — sum of all worker queue depths
- `worker_count() -> usize`
- Register module in `queue/mod.rs`

### Step 7: Update `run_embed_worker` to manage idle flag
- **File:** `crates/u-forge-core/src/queue/workers.rs`
- Add `idle: Arc<AtomicBool>` parameter to `run_embed_worker`
- Set `idle.store(true, Relaxed)` before `notified.await`
- Set `idle.store(false, Relaxed)` when a job is popped
- The flag is best-effort (brief race window is acceptable)

### Step 8: Update `InferenceQueueBuilder` to use config + weighted dispatch
- **File:** `crates/u-forge-core/src/queue/builder.rs`
- Add `config: DeviceConfig` field (defaults to `DeviceConfig::default()`)
- Add `.with_device_config(config: DeviceConfig) -> Self` method
- In `build()`, replace the single shared `embed_queue` with a `WeightedEmbedDispatcher`:
  - If `config.embedding.npu_enabled`: spawn NPU embedding workers with `npu_weight`
  - If `config.embedding.gpu_enabled`: spawn GPU embedding workers with `gpu_weight`
  - If `config.embedding.cpu_enabled`: spawn CPU embedding workers with `cpu_weight`
  - Extra embedding providers: spawn with `cpu_weight` (or add a field for custom weight)
- `with_embedding_provider()` continues to work as before (backward compat)

### Step 9: Update `InferenceQueue` to use `WeightedEmbedDispatcher`
- **File:** `crates/u-forge-core/src/queue/dispatch.rs`
- Replace `embed_queue: Arc<WorkQueue<EmbedJob>>` with `embed_dispatcher: Arc<WeightedEmbedDispatcher>`
- `embed()` calls `self.embed_dispatcher.submit(job)` instead of `self.embed_queue.push(job)`
- `embed_many()` unchanged (still creates per-text jobs)
- `QueueStats.pending_embeddings` calls `embed_dispatcher.pending()`

### Step 10: Update `cli_demo.rs`
- **File:** `crates/u-forge-core/examples/cli_demo.rs`
- Load `DeviceConfig::load_default()` early
- Pass config to builder via `.with_device_config(config)`
- Replace `registry.cpu_embedding_model()` with `registry.llamacpp_embedding_model()`
- When GPU embedding is enabled in config, create GPU embedding workers via `GpuDevice` instead of standalone providers
- Show config-driven device status in the diagnostic output

## Critical Files
- `crates/u-forge-core/src/config.rs` (new)
- `crates/u-forge-core/src/queue/weighted.rs` (new)
- `crates/u-forge-core/src/queue/builder.rs`
- `crates/u-forge-core/src/queue/dispatch.rs`
- `crates/u-forge-core/src/queue/workers.rs`
- `crates/u-forge-core/src/hardware/gpu.rs`
- `crates/u-forge-core/src/hardware/cpu.rs`
- `crates/u-forge-core/src/lemonade/registry.rs`
- `crates/u-forge-core/examples/cli_demo.rs`
- `crates/u-forge-core/Cargo.toml`

## Verification

1. `cargo test --workspace -- --test-threads=1` — all existing tests pass after renames
2. Unit tests for `DeviceConfig` — default values, TOML parsing, missing file gracefully returns defaults
3. Unit tests for `WeightedEmbedDispatcher` — single worker, prefer higher weight, fallback when busy
4. Unit tests for `GpuDevice`/`CpuDevice` embedding capability advertisement
5. Integration test (requires Lemonade Server): build queue with config, verify embedding works through weighted dispatch
6. `cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_demo` — verify it works with zero env vars (project anti-pattern #1)
