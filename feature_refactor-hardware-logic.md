# Feature: Refactor Hardware Abstraction to Trust Lemonade Server

**Status:** Complete — All phases (1–7) implemented  
**Branch:** `refactor-again`  
**Scope:** `crates/u-forge-core/src/hardware/`, `crates/u-forge-core/src/lemonade/`, `crates/u-forge-core/src/queue/`, `crates/u-forge-core/src/ai/`, `crates/u-forge-core/src/config.rs`

---

## Problem Statement

The current hardware abstraction layer models a rigid 3-tier compute hierarchy
(NPU → GPU → CPU) with **30 files totalling ~8,800 lines**. It manually tracks
which device can do what via 16-boolean `LemonadeCapabilities`, a 10-variant
`ModelRole` enum, 3 concrete device structs with ~24 combined constructors, a
10-function device factory, and a `DeviceWorker` trait that the queue builder
never actually dispatches through.

In practice, the Lemonade Server already knows everything about device/backend
capabilities. Its `/api/v1/models` endpoint tells us exactly which models are
available, what recipe they use (llamacpp, flm, whispercpp, kokoro), and what
labels they carry (embeddings, reranking, audio, tts, vision, etc.). Its
`/api/v1/system-info` endpoint enumerates installed backends and their device
affinities. Its `/api/v1/health` endpoint reports what's loaded and on which
device.

We are duplicating this knowledge in Rust with brittle pattern-matching and
manual classification. The refactoring goal is to **let the server be
authoritative** and reduce our code to:

1. A thin model catalog derived from the Lemonade API.
2. A unified provider type that wraps any lemonade-backed capability.
3. Per-model config overrides (context length, batch sizes, quality tier).
4. A duplicate-model guard for the llama.cpp cross-backend limitation.
5. The existing `InferenceQueue` dispatch, largely unchanged.

---

## Current Architecture (what exists today)

```
SystemInfo::fetch() ──► LemonadeCapabilities (16 bools)
                              │
                              ▼
LemonadeModelRegistry::fetch() ──► ModelRole (10 variants)
                              │        per-model classification
                              ▼
        ┌─ NpuDevice (9 constructors) ──┐
        │  GpuDevice (7 constructors)   │ via device_factory.rs (10 functions)
        │  CpuDevice (8 constructors)  ─┘
        ▼
InferenceQueueBuilder
  .with_npu_device(npu)      ← reads npu.embedding, npu.transcription, npu.chat
  .with_gpu_device(gpu)      ← reads gpu.embedding, gpu.stt, gpu.chat
  .with_cpu_device(cpu)      ← reads cpu.embedding, cpu.tts
  .build()                   ← spawns Tokio tasks → InferenceQueue
```

**Key observation:** The builder ignores the `DeviceWorker` trait entirely and
destructures concrete struct fields. The 3 device types are just bags of
`Option<Arc<dyn Provider>>` with a name and backend tag. The `DeviceWorker`
trait serves only logging.

---

## Target Architecture

```
LemonadeServerCatalog::discover(url)
  ├─ GET /api/v1/models        → Vec<CatalogModel>  (id, recipe, labels, downloaded, size)
  ├─ GET /api/v1/system-info   → SystemSnapshot      (devices, installed backends)
  └─ GET /api/v1/health        → HealthSnapshot       (loaded models, device assignments)
       │
       ▼
  ModelSelector (replaces ModelRole + LemonadeCapabilities + device_factory)
    - select_embedding_models()  → Vec<SelectedModel>  (low-quality + high-quality)
    - select_reranker()          → Option<SelectedModel>
    - select_stt_models()        → Vec<SelectedModel>
    - select_llm_models()        → Vec<SelectedModel>
    - select_tts()               → Option<SelectedModel>
    (each SelectedModel carries: model_id, recipe, resolved backend, load_opts)
       │
       ▼
  DuplicateGuard::check(selections)  → Result<(), conflict description>
    - Prevents identical model names across CPU + GPU llamacpp backends
       │
       ▼
  ProviderFactory::build(selected_model, http_client) → Box<dyn CapabilityProvider>
    - Single factory function, not 10
    - Internally branches on recipe/label to construct the right provider
       │
       ▼
  InferenceQueueBuilder (simplified)
    .with_provider(provider, capability, weight)   ← single registration method
    .with_config(app_config)
    .build() → InferenceQueue                      ← unchanged dispatch
```

