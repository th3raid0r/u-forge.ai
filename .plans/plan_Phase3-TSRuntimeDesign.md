# Phase 3 — TS Agent Sandbox Runtime Design

**Status (2026-08-04): Open.** The `LemonadeRuntime` added in PR #33 coordinates server-global LLM model/reasoning reloads and is unrelated to `u-forge-ts-runtime`. The TypeScript agent sandbox remains a stub and F2 remains open. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Source findings:** D1, F2

**Why Phase 3:** `u-forge-ts-runtime` is currently a stub. F2 flags that
the existing scaffold has already locked in a serialisation contract
(`serde_json` JSON values) which may not match how `deno_core` actually
exposes `v8::Local<v8::Value>`. This needs a design pass before any
further code, otherwise the eventual real implementation has to undo a
shape decision baked in by the stub.

This plan exists to **reset the design** before more code lands on top of
the stub.

**Branch name suggestion (design):**
`design/phase3-ts-runtime`

---

## Scope

| ID | What | Where |
|----|------|-------|
| D1 | `u-forge-ts-runtime` is a literal stub | `crates/u-forge-ts-runtime/src/lib.rs` (3 lines of comments); `Cargo.toml` doesn't depend on `deno_core` |
| F2 | Existing skeleton presumes JSON-serialisable v8 values | `crates/u-forge-ts-runtime/Cargo.toml` (depends on `u-forge-core` and `serde_json`) |

---

## Suggested approach

Produce or update `feature_TS-Agent-Sandbox.md` with the following:

### Section 1 — Goals

- What is the sandbox *for*? Plugin authoring? Agent extensions? End-user
  scripting?
- Trust model: who writes scripts, who runs them, what they can access.
- Concurrency model: per-script isolate, shared isolate, worker pool.

### Section 2 — Real `deno_core` integration

- Confirm `deno_core` is the chosen runtime (vs. `quickjs-rs`,
  `boa-engine`, `rusty_v8` direct).
- Audit what `deno_core` actually provides: ops, extensions, snapshots,
  permissions.
- Plan the FFI shape between Rust host and TS guest.

### Section 3 — Value bridging

- `v8::Local<v8::Value>` is *not* trivially JSON-serialisable. Decide:
  - Bridge layer that converts to/from `serde_json::Value` for
    graph-stored data (lossy for some types — confirm acceptable).
  - Or: keep v8 handles inside the runtime and pass references to
    graph nodes via opaque IDs only.
- Document the trade-off explicitly so a future contributor doesn't
  silently re-introduce the assumption.

### Section 4 — Graph access surface

- What ops does the runtime expose to TS?
  - Read-only or mutate? (likely read-only first)
  - Pagination / streaming for large queries.
  - Schema-aware helpers vs. raw JSON.
- Permission model: per-script ACLs.

### Section 5 — Lifecycle

- Loading scripts: from disk? from the graph itself?
- Reloading: hot-reload vs. process restart.
- Errors: how runtime errors surface to the host UI.

### Section 6 — Implementation roadmap

- Phase A: real `deno_core` dependency added; minimal "hello world" op.
- Phase B: read-only graph ops.
- Phase C: schema-aware helpers.
- Phase D: mutate ops (gated, with explicit permission).

### Step 2 — only after design lands, start coding

Once the design is approved:

1. Add `deno_core` to `Cargo.toml`.
2. Replace the stub with a minimal isolate that exposes one op.
3. Write integration tests against the new isolate.
4. Iterate per the roadmap.

---

## Testing instructions

Design phase produces no code; nothing to test.

For the eventual implementation:

- Canonical command: `cargo test --workspace -- --test-threads=1`.
- Each new op should have a round-trip integration test (Rust calls into
  TS, asserts result; TS calls Rust op, asserts return).
- Sandbox isolation tests: assert a misbehaving script can't escape its
  isolate (infinite loops, OOM, fs access).

---

## Documentation fold-in

- **`feature_TS-Agent-Sandbox.md`** — this is the primary deliverable
  of this plan.
- **`ARCHITECTURE.md`** — once code lands, add a runtime section
  describing the isolate model and graph access surface.
- **`README.md`** — note the runtime as "experimental" until stabilised.
- **`bugfinding.md`** — leave alone.

---

## User input prompts

This plan is *all* user input. Specifically ask:

1. **Sandbox purpose.** Most important question. Answers shape every
   downstream decision.
2. **Trust model.** Who writes scripts? Untrusted user input vs.
   trusted plugin authors radically changes the design.
3. **Runtime choice.** `deno_core` is the assumed answer; ask whether
   to revisit (smaller alternatives exist).
4. **Mutation policy.** Read-only first vs. mutate-from-day-one.
5. **Persistence.** Scripts on disk, in the graph, or both?
6. **Defer or ship.** Should this work be deferred entirely until other
   Phase 3 work lands? The current stub does no harm.

---

## Commit & push

Design phase:

1. Commit the updated `feature_TS-Agent-Sandbox.md` on the design branch.
2. Open a PR for review of the design only (no code).
3. Iterate on review feedback before any implementation work.

Implementation phase: per the roadmap, each step is its own PR.

---

## Out of scope

- Implementing any TS code right now.
- Picking the script editor UI.
- Distribution / package management for scripts.
- Job cancellation infrastructure (F7 — separate plan, but the runtime
  will probably depend on it eventually).
