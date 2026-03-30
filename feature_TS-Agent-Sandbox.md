# Feature: TypeScript Agentic Sandbox

## Status: Not started
## Prerequisites: `feature_refactor-for-extensibility.md` complete (workspace split)

---

## Goal

Embed a sandboxed V8 runtime in Rust using `deno_core`. An AI agent generates TypeScript programs that execute against a curated set of graph operations. The runtime is in-process — no HTTP server, no network round-trip.

Key properties:
- **Sandboxed**: each execution gets its own `JsRuntime` (fresh V8 isolate). No Deno standard library, no filesystem, no network. Only the ops you explicitly register are callable.
- **Resource-limited**: each isolate has a V8 heap limit and execution timeout to prevent infinite loops and memory bombs.
- **Typed**: a `.d.ts` file defines the full API surface. This file is injected into the AI agent's system prompt so it knows what to call. The runtime does not type-check — it transpiles (strips types) and runs. The agent iterates on runtime errors.
- **MCP-like primitives**: each graph operation is a named async function in the `UForge` namespace. The agent writes a TypeScript program; the program calls `UForge.*`; those calls dispatch to Rust.

---

## Crate: `u-forge-ts-runtime`

### Dependencies

- `deno_core` ^0.311 — V8 runtime embedding and `#[op2]` macro for Rust→JS op bindings. **Pin to a specific minor version** — `deno_core` does not follow semver and frequently changes the `#[op2]` macro API and extension registration. Check the [deno_core changelog](https://github.com/denoland/deno_core/releases) for breaking changes before updating.
- `deno_ast` — TypeScript transpilation (type-stripping). Evaluate `swc_core` as an alternative if `deno_ast` pulls in too many transitive dependencies.
- `u-forge-core`
- `tokio`, `serde`, `serde_json`, `anyhow`

### Structure

```
crates/u-forge-ts-runtime/
  src/
    lib.rs              — public API: TsRuntime, ExecutionResult
    runtime.rs          — JsRuntime construction and execution
    ops.rs              — all #[op2(async)] op definitions
    module_loader.rs    — restricted ModuleLoader: rejects file:// and http://, transpiles .ts
    transpile.rs        — TypeScript → JavaScript via deno_ast
    console.rs          — console.log/warn/error capture (JS polyfill + Rust op)
    js/
      u_forge_init.js   — wraps Deno.core.ops.op_* into globalThis.UForge.*
      console_init.js   — globalThis.console polyfill that routes to op_console_log
  types/
    u_forge.d.ts        — TypeScript API contract (source of truth, embedded in binary)
```

### Public API

```rust
pub struct TsRuntime {
    graph: Arc<KnowledgeGraph>,
    queue: Option<Arc<InferenceQueue>>,
}

pub struct ExecutionResult {
    pub output: String,                           // captured console.log/warn/error output
    pub return_value: Option<serde_json::Value>,
    pub error: Option<String>,
    pub duration_ms: u64,
}

pub struct ExecutionConfig {
    pub heap_limit_bytes: usize,      // default: 64 MB
    pub timeout: Duration,            // default: 30 seconds
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            heap_limit_bytes: 64 * 1024 * 1024,
            timeout: Duration::from_secs(30),
        }
    }
}

impl TsRuntime {
    pub fn new(graph: Arc<KnowledgeGraph>, queue: Option<Arc<InferenceQueue>>) -> Self;
    pub async fn execute(&self, typescript_source: &str) -> ExecutionResult;
    pub async fn execute_with_config(&self, typescript_source: &str, config: ExecutionConfig) -> ExecutionResult;
}
```

`execute()` creates a new `JsRuntime`, inserts the graph/queue into `OpState`, transpiles the source, runs the event loop to completion, collects output, and drops the runtime. One isolate per call — no state leaks between executions.

### Resource limits

Every `JsRuntime` instance must be configured with:
- **Heap limit**: `runtime.v8_isolate().set_heap_limit(config.heap_limit_bytes)`. Default 64 MB. Exceeding this terminates the isolate with a clear error in `ExecutionResult.error`.
- **Execution timeout**: Wrap the event loop in `tokio::time::timeout(config.timeout, ...)`. Default 30 seconds. On timeout, drop the `JsRuntime` and return an error.

### `console.log` capture

V8/`deno_core` does not provide `console` by default. Implement it as:

1. Register `op_console_log(level: String, msg: String)` in the op surface. This op appends to a `Vec<String>` in `OpState`.
2. Provide `console_init.js` that creates `globalThis.console = { log, warn, error, info, debug }`, each calling `Deno.core.ops.op_console_log(level, ...)`.
3. Load `console_init.js` before the user script.
4. After execution, drain the captured output into `ExecutionResult.output`.

### Op surface

All ops are `#[op2(async)]`. The graph and queue are retrieved from `OpState` inside each op. Register all ops in a single `deno_core::extension!(u_forge_ops, ops = [...])`.

**Note on `deno_core` op access pattern:** In recent `deno_core` versions (0.290+), ops are accessed via `Deno.core.ops.op_name()`. The init shim maps these to clean `UForge.*` names. Verify the exact pattern against the pinned `deno_core` version — the internal API surface has changed multiple times.

Graph reads: `op_get_stats`, `op_get_all_nodes`, `op_get_node`, `op_get_edges`, `op_get_subgraph`

Graph writes: `op_add_node`, `op_update_node`, `op_delete_node`, `op_connect_nodes`

Search: `op_search_fts`, `op_search_hybrid` (hybrid requires `InferenceQueue` in `OpState`; return a clear error if absent)

Inference (gated): `op_generate`, `op_embed` (only callable if `InferenceQueue` is present)

Console: `op_console_log` (captures output to `OpState`)

### Op-to-KnowledgeGraph method mapping

| Op | KnowledgeGraph method | Notes |
|----|----------------------|-------|
| `op_get_stats` | `get_stats()` | Direct mapping |
| `op_get_all_nodes` | `get_all_objects()` | Returns `Vec<ObjectMetadata>`, map to `NodeSummary[]`. **Note: loads all nodes into memory.** For graphs > 10k nodes, consider using `get_nodes_paginated()` instead. |
| `op_get_node` | `get_object(id)` | Map `ObjectMetadata` → `NodeDetail` DTO |
| `op_get_edges` | `get_relationships(id)` | Returns `Vec<Edge>`, map to `Edge[]` DTO |
| `op_get_subgraph` | `query_subgraph(id, hops)` | Returns `QueryResult` — map to `SubgraphResult` DTO. `QueryResult` includes objects, edges, and chunks with a token budget; extract only the fields the `.d.ts` exposes (root node, edges, neighbor nodes). |
| `op_add_node` | `add_object(metadata)` | Construct `ObjectMetadata` from `NewNode` DTO |
| `op_update_node` | `update_object(metadata)` | Merge `NodeUpdate` fields into existing `ObjectMetadata` |
| `op_delete_node` | `delete_node(id)` | Direct mapping |
| `op_connect_nodes` | `connect_objects_str(from, to, edge_type)` | Direct mapping |
| `op_search_fts` | `search_chunks_fts(query, limit)` | Group results by node, map to `SearchResult[]` |
| `op_search_hybrid` | `search::search_hybrid(graph, queue, query, config)` | Free function, not a method. Requires `InferenceQueue`. Map `HybridSearchResult` → `SearchResult` DTO. |
| `op_generate` | `queue.generate(prompt)` | Requires `InferenceQueue` |
| `op_embed` | `queue.embed(text)` | Requires `InferenceQueue` |

### JS init shim (`js/u_forge_init.js`)

Maps raw `Deno.core.ops.op_*` calls to a clean `UForge` namespace matching the `.d.ts`:

```javascript
const { ops } = Deno.core;
globalThis.UForge = {
  getStats:      ()                    => ops.op_get_stats(),
  getAllNodes:    ()                    => ops.op_get_all_nodes(),
  getNode:       (id)                  => ops.op_get_node(id),
  getEdges:      (id)                  => ops.op_get_edges(id),
  getSubgraph:   (id, hops)            => ops.op_get_subgraph(id, hops),
  addNode:       (node)                => ops.op_add_node(node),
  updateNode:    (node)                => ops.op_update_node(node),
  deleteNode:    (id)                  => ops.op_delete_node(id),
  connectNodes:  (from, to, edgeType)  => ops.op_connect_nodes(from, to, edgeType),
  searchFts:     (query, limit)        => ops.op_search_fts(query, limit),
  searchHybrid:  (config)              => ops.op_search_hybrid(config),
  generate:      (prompt)              => ops.op_generate(prompt),
  embed:         (text)                => ops.op_embed(text),
};
```

**Important:** Verify the `Deno.core.ops` access pattern against the pinned `deno_core` version. In some versions, the pattern is `Deno[Deno.internal].core.ops` or requires explicit `Deno.core.ensureFastOps()` call first.

### Module loader

`RestrictedModuleLoader` implements `deno_core::ModuleLoader`. It:
- Allows relative imports within the agent script
- Resolves `"u-forge"` to an empty module (the `.d.ts` is for the agent's editor, not the runtime)
- Calls `transpile_typescript()` on `.ts` sources before returning them to V8
- **Rejects all `file://` and `http://` specifiers unconditionally**

---

## TypeScript API contract (`types/u_forge.d.ts`)

This file is the agent's interface spec. It must stay in sync with `ops.rs`. Embed it in the binary via `include_str!` so it can be programmatically injected into agent prompts.

```typescript
declare namespace UForge {
  interface GraphStats {
    nodeCount: number; edgeCount: number; chunkCount: number; totalTokens: number;
  }
  interface NodeSummary {
    id: string; name: string; objectType: string; tags: string[];
  }
  interface NodeDetail extends NodeSummary {
    description?: string; schemaName?: string;
    properties: Record<string, unknown>;
    createdAt: string; updatedAt: string;  // ISO 8601
  }
  interface NewNode {
    name: string; objectType: string;
    description?: string; tags?: string[];
    properties?: Record<string, unknown>; schemaName?: string;
  }
  interface NodeUpdate {
    id: string; name?: string; description?: string;
    tags?: string[]; properties?: Record<string, unknown>;
  }
  interface Edge {
    source: string; target: string; edgeType: string; weight: number;
  }
  interface SubgraphResult {
    root: NodeDetail; edges: Edge[]; neighbors: NodeDetail[];
  }
  interface SearchResult {
    node: NodeSummary; score: number; matchingChunks: string[];
  }
  interface HybridSearchConfig {
    query: string;
    alpha?: number;    // 0.0 = FTS only, 1.0 = semantic only, default 0.5
    limit?: number;    // default 10
    rerank?: boolean;  // default true, requires inference queue
  }

  function getStats(): Promise<GraphStats>;
  function getAllNodes(): Promise<NodeSummary[]>;
  function getNode(id: string): Promise<NodeDetail | null>;
  function getEdges(nodeId: string): Promise<Edge[]>;
  function getSubgraph(rootId: string, hops: number): Promise<SubgraphResult>;
  function addNode(node: NewNode): Promise<string>;
  function updateNode(node: NodeUpdate): Promise<void>;
  function deleteNode(id: string): Promise<void>;
  function connectNodes(fromId: string, toId: string, edgeType: string): Promise<void>;
  function searchFts(query: string, limit?: number): Promise<SearchResult[]>;
  function searchHybrid(config: HybridSearchConfig): Promise<SearchResult[]>;
  function generate(prompt: string): Promise<string>;
  function embed(text: string): Promise<number[]>;
}
```

All Rust DTO structs must use `#[serde(rename_all = "camelCase")]` to match TypeScript field naming conventions. **Note:** The `.d.ts` above already uses camelCase (`nodeCount`, `objectType`, `edgeType`, `createdAt`, etc.) — the Rust DTO field names use snake_case and serde handles the conversion.

---

## Invariants

- **One isolate per execution.** `JsRuntime` is created and destroyed inside `execute()`. Never reuse.
- **No Deno standard library.** Do not add `deno_runtime`. `Deno.` beyond `Deno.core.ops` must not be accessible.
- **Resource limits enforced.** Every isolate has a heap limit (default 64 MB) and execution timeout (default 30s).
- **`u_forge.d.ts` is the contract.** Adding an op in `ops.rs` requires a matching declaration in the `.d.ts`.
- **DTO field names are camelCase.** All Rust DTOs use `#[serde(rename_all = "camelCase")]`.
- **`console` is a polyfill.** `globalThis.console` is provided by `console_init.js` → `op_console_log`. It is not V8-native.

---

## Implementation order

1. **Minimal runtime** — `TsRuntime` that can execute plain JavaScript (no TypeScript, no ops). Verify isolate creation/destruction and output capture.
2. **Console capture** — `op_console_log` + `console_init.js`. Test with `console.log("hello")`.
3. **TypeScript transpilation** — `deno_ast` or `swc_core` integration. Verify `.ts` source → `.js` → execution.
4. **Resource limits** — heap limit + timeout. Test with infinite loop and large allocation.
5. **Read-only graph ops** — `op_get_stats`, `op_get_all_nodes`, `op_get_node`, `op_get_edges`, `op_get_subgraph`. Test against `memory.json` data.
6. **Write graph ops** — `op_add_node`, `op_update_node`, `op_delete_node`, `op_connect_nodes`. Test round-trip.
7. **Search ops** — `op_search_fts`, `op_search_hybrid`. Hybrid requires `InferenceQueue` wiring.
8. **Inference ops** — `op_generate`, `op_embed`. Gated on `InferenceQueue` presence.
9. **Module loader** — `RestrictedModuleLoader` with import rejection tests.
10. **Init shim** — `u_forge_init.js` mapping `Deno.core.ops` → `UForge.*`.

---

## Verification

Integration tests in `crates/u-forge-ts-runtime/tests/`:

1. **Basic execution:** load `memory.json` (from `defaults/data/` at workspace root) into a temp DB, execute `const s = await UForge.getStats(); console.log(s.nodeCount)`, assert output contains the correct node count
2. **Sandbox isolation:** execute `Deno.readFileSync('/etc/passwd')` — assert `result.error` is set with `ReferenceError`; execute `fetch('http://example.com')` — assert error is set
3. **Write ops round-trip:** execute a script that calls `UForge.addNode(...)` and `UForge.connectNodes(...)`, then query the `KnowledgeGraph` directly in Rust and assert the node and edge exist
4. **Per-execution isolation:** set `globalThis.secret = "abc"` in execution A, attempt to read it in execution B — assert it is not present
5. **Inference gating:** construct a `TsRuntime` with `queue: None`, execute `await UForge.generate("hello")`, assert a clear error is returned rather than a panic
6. **Heap limit:** execute a script that allocates a large array in a loop (`let a = []; while(true) a.push(new Array(10000))`) — assert `result.error` contains a memory-related error, not a panic
7. **Timeout:** execute `while(true) {}` — assert execution completes within timeout + small margin and `result.error` indicates timeout
8. **Console capture:** execute `console.log("a"); console.warn("b"); console.error("c")` — assert `result.output` contains all three messages with level prefixes
