# Feature: Unified Inference Queue Lifecycle

## Status: Completed — 2026-08-24

- **Primary candidate remediated:** `CORE-05`
- **Bundled supporting candidates remediated:** `CORE-09`, `CORE-11`
- **Acceptance outcome:** `ALLOW-08` removed after verifying that idle state had no dispatch authority.
- **Implementation commits:** `12a01de`, `9ab6048`, `f9263fc`, `df44f1f`, `c982dc1`, `d70cbbf`, `4fbb5d7`, `bce58f7`, `6baa901`.

## Goal

Create one internal lifecycle protocol from queue submission through terminal accounting while retaining capability-specific provider execution. The rewrite should make cancellation, timing, metrics, tracing, completion delivery, and worker teardown consistent by construction.

This is not a request for one universal generic worker. Streaming, embedding retry/EWMA behavior, and one-shot capabilities are materially different. The common abstraction ends at lifecycle ownership; provider payloads and execution policy stay capability-specific.

## Why this ranks second

Queue lifecycle mechanics are repeated at both ends of every AI capability. Submission duplicates channels, `JobContext`, queue insertion, cancellation cloning, metrics ownership, and completion construction. Workers duplicate pending-cancellation handling, spans, timing, terminal metrics, logging, and response delivery. Every future queue capability otherwise expands this correctness surface.

## Current authority and affected code

- `crates/u-forge-core/src/queue/jobs.rs` — job payloads, completion handles, cancellation, and context.
- `crates/u-forge-core/src/queue/dispatch.rs` — public capability submission APIs.
- `crates/u-forge-core/src/queue/workers.rs` — worker loops and terminal behavior.
- `crates/u-forge-core/src/queue/lifecycle.rs` — metrics and lifecycle helpers.
- `crates/u-forge-core/src/queue/telemetry.rs` — spans and queue statistics.
- `crates/u-forge-core/src/queue/builder.rs` — capability registration and task spawning.
- `crates/u-forge-core/src/queue/weighted.rs` — embedding routing, stealing, wakeups, and EWMA state.
- `crates/u-forge-core/benches/inference_lifecycle.rs` — evidence-retained routing and lifecycle scenarios.

## Required invariants

- Preserve every `InferenceError` distinction through the queue boundary.
- Pending cancellation does not invoke a provider.
- Active cancellation interrupts provider work, model activation, stream reads, embedding retry backoff, and bounded fan-out.
- Dropping a receiver is a fallback signal, not the normal cancellation contract.
- Each submission increments submitted metrics exactly once.
- Each started job records queue wait/service start exactly once and reaches exactly one terminal metric/span transition.
- Cancellation does not train embedding EWMA; successful work and terminal provider failures do.
- Streaming workers remain occupied until stream completion or cancellation.
- Streaming item delivery and awaitable terminal completion remain separate.
- A stream item receiver closing cancels remaining producer work and still completes the terminal future.
- Workers register `Notify::notified()` before checking queues so the lost-wakeup protection remains intact.
- Weighted routing remains `(pending + 1) × EWMA`, static weight remains a tie-breaker, and work stealing remains available.
- Retry and EWMA constants change only with benchmark evidence.
- Provider traits, `ProviderFactory`, and `InferenceQueueBuilder::with_provider` remain the capability boundary. Do not recreate `DeviceWorker`.

## Target design

Use two narrow internal lifecycle components:

1. A one-shot submission helper owns response-channel creation, `JobContext`, completion construction, and unavailable-capability behavior. Public methods still own capability checks, payload conversion, tracing fields, and queue choice.
2. A terminal reporter created when a worker begins owns timing, metrics, span completion, and final response delivery. Capability workers provide the provider future and capability-specific diagnostics.

Streaming gets a sibling reporter with one explicit terminal path, not a forced fit into the one-shot helper. Embedding retains its specialized retry and EWMA update logic but delegates start/finish accounting to the same lifecycle owner.

Avoid macros for control flow. The lifecycle should stay visible to the type checker and to tests.

## Implementation stages

### 1. Specify lifecycle transitions

