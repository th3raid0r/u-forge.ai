# Feature: Refactor Test Suite

Reduce Lemonade integration test surface, add an explicit toggle mechanism, and fix test quality issues.

---

## Context

The test suite has 226 tests: 197 unit tests and 29 integration tests requiring a live Lemonade Server. Several integration tests are redundant (duplicate coverage across modules, test LLM behavior rather than u-forge code, or make zero assertions). The skip guard pattern is inconsistent across modules — transcription tests use a different mechanism than everything else. There is no way to make integration tests mandatory in CI or explicitly skip them.

---

## Phase 1: Toggle Mechanism

**File:** `crates/u-forge-core/src/test_helpers.rs`

Replace the single re-export with a proper `integration_test_url()` function and `require_integration_url!()` macro controlled by `UFORGE_INTEGRATION_TESTS` env var:

| Env var value | Behavior |
|---|---|
| unset | Probe localhost; skip silently if unreachable (current default behavior preserved) |
| `"require"` | Probe localhost; **panic** if unreachable |
| `"skip"` | Always skip, even if server is reachable |
| any URL | Use that URL; **panic** if unreachable |

The macro replaces the 4-line skip guard boilerplate:

```rust
// Before (repeated in every integration test):
let Some(url) = lemonade_url().await else {
    eprintln!("Skipping: no Lemonade Server reachable");
    return;
};

// After:
let url = require_integration_url!();
```

Keep the existing `lemonade_url` re-export until all tests are migrated, then remove it.

---

## Phase 2: Fix Quality Issues

### 2a. Inconsistent skip guard in transcription tests

**File:** `crates/u-forge-core/src/ai/transcription.rs` (line 273-275)

The transcription test module defines its own synchronous `fn lemonade_url() -> Option<String>` that only checks the `LEMONADE_URL` env var (no localhost probe). This means transcription integration tests silently skip even when a server IS running on localhost. Replace with the shared `require_integration_url!()` macro.

### 2b. Miscategorized unit test

**File:** `crates/u-forge-core/src/hardware/gpu.rs` (line ~537)

`test_stt_blocked_when_llm_active` is placed under the "Integration tests" comment header but only uses `GpuResourceManager` in-process — no server needed. Move it above the integration comment block.

### 2c. Assertion-free tests (test nothing)

| Test | File | Problem |
|---|---|---|
| `test_from_registry_discovers_tts_model` | `hardware/cpu.rs:423` | Makes no assertions. Comment says "We do not assert specific capabilities." |
| `test_from_registry_discovers_models` | `hardware/gpu.rs:569` | Same — prints summary, asserts nothing. |

**Action:** Delete both. If "does it crash" coverage is desired, a single `test_device_from_registry_smoke` that asserts the device summary is non-empty would suffice.

### 2d. Error-path tests that depend on environment state

| Test | File | Problem |
|---|---|---|
| `test_try_new_auto_fails_without_url` | `ai/embeddings.rs` | Has inverted skip guard (skips when server IS reachable). Fragile. |
| `test_transcription_manager_fails_without_url` | `ai/transcription.rs:322` | Same pattern. |

**Action:** Convert both to proper unit tests. Use an unreachable address (e.g., `http://192.0.2.1:1`) and pass it explicitly rather than relying on ambient env state.

---

## Phase 3: Prune Redundant Integration Tests

### 3a. Registry tests — 6 become 1

**File:** `crates/u-forge-core/src/lemonade/registry.rs` (lines 477-568)

Six integration tests each fetch the registry independently and check one model role. Consolidate into a single `test_registry_fetch_and_classify` that fetches once and asserts all expected roles. Saves 5 redundant HTTP round trips.

| Remove | Reason |
|---|---|
| `test_registry_identifies_npu_embedding_model` | Consolidated |
| `test_registry_identifies_tts_model` | Consolidated |
| `test_registry_identifies_stt_model` | Consolidated |
| `test_registry_identifies_llm_model` | Consolidated |
| `test_registry_by_role_roundtrip` | Consolidated |

Keep `test_registry_fetch_returns_models` as the base, extend it with the role assertions.

