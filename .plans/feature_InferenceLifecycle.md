# Feature Plan: Inference Lifecycle, Cancellation, and Queue Evidence

## Status

Approved for post-foundation Alpha work. Not implemented. This plan covers
real backend cancellation and evidence collection; it does not pre-approve
EWMA seed constants, retry jitter, or a new metrics exporter.

## Cancellation contract

- [ ] **IL-01 — Cancellable submission.** Return a job handle containing a
  cancellation token and completion future for embed, transcribe, synthesize,
  generate, and rerank requests. Keep convenience await-only methods.
- [ ] **IL-02 — Queue removal/skip.** Cancelled pending jobs must be removed or
  skipped before provider invocation and must not contribute successful work
  metrics.
- [ ] **IL-03 — Retry cancellation.** Check cancellation before every attempt
  and during backoff; use cancellation-aware selection instead of uninterruptible
  sleeps.
- [ ] **IL-04 — Active provider cancellation.** Propagate cancellation through
  HTTP request futures, runtime activation/load, first-token wait, established
  streams, and batch embedding fan-out.
- [ ] **IL-05 — Parent operations.** One parent token governs an embedding plan,
  import indexing, search, or chat operation and all of its child jobs.
- [ ] **IL-06 — UI ownership.** Replacing or closing a GPUI task cancels its
  parent token and awaits/observes termination. A dropped receiver alone is a
  fallback, not the documented contract.
- [ ] **IL-07 — Result semantics.** Distinguish cancelled, timed out, superseded,
  provider-failed, and worker-dropped outcomes without logging expected user
  cancellation as an error.

## Observability before tuning

- [ ] **IL-08 — Queue spans.** Record queue wait, service time, retries,
  selected worker, steals, cancellation point, timeout class, and outcome.
- [ ] **IL-09 — Snapshot counters.** Extend queue stats with bounded counters
  and latency summaries needed by the UI/perf logs; do not expose credentials,
  prompts, or content.
- [ ] **IL-10 — Graph event lag.** Record broadcast lag/full-refresh recovery
  before changing capacity. Keep correctness fallback intact.
- [ ] **IL-11 — Server telemetry bridge.** Prefer consuming or linking
  Lemonade's existing metrics/OTLP information over duplicating server metrics
  inside u-forge.
- [ ] **IL-12 — Benchmark scenarios.** Measure cold heterogeneous routing,
  steady-state routing, capacity-one load churn, retry recovery, and work
  stealing with deterministic mock providers.

## Evidence-gated tuning

- [ ] **IL-13 — EWMA initialization decision.** Choose zero/fallback behavior
  or per-device seeds only after benchmark comparison. Record selected values
  and evidence in source comments/tests, not shared architecture docs.
- [ ] **IL-14 — Retry policy decision.** Add jitter or server-directed retry
  only if recovery tests show lockstep contention. Bound total retry time and
  honor cancellation.
- [ ] **IL-15 — Capacity/config decisions.** Change worker counts, broadcast
  capacity, or metric-export dependencies only from observed bottlenecks.

## Tests and acceptance

- Deterministic tests cover cancellation pending, during retry sleep, during
  model load, before first token, mid-stream, and during embedding batches.
- A cancelled operation performs no later graph/vector writes and releases GPU
  and runtime guards.
- Queue stats remain race-safe under concurrent completion/cancellation.
- Benchmark output establishes whether seed/jitter changes are warranted; no
  speculative constants land merely to close archived findings.