### What stays the same

- **`InferenceQueue`** — the public API (`embed`, `transcribe`, `generate`,
  `synthesize`, `rerank`, `generate_stream`) is unchanged.
- **`WeightedEmbedDispatcher`** — multi-worker embedding dispatch with EWMA
  and work stealing stays.
- **Worker loops** in `queue/workers.rs` — unchanged.
- **`WorkQueue<T>`** job types — unchanged.
- **`LemonadeHttpClient`** / `client.rs` — unchanged.
- **Provider implementations** — `LemonadeProvider` (embedding),
  `LemonadeRerankProvider`, `LemonadeSttProvider`, `LemonadeTtsProvider`,
  `LemonadeChatProvider` all stay. They're already thin HTTP wrappers.
- **`GpuResourceManager`** — the STT/LLM GPU lock stays. It solves a real
  contention problem that the server doesn't manage for us.
- **`EmbeddingProvider` / `TranscriptionProvider` traits** — unchanged.
- **Per-model config overrides** (`ModelConfig` / `ModelLoadParams`) — stay,
  still needed for context length, batch sizes, and quality tiers.

### What gets deleted or collapsed

| Current file(s) | Lines | Disposition |
|---|---:|---|
| `hardware/mod.rs` | 356 | **Delete.** `DeviceCapability`, `HardwareBackend`, `DeviceWorker` trait all removed. Replace with a simple `Capability` enum (5 variants, no trait). |
| `hardware/npu.rs` | 388 | **Delete.** Replaced by `ProviderFactory` + generic model selection. |
| `hardware/gpu.rs` | 591 | **Delete.** Same. |
| `hardware/cpu.rs` | 407 | **Delete.** Same. |
| `lemonade/device_factory.rs` | 388 | **Delete.** 10 factory functions → 1 function in `ProviderFactory`. |
| `lemonade/system_info.rs` | 217 | **Rewrite → ~80 lines.** Keep `SystemInfo::fetch()` for display/logging. Delete `LemonadeCapabilities` (16 bools) — the catalog replaces it. |
| `lemonade/registry.rs` | 523 | **Rewrite → ~150 lines.** Drop `ModelRole` enum. `CatalogModel` is a flat struct with the fields lemonade gives us. Selection logic moves to `ModelSelector`. |
| `lemonade/model_limits.rs` | 62 | **Delete entirely.** `effective_ctx_size()` is vestigial — always returns a constant. Callers use `ModelConfig::ctx_size_for()` directly. |
| `lemonade/stack.rs` | 112 | **Delete.** Parallel construction path that bypasses the queue; no longer needed. |
| `lemonade/transcription.rs` | 180 | **Merge into `stt.rs`.** `LemonadeTranscriptionProvider` (no GPU lock) and `LemonadeSttProvider` (with GPU lock) become one type with an optional lock parameter. `TranscriptionManager` convenience wrapper deleted. |
| `lemonade/embedding.rs` (EmbeddingManager) | ~50 | **Delete `EmbeddingManager`.** `LemonadeProvider` stays; the manager is a pre-queue convenience that's now redundant. |
| `queue/builder.rs` | 366 | **Rewrite → ~150 lines.** No more per-device-type registration. Single `.with_provider()` method. |

**Estimated net reduction: ~2,400 lines deleted, ~400 lines of new code = ~2,000 fewer lines.**

---

## Detailed Design

### Phase 1: `LemonadeServerCatalog` (replaces registry + system_info + capabilities)

