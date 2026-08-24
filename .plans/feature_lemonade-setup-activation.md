# Feature: Catalog-Derived Lemonade Setup and Activation

## Status: Planned — impact rank 5

- **Primary candidate:** `UI-04`
- **Bundled supporting candidates:** `UI-01`, `UI-05`, `LEMON-08`

## Goal

Move Lemonade setup decisions out of GPUI rendering and ad hoc async loops. A fresh `LemonadeServerCatalog` plus persisted configuration should produce:

1. an immutable setup/readiness model,
2. a typed ordered provisioning plan, and
3. one chat activation/profile result used by both metadata-first preparation and full capability activation.

The UI should render derived state and emit user intent. It should not independently rediscover component, backend, recipe, model-selection, or profile-reconciliation policy.

## Why this ranks fifth

`SetupPanel::render` is a 750-line mixed decision/presentation function; provisioning repeats standard-component and chat mechanics; and chat profile construction has a 52-line contiguous clone across startup phases. These paths cross catalog discovery, backend management, downloads, configuration persistence, startup milestones, provider construction, and GPUI state. Establishing one catalog-derived plan removes competing decision sites instead of merely shortening render methods.

## Current authority and affected code

- `crates/u-forge/src/setup_panel.rs` — setup interaction state, derived rows, and rendering.
- `crates/u-forge/src/app_view/lemonade.rs` — discovery, metadata-first chat preparation, capability activation, provisioning, and management task ownership.
- `crates/u-forge/src/app_view/state.rs` — root Lemonade/init state and generation authority.
- `crates/u-forge-core/src/lemonade/catalog.rs` — required model catalog and optional health/system-info diagnostics.
- `crates/u-forge-core/src/lemonade/management.rs` — component state, backend choice, management requests, and SSE progress.
- `crates/u-forge-core/src/lemonade/selector.rs` — catalog/config model selection.
- `crates/u-forge-core/src/lemonade/provider_factory.rs` — provider construction.
- `crates/u-forge-core/src/config.rs` — persisted setup and chat configuration.
- `crates/u-forge/src/startup_tests.rs` — startup generation and degraded-mode behavior.

## Required invariants

- Every setup, provisioning, and activation decision begins from live server state fetched through `LemonadeServerCatalog::discover(connection)` or its shared-connection form.
- `/models` remains required. `/health` and `/system-info` failures remain optional diagnostics except where backend mutation specifically requires system-info data.
- No model, backend, device, or capability is assumed from environment variables or hardcoded availability.
- `LEMONADE_URL` remains only an override; graph-only startup works with zero environment variables and no server.
- Standard embedding is provisioned and activated before HQ; HQ remains additive.
- External server mutation requires explicit user confirmation.
- Owned-runtime settings are applied only to the owned embedded runtime.
- The management lock continues to serialize catalog mutations.
- Every management SSE operation is consumed through a terminal event.
- Setup may close/reopen without cancelling root-owned management work.
- Metadata-first chat readiness remains available before slower provider activation completes.
- Superseded initialization generations cannot apply late catalog/provider state.
- Persisted setup choices and in-memory `AppConfig` remain coherent.
- Provider creation continues through `ProviderFactory` and queue registration through `InferenceQueueBuilder`.
- Runtime acquisition still verifies live effective LLM state; catalog loaded IDs remain warm-start hints only.
- GPUI render/paint closures do not mutate state or call `cx.notify()`.
- Render decomposition is a maintainability change, not a performance claim.

## Target design

### Catalog-derived domain model

Unify ordinary and chat component classification behind one setup target/state operation in `u-forge-core::lemonade`. It should represent model identity, role, recipe/backend requirements, pull specification, and conflict/readiness state without GPUI types.

### Provisioning plan

Compile a selected setup request and a freshly discovered catalog into ordered typed steps: backend installations, model pulls, and the embedding-ready barrier. Execution remains under the root-owned management lock and forwards existing progress events.

### UI model

Build immutable component rows, backend groups, chat choices, readiness, conflicts, and diagnostics before `render()`. `SetupPanel` retains interaction-only state such as page, dropdown visibility, optional choices, and confirmation state.

### Chat activation

Use one function to build available model profiles, preferred selection, reconciled generation/context limits, agent budgets, diagnostics, provider, GPU manager policy, and runtime. Metadata-first and full capability activation consume the same result instead of duplicating construction.