### 3b. TTS tests — 3 become 1

**File:** `crates/u-forge-core/src/lemonade/tts.rs` (lines 157-216)

| Remove | Reason |
|---|---|
| `test_tts_multiple_voices` | Voice string is just a JSON field — Rust code doesn't process it differently per voice. Tests server behavior, not u-forge code. |
| `test_tts_long_text` | Same HTTP path as short text — no chunking or different code path in Rust. |

Keep `test_tts_returns_audio_bytes` as the single canonical TTS smoke test.

### 3c. CPU TTS duplicates TTS module — 2 tests removed

**File:** `crates/u-forge-core/src/hardware/cpu.rs` (lines 445-500)

| Remove | Reason |
|---|---|
| `test_speak_returns_audio_bytes` | Identical to `tts::test_tts_returns_audio_bytes`. `CpuDevice::speak()` is a one-line delegation. |
| `test_speak_with_different_voices` | Identical to `tts::test_tts_multiple_voices` (which is also being removed). |

### 3d. Chat tests — 4 become 2

**File:** `crates/u-forge-core/src/lemonade/chat.rs` (lines 260-341)

| Remove | Reason |
|---|---|
| `test_chat_with_system_prompt` | `ask_with_system` just prepends a system message to the array. Tests LLM behavior (does it follow a system prompt?), not u-forge code. |
| `test_chat_multi_turn_conversation` | Same HTTP call with more messages. No multi-turn state in Rust. Tests LLM behavior, not u-forge code. |

Keep `test_chat_ask_returns_response` (canonical smoke) and `test_chat_request_with_overrides` (tests `complete()` with overrides path).

### 3e. Embedding tests — 2 removed

**File:** `crates/u-forge-core/src/ai/embeddings.rs`

| Remove | Reason |
|---|---|
| `test_lemonade_embed_single` | Redundant with `test_lemonade_provider_connect_and_dimensions` which already exercises the same embed endpoint. |
| `test_embedding_manager_try_new_auto` | Thin wrapper — `try_new_auto(Some(&url), None)` routes to `try_new_lemonade`. The connect test already proves the provider works. |

### 3f. Transcription tests — 2 removed

**File:** `crates/u-forge-core/src/ai/transcription.rs`

| Remove | Reason |
|---|---|
| `test_transcription_manager_try_new_auto` | `TranscriptionManager::try_new_auto` with explicit URL is synchronous and does no HTTP. No integration value. |
| `test_lemonade_transcribe_returns_string_via_manager` | Duplicates `test_lemonade_transcribe_silence_wav` through a thin Arc wrapper. Unit test `test_manager_from_provider` already covers the Arc delegation. |

---

## Phase 4: Migrate Remaining Tests to New Macro

Replace all remaining `let Some(url) = lemonade_url().await else { ... return; };` patterns with `let url = require_integration_url!();` across all test modules. Remove the `lemonade_url` re-export from `test_helpers.rs` once migration is complete.

---

## Phase 5: Documentation

Update `.rulesdir/testing-debugging.mdc`:
- Document `UFORGE_INTEGRATION_TESTS` env var and its modes
- Update skip guard examples to use the new macro
- Update the integration test count (29 -> ~11)

---

## Summary

| Metric | Before | After |
|---|---|---|
| Integration tests | 29 | ~11 |
| Lemonade HTTP round trips per full run | ~35+ | ~8 |
| Assertion-free tests | 2 | 0 |
| Skip guard implementations | 2 (inconsistent) | 1 (unified macro) |
| Env var for toggle | none | `UFORGE_INTEGRATION_TESTS` |

---

## Verification

1. `cargo test --workspace -- --test-threads=1` passes with zero env vars (all integration tests skip)
2. `UFORGE_INTEGRATION_TESTS=require cargo test --workspace -- --test-threads=1` runs all integration tests and fails loudly if server is down
3. `UFORGE_INTEGRATION_TESTS=skip cargo test --workspace -- --test-threads=1` runs only unit tests even if server is available
4. `cargo clippy --workspace -- -D warnings` passes
5. No test name references remain to deleted tests in any documentation