New file: `lemonade/catalog.rs` (~200 lines)

```rust
/// A model entry as returned by GET /api/v1/models.
/// No classification — just the raw server data plus our config overlay.
pub struct CatalogModel {
    pub id: String,
    pub recipe: String,           // "llamacpp", "flm", "whispercpp", "kokoro", "sd-cpp"
    pub labels: HashSet<String>,  // "embeddings", "reranking", "audio", "tts", etc.
    pub downloaded: bool,
    pub size_gb: Option<f64>,
    pub checkpoint: String,
}

/// Installed backend info from GET /api/v1/system-info → recipes.
pub struct InstalledBackend {
    pub recipe: String,          // "llamacpp"
    pub backend: String,         // "rocm", "vulkan", "cpu", "npu", "default"
    pub devices: Vec<String>,    // ["amd_igpu", "cpu"]
    pub state: String,           // "installed", "installable", "unsupported", ...
}

/// Snapshot of loaded models from GET /api/v1/health.
pub struct LoadedModel {
    pub model_name: String,
    pub recipe: String,
    pub device: String,          // "gpu", "npu", "cpu", "gpu npu"
    pub model_type: String,      // "llm", "embedding", "reranking", "audio", "tts"
    pub backend_url: String,
}

/// One-shot discovery: fetches /models, /system-info, /health and caches them.
pub struct LemonadeServerCatalog {
    pub base_url: String,
    pub models: Vec<CatalogModel>,
    pub backends: Vec<InstalledBackend>,
    pub loaded: Vec<LoadedModel>,
    pub processor: String,
    pub memory_gb: f64,
}

impl LemonadeServerCatalog {
    /// Single async constructor that fetches all three endpoints.
    pub async fn discover(base_url: &str) -> Result<Self>;

    /// Convenience predicates derived from the live data — NOT stored booleans.
    pub fn has_installed_backend(&self, recipe: &str, backend: &str) -> bool;
    pub fn has_npu(&self) -> bool;
    pub fn has_gpu(&self) -> bool;
    pub fn downloaded_models_with_label(&self, label: &str) -> Vec<&CatalogModel>;
    pub fn downloaded_models_with_recipe(&self, recipe: &str) -> Vec<&CatalogModel>;
    pub fn is_model_loaded(&self, model_id: &str) -> bool;
}
```

**Key difference from today:** No `ModelRole` classification. No 16-boolean
capability struct. Predicates are computed on-the-fly from the cached API data.

### Phase 2: `ModelSelector` (replaces device_factory + role-based lookups)

New file: `lemonade/selector.rs` (~200 lines)

```rust
/// A model we've decided to use, with its resolved backend and load options.
pub struct SelectedModel {
    pub model_id: String,
    pub recipe: String,
    pub backend: Option<String>,     // e.g. "rocm", "vulkan", "cpu"; None for non-llamacpp
    pub load_opts: ModelLoadOptions,
    pub quality_tier: QualityTier,   // for embeddings only
}

pub enum QualityTier {
    Standard,
    High,
    NotApplicable,
}

pub struct ModelSelector<'a> {
    catalog: &'a LemonadeServerCatalog,
    config: &'a ModelConfig,
}
```

Selection methods use a **preference list** pattern instead of hard-coded model
IDs. The preference lists themselves are configurable via `AppConfig` but ship
with sensible defaults:

```rust
impl<'a> ModelSelector<'a> {
    /// Returns embedding models to register as workers, ordered by priority.
    /// Filters to downloaded models with the "embeddings" label.
    /// Applies QualityTier based on config (high_quality_models list).
    /// Respects config.embedding.{npu,gpu,cpu}_enabled flags.
    pub fn select_embedding_models(&self) -> Vec<SelectedModel>;

    /// Returns the best available reranker (label: "reranking").
    pub fn select_reranker(&self) -> Option<SelectedModel>;

    /// Returns STT models (label: "audio" or "transcription").
    pub fn select_stt_models(&self) -> Vec<SelectedModel>;

    /// Returns LLM models (recipe: "llamacpp"/"flm", no embeddings/reranking/audio label).
    pub fn select_llm_models(&self) -> Vec<SelectedModel>;

    /// Returns TTS model (recipe: "kokoro" or label: "tts").
    pub fn select_tts(&self) -> Option<SelectedModel>;
}
```

