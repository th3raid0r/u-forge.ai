# Core Library Refactoring Plan

Two goals:
1. **Make things generic** — decouple the hardware abstraction from Lemonade-specific types, reduce imports in device files
2. **Adopt well-known crates** — replace hand-rolled primitives where crates reduce maintenance burden

---

## Phase 1: Eliminate Duplication in Hardware Module (High Value, Low Risk) — **complete**

### 1A. Extract `init_llamacpp_embedding()` to `hardware/mod.rs`

**Problem:** `cpu.rs` and `gpu.rs` contain a **character-for-character identical** 24-line function:

```u-forge.ai/crates/u-forge-core/src/hardware/cpu.rs#L62-85
async fn init_llamacpp_embedding(
    registry: &LemonadeModelRegistry,
    load_opts: Option<&ModelLoadOptions>,
    capabilities: &mut Vec<DeviceCapability>,
    device_label: &str,
) -> Option<Arc<dyn EmbeddingProvider>> {
    // ... identical in both files ...
}
```

The same `match load_opts { Some → new_with_load, None → new }` pattern also appears **5 times** across all three device files.

**Action:**
- Move `init_llamacpp_embedding()` to `hardware/mod.rs` as `pub(crate)`.
- Extract `init_embedding_provider(base_url, model_id, load_opts) -> Result<Arc<dyn EmbeddingProvider>>` for the shared match pattern.

### 1B. Extract `with_embedding()` builder method

**Problem:** `CpuDevice::with_embedding()` and `GpuDevice::with_embedding()` are identical except for the log string.

**Action:** Parameterize via the device name that already exists on `self`.

### 1C. Unify `DeviceWorker` trait impl boilerplate

**Problem:** All three `impl DeviceWorker` blocks are structurally identical. `CpuDevice` hardcodes `HardwareBackend::Cpu` while the others store it as a field.

**Action:** Add a `backend` field to `CpuDevice`. Consider a `DeviceBase` struct or `impl_device_worker!` macro for the three identical methods.

---

## Phase 2: Add Missing Provider Traits (Medium Value, Medium Risk) — **complete**

### 2A. Make `LemonadeSttProvider` implement `TranscriptionProvider`

**Problem:** `LemonadeSttProvider` (GPU-managed) does NOT implement the existing `TranscriptionProvider` trait. Its `transcribe()` returns `TranscriptionResult { text }` instead of `String`. This forces a **separate worker function** in the queue module:

```u-forge.ai/crates/u-forge-core/src/queue/workers.rs#L222-253
pub(super) async fn run_gpu_stt_worker(
    queue: Arc<WorkQueue<TranscribeJob>>,
    stt: LemonadeSttProvider,  // ← concrete type, not trait object
    device_name: String,
) {
    // ... identical to run_transcribe_worker except for .map(|r| r.text)
}
```

**Action:**
- Add `impl TranscriptionProvider for LemonadeSttProvider` in `lemonade/stt.rs`.
- Delete `run_gpu_stt_worker`.
- Use `run_transcribe_worker` for both NPU and GPU STT.

### 2B. Trait-objectify `GpuDevice.stt` field

After 2A, change `GpuDevice.stt` from `Option<LemonadeSttProvider>` to `Option<Arc<dyn TranscriptionProvider>>` (the trait already exists).

### 2C. Remove `DisabledEmbeddingProvider` sentinel in `NpuDevice`

**Problem:** `NpuDevice.embedding` uses a sentinel type that returns errors, while the other devices use `Option<Arc<dyn EmbeddingProvider>>`.

**Action:** Make it `Option` like the others. Remove the sentinel type.

---

## Phase 3: Decouple Device Construction from Lemonade Registry (Medium Value, Medium Risk) — **complete**

### 3A. Move `from_registry()` logic to a Lemonade factory module

**Problem:** Every device file's `from_registry()` directly imports `LemonadeModelRegistry`, `ModelLoadOptions`, `ModelConfig`, and various Lemonade provider constructors. This is the primary source of Lemonade-specific imports leaking into the hardware abstraction.

Current imports in `hardware/gpu.rs`:
```u-forge.ai/crates/u-forge-core/src/hardware/gpu.rs#L68-80
use crate::ai::embeddings::{EmbeddingProvider, LemonadeProvider};
use crate::config::ModelConfig;
use crate::lemonade::{
    GpuResourceManager, LemonadeChatProvider, LemonadeModelRegistry, LemonadeSttProvider,
};
use crate::lemonade::ModelLoadOptions;
```