## Implementation stages

### 1. Characterize catalog/config decisions

- Add pure matrix tests for no models, optional endpoint failures, standard-only embedding, standard plus HQ, missing/installable backend, external confirmation, unavailable preferred chat device, and invalid configured agent budget.
- Lock down provisioning order, backend deduplication, component conflicts, and the embedding-ready barrier.
- Preserve startup tests for degraded discovery and superseded generations.

### 2. Unify component classification

- Replace `component_state` and `chat_component_state` with one typed state classifier parameterized by an explicit setup target.
- Centralize recipe/model lookup and backend requirement derivation.
- Keep capability-specific filters explicit; do not infer support from model names when the catalog provides authoritative capability data.

### 3. Build an immutable setup model

- Derive component rows, backend groups, chat choices, status text inputs, conflicts, and readiness from catalog/config once per relevant state change.
- Keep presentation labels and GPUI theme values in the UI crate.
- Make `SetupPanel::render` consume the immutable model rather than recalculating setup policy.

### 4. Compile and execute a typed provisioning plan

- Convert `SetupRequested`, persisted configuration, and a fresh catalog into ordered typed steps.
- Use the same step representation for standard components and chat.
- Deduplicate backend installation by typed recipe/backend identity.
- Reject stale selected models before mutation.
- Execute serially under the management lock, forward every SSE event, signal embedding readiness only after required embedding steps, then rediscover the catalog.

### 5. Unify chat profile construction

- Extract the duplicated model selection, profile-limit reconciliation, agent-budget reconciliation, diagnostics, provider, GPU policy, and runtime construction.
- Reuse it in `prepare_lemonade_chat` and `activate_lemonade_capabilities`.
- Keep metadata-first preparation synchronous and non-loading.
- Keep full capability activation responsible for provider builds and queue construction.

### 6. Decompose rendering by stable model sections

- Render component status, backend selection, chat choice, downloads, footer/navigation, and confirmation from the immutable model.
- Preserve modal occlusion, focus behavior, event propagation, startup milestones, and root task ownership.
- Do not introduce many new GPUI entities/subscriptions solely to reduce line count.

### 7. Reconcile root lifecycle authority

- Keep actual GPUI entity updates in `AppView`.
- Consolidate generation checks and legal discovery/metadata/loading/ready/degraded/failed/superseded transitions in a GPUI-free state authority where this eliminates duplicate late-result checks.
- Do not broaden this slice into a full `AppView` root rewrite (`UI-11`).

## Acceptance criteria

- Setup policy is derived outside `SetupPanel::render` from a fresh catalog/config snapshot.
- Ordinary components and chat use one component state model and one provisioning step vocabulary.
- Backend installation, pull, progress forwarding, and terminal handling are implemented once.
- Metadata-first and full activation share one chat-profile construction path.
- Standard-before-HQ ordering, external confirmation, management serialization, and embedding readiness remain explicit and tested.
- Setup rendering is partitioned by immutable view-model sections without changing GPUI lifetime/caching contracts.
- `LEMON-08`, `UI-01`, `UI-05`, and the policy concentration in `UI-04` are resolved by ownership changes, not helper extraction alone.

## Validation

```bash
cargo test -p u-forge-core lemonade::management -- --test-threads=1
cargo test -p u-forge setup -- --test-threads=1
cargo test -p u-forge startup -- --test-threads=1
make clippy
make test-ci
```

Before remediation is marked complete, run `make test` for owned-runtime setup, management SSE, discovery, and activation coverage. If render structure changes materially, use the existing perf overlay as a regression check; do not infer a speedup from reduced function size.

## Dependencies and sequencing

This slice can begin with pure catalog/config tests, but implementation should consume the settled queue lifecycle from `feature_inference-queue-lifecycle.md`. `LEMON-03`, `LEMON-06`, and `LEMON-09` remain surfaced follow-ups; do not absorb them unless source changes prove they are required to establish one setup authority.

## Out of scope

- Making catalog snapshots authoritative for later runtime acquisition.
- Rewriting health/system-info transport models (`LEMON-03`) or all model-loading entry layers (`LEMON-06`).
- A full `AppView` construction/render decomposition.
- Adding a generic multi-provider control plane without a second backend.
- Claiming UI performance improvement without perf-overlay evidence.