**Backend resolution logic** (the only piece of "hardware reasoning" we keep):

For `llamacpp` models, the selector needs to pick a backend (`rocm`, `vulkan`,
or `cpu`). The rule is simple:

1. If the model already has a `recipe_options.llamacpp_backend` from the
   server (visible on `/api/v1/models/{id}`), use that.
2. Otherwise, check which llamacpp backends are installed (from catalog).
   Prefer `rocm` > `vulkan` > `cpu` (configurable order).
3. If only `cpu` is available, use `cpu`.

For `flm`, `whispercpp`, `kokoro` — the backend is implicit in the recipe
(NPU, CPU/Vulkan, CPU respectively). No decision needed.

### Phase 3: `DuplicateGuard` (new, ~40 lines)

New file: `lemonade/duplicate_guard.rs`

```rust
/// Lemonade cannot load two models with the same name on different backends.
/// This only affects llamacpp models that could run on both CPU and GPU.
/// FLM models have different names (*-FLM) so they're never in conflict.
pub struct DuplicateGuard;

impl DuplicateGuard {
    /// Returns Err if any model_id appears more than once across different
    /// llamacpp backends in the selection set.
    pub fn check(selections: &[SelectedModel]) -> Result<()>;

    /// Given a conflict, removes the lower-priority duplicate (CPU loses to GPU).
    pub fn deduplicate(selections: &mut Vec<SelectedModel>);
}
```

### Phase 4: `ProviderFactory` (replaces device_factory.rs + hardware/*.rs constructors)

New file: `lemonade/provider_factory.rs` (~150 lines)

```rust
/// Capability tag for queue registration. Replaces DeviceCapability enum.
pub enum Capability {
    Embedding,
    Transcription,
    TextGeneration,
    TextToSpeech,
    Reranking,
}

/// A constructed provider ready for queue registration.
pub struct BuiltProvider {
    pub name: String,                // human label for logging, e.g. "rocm/Qwen3-Embedding-8B"
    pub capability: Capability,
    pub provider: ProviderSlot,      // see below
    pub weight: u32,                 // dispatch weight (from config)
}

/// Type-safe union of the provider trait objects the queue accepts.
pub enum ProviderSlot {
    Embedding(Arc<dyn EmbeddingProvider>),
    Transcription(Arc<dyn TranscriptionProvider>),
    Chat(LemonadeChatProvider),
    Tts(LemonadeTtsProvider),
    Rerank(LemonadeRerankProvider),
}

impl ProviderFactory {
    /// Build a provider from a SelectedModel.
    /// - For embeddings: constructs LemonadeProvider, calls load, probes dimensions.
    /// - For reranking: constructs LemonadeRerankProvider, calls load.
    /// - For STT: constructs LemonadeSttProvider with optional GpuResourceManager.
    /// - For LLM: constructs LemonadeChatProvider with optional GpuResourceManager.
    /// - For TTS: constructs LemonadeTtsProvider.
    pub async fn build(
        selected: &SelectedModel,
        base_url: &str,
        gpu_manager: Option<Arc<GpuResourceManager>>,
    ) -> Result<BuiltProvider>;
}
```

**GPU resource manager attachment:** The factory checks if the selected model's
resolved backend uses the GPU (recipe=llamacpp + backend=rocm|vulkan, or
recipe=whispercpp + backend=vulkan). If so, and a `GpuResourceManager` is
provided, it wires the lock into the provider. This replaces the per-device-type
GPU awareness that was spread across `GpuDevice`, `LemonadeSttProvider`, and
`LemonadeChatProvider`.

