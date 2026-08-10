# Feature: TypeScript Agentic Sandbox

## Status: Active design gate — post Alpha

The workspace crate is a placeholder with no `deno_core` dependency or runtime
implementation. This document records the questions and invariants that must be
resolved before dependencies or code land; it is not implementation approval.

The Alpha correctness, Lemonade runtime, inference lifecycle, agent-budget, and
desktop-foundation prerequisites are complete. The sandbox consumes the
implemented `InferenceJob` / `StreamingInferenceJob` and parent
`CancellationToken` contracts rather than defining a parallel queue API.

## Product goal

An AI agent may generate a TypeScript program that executes in-process against
a deliberately small u-forge API. Each execution receives a fresh V8 isolate,
no ambient filesystem or network access, explicit CPU/time/memory limits, and a
schema-authoritative graph mutation boundary.

JSON-compatible DTOs are an acceptable op boundary. They do not imply that raw
`v8::Local<Value>` handles are persisted or cross isolate boundaries.

## Go/no-go decisions required

- Pin a reviewed `deno_core` release and verify its current `op2`, module-loader,
  isolate termination, heap-limit, and extension APIs from primary sources.
- Choose and benchmark the TypeScript transpiler for that release. Do not select
  `deno_ast` or SWC solely from this older design's dependency guesses.
- Produce a threat model covering host access, module resolution, V8 snapshots,
  op argument validation, denial of service, logs, graph writes, inference, and
  secrets in prompts/results.
- Prove that synchronous infinite JavaScript can be terminated from outside the
  isolate. Wrapping only the async event-loop future in `tokio::time::timeout`
  is insufficient when the isolate never yields.
- Decide whether v1 is read-only. Write and inference ops materially increase
  the security and cancellation surface and should not be assumed into v1.

Implementation may begin only after those decisions are written here with the
pinned APIs and reviewed test strategy.

## Required runtime invariants

- One fresh isolate per execution; no globals, module cache, handles, or op
  state survive between executions.
- No `deno_runtime`, Deno standard library, filesystem, network, subprocess,
  environment, dynamic native module, or unrestricted import surface.
- Only embedded bootstrap modules and explicitly supplied in-memory user modules
  resolve. All other specifiers fail closed.
- Heap exhaustion, synchronous CPU loops, async hangs, output floods, op floods,
  and parent cancellation terminate the isolate and return a typed outcome.
- Isolate termination must not leave graph writes, inference requests, or model
  runtime leases running in the background.
- All op arguments are validated before deserialization or graph access.
- All graph writes go through schema-authoritative `KnowledgeGraph` facade
  mutations; ops never call storage internals.
- Console and returned values are bounded. Values crossing the Rust/JS boundary
  are explicit serde DTOs with camelCase wire names.
- The embedded `.d.ts`, runtime shim, Rust DTOs, validators, and registered ops
  are one versioned contract with drift tests.

## Candidate crate shape

This layout is provisional until the pinned `deno_core` release is selected:

```text
crates/u-forge-ts-runtime/
  src/
    lib.rs              public runtime/result/config types
    runtime.rs          isolate construction, termination, event loop
    ops.rs              validated op registration
    module_loader.rs    embedded/in-memory allowlist loader
    transpile.rs        TypeScript-to-JavaScript adapter
    console.rs          bounded output capture
    dto.rs              explicit Rust/TypeScript wire types
    js/                 embedded bootstrap shims
  types/u_forge.d.ts    versioned agent-facing contract
  tests/                isolation and adversarial integration tests
```

The public result must distinguish success, script error, validation error,
resource limit, timeout, cancellation, unavailable capability, and internal
failure. It must not encode failure only as `Option<String>` fields.

## Candidate v1 op surface

Read-only graph operations are the default v1 candidate:

| TypeScript capability | Current Rust boundary |
|-----------------------|-----------------------|
| Graph statistics | `KnowledgeGraph::get_stats` |
| Paginated node summaries | `KnowledgeGraph::get_nodes_paginated` |
| Node by ID | `KnowledgeGraph::get_object` |
| Edges for a node | `KnowledgeGraph::get_relationships` |
| Bounded subgraph | `KnowledgeGraph::query_subgraph` |
| FTS search | `KnowledgeGraph::search_chunks_fts` |
| Hybrid search | `search_hybrid_response_with_cancellation(graph, queue, hq_queue, query, config, token)` |

Potential later write ops must construct typed `GraphMutation` values or call
equivalent validated facade methods. They must not rely on stale names such as
`delete_node`, and relationship writes must preserve loaded-schema endpoint
validation.

Potential inference ops use the coordinated/cancellable request types in
`u_forge_core::queue`. The await-only `InferenceQueue::generate(ChatRequest)`
convenience method is not itself a sandbox API; sandbox work is parented by an
explicit cancellation token.

## Resource-control design requirements

- Configure V8 heap limits before user code is compiled or run.
- Retain a thread-safe isolate termination handle capable of interrupting
  synchronous JavaScript when the deadline or parent cancellation fires.
- Bound wall time, CPU execution where the selected V8 API permits it, pending
  ops, module count/source bytes, console bytes, return-value bytes, graph
  result counts, and inference/tool calls.
- Run blocking isolate work on an execution model that cannot starve the main
  Tokio/GPUI executors.
- Define cleanup ordering: cancel child work, terminate isolate, drain/close
  bounded channels, release runtime leases, then return the result.

## Required verification before feature approval

- Basic TypeScript execution and bounded console capture.
- Filesystem, network, environment, subprocess, dynamic import, and unsupported
  module attempts fail closed on every supported platform.
- Synchronous `while (true) {}` and asynchronous never-resolving promises both
  terminate within the configured deadline plus a small measured margin.
- Heap bombs, output floods, op floods, oversized source/modules, and deeply
  nested/hostile JSON return controlled failures without process abort.
- Execution A cannot observe globals, modules, handles, logs, or graph/inference
  state from execution B except committed graph data explicitly exposed by ops.
- Parent cancellation leaves no queued inference work or partial graph write.
- Read and write DTO contract tests keep `.d.ts`, serde names, validators, and
  op registration synchronized.
- Every graph test uses a fresh `TempDir`; every AI test uses mocks or the
  repository's optional live-server skip guard.

After the design gate is approved, replace this document with a pinned,
decision-complete implementation plan before adding runtime dependencies.
