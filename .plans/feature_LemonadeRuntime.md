# Feature Plan: Lemonade Connection and Runtime Coordination

## Status and constraints

Approved, not implemented. Lemonade is OpenAI-compatible where practical but
has a custom management plane and reasoning/load behavior. Preserve the mixed
transport strategy. The application must still start and pass tests with no
server, URL, or API key configured.

Reasoning mode is part of effective loaded-model identity. A mode change must
perform the backend-appropriate reload and remain serialized through inference.

## Connection and discovery

- [ ] **LR-01 — Shared connection context.** Introduce a normalized connection
  value containing base URL, optional credential, redacted debug output, and
  phase-specific HTTP clients/timeouts. Thread it through probing, catalog,
  load/reload, custom chat, `async-openai`, and Rig.
- [ ] **LR-02 — Optional authentication.** Read `LEMONADE_API_KEY` as an
  override, propagate it consistently, and never log it. No key remains valid
  for the normal local server.
- [ ] **LR-03 — Partial catalog.** Require `/models`; treat `/health` and
  `/system-info` as optional enrichment. Retain endpoint-specific failures so
  the UI can explain degraded discovery.
- [ ] **LR-04 — Current wire data.** Preserve health recipe options, busy or
  streaming state, server version, model context limits, and capacity data.
  Accept additive unknown response fields.
- [ ] **LR-05 — Live authority.** Compare requested effective profiles with
  live health state. Treat the in-process runtime cache and `already_loaded`
  list as optimizations, never sole authority.

## Effective profiles and selection

- [ ] **LR-06 — Unified selected profile.** Resolve model, recipe, backend,
  device, load options, context limit, sampling options, tool capability, and
  reasoning policy together. Device fallback changes the entire profile and
  produces a visible diagnostic.
- [ ] **LR-07 — Context reconciliation.** Clamp or reject configured load,
  history, reserve, direct-generation, and agent limits against the catalog's
  model context window. Use the same effective limits on direct and agent paths.
- [ ] **LR-08 — Capacity-aware embeddings.** Respect server capacity by model
  type. With one embedding slot, choose HQ when explicitly enabled and standard
  otherwise; build both only when capacity supports both.
- [ ] **LR-09 — Tool capability gating.** Register graph tools only for models
  whose catalog capabilities support tool calling. Fall back to direct chat
  with a clear UI explanation.

## Reasoning and serialized execution

- [ ] **LR-10 — Three-state policy.** Add `Default`, `Enabled`, and `Disabled`
  reasoning modes. The UI toggle maps to explicit enabled/disabled; callers may
  retain a model/server default.
- [ ] **LR-11 — Recipe-aware load adapter.** For llama.cpp reasoning-capable
  models, generate the managed chat-template configuration required at load.
  Reject conflicts with user-provided flags owned by u-forge. Do not forward
  llama.cpp arguments to FLM or unrelated recipes.
- [ ] **LR-12 — Request compatibility hint.** Continue sending
  `enable_thinking` where supported in addition to the effective reload; do not
  treat the request Boolean as proof the backend changed mode.
- [ ] **LR-13 — Runtime execution lease.** Replace activation-only locking with
  a coordinator/lease held across health comparison, reload, request startup,
  and the complete response stream. Cancellation/drop releases the lease.
- [ ] **LR-14 — External reload detection.** Invalidate local state when health
  reports a different recipe/profile or the server was changed by another
  client.

## Transport responsibilities

- [ ] **LR-15 — Keep the intentional split.** Custom HTTP owns management,
  reranking, and direct-chat deviations. `async-openai` owns compatible
  embedding/audio calls. Rig owns the agent/tool loop.
- [ ] **LR-16 — Shared chat semantics.** Normalize request options, reasoning
  events, text, finish reason, usage, and errors above direct and Rig adapters.
- [ ] **LR-17 — Coordinated queue generation.** Route `InferenceQueue`
  generation and streaming through the same profile/coordinator contract;
  remove direct-provider stream bypasses.
- [ ] **LR-18 — Strict SSE parser.** Support arbitrary byte fragmentation and
  multiple events per chunk; surface malformed protocol payloads, finish
  reasons, usage, and server error bodies instead of silently dropping them.

## Timeout policy

- [ ] **LR-19 — Separate timeout classes.** Configure connect, metadata,
  load/readiness, first-token, stream-idle, and total non-stream completion
  independently. Defaults: 5s connect, 30s metadata, 300s load, 120s first
  token, 60s stream idle, and 300s non-stream completion.
- [ ] **LR-20 — GPU release.** Every timeout/error/cancel path must release the
  runtime and GPU guards before reporting failure. Do not use the archived
  blanket five-second generation timeout.

## Tests and acceptance

- Use an in-process mock server for API-key propagation, partial discovery,
  current health shapes, unknown fields, capacity, context validation, and
  profile comparison; no live server is required.
- Verify default/on/off effective load bodies, exactly one reload per effective
  change, no reload for an unchanged live profile, external reload detection,
  and serialization through stream completion.
- Cover preferred-device fallback as a coherent profile and prove non-tool
  models never receive graph tool declarations.
- Cover SSE fragmentation, multiple buffered events, reasoning deltas,
  malformed JSON, server errors, finish reason, usage, timeout, and receiver
  cancellation.
- Optional live tests remain skip-guarded and recipe/model-specific.

## Primary external references

- <https://lemonade-server.ai/docs/api/openai/>
- <https://lemonade-server.ai/docs/api/lemonade/>
- <https://lemonade-server.ai/docs/guide/configuration/>
- <https://lemonade-server.ai/docs/guide/cli-chat/>
- <https://github.com/lemonade-sdk/lemonade/issues/1511>