### Phase 5: Simplified `InferenceQueueBuilder`

Rewrite `queue/builder.rs` (~150 lines, down from 366):

```rust
pub struct InferenceQueueBuilder {
    providers: Vec<BuiltProvider>,
    config: AppConfig,
}

impl InferenceQueueBuilder {
    pub fn new() -> Self;

    /// Register any provider. The builder routes it to the correct internal
    /// channel based on provider.capability.
    pub fn with_provider(self, provider: BuiltProvider) -> Self;

    /// Convenience: register all providers from a Vec (the output of the
    /// discovery pipeline).
    pub fn with_providers(self, providers: Vec<BuiltProvider>) -> Self;

    pub fn with_config(self, config: AppConfig) -> Self;

    /// Spawn worker tasks and return the queue handle. Same internal logic
    /// as today: embedding workers go through WeightedEmbedDispatcher,
    /// all others through WorkQueue<T>.
    pub fn build(self) -> InferenceQueue;
}
```

The builder no longer knows about NPU/GPU/CPU as concepts. It receives
`BuiltProvider` values and routes them by `Capability` tag.

### Phase 6: Streamlined top-level discovery flow

The examples (`cli_demo`, `cli_chat`) and any future UI main will use:

```rust
// 1. Discover what the server has
let catalog = LemonadeServerCatalog::discover(&url).await?;

// 2. Select models based on what's available + our config
let config = AppConfig::load_default()?;
let selector = ModelSelector::new(&catalog, &config.models);

let embed_selections = selector.select_embedding_models();
let reranker = selector.select_reranker();
let stt_selections = selector.select_stt_models();
let llm_selections = selector.select_llm_models();
let tts = selector.select_tts();

// 3. Guard against duplicate model conflicts
let mut all_selections = /* flatten the above */;
DuplicateGuard::deduplicate(&mut all_selections);

// 4. Build providers (loads models on the server)
let gpu_mgr = Arc::new(GpuResourceManager::new());
let mut providers = Vec::new();
for selected in &all_selections {
    match ProviderFactory::build(selected, &url, Some(Arc::clone(&gpu_mgr))).await {
        Ok(p) => providers.push(p),
        Err(e) => tracing::warn!("Skipping {}: {e}", selected.model_id),
    }
}

// 5. Build the queue
let queue = InferenceQueueBuilder::new()
    .with_providers(providers)
    .with_config(config)
    .build();
```

Compare this to the current `cli_demo` flow which manually checks 16 boolean
capability flags, calls 6 different factory functions, and has 3 separate
builder registration paths.

---

## `config.rs` Changes

### What stays

- `AppConfig`, `EmbeddingDeviceConfig` (weights + enabled flags) — unchanged.
- `ModelConfig` / `ModelLoadParams` (per-model ctx_size, batch_size, ubatch_size) — unchanged.
- `ChatConfig` — unchanged.

### What changes

Add to `ModelConfig`:

```rust
/// Models considered "high quality" for embedding. These get a separate
/// InferenceQueue with their own dispatch. Listed by model ID.
pub high_quality_embedding_models: Vec<String>,

/// Preferred llamacpp backend order. First installed backend wins.
/// Default: ["rocm", "vulkan", "cpu"]
pub llamacpp_backend_preference: Vec<String>,

/// Model preference lists per capability. First downloaded match wins.
/// If empty (default), any downloaded model with the right label is used.
pub embedding_model_preferences: Vec<String>,
pub reranker_model_preferences: Vec<String>,
pub stt_model_preferences: Vec<String>,
pub llm_model_preferences: Vec<String>,
pub tts_model_preferences: Vec<String>,
```

These replace the hard-coded model IDs currently scattered across
`registry.rs` (`npu_embedding_model()` prefers `"embed-gemma-300m-FLM"`,
`llm_model()` prefers `"GLM-4.7-Flash-GGUF"`, etc.) with user-configurable
preference lists that have the same defaults.

