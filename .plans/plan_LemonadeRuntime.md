# Approved Plan: Lemonade Runtime and Embedded Distribution

## Status and upstream basis

Approved on 2026-08-04 and updated with the verified Cargo bootstrap on
2026-08-05. This is the decision-complete implementation plan for
the active checklist in `feature_LemonadeRuntime.md`. Source remains
authoritative where the repository has already implemented part of the older
runtime plan.

The implementation targets Embeddable Lemonade v11.5.1 on Ubuntu x64. The
release provides `lemonade-embeddable-11.5.1-ubuntu-x64.tar.gz`, uses port
13305 with `/v1` as the canonical API prefix, and includes current model IDs,
capacity and context metadata, runtime management, and request-scoped thinking
normalization. The old assumption that every reasoning-mode change must reload
the model is therefore no longer unconditional. Request controls are the
default, while the known llama.cpp load-time workaround remains available by
configuration until it has been validated across the models u-forge supports.

Primary upstream sources:

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

## Product decisions and constraints

- With no explicit URL, u-forge owns a private bundled `lemond` process. An
  explicit `LEMONADE_URL` selects an external server and suppresses embedded
  launch; u-forge never silently attaches to a process occupying the embedded
  port.
- The first supported packaged target is Ubuntu x64. Windows, macOS, Ubuntu
  arm64, and AUR publication are follow-up work.
- The application bundle includes only the pinned Lemonade runtime and required
  resources. Models and backends are acquired in a first-run/reopenable setup
  flow and stored in app-private persistent data.
- Setup always provisions a fixed standard embedding model and fixed reranker,
  offers the fixed HQ embedding model with opt-out, and lets the user choose a
  chat model. STT, TTS, image generation, audio generation, and 3D/STL
  generation are not exposed yet, but setup is structured so those roles can
  be added later.
- Settings UI is an interface to TOML, not a second settings store. Explicit
  user TOML values remain authoritative. Credentials are never persisted in
  TOML.
- Reasoning uses a three-state request policy (`Default`, `Enabled`,
  `Disabled`) plus one global control strategy. `request` is the default;
  `reload` retains the llama.cpp workaround without version gating.
- Configured context limits are clamped to live catalog limits and every clamp
  is explained. Direct and Rig paths use the same effective budgets.
- External management requires both `LEMONADE_API_KEY` and
  `LEMONADE_ADMIN_API_KEY`, plus explicit confirmation before pull/install.
  u-forge never shuts down or reconfigures an external process.
- The application and canonical tests must still work with no runtime artifact,
  URL, server, API key, or admin key.

## Embedded release and process lifecycle

### GitHub Release workflow

Provision the supported Embeddable Lemonade artifact from the UI crate's Cargo
build, and make the manual `workflow_dispatch` release consume that exact
output:

1. For Linux x86_64 GNU builds, download the exact v11.5.1 Ubuntu x64
   embeddable archive into Cargo's ignored `target/` cache and verify the
   committed SHA-256 digest before extraction. A normal offline build warns
   and remains graph-only when no artifact is available; release CI makes
   provisioning mandatory.
2. Patch `resources/server_models.json` idempotently so every built-in model
   named `Gemma-4-*GGUF` includes the empirically verified `reasoning` label.
   Reject an unexpected manifest shape or a pinned manifest with no matching
   models.
3. Exclude the optional `lemonade` CLI and web application. Install this stable
   relative layout beside the Cargo-built executable:

   ```text
   u-forge-<version>-ubuntu-x64/
     u-forge-ui-gpui
     lemonade/
       lemond
       LICENSE
       resources/
         server_models.json
         backend_versions.json
         defaults.json
   ```

4. Build `u-forge-ui-gpui` in release mode and have the Ubuntu x64 workflow copy
   Cargo's already verified and patched `target/release/lemonade/` directory.
   Verify executable modes, `lemond --version`, required files, and dynamic
   library resolution.
5. Produce `u-forge-<version>-ubuntu-x64.tar.gz` and its SHA-256 file, then
   create a draft GitHub Release containing both. GitHub-generated source
   archives remain part of the draft release.