- Add a matrix covering unavailable capability, pending cancellation, active cancellation, timeout classes, provider failure, receiver drop, worker drop, and success.
- Assert lifecycle counters and terminal classifications, not only returned values.
- Add streaming cases for model mismatch, lease acquisition failure, item-channel closure, provider error, cancellation, and normal completion.
- Retain deterministic routing/work-stealing tests before changing internals.

### 2. Centralize one-shot submission

- Introduce a private constructor for `InferenceJob<T>` plus its `JobContext` and response channel.
- Let each capability provide its payload-to-job closure and queue insertion.
- Keep embedding worker selection and selected-worker tracing explicit at the embedding call site.
- Keep await-only convenience methods as thin wrappers over explicit submission.

### 3. Centralize one-shot terminal reporting

- Introduce a small reporter that begins a job, owns elapsed time, records terminal metrics/span state once, and sends the result once.
- Convert LLM, rerank, transcription, and TTS workers to the reporter without erasing capability-specific provider calls or tracing fields.
- Preserve timeout classification at the provider boundary.

### 4. Give streaming one terminal authority

- Refactor the stream worker so model mismatch, lease failure, cancellation, provider failure, downstream receiver closure, and normal stream closure converge on one completion path.
- Keep stream items distinct from lifecycle completion.
- Ensure receiver closure cancels the parent token and completion is still observable.

### 5. Integrate embedding retry and EWMA

- Delegate pending cancellation, begin, and terminal accounting to the shared lifecycle owner.
- Keep retry attempts, cancellable backoff, stolen-work annotation, and EWMA policy in embedding-specific code.
- Prove that cancellation and timeout classes continue to avoid EWMA training.

### 6. Decompose builder registration

- Move capability-specific registration into private builder operations that return registration counts/state.
- Preserve the two-phase embedding registration required to construct the shared dispatcher before workers spawn.
- Derive provider counts and the sorted embedding fingerprint from registration output rather than parallel mutable bookkeeping where practical.

### 7. Resolve weighted idle ownership

- Verify whether the dispatcher reads `WeightedWorkerSlot::idle` or whether pending depth, global notification, and stealing fully determine routing.
- Remove stale slot state and its worker parameter if it has no behavioral authority.
- Do not start using `idle` merely to remove `ALLOW-08`; any dispatch-policy change requires benchmark evidence.

## Acceptance criteria

- One-shot job/channel/completion construction exists in one internal implementation.
- One-shot terminal metrics/span/response delivery exists in one implementation used by LLM, rerank, transcription, and TTS.
- Streaming has one auditable terminal completion path.
- Embedding shares lifecycle accounting while retaining specialized retry, routing, stealing, and EWMA policy.
- Builder capability dispatch is partitioned by capability without introducing a replacement hardware abstraction.
- Every started job records one and only one terminal result.
- `ALLOW-08` is removed or retained with a verified semantic reason.

## Validation

Completed serially on 2026-08-24:

```bash
cargo test -p u-forge-core queue -- --test-threads=1
make fmt-check
make clippy
make test-ci
cargo bench -p u-forge-core --bench inference_lifecycle -- --noplot
make test
```

Final results: 63 focused queue tests passed; formatting and workspace Clippy passed; `make test-ci` passed 370 workspace and 16 patched `cosmic-text` tests; `make test` passed 547 workspace and 16 patched `cosmic-text` tests with the owned embedded runtime shutting down cleanly.

The final deterministic benchmark reported no statistically significant change in any retained scenario. Cold and preseeded heterogeneous routing remained near 35 ms, retry recovery and lockstep recovery remained near 307 ms, and work stealing remained near 81 ms. Routing, retry, EWMA, and stealing constants were unchanged.

## Dependencies and sequencing

The rewrite landed after the staged hybrid-search pipeline and revalidated its queue submission points through the complete workspace suites. Search, chat, ingestion embedding, and future sandbox work now consume the unified lifecycle boundary and should not create local substitutes.

## Out of scope

- Changing provider selection or Lemonade model activation policy.
- Changing retry, EWMA, routing, or work-stealing constants without evidence.
- Combining all capabilities into one payload enum or universal provider trait.
- Reintroducing `DeviceWorker` or direct provider HTTP calls from business logic.
- Treating fewer worker functions as success if terminal behavior remains duplicated.