---

## Migration Strategy

### Phase execution order

The phases above are designed to be implemented sequentially with the codebase
compiling at each step:

1. ✅ **Phase 1 (Catalog):** `lemonade/catalog.rs` added (~250 lines). Fetches
   `/models`, `/system-info`, `/health` concurrently via `tokio::try_join!`.
   `LemonadeServerCatalog` with `discover()` + on-the-fly predicate methods.
   8 unit tests + 1 skip-guarded integration test. No deletions.

2. ✅ **Phase 2 (Selector):** `lemonade/selector.rs` added (~300 lines).
   `ModelSelector::new(catalog, models_config, embedding_config)` — takes
   `&EmbeddingDeviceConfig` as a third parameter (not in original spec) to
   support `npu/gpu/cpu_enabled` flags without coupling to `AppConfig`.
   `ModelConfig` extended with 7 new preference-list fields (defaults encode
   the same priorities previously hardcoded in `registry.rs`). Default LLM
   models updated: `qwen3.5-4B-FLM` (was `qwen3-8b-FLM`),
   `Gemma-4-26B-A4B-it-GGUF` (was `GLM-4.7-Flash-GGUF`). Nomic embed
   entries removed from `default_model_load_params`; all standardised on
   the embeddinggemma family. 17 unit tests.

3. ✅ **Phase 3 (DuplicateGuard):** `lemonade/duplicate_guard.rs` added
   (~160 lines). `check()` returns `Err` on conflict; `deduplicate()` resolves
   in-place, keeping `rocm > vulkan > metal > cpu`. Non-llamacpp entries are
   never touched. 12 unit tests.

4. ✅ **Phase 4 (ProviderFactory):** `lemonade/provider_factory.rs` added
   (~280 lines). `Capability` enum lives here (not on `SelectedModel`) to
   avoid a circular dependency with `selector.rs`. `ProviderFactory::build()`
   takes explicit `capability: Capability` and `weight: u32` parameters.
   STT dispatch: `whispercpp` + `gpu_manager` → `LemonadeSttProvider` (GPU
   lock); `flm` or `whispercpp` without gpu_manager →
   `LemonadeTranscriptionProvider` (no lock). `merge_backend()` injects the
   resolved `llamacpp_backend` into `ModelLoadOptions` before the `/load`
   call. 17 unit tests + 1 skip-guarded integration test. All 284 tests pass.

5. ✅ **Phase 5 (Builder rewrite):** Rewrote `queue/builder.rs` (~130 lines,
   down from 366). Single `.with_provider(BuiltProvider)` and
   `.with_providers(Vec<BuiltProvider>)` methods. All device-type registration
   paths removed. Updated `queue/dispatch.rs` integration tests to use
   `ProviderFactory::build()` flow.

6. ✅ **Phase 6 (Example migration):** Migrated `cli_demo` and `cli_chat` to
   the new `LemonadeServerCatalog → ModelSelector → ProviderFactory →
   InferenceQueueBuilder` flow. Removed all `NpuDevice`, `GpuDevice`,
   `CpuDevice`, `LemonadeModelRegistry` references from both examples.

7. ✅ **Phase 7 (Cleanup):** Deleted `hardware/` directory (4 files, ~1,742
   lines). Deleted `lemonade/device_factory.rs`, `lemonade/model_limits.rs`,
   `lemonade/stack.rs`, `lemonade/registry.rs`. Removed `EmbeddingManager`
   from `lemonade/embedding.rs` and `TranscriptionManager` from
   `lemonade/transcription.rs` (both superseded by `ProviderFactory`).
   Rewrote `lemonade/system_info.rs` to display/logging only (~105 lines, down
   from 217). Removed all stale re-exports from `lib.rs` and `ai/mod.rs`.
   Updated `from_registry` callers (`chat.rs`, `stt.rs`, `tts.rs`, `rerank.rs`,
   `gpu_manager.rs`) to use catalog-based construction.