**Action:**
- Create `lemonade/device_factory.rs` — owns all `from_registry` logic for all three devices.
- Device files retain only constructors that accept already-resolved provider trait objects.
- **Result:** Hardware module imports drop from ~10 Lemonade types per file to zero.

---

## Phase 4: Separate Trait Definitions from Provider Implementations (Medium Value, Low Risk) — **complete**

### 4A–B. Split `LemonadeProvider` and `LemonadeTranscriptionProvider` into their own files

**Problem:** `ai/embeddings.rs` defines both the `EmbeddingProvider` trait AND the `LemonadeProvider` impl. The file can't compile without `async_openai::*` and `crate::lemonade::*`.

**Action:** Move provider impls to `lemonade/embedding.rs` and `lemonade/transcription.rs` (or sub-modules under `ai/`). Trait files become dependency-free.

### 4C. Remove backward-compat re-exports

`ai/embeddings.rs` still re-exports transcription types from the old pre-split path. Clean this up.

---

## Phase 5: Adopt Well-Known Crates (Low-Medium Value, Low Risk) — **complete**

### 5A. Replace `mime_for_filename()` with `mime_guess` — **complete**

Implemented: `mime_guess` crate added to `Cargo.toml`; `mime_for_filename` now delegates to `mime_guess::from_path().first()` and falls back to `"audio/wav"`. Return type changed from `&'static str` to `String`; all callers updated.



Current hand-rolled code covers only 5 extensions:
```u-forge.ai/crates/u-forge-core/src/ai/transcription.rs#L252-265
pub fn mime_for_filename(filename: &str) -> &'static str {
    let lower = filename.to_lowercase();
    if lower.ends_with(".mp3") { "audio/mpeg" }
    else if lower.ends_with(".ogg") { "audio/ogg" }
    // ... 3 more branches
}
```

`mime_guess` is likely already a transitive dep via `reqwest` and covers hundreds of formats.

### 5B. Replace `embed_many` with `futures::future::try_join_all`

The current 35-line `JoinSet` + index-tracking + `Option` reassembly in `queue/dispatch.rs` does what `try_join_all` does in 2 lines, and `futures` is already a dependency.

### 5C. Use `thiserror` in `error.rs`

`thiserror = "2.0"` is in `Cargo.toml` but `error.rs` hand-writes `Display` and `From`. Switch to `#[derive(thiserror::Error)]` for a free `std::error::Error` impl.

---

## Phase 6: Queue Module Cleanup (Medium Value, Low Risk) — **complete**

### 6A. Extract generic worker loop

Five of six worker functions share identical structure. Extract `run_worker_loop<J, F>` — eliminates ~100 lines of boilerplate.

### 6B. Remove redundant `has_*` boolean fields

`InferenceQueue` has 5 `bool` fields alongside 5 `usize` worker counts. The bools are always `count > 0`. Derive them instead.

---

## Phase 7: Minor Cleanup (Low Value, Trivial Risk) — **complete**

- **7A.** Remove dead `import_default_data` method (never called, violates anti-pattern §1).
- **7B.** Remove `async` from `SchemaManager` methods that contain no `.await`.

---

## NOT Doing (and Why)

| Considered | Verdict | Reasoning |
|---|---|---|
| Replace `AppConfig` TOML with `figment`/`config-rs` | **Skip** | 60 lines, well-tested, "code defaults first" rule. Layered frameworks encourage env-var requirements. |
| Replace `estimate_token_count` with `tiktoken-rs` | **Skip** | Intentionally coarse heuristic. Lemonade models don't use OpenAI BPE tokenizers. |
| Replace `fts5_sanitize` with a crate | **Skip** | 15 lines, 9 tests, no crate does FTS5 sanitization. |
| Replace `GpuResourceManager` with `Semaphore` | **Skip** | Asymmetric STT-vs-LLM priority can't be expressed with a semaphore. |
| Add `TtsProvider`/`ChatProvider`/`RerankProvider` traits | **Defer** | Single-implementor traits are speculative. Extract when a 2nd backend arrives. |
| Replace `WorkQueue<T>` with `flume` | **Defer** | 30 lines, correct, `parking_lot` already required by `dashmap`. |

---

## Execution Order — **all phases complete**

```
Phase 1  (hardware dedup)            ✓ done
Phase 5B-C (crate adoption)         ✓ done
Phase 7  (minor cleanup)             ✓ done
Phase 2A (STT trait impl)            ✓ done
Phase 6  (queue cleanup)             ✓ done
Phase 2B-C (trait-objectify)         ✓ done
Phase 5A (mime_guess)                ✓ done
Phase 3  (factory extraction)        ✓ done
Phase 4  (trait/impl split)          ✓ done
```
