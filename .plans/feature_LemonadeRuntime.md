# Feature Checklist: Lemonade Runtime and Embedded Distribution

## Status and authority

Implemented and verified on 2026-08-05. `plan_LemonadeRuntime.md` is the
decision-complete specification for this checklist. Source is authoritative for
the existing connection probing, providers, model selection, activation-only
runtime cache, reasoning request hint, and UI integration introduced before
this revision.

Target Embeddable Lemonade v11.5.1 on Ubuntu x64 first. Preserve the intentional
mixed transport strategy and the graph-only degraded path. The application and
canonical tests must start with no bundled runtime, external server, URL, API
key, or admin key.

Reasoning is request-scoped by default. A global TOML strategy retains the
llama.cpp reload workaround without version gating until it is validated across
supported models; reasoning belongs to loaded-model identity only while that
fallback is selected.

## Embedded runtime and release

- [x] **LR-01 — Cargo bootstrap and draft release workflow.** The UI Cargo
  build pins, downloads, and verifies the v11.5.1 embeddable archive; patches
  all built-in Gemma 4 GGUF entries with the empirically verified `reasoning`
  label; and installs the minimal sibling `lemonade/` runtime. The manual
  Ubuntu x64 workflow requires that bootstrap, packages its exact output, and
  creates a complete draft GitHub Release with archive and checksum.
- [x] **LR-02 — Private runtime root.** Locate `lemonade/lemond` relative to the
  installed executable, support `UFORGE_LEMOND_PATH` for owned development,
  and initialize versioned resources plus persistent config/backends/models in
  app-private user data.
- [x] **LR-03 — Owned child lifecycle.** Start a loopback-only child on the
  first available port from 13305 through 13315 without attaching to an
  occupant; probe `/live`, retain bounded redacted diagnostics, detect exits,
  and degrade without preventing application startup.
- [x] **LR-04 — Safe shutdown.** Gracefully call authenticated
  `/internal/shutdown`, then terminate/kill only the owned child after bounded
  waits. Never shut down an external server.

## Connection and discovery

- [x] **LR-05 — Shared connection context.** Introduce a normalized origin/API
  value with ownership, separate optional API/admin secrets, redacted debug,
  and phase-specific HTTP clients. Thread it through probing, catalog,
  load/reload, setup, custom chat, `async-openai`, and Rig.
- [x] **LR-06 — Optional split authentication.** Read `LEMONADE_API_KEY` and
  `LEMONADE_ADMIN_API_KEY` as overrides, generate separate secrets for the
  owned child when absent, use each on the endpoint class Lemonade accepts, and
  never log or persist either key. Keyless external inference remains valid.
- [x] **LR-07 — Partial current catalog.** Require `/models?show_all=true`;
  treat `/health`
  and `/system-info` as independent enrichment. Preserve endpoint failures,
  current GPU/backend states, server version, recipe options, busy/streaming,
  context windows, capabilities, and capacity while accepting additive fields.
- [x] **LR-08 — Live authority.** Compare requested loaded-profile keys with
  health recipe options. Treat runtime cache and `already_loaded` as
  optimizations; detect external changes and explicitly load when health cannot
  establish authority.

## Setup and TOML settings

- [x] **LR-09 — Extensible setup model.** Build a reopenable, role-based setup
  flow that exposes required standard embedding, NPU FLM embedding when
  enabled, required reranker, optional-by-opt-out HQ embedding, and
  user-selected chat.
- [x] **LR-10 — Managed standard embedding recipe.** Pull the registration name
  `user.ggml-org/embeddinggemma-300M-GGUF` from
  `ggml-org/embeddinggemma-300M-GGUF:Q8_0` as a llama.cpp embedding model, while
  using `ggml-org/embeddinggemma-300M-GGUF` as the canonical catalog/inference
  ID and accepting either form during discovery. Pull the built-in FLM model
  by its canonical `embed-gemma-300m-FLM` ID without custom-registration
  fields; tolerate the legacy `user.` form during discovery, but prefer the
  canonical entry when both exist. Reject conflicting existing registrations.
- [x] **LR-11 — Durable provisioning.** Select/install compatible backends from
  live system information, start server-owned model downloads, restore
  `/v1/downloads` state, expose pause/cancel/remove, and implement resume/retry
  by repeating the exact durable pull after a stopped job is removed, without
  tying jobs to the setup view lifetime.
- [x] **LR-12 — TOML-backed settings.** Preserve comments and unknown keys,
  write atomically to the active/per-user config, and persist HQ selection,
  preferred chat device/model, and `chat.reasoning_control`. Do not create a
  second settings store.
- [x] **LR-13 — Guarded external management.** Require both credentials plus
  explicit confirmation for external pull/install. Keep older/incomplete
  external servers usable for discovery/inference but read-only for setup.

## Effective profiles and selection

- [x] **LR-14 — Unified effective profile.** Resolve selection, recipe,
  backend/device, load options, loaded identity, catalog context, request
  sampling, tool capability, reasoning policy/strategy, and diagnostics
  together. Rebuild the whole profile on device fallback.
