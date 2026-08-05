# Bug Plan: Alpha Correctness and Diagnostics

## Status

Approved, not implemented. This plan consolidates verified correctness work
from the 2026-04 audit and the 2026-08 source review. It does not include the
Lemonade runtime redesign or the Zed workspace refactor.

## Outcomes

- Imports never choose an ambiguous node silently.
- Schema validation has consistent required-field semantics.
- Graph snapshots and viewport transforms remain finite and spatially correct.
- GPUI paint stays side-effect free apart from documented instrumentation.
- Search callers can distinguish requested, skipped, unavailable, and failed
  retrieval stages, and the UI surfaces degradation.
- Small, verified maintenance gaps are closed through canonical tests.

## Import and schema integrity

- [x] **AC-01 — Candidate-aware in-session resolution.** Replace the import
  `name -> ObjectId` map with `name -> candidates`. Preserve every same-name
  object across types rather than overwriting earlier entries.
- [x] **AC-02 — No persisted first-match fallback.** Resolve an edge endpoint
  only when the combined in-session/persisted candidate set is unique. On zero
  or multiple candidates, skip the edge and write a diagnostic containing the
  reference, object types, names, and IDs. Never use `results[0]`.
- [x] **AC-03 — Explicit qualification compatibility.** Accept existing plain
  names when unique. Add UUID and `object_type:name` qualification without
  widening schemas or inferring object types from records.
- [x] **AC-04 — Required-property contract.** Add a missing-required issue to
  `SchemaManager::validate_and_coerce_properties`; align its callers with the
  already-strict import boundary.
- [x] **AC-05 — Agent ambiguity diagnostics.** For large name matches, group
  candidates by object type and explain how to use a UUID instead of listing an
  arbitrary first five.

## Graph and viewport correctness

- [x] **AC-06 — Spatial rebuild policy.** Bulk-load the R-tree after committed
  graph snapshot refreshes. Keep drag-time local rebuilding, but remove the
  fragile deleted-entry micro-optimization whose benefit is dominated by the
  existing full node/edge fetch.
- [x] **AC-07 — Saved/unsaved placement.** Preserve saved nodes as fixed. Seed
  new nodes near connected saved neighbors, otherwise on a deterministic ring
  around the current graph extent; relax only unsaved nodes.
- [x] **AC-08 — Viewport invariants.** Make viewport fields private and require
  finite center/size plus finite positive zoom through constructors/setters.
  Apply the invariant to every transform, world rectangle, fit, and LOD path.
- [x] **AC-09 — Finite layout guard.** Reject or repair non-finite persisted
  positions at the storage/snapshot boundary and assert finite layout output.
- [x] **AC-10 — Measured convergence.** Add displacement/convergence metrics
  and small/large graph benchmarks before replacing the fixed iteration cap.
  Do not adopt guessed thresholds without measurements.

## GPUI state and destructive actions

- [x] **AC-11 — Text layout outside paint.** Move `TextFieldView` shaping,
  hit-test layout, origin, visible-size, and content-height updates into
  layout/prepaint preparation. Paint consumes prepared state only.
- [x] **AC-12 — Editor measurement outside paint.** Move `NodeEditorPanel`
  size measurement out of its paint closure. The perf timing canvas and
  GraphCanvas's local bounds cell remain documented exceptions.
- [x] **AC-13 — Path picker task ownership.** Store and replace the browse task
  so stale portal results cannot update a newer modal state.
- [x] **AC-14 — Embedding UI lifecycle.** Replace the epoch plus atomic cancel
  split with one authority. Until queue cancellation lands, label superseded
  work accurately rather than claiming it was cancelled.
- [ ] **AC-15 — Event propagation and confirmation.** Stop delete-button events
  from selecting their parent row. Require confirmation for node deletion and
  clear-data/clear-schema actions; preserve errors and cancellation cleanly.

## Search outcome contract

- [ ] **AC-16 — Structured stage outcomes.** Record an outcome for FTS,
  standard semantic, HQ semantic, and reranking: applied, intentionally
  skipped, unavailable, or failed with a safe diagnostic.
- [ ] **AC-17 — Runtime failure propagation.** Refactor hybrid search so embed,
  ANN, and rerank failures contribute outcomes while successful fallback
  results are retained.
- [ ] **AC-18 — UI presentation.** Preserve the structured response through
  `SearchPanel`; show a concise Zed-style status hint for degraded results and
  keep detailed diagnostics available for logs/tooltips.
- [ ] **AC-19 — Mode enablement.** Derive semantic availability from an actual
  compatible embedding lane, not merely `Option<InferenceQueue>` presence.

## Maintenance closure

- [ ] **AC-20** Reject unknown keys on `AppConfig` and nested configuration
  sections, with path-specific parse tests.
- [ ] **AC-21** Make schema saving synchronous and update all callers.
- [ ] **AC-22** Keep one FTS sanitizer implementation and reuse it from agent
  tools.
- [x] **AC-23** Do not install cursor-blink tasks for read-only text fields.
- [ ] **AC-24** Compile-check `convert_memorymesh` from the root Makefile/CI.
- [ ] **AC-25** Remove stale deprecated `EdgeType` guidance without changing
  backward-compatible code unless separately justified.

## Tests and acceptance

- Import tests cover same-file cross-type collisions, collisions with existing
  database nodes, UUID/type qualification, and diagnostic JSONL contents.
- Graph tests cover saved-plus-unsaved placement, non-finite stored values,
  finite transforms, and deletion/refresh while a drag is active.
- Search tests inject stage failures and prove results plus structured outcomes
  reach the UI-facing boundary.
- GPUI logic is separated into testable state reducers; manual verification
  uses the frame overlay to confirm paint changes do not create redraw loops.
- Final verification: `make fmt-check`, `make check`, `make clippy`, and the
  unfiltered `make test`, with no environment variables or live server.