The release artifact and digest are pinned inputs. Updating Lemonade requires
an intentional version/digest change and review of upstream release notes and
breaking changes. AUR packaging consumes a later published tarball but is not
implemented here.

### Runtime location and private data

- Installed lookup is strictly relative to the current executable:
  `lemonade/lemond`.
- `UFORGE_LEMOND_PATH` may override the binary for owned-runtime development.
  `LEMONADE_URL` remains the explicit external-server path.
- Create a writable Lemonade root below the platform per-user u-forge data
  directory. Copy packaged resource manifests into it atomically on first use
  and refresh versioned packaged resources on a runtime upgrade while
  preserving `config.json`, `recipe_options.json`, `user_models.json`,
  downloaded backends, and models.
- Seed owned defaults for loopback-only hosting, no UDP broadcast, disabled
  telemetry, and `models_dir="./models"`. The packaged installation directory
  remains read-only; all mutable Lemonade state lives in the private data root.

### Launch, readiness, and shutdown

- Try ports 13305 through 13315. A preflight free-port check is only an
  optimization: if `lemond` reports a bind failure, retry the next port. Never
  probe an occupant and treat it as the owned instance.
- Generate independent random API and admin secrets per launch unless their
  respective environment overrides are present. Pass them only to the child
  environment and connection context.
- Spawn the installed `lemond` with its private data root, loopback host, and
  selected port. Capture stdout/stderr into a bounded diagnostic buffer with
  credential redaction.
- Poll the unversioned `/live` endpoint until ready or the readiness timeout
  expires. A child exit before readiness is a structured startup failure.
- Keep the child handle outside provider objects so the application owns the
  full lifecycle. On normal exit, call `/internal/shutdown` with the admin key,
  wait for unload/exit, then terminate and finally kill only if the owned child
  does not stop within bounded grace periods.
- Missing artifacts, exhausted ports, child failure, and readiness failure are
  visible degraded-runtime diagnostics; none prevents the graph-only app from
  starting.

## Connection and discovery

### Shared connection interface

Introduce `LemonadeConnection` containing:

- normalized origin and API base;
- ownership (`Embedded` or `External`);
- optional API and admin secret values with fully redacted `Debug`;
- phase-specific clients and the timeout policy;
- helpers for API paths, origin paths such as `/live`, and `/internal` paths;
- builders/configuration for custom HTTP, `async-openai`, and Rig.

For the embedded runtime, use `http://127.0.0.1:<selected-port>/v1`. For an
external URL, accept an origin, `/v1`, or legacy `/api/v1` suffix; preserve an
explicit supported prefix and otherwise default to `/v1`.

Credential policy:

- API endpoints use `LEMONADE_API_KEY` when provided.
- Internal endpoints use `LEMONADE_ADMIN_API_KEY`; the owned runtime always has
  an independently generated or overridden admin key.
- Keyless external/local servers remain valid for discovery and inference.
- No credential may appear in logs, errors, URL query strings, serialization,
  or debug output.

### Partial catalog and current wire shapes

- `/models?show_all=true` is the only required discovery endpoint so setup can
  see built-in models that are not downloaded yet.
- Fetch `/health` and `/system-info` independently as optional enrichment.
  Store endpoint-specific errors so the UI can distinguish an unavailable
  endpoint from an empty capability.
- Preserve canonical model ID, checkpoint, recipe, labels, downloaded state,
  size, maximum context, recipe options, tool/reasoning capability, server
  version, loaded model type/device/backend, pinned/busy/streaming state, and
  capacity per model type.
- Update system information for the current GPU arrays and recipe/backend
  lifecycle states (`unsupported`, `installable`, `update_required`, and
  `installed`). Ignore additive unknown response fields.
- Remove unauthenticated, standalone system-info fetches; every caller uses the
  shared connection.

## Setup and configuration

### Extensible setup model

Represent setup work as role-based component descriptors and states rather
than hard-wiring the view to three models. The initial visible roles are:

- Standard embedding, required:
  - canonical catalog/inference model: `ggml-org/embeddinggemma-300M-GGUF`
  - `/pull` registration name: `user.ggml-org/embeddinggemma-300M-GGUF`
  - accepted catalog IDs: both prefixed and unprefixed forms
  - checkpoint: `ggml-org/embeddinggemma-300M-GGUF:Q8_0`
  - recipe: `llamacpp`
  - registration flag: `embedding=true`
- NPU embedding, selected when NPU embeddings are enabled:
  - canonical catalog/inference model: `embed-gemma-300m-FLM`
  - `/pull` built-in model name: `embed-gemma-300m-FLM`
  - pull mode: built-in (omit checkpoint, recipe, and capability registration
    fields; retain only the durable job controls)
  - accepted catalog IDs: both prefixed and unprefixed forms, preferring the
    canonical entry when both exist
  - checkpoint: `embed-gemma:300m`
  - recipe: `flm`
- Reranking, required: `bge-reranker-v2-m3-GGUF`.
- HQ embedding, default selected but optional:
  `Qwen3-Embedding-8B-GGUF`.
- Chat, required: one user-selected compatible catalog model.

If the canonical standard embedding registration is absent, register and pull
it through `/v1/pull` using the required `user.`-prefixed registration name.
Continue to use the unprefixed canonical ID for selection/inference, and accept
either form when detecting an existing compatible registration. If an existing
entry has a different checkpoint, recipe, or capability, block that component
with an actionable conflict instead of silently overwriting it.

FLM Embed Gemma is already in Lemonade's built-in registry. Pull it by the
canonical `embed-gemma-300m-FLM` model name only; including checkpoint, recipe,
or embedding fields turns the request into custom registration and incorrectly
requires a `user.` namespace. Discovery tolerates the legacy prefixed form but
uses the canonical catalog entry when both are present.

Use live system information and the configured backend preference order to
choose/install the backend for each selected model. Device fallback rebuilds
the entire profile and is shown before the user confirms downloads.

### Durable downloads and recovery

- Start model pulls with `stream=true, subscribe=false` so Lemonade owns the
  jobs.
- Restore progress from `/v1/downloads` whenever setup opens or the application
  restarts. The current `/v1/downloads/control` wire supports `pause`, `cancel`,
  and `remove`. Resume/retry removes a stopped job and repeats the same durable
  pull body, allowing Lemonade to reuse partial files.
- Treat backend installation as an idempotent application task. If the view or
  app closes, re-read `/system-info` next time and continue any still-required
  install/update.
- Show component, file, bytes, percent, current action, recoverable failure,
  and retry/cancel controls. Do not constrain server-owned downloads with a
  client total timeout.

### TOML persistence

- Use a comment- and unknown-key-preserving TOML editor with atomic
  temp-file/rename writes.
- Edit the configuration file that `AppConfig` actually loaded. When no file
  exists, create the per-user config path rather than a working-directory file.
- Persist `embedding.high_quality_embedding`, `chat.preferred_device`, and the
  selected `chat.<device>.model`. Existing explicit per-device overrides take
  precedence over automatic selection.
- Add `chat.reasoning_control = "request" | "reload"`, defaulting to
  `"request"`.
- Setup completeness is derived from TOML plus live catalog/backend/download
  state; do not add a separate opaque completion flag.

### External management

- External discovery/inference works with no admin credential when the server
  permits it.
- Expose setup mutations only when both API and admin credentials are present
  and a read-only admin probe succeeds.
- Require explicit confirmation before each external pull or backend install.
  Send the API credential to `/v1/pull` and `/v1/install`, because those are API
  endpoints; use the admin credential only for `/internal/*`.
- Require the durable download endpoints for external setup. If unavailable,
  report that the server is usable for inference but too old/incomplete for
  managed setup; do not fall back to an untracked client-owned download.
- Never call shutdown, change global runtime configuration, or delete models or
  backends on an external server.

## Effective profiles and selection

Introduce an `EffectiveProfile` that resolves together:

- model ID, checkpoint, recipe, backend, and device;
- normalized load options and effective loaded context;
- sampling and completion options;
- tool capability;
- reasoning policy and reasoning-control strategy;
- effective history/reserve/generation budgets;
- fallback, clamp, and degraded-authority diagnostics.