- [x] **LR-15 — Context reconciliation.** Clamp load, chat, history, reserve,
  direct-generation, and agent limits to one catalog-backed effective budget;
  explain every clamp and reject only unusable prompt/response allocations.
- [x] **LR-16 — Capacity-aware embeddings.** With one embedding slot, activate
  HQ when enabled and standard otherwise; activate both only when capacity
  supports both, and conservatively assume one slot when capacity is absent.
- [x] **LR-17 — Tool capability gating.** Register graph tools only for models
  advertising tool calling. Keep non-tool models available through direct chat
  with a clear UI explanation.

## Reasoning and serialized execution

- [x] **LR-18 — Three-state request policy.** Add `Default`, `Enabled`, and
  `Disabled`; omit the request field for default and send explicit
  `enable_thinking` for enabled/disabled across direct and Rig paths.
- [x] **LR-19 — Configured reload fallback.** Default the global strategy to
  `request`. Under `reload`, include reasoning in llama.cpp loaded identity,
  generate the owned chat-template argument, reject flag conflicts, and never
  forward llama.cpp arguments to other recipes.
- [x] **LR-20 — Runtime execution lease.** Replace activation-only locking with
  an RAII coordinator held across live comparison, reload, request startup, and
  complete direct stream or Rig tool loop. Cancellation/drop/error releases it;
  device guards cover actual inference rather than tool execution.
- [x] **LR-21 — Coordinated queue generation.** Route `InferenceQueue`
  generation and streaming through the same effective-profile/coordinator
  contract and remove the direct-provider streaming bypass.

## Transport and failure semantics

- [x] **LR-22 — Preserve transport responsibilities.** Custom HTTP owns
  management, reranking, and direct-chat deviations; `async-openai` owns
  compatible embedding/audio calls; Rig owns the agent/tool loop. All consume
  the shared connection.
- [x] **LR-23 — Shared chat events.** Normalize request options, reasoning/text,
  tool activity, terminal reason, usage, and fatal errors above direct and Rig
  adapters without fabricating unavailable Rig finish reasons.
- [x] **LR-24 — Strict shared SSE parser.** Support byte/UTF-8 fragmentation,
  CRLF, multi-line data, and multiple events per chunk for chat and any future
  subscribed setup operation; the current durable setup path consumes the
  immediate JSON snapshot returned by `subscribe=false`. Surface malformed
  payloads, finish/usage, SSE errors, and bounded HTTP error bodies.
- [x] **LR-25 — Timeout classes.** Use 5s connect, 30s metadata, 300s
  readiness/load/backend install, 120s first token, 60s stream idle, and 300s
  non-stream completion. Do not impose a total timeout on streams or
  server-owned downloads.
- [x] **LR-26 — Resource release.** Every timeout, HTTP/protocol error, child
  exit, receiver cancellation, and task abort releases runtime/device guards
  before reporting failure.

## Tests and acceptance

- [x] Use an in-process mock server for URL/auth routing, redaction, partial
  discovery, current v11.5.1 shapes, unknown fields, capacity/context, external
  management gates, setup jobs, and server error bodies.
- [x] Verify exact custom embedding registration, fixed/optional setup roles,
  backend choice, durable download restoration/control, and preserving atomic
  TOML edits.
- [x] Verify default/on/off request bodies, global reload fallback, flag
  conflicts, no cross-recipe leakage, exactly one necessary reload, unchanged
  live profiles, and external reload detection.
- [x] Cover coherent device fallback, context clamp diagnostics,
  capacity-aware embedding activation, and non-tool model declarations.
- [x] Cover lease serialization through direct and Rig completion, queue
  coordination, guard release, cancellation, timeout, and child failure.
- [x] Cover SSE fragmentation, multiple buffered events, reasoning deltas,
  malformed UTF-8/JSON, server errors, terminal reason, usage, timeout, and
  receiver cancellation.
- [x] Test owned process lookup/private data, port retry, readiness, shutdown
  escalation, missing artifacts, and strict external non-ownership with a fake
  child/spawner.
- [x] Keep optional live tests skip-guarded and v11.5.1 recipe/model-specific;
  finish with `make fmt-check`, `make check`, `make clippy`, and `make test`.

## Deferred from this feature

- AUR and non-Ubuntu-x64 packaging.
- Bundled models or backends.
- Setup/UI for STT, TTS, image, audio generation, or 3D/STL generation.
- Generic provider abstraction and the broader cancellation/observability work
  owned by `feature_InferenceLifecycle.md`.

## Primary external references

- <https://github.com/lemonade-sdk/lemonade/releases/tag/v11.5.1>
- <https://lemonade-server.ai/docs/embeddable/>
- <https://lemonade-server.ai/docs/embeddable/runtime/>
- <https://lemonade-server.ai/docs/embeddable/backends/>
- <https://lemonade-server.ai/docs/embeddable/models/>
- <https://lemonade-server.ai/docs/api/openai/>
- <https://lemonade-server.ai/docs/api/lemonade/>
- <https://lemonade-server.ai/docs/guide/configuration/>
- <https://github.com/lemonade-sdk/lemonade/blob/v11.5.1/src/cpp/server/thinking_controls.cpp>
- <https://github.com/lemonade-sdk/lemonade/issues/1511>