### What callers need to change

| Caller pattern | Before | After |
|---|---|---|
| Probe hardware | `SystemInfo::fetch` → `lemonade_capabilities()` → check 16 bools | `LemonadeServerCatalog::discover()` → use predicate methods |
| Pick models | `registry.npu_embedding_model()` / `registry.llm_model()` / etc. | `ModelSelector::select_embedding_models()` / etc. |
| Build devices | `NpuDevice::from_registry_with_config(...)` | `ProviderFactory::build(selected, url, gpu_mgr)` |
| Register with queue | `builder.with_npu_device(npu).with_gpu_device(gpu).with_cpu_device(cpu)` | `builder.with_providers(providers)` |
| Check queue caps | `queue.has_embedding()` — **unchanged** | **unchanged** |
| Submit work | `queue.embed("text")` — **unchanged** | **unchanged** |

---

## Risks and Mitigations

### The Lemonade API could change model metadata formats

**Mitigation:** `CatalogModel` uses `String` for recipe and `HashSet<String>`
for labels. We don't enum-match on these at the catalog layer — only the
selector layer interprets them, and that's a single file to update.

### GPU resource contention is a real problem the server doesn't manage

**Mitigation:** `GpuResourceManager` is explicitly kept. The `ProviderFactory`
attaches it when the resolved backend targets the GPU. This is the one piece
of "hardware awareness" we retain because the server's LRU eviction doesn't
prevent two simultaneous inference calls from OOMing a shared-memory iGPU.

### The duplicate model guard is a workaround for a Lemonade bug

**Mitigation:** `DuplicateGuard` is isolated in its own file. When Lemonade
fixes this, we delete the file and remove the one call site. No architectural
impact.

### Preference lists could become stale as models are added/removed

**Mitigation:** Preference lists are a priority hint, not a hard requirement.
If no preferred model is downloaded, the selector falls back to "any downloaded
model with the right label." The defaults ship in `config.rs` and can be
updated in patch releases.

---

## Files Affected (complete list)

### New files
- `crates/u-forge-core/src/lemonade/catalog.rs` (~200 lines)
- `crates/u-forge-core/src/lemonade/selector.rs` (~200 lines)
- `crates/u-forge-core/src/lemonade/duplicate_guard.rs` (~40 lines)
- `crates/u-forge-core/src/lemonade/provider_factory.rs` (~150 lines)

### Rewritten files
- `crates/u-forge-core/src/queue/builder.rs` (366 → ~150 lines)
- `crates/u-forge-core/src/lemonade/system_info.rs` (217 → ~80 lines)
- `crates/u-forge-core/src/lemonade/registry.rs` (523 → deleted or ~50 line re-export shim)
- `crates/u-forge-core/src/lemonade/mod.rs` (131 → updated re-exports)

### Deleted files
- `crates/u-forge-core/src/hardware/mod.rs` (356 lines)
- `crates/u-forge-core/src/hardware/npu.rs` (388 lines)
- `crates/u-forge-core/src/hardware/gpu.rs` (591 lines)
- `crates/u-forge-core/src/hardware/cpu.rs` (407 lines)
- `crates/u-forge-core/src/lemonade/device_factory.rs` (388 lines)
- `crates/u-forge-core/src/lemonade/model_limits.rs` (62 lines)
- `crates/u-forge-core/src/lemonade/stack.rs` (112 lines)

### Modified files (minor edits)
- `crates/u-forge-core/src/lemonade/stt.rs` — absorb `transcription.rs`, add optional GPU lock param
- `crates/u-forge-core/src/lemonade/embedding.rs` — remove `EmbeddingManager`
- `crates/u-forge-core/src/lemonade/transcription.rs` — delete after merge into `stt.rs`
- `crates/u-forge-core/src/config.rs` — add preference lists and HQ model list
- `crates/u-forge-core/src/ai/embeddings.rs` — remove `EmbeddingManager` re-export
- `crates/u-forge-core/src/ai/transcription.rs` — remove `TranscriptionManager` re-export
- `crates/u-forge-core/src/lib.rs` — remove `pub mod hardware`, update re-exports
- `crates/u-forge-core/examples/cli_demo.rs` — rewrite setup flow
- `crates/u-forge-core/examples/cli_chat.rs` — rewrite setup flow