Derive a separate `LoadedProfileKey` containing only state that affects the
loaded backend: model, recipe, backend/device, normalized load options, and
reasoning mode only when the global strategy is `reload` for llama.cpp. Sampling
changes must not reload a model.

### Context reconciliation

1. Clamp configured load context to the catalog's maximum context when known.
2. Clamp chat context to the effective loaded context.
3. Clamp response reserve and direct/agent completion ceilings to that context.
4. Derive one remaining history/prompt budget and apply it to both direct and
   Rig paths.
5. Show a diagnostic for each changed value.
6. Reject only an unusable result with no positive prompt/response allocation,
   or a current system/user/tool prompt that cannot fit even after history is
   trimmed.

When catalog context is absent, retain configured limits and note that the
server limit could not be verified.

### Capacity and capabilities

- Treat health `max_models` as authoritative by model type. When embedding
  capacity is one, activate HQ if explicitly enabled and standard otherwise;
  activate both only when capacity supports both. Assume one when capacity is
  absent.
- Gate graph tool registration on the selected model's `tool-calling`
  capability. Non-tool models remain selectable but use direct chat with an
  explicit explanation.
- A backend/device fallback produces a new coherent profile, not a changed
  device field on the old one.

## Reasoning and runtime coordination

### Three-state request policy

Add `ReasoningPolicy::{Default, Enabled, Disabled}`. The existing UI toggle maps
to explicit enabled/disabled; non-UI callers may use `Default`.

With the default `request` strategy:

- omit thinking fields for `Default`;
- send `enable_thinking=true` for `Enabled`;
- send `enable_thinking=false` for `Disabled`;
- exclude reasoning from loaded-profile identity.

With the configured `reload` strategy:

- continue sending the same request field;
- for reasoning-capable llama.cpp models, own and generate the matching
  `--chat-template-kwargs` load argument and include the effective reasoning
  state in `LoadedProfileKey`;
- reject user `llamacpp_args` that contain the u-forge-owned
  `--chat-template-kwargs` flag;
- do not forward llama.cpp arguments to FLM or any unrelated recipe;
- for unsupported recipes, retain request control and surface that the reload
  workaround was not applicable.

This strategy is configuration-controlled, not version-gated. It keeps the
workaround available while letting the pinned runtime use its current
request-scoped behavior by default.

### Runtime execution lease

Replace activation-only locking with an RAII `RuntimeLease`:

- acquire it before comparing live state;
- hold it through any load/reload, request startup, and the complete direct
  response stream or Rig multi-turn agent run;
- release it on normal completion, receiver cancellation/drop, timeout, HTTP or
  protocol error, child exit, or task abort.

The lease serializes the LLM loaded-profile conflict domain. It does not block
independent embedding/reranking types merely because the server supports them.
GPU/device guards are acquired around actual load/inference calls, not across
Rig tool execution, so graph tools can perform embeddings without deadlock.

Before each lease-backed request, compare `LoadedProfileKey` with live health
recipe options. The in-process cache and `already_loaded` lists are only
optimizations. A live mismatch invalidates the cache and performs exactly one
required reload. If health is unavailable, explicitly load the effective
profile rather than trusting local state and emit a degraded-authority
diagnostic.

Route `InferenceQueue` generation and streaming through this same contract;
remove its direct-first-provider streaming bypass.

## Transport, events, and timeouts

### Intentional transport split

- Custom HTTP: `/live`, management, health/catalog, load/unload, reranking,
  direct-chat deviations, and setup jobs.
- `async-openai`: compatible embedding and currently supported audio calls.
- Rig: agent/tool loop.

All transports receive their URL, credential, and HTTP configuration from
`LemonadeConnection`; no provider may hardcode `Bearer lemonade` or an API key.

### Shared stream semantics

Normalize direct and Rig output above their adapters into events for:

- reasoning delta;
- text delta;
- tool call and tool result;
- terminal reason;
- input/output/total usage when present;
- structured fatal error.

Direct chat preserves the provider finish reason. Rig supplies aggregate usage
from its final response; when Rig does not expose a provider finish reason, use
an explicit agent-complete terminal reason rather than inventing `stop`.

