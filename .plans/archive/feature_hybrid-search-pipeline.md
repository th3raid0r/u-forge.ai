# Feature: Staged Hybrid Search Pipeline

## Status: Completed — 2026-08-24

- **Primary candidate remediated:** `CORE-01`
- **Bundled supporting candidate remediated:** `AGENT-07`
- **Acceptance outcome:** `ALLOW-11` removed.
- **Implementation commits:** `608d8ac`, `9dfde0b`, `f28916a`, `ab70e78`.

## Goal

Replace the monolithic hybrid-search control flow with an explicit staged retrieval pipeline. The rewrite must reduce the number of search policies and failure rules that can only be understood by reading `search_hybrid_response_with_cancellation` end to end.

This is not complete if the current function is merely divided into similarly stateful helpers. A successful design gives retrieval lanes, fusion, hydration, reranking, and outcome assembly explicit inputs and outputs while leaving one small coordinator responsible for sequencing.

## Why this ranks first

`CORE-01` is the strongest measured hotspot in the inventory: 776 physical lines, cognitive complexity 92, duplicated standard/HQ lane mechanics, and three repeated fusion blocks. It also sits on a public boundary used by desktop search and agent tools, so improving it reduces complexity in core retrieval and its callers rather than in one isolated view.

## Current authority and affected code

- `crates/u-forge-core/src/search/mod.rs` — public search types and compatibility entry points.
- `crates/u-forge-core/src/search/pipeline.rs` — staged retrieval, fusion, aggregation, hydration, reranking, and response assembly.
- `crates/u-forge-core/src/search/sanitize.rs` — FTS-only query sanitization.
- `crates/u-forge-agent/src/tools/search.rs` — typed semantic/hybrid request adapter.
- `crates/u-forge/src/search_panel.rs` — desktop cancellation owner.
- `crates/u-forge-core/src/queue/dispatch.rs` — cancellable embedding and reranking submission boundary consumed by search.

The public `SearchResponse`, `SearchStageOutcomes`, and compatibility entry points remained stable.

## Required invariants

- One parent `queue::CancellationToken` governs all standard embedding, HQ embedding, and reranking work.
- Cancellation and supersession terminate the operation; they are never converted into degraded stage outcomes.
- FTS, standard semantic, HQ semantic, and reranking retain the current `Applied`, `IntentionallySkipped`, `Unavailable`, and `Failed` distinctions.
- A failed or unavailable stage preserves successful results from other stages.
- Semantic lanes execute only when a live queue capability exists and its provider fingerprint is compatible with the stored lane.
- Standard and HQ vector spaces remain independent. HQ is additive and cannot substitute for the standard-lane contract.
- The original query goes to embedding and reranking. Only FTS receives `fts5_sanitize` output.
- Reciprocal-rank fusion retains the current weights and `K = 60` behavior unless separately approved with ranking evidence.
- Chunk evidence, node-level aggregation, hydration behavior, and deterministic descending ordering remain stable.
- Nodes deleted between retrieval and hydration are skipped; actual storage failures remain errors.
- A malformed successful reranker response is a failed reranking stage, not a partially applied success.
- `KnowledgeGraph` remains synchronous and AI-independent; the pipeline coordinates graph and queue calls from outside the graph facade.

## Target design

Introduce an internal request/context and explicit stage results rather than an async trait hierarchy:

1. A search request/context owns the graph reference, standard/HQ queues, query, normalized config, cancellation token, and stage outcomes.
2. FTS and each semantic lane return normalized ranked chunk evidence plus one stage outcome.
3. A pure fusion step merges ranked evidence and performs node aggregation.
4. Hydration converts ranked node accumulators into `NodeSearchResult` values.
5. Reranking validates and applies scores or returns the unchanged RRF result set plus a failed/unavailable outcome.
6. The public coordinator sequences those stages and assembles `SearchResponse`.

Prefer concrete functions and enums over a generic “pipeline framework.” The standard and HQ lane executor may share mechanics, but lane identity, graph target, fingerprint, and outcome slot must remain explicit.

## Implementation stages

### 1. Characterize observable behavior

- Add table-driven tests for FTS-only, semantic-only, hybrid, dual semantic lane, empty, and punctuation-only queries.
- Lock down stage outcomes and safe diagnostics for missing capability, fingerprint mismatch, embedding failure, ANN failure, hydration deletion, reranker failure, and malformed reranker success.
- Add cancellation checks before retrieval, during embedding, and during reranking.
- Lock down result ordering, source evidence, matched chunks, and node aggregation.

### 2. Normalize retrieval lane output

- Give FTS, standard ANN, and HQ ANN one internal ranked-evidence shape.
- Centralize standard/HQ capability and fingerprint eligibility without erasing their distinct storage methods or diagnostics.
- Keep queue submissions on `EmbeddingProvider`/`InferenceQueue`; do not introduce direct provider calls.
- Move verbose per-lane diagnostics behind the lane result rather than retaining three parallel formatting blocks in the coordinator.

### 3. Extract pure fusion and aggregation

- Replace the three repeated RRF insertion blocks with one evidence-aware merge operation.
- Separate chunk fusion from node aggregation so each can be tested without SQLite or inference providers.
- Preserve all source-specific evidence fields and the current score weighting.

### 4. Isolate hydration and reranking

- Hydration receives ranked node accumulators and performs only graph reads and connected-node resolution.
- Reranking owns document construction, queue submission, response validation, score application, and fallback outcome.
- Check the parent token before expensive follow-up work and before returning a final response.

### 5. Reduce orchestration and caller duplication

- Reduce `search_hybrid_response_with_cancellation` to request initialization, stage sequencing, and response assembly.
- Keep the results-only compatibility wrappers thin.
- Introduce a typed agent search request only if it removes the repeated semantic/hybrid wrapper protocol; use it to resolve `AGENT-07` and reassess `ALLOW-11`.
- Keep desktop and agent callers responsible for parent-token ownership.

## Acceptance criteria

- `CORE-01` no longer concentrates retrieval, fusion, hydration, reranking, diagnostics, and response assembly in one control-flow tree.
- Standard and HQ retrieval share mechanics without sharing vector identity or storage targets.
- RRF insertion exists in one implementation.
- Stage outcome mutation is owned by stage results or one assembly boundary, not scattered through the coordinator.
- Agent semantic and hybrid tools share one typed execution adapter without hiding their intentional config differences.
- `ALLOW-11` was removed with the oversized helper signature.
- Existing public search behavior and graceful degradation remain compatible.

## Validation

Completed serially on 2026-08-24:

```bash
cargo test -p u-forge-core search -- --test-threads=1
cargo test -p u-forge-agent -- --test-threads=1
make clippy
make test-ci
```

Final results: 51 focused search tests and 32 agent tests passed; `make clippy` passed; `make test-ci` passed 348 workspace and 16 patched `cosmic-text` tests; `make test` passed 525 workspace and 16 patched `cosmic-text` tests, with the owned embedded runtime shutting down cleanly.

## Dependencies and sequencing

The search design was implemented against the existing cancellable queue boundary without creating a second lifecycle abstraction. The later [unified inference queue lifecycle](feature_inference-queue-lifecycle.md) revalidated these submission points through the complete workspace suites.

## Out of scope

- Rewriting ingestion embedding orchestration (`CORE-06`).
- Changing ranking constants or claiming a relevance improvement without evaluation data.
- Adding a third embedding space or a generic embedding-space registry.
- Moving AI capability ownership into `KnowledgeGraph`.
- Treating function-size reduction alone as remediation.