### Unchanged files
- `crates/u-forge-core/src/queue/dispatch.rs` — `InferenceQueue` public API unchanged
- `crates/u-forge-core/src/queue/workers.rs` — worker loops unchanged
- `crates/u-forge-core/src/queue/jobs.rs` — job types unchanged
- `crates/u-forge-core/src/queue/weighted.rs` — `WeightedEmbedDispatcher` unchanged
- `crates/u-forge-core/src/lemonade/chat.rs` — `LemonadeChatProvider` unchanged
- `crates/u-forge-core/src/lemonade/tts.rs` — `LemonadeTtsProvider` unchanged
- `crates/u-forge-core/src/lemonade/rerank.rs` — `LemonadeRerankProvider` unchanged
- `crates/u-forge-core/src/lemonade/load.rs` — `ModelLoadOptions` / `load_model()` unchanged
- `crates/u-forge-core/src/lemonade/client.rs` — HTTP client unchanged
- `crates/u-forge-core/src/lemonade/health.rs` — `LemonadeHealth` unchanged
- `crates/u-forge-core/src/lemonade/gpu_manager.rs` — `GpuResourceManager` unchanged

---

## Line Count Estimate

| Category | Before | After | Delta |
|---|---:|---:|---:|
| `hardware/` (4 files) | 1,742 | 0 | −1,742 |
| `lemonade/device_factory.rs` | 388 | 0 | −388 |
| `lemonade/model_limits.rs` | 62 | 0 | −62 |
| `lemonade/stack.rs` | 112 | 0 | −112 |
| `lemonade/registry.rs` | 523 | ~50 | −473 |
| `lemonade/system_info.rs` | 217 | ~80 | −137 |
| `lemonade/transcription.rs` | 180 | 0 | −180 |
| `lemonade/embedding.rs` (manager portion) | ~50 | 0 | −50 |
| `queue/builder.rs` | 366 | ~150 | −216 |
| New: `catalog.rs` | 0 | ~200 | +200 |
| New: `selector.rs` | 0 | ~200 | +200 |
| New: `duplicate_guard.rs` | 0 | ~40 | +40 |
| New: `provider_factory.rs` | 0 | ~150 | +150 |
| Config additions | 0 | ~30 | +30 |
| Example rewrites | ~0 | ~0 | ~0 (net neutral) |
| **Total** | | | **≈ −2,740** |

---

## Acceptance Criteria

- ✅ `cargo build --manifest-path crates/u-forge-core/Cargo.toml` succeeds
- ✅ `cargo test --manifest-path crates/u-forge-core/Cargo.toml -- --test-threads=1` passes with no env vars set (217 passing unit tests + 5 doc tests)
- ✅ `cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_demo` — migrated to catalog flow
- ✅ `cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_chat` — migrated to catalog flow
- ✅ No file in `src/hardware/` exists
- ✅ `DeviceWorker` trait, `NpuDevice`, `GpuDevice`, `CpuDevice` types are gone
- ✅ `LemonadeCapabilities` (16-bool struct) is gone
- ✅ `ModelRole` (10-variant enum) is gone
- ✅ Duplicate llamacpp model names across CPU/GPU backends are detected and prevented (`DuplicateGuard`)
- ✅ Per-model config overrides (ctx_size, batch_size, ubatch_size) still work
- ✅ Embedding quality tiers (standard vs high) still produce separate queues (wired in Phase 5/ingest)
- ✅ `GpuResourceManager` still coordinates GPU-resident STT and LLM workloads (`ProviderFactory` attaches it)