### Strict SSE decoder

Use one incremental byte-oriented SSE decoder for direct chat and any future
subscribed setup operation. The implemented durable setup path uses
`stream=true, subscribe=false` and therefore receives an immediate JSON job
snapshot rather than SSE:

- arbitrary byte and UTF-8 fragmentation;
- LF and CRLF framing;
- multiple events per HTTP chunk;
- comments/keepalives, event names, and multi-line data;
- `[DONE]`, finish reason, usage, and reasoning fields;
- explicit malformed UTF-8, malformed JSON, unsupported payload, SSE error
  event, HTTP status, and bounded server-body errors.

Never silently skip malformed protocol data or decode with a lossy UTF-8 path.

### Timeout classes and cleanup

Use these independent defaults:

- connect: 5 seconds;
- metadata: 30 seconds;
- embedded readiness, model load, and backend install: 300 seconds;
- first semantic token: 120 seconds;
- stream idle: 60 seconds;
- total non-stream completion: 300 seconds.

Do not place a blanket total timeout on a streaming generation or server-owned
download. Every timeout/error/cancel path releases runtime and GPU guards before
the error reaches the UI.

## Tests and acceptance

### HTTP and catalog tests

Use an in-process mock server; no live Lemonade instance is required. Cover:

- embedded and external URL normalization, API/admin credential routing, and
  redaction;
- required models plus independently failing health/system-info;
- v11.5.1 health, system-info, unknown fields, canonical IDs, recipe options,
  context, capacity, busy/streaming state, and endpoint diagnostics;
- external read-only mode, credential validation, and confirmation gates;
- status codes and bounded server error bodies.

### Setup and configuration tests

- Verify the exact custom standard-embedding registration body, fixed
  reranker/HQ IDs, opt-out behavior, user chat selection, backend selection,
  and conflicting custom registration diagnostics.
- Verify server-owned job restoration and every download control operation.
- Verify comment/unknown-key-preserving TOML edits, atomic writes, active-file
  targeting, chat-device/model persistence, HQ persistence, and reasoning
  strategy defaults.

### Profile, reasoning, and coordination tests

- Verify default/enabled/disabled request bodies under `request` strategy.
- Verify the global `reload` fallback, managed llama.cpp arguments, conflict
  rejection, no FLM leakage, one reload per effective change, no reload for an
  unchanged live profile, and external reload detection.
- Verify coherent device fallback, all context clamps and diagnostics,
  capacity-aware standard/HQ activation, and tool gating.
- Prove lease serialization through stream completion and a complete Rig tool
  loop; prove receiver cancellation, errors, timeouts, and task abort release
  the lease and device guards.
- Prove `InferenceQueue` streaming no longer bypasses coordination.

### SSE and process tests

- Cover fragmented bytes and UTF-8, multiple buffered events, CRLF, multi-line
  data, reasoning deltas, malformed UTF-8/JSON, server error events, finish
  reason, usage, first-token timeout, idle timeout, and receiver cancellation.
- Test the process manager through an injectable fake child/spawner: runtime
  lookup, private-root initialization, port collision/retry, readiness,
  unexpected exit, graceful shutdown, termination/kill fallback, missing
  artifact, secret redaction, and no ownership of external processes.
- The packaging workflow verifies the archive layout and pinned runtime version.

Optional live tests remain skip-guarded and are pinned to v11.5.1 plus explicit
recipe/model requirements. Final verification is:

```text
make fmt-check
make check
make clippy
make test
```

## Boundaries with other work

This plan owns the runtime/process boundary, profile lease, provider
coordination, setup jobs, and cancellation-safe release of those resources.
The separate Inference Lifecycle plan continues to own generalized request IDs,
end-to-end queue cancellation beyond this boundary, and evidence-led
observability/tuning.

Explicitly out of scope here:

- AUR packaging and all non-Ubuntu-x64 release artifacts;
- bundling models or backends in the application archive;
- exposing STT, TTS, image, audio generation, or 3D/STL setup;
- replacing the intentional transport split or creating a generic provider
  abstraction.
