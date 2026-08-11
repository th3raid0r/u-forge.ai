# u-forge.ai — Bug-Finding Audit

> **Archived audit snapshot.** This document describes the 2026-04-24 tree and
> is not a list of current defects or implementation instructions. The Alpha
> correctness, inference lifecycle, agent-budget, Lemonade runtime, and desktop
> foundation work reconciled its actionable findings. Current behavior is
> documented in source, `ARCHITECTURE.md`, and `.rulesdir/`; the remaining
> product decisions are recorded in `.plans/README.md`.

Date: 2026-04-24
Scope: every Rust crate in the workspace plus configuration, sample data, tests,
and tooling. Findings come from five focused subagent passes synthesised here,
with the most consequential claims spot-verified against source.

## Final reconciliation (2026-08-09)

The audit no longer contains active defect status. Its actionable findings were
reconciled as follows:

- schema-aware import validation, endpoint ambiguity, required properties,
  graph finiteness/spatial rebuilding, paint-phase mutation, async UI ownership,
  structured search degradation, and the mechanical cleanup landed in the
  Alpha correctness work;
- partial Lemonade discovery and timeout/resource-release behavior landed in
  the Lemonade runtime work;
- cancellable jobs, parent cancellation, explicit outcomes, queue/graph
  telemetry, and evidence-led EWMA/retry decisions landed in inference
  lifecycle;
- bounded schema injection, cumulative request/tool budgets, repeat detection,
  and semantic output truncation landed in agent budgets;
- Linux window behavior landed through negotiated client-side decorations;
- provider generalization, multi-user identity, typed/indexed properties, a
  general embedding-space registry, undo/redo, and TypeScript execution were
  product/design topics rather than current Alpha defects. Their present state
  is recorded in `.plans/README.md`.

The dated status notes and suggested fixes below are preserved exactly as audit
history; they do not override that final reconciliation.

The list is opinionated about prioritisation: items are grouped by **what they
block**, not just by raw severity. If a finding would make a future feature
hard, it gets called out as such — even if the bug today is benign.

## Status after SQLite salvage (2026-08-03)

This document is retained as an audit snapshot. Paths and symbol names remain
useful, but line numbers refer to the 2026-04-24 tree.

- **C1 / H2 — partially resolved:** strict JSONL import now drops undeclared
  properties, skips records missing required properties, validates object and
  edge types/endpoints, and writes structured diagnostics. General
  schema-manager required-field/coercion policy remains open.
- **C2 — open:** ambiguous cross-type name fallback still needs an explicit
  import policy.
- **M3 / M6 / L3 — resolved:** the Lemonade probe message, configured-dimension
  mismatch coverage, and project-wide strict clippy cleanup are in place.
- **F5 — partially resolved:** both SQLite vector lanes are configurable and
  protected by persisted dimension metadata; a general embedding-space
  registry remains future work.

All other findings remain open unless source code now demonstrates otherwise.

## Status after the 2026-08-04 source and plan reconciliation

This file remains the original audit record; its suggested fixes are not active
implementation instructions. Current work is tracked in `.plans/README.md`.

- The original C5 collision explanation and M12 selection consequence do not
  match current control flow: layout reseeds nodes, tiny distances are skipped,
  and non-finite distances fail hit-test filters. Graph hardening remains useful
  under a revised saved/unsaved placement and viewport-invariant specification.
- C6's float-equality explanation is incomplete because `NodeEntry` equality is
  ID-based and drag mouse-up rebuilds the index. A narrow refresh-during-drag
  race remains; active planning chooses a simpler bulk-rebuild policy.
- SearchPanel now owns stale-search cancellation, list-state helpers exist, CI
  and dimension mismatch coverage are present, and HQ backfill is automatic.
- The proposed five-second C3 completion timeout is rejected for local models.
  Active planning separates connection, load, first-token, idle, and completion
  timeouts and retains guards until each operation terminates.
- Generic provider abstraction, multi-tenant identity, typed property storage,
  and a general embedding-space registry are parked decisions rather than Alpha
  defects.

---

## How to read this

Every finding has:
- **Where** — file path and line range, taken from the current tree.
- **What** — the actual behaviour, not a paraphrase.
- **Why it matters** — concrete impact, including blast radius if the dormant
  case fires.
- **Suggested fix** — a one-liner, a small refactor, or a pointer to the design
  decision needed.

Findings I deliberately removed:
- Speculative claims agents withdrew during their own review (e.g. agents
  flagged a missing `chunks_vec_hq_ad` trigger that already exists at
  `graph/storage.rs:121-123`).
- "Bugs" that turned out to be performance preferences without evidence.
- Doc-mirror complaints with no behavioural consequence.

---

## Critical — fix before next user-facing release

### C1. Property validation is silently skipped during JSONL import (partially resolved)

**Where:** `crates/u-forge-core/src/ingest/data.rs:326-348`,
`crates/u-forge-core/src/schema/manager.rs:459-548`.

**What:** `JsonDataIngestion::create_object_by_type` calls
`add_properties_to_builder`, which iterates the property map and writes every
key straight onto the `ObjectBuilder` without ever invoking
`SchemaManager::validate_and_coerce_properties`. The validator function exists
and reports `UnknownProperty`, `TypeMismatch`, and enum-value issues, but no
caller in the import path consumes its output. Net effect:

- Misspelled property names land in the `properties` JSON blob unchanged.
- `String("42")` is *not* coerced to `Number(42)`, even though the schema
  declares the field as numeric, because the coercion happens inside the
  validator the import bypasses.
- `Bool` aliases ("yes" / "true" / "1") are not normalised either.

**Why it matters:** Two corrosive consequences. First, downstream code that
expects coerced types (e.g. range filters in future query work, or numeric
comparisons in a UI sort) sees the wrong type and either errors or silently
ignores the value. Second, the validator was the only line of defence against
schema drift via typo — without it, every typo persists as a "real" property
and can never be repaired without explicit cleanup.

**Fix:** In `add_properties_to_builder`, call
`schema_manager.validate_and_coerce_properties(object_type, &mut props)`,
log/return the issue list, and use the coerced values when building. The
import-time policy should be: warn on `UnknownProperty`, error on
`InvalidEnumValue`, coerce on `TypeMismatch` where possible.

---

### C2. Cross-type name collisions silently pick the wrong node during edge resolution

**Where:** `crates/u-forge-core/src/ingest/data.rs:245-271`.

**What:** `resolve_node_id` falls back to `find_by_name_only` when the
in-session map misses. When that returns multiple matches, it logs
`"Ambiguous node name … using first match"` and returns `results[0].id`. There
is no determinism guarantee on which node ranks first, and the ambiguity is
common in TTRPG content — "Gandalf" can plausibly exist as a `character`,
an `event`, and a `quest` reference simultaneously.

**Why it matters:** Edges are silently wired to the wrong node. A re-import
with the same data may produce a different graph if SQLite's row order
changes. Once the wrong edge is in the database, there is no automated way to
detect it because the UNIQUE constraint on `(source_id, target_id, edge_type)`
considers it valid.

**Fix:** Either (a) require the import format to qualify references as
`type:name` and remove the cross-type fallback, or (b) fail the import with a
diagnostic when ambiguity is hit, listing all matches so the author can fix
their JSONL. Option (b) is the smaller change and matches what the agent tool
`resolve_node` already does (`u-forge-agent/src/lib.rs:713-741`).

---

### C3. GPU-locked LLM call has no per-request timeout

**Where:** `crates/u-forge-core/src/lemonade/chat.rs:237` (acquires
`gpu.begin_llm()` guard) and `crates/u-forge-core/src/lemonade/client.rs:40-45`
(client built with a 30s request timeout, 5s connect timeout).

**What:** `run_llm_worker` (workers.rs:65-88) calls
`provider.complete(job.request).await`. Inside, the chat provider acquires the
GPU guard, then issues an HTTP request. The request is bounded by reqwest's
30s timeout, but during those 30 seconds the GPU guard is held. While the
guard is held, every queued STT request is *rejected immediately* with
"LLM inference in progress" (the GPU policy is asymmetric — see ARCHITECTURE.md
Hardware section).

**Why it matters:** A hung Lemonade Server doesn't just slow you down; it
locks STT for 30 seconds at a time and emits a misleading error that blames
contention rather than the actual upstream hang. On a flaky network this
becomes user-visible.

**Fix:** Wrap `provider.complete(...)` in a tighter `tokio::time::timeout` (5s
seems reasonable for non-streaming completions; streaming already has its
own bounds). On timeout, drop the guard before propagating the error so STT
can proceed.

---

### C4. `paint_entity.update(...)` is called from inside a canvas paint closure

**Where:** `crates/u-forge/src/text_field.rs:808-810` and `:833-840`.

**What:** During canvas paint, the closure mutates `TextFieldView` state via
`paint_entity.update(cx, |this, _cx| { this.shaped_layout = … })` and again
to record `field_origin_x`, `field_origin_y`, `measured_line_h`,
`content_height`, `visible_height`, `visible_width`. The project's GPUI rules
(`.rulesdir/gpui-patterns.mdc`) and `.rules` Anti-Pattern #4 explicitly call
out paint-time mutation as a redraw amplifier.

**Why it matters:** Every text field renders on every frame. If a paint-time
mutation invalidates anything that influences layout, the next frame re-runs
layout with the new measurements and re-paints, applying the mutation again.
With multiple text fields visible (chat input + several read-only message
bodies + node editor), the cost stacks. This is the sort of latent perf bug
that only shows up when a user opens a long chat with the node editor open.

**Fix:** Move the measurement recording into a prepaint hook (`on_before_paint`
or a layout pass). The values are needed for click hit-testing and scroll, so
they have to live somewhere on the entity — but they don't have to be written
during paint. A one-frame lag (read previous frame's measurements in event
handlers) is the standard pattern in Zed's editor.

---

### C5. Force-directed layout can produce NaN/Inf positions when nodes coincide

**Where:** `crates/u-forge-graph-view/src/layout.rs:84-94`.

**What:** Inside the repulsion loop, the guard is `if dist_sq > max_sq ||
dist_sq < 0.01 { continue; }`. For nodes whose positions differ by a tiny
amount such that `dist_sq` is in `(0.0, 0.01)` after rounding, the code
proceeds to compute `dist = sqrt(dist_sq)` (very small), `force =
REPULSION / dist_sq` (very large), and `dir = delta / dist`. Each of these is
a NaN/Inf risk if `delta` rounds to zero while `dist_sq` does not, or if
`force` overflows.

The initial seed (`layout.rs:46-54`) places nodes on a deterministic grid, so
on first layout pass collisions are unlikely. The risk surfaces when:
- A user drags a node directly on top of another (graph_canvas drag handler
  has no minimum-separation check).
- A new node is added incrementally without re-running the layout.
- A snapshot is rebuilt from saved positions where two nodes share a saved
  position.

**Why it matters:** A single NaN propagates: the spatial index (`rstar`)
silently rejects NaN entries, so the offending node disappears from
hit-testing. Surrounding nodes drift to NaN over subsequent iterations
because `displacements[i] += dir * force` poisons them.

**Fix:** Treat collisions as "jitter, then continue":
```rust
const MIN_SEPARATION_SQ: f32 = 1e-4;
if dist_sq < MIN_SEPARATION_SQ {
    nodes[i].position += Vec2::new(0.5, 0.0);  // deterministic nudge
    continue;
}
```
Plus a NaN-defensive clamp in `Viewport::screen_to_world`
(`u-forge-ui-traits/src/lib.rs:78-85`) so a broken zoom doesn't take the canvas
with it.

---

### C6. R-tree spatial index update relies on exact-position match for removal

**Where:** `crates/u-forge-graph-view/src/snapshot.rs:316-340` (incremental
build path).

**What:** The "all_saved" fast path clones the previous R-tree and then calls
`index.remove(&entry)` for each removed node, where `entry` carries the *old*
position. `rstar::RTree::remove` requires exact equality (including the
position floats). If the node was moved between the layout pass that wrote the
R-tree and the snapshot rebuild, removal silently fails — the entry stays in
the tree forever, with stale position data.

**Why it matters:** Hit-testing returns a node that no longer exists. Viewport
queries return phantom nodes. The bug is invisible on small graphs (the next
full rebuild flushes it) but accumulates in long-lived sessions.

**Fix:** Switch from incremental remove to bulk-load the survivors:
```rust
let removed_set: HashSet<ObjectId> = removed.iter().copied().collect();
let survivors: Vec<NodeEntry> = prev.spatial_index.iter()
    .filter(|e| !removed_set.contains(&e.id))
    .cloned()
    .collect();
let index = RTree::bulk_load(survivors);
```
`bulk_load` is O(N log N) but keeps the tree balanced and dodges the
position-equality trap.

---

## High — pernicious bugs / data integrity / silent failure

### H1. `find_by_name_only` shape leaks into search and tool layers

**Where:** Affects both `u-forge-core/src/ingest/data.rs:254` and
`u-forge-agent/src/lib.rs:713-741`.

**What:** Same multi-match issue as C2, but the agent variant *does* fail
loudly with a list — except it caps the list to 5 (`.take(5)`) while
quoting the full ambiguous count `n`. For common nouns ("Item", "Event") the
LLM gets "matched 100 nodes — here are 5", which is unhelpful and makes the
agent loop waste turns.

**Why it matters:** Agent loop spins. Wastes tokens. Makes multi-turn flows
brittle.

**Fix:** When `n > 10`, summarise by `object_type` instead of listing
individuals: "matched 47 nodes across types: character (12), event (30),
faction (5) — narrow by passing the type or a UUID".

### H2. `validate_and_coerce_properties` doesn't enforce required properties (resolved at import boundary)

**Where:** `crates/u-forge-core/src/schema/manager.rs:459-548`.

**What:** The function name implies validation, but it only iterates the
`properties` map and validates entries that are *present*. It never compares
against `ObjectTypeSchema::required_properties` to flag missing ones. Schemas
declaring `required: ["name", "description"]` will accept objects with no
description at all, with no `PropertyIssue` raised.

**Why it matters:** Combined with C1 (validation is bypassed at import
anyway), required-property guarantees do not exist anywhere in the pipeline.
Schema declarations are decorative.

**Fix:** Add a required-fields pass at the top of
`validate_and_coerce_properties`. Add `PropertyIssue::MissingRequired { key:
String }` so callers can decide policy (warn vs error).

### H3. Search pipeline silently degrades to FTS-only on embedding failure

**Where:** `crates/u-forge-core/src/search/mod.rs` (the embed→ANN stage).

**What:** When `queue.embed(query)` fails, the code logs `warn!` and continues
with an empty semantic-result vector. The downstream RRF merge then operates
purely on FTS hits, and the caller cannot tell whether the empty semantic
contribution was deliberate (alpha=1.0) or a real failure.

**Why it matters:** Hybrid search becomes silently degraded for the duration
of the embedding outage. A user filing "the search has been bad lately" gets
no log signal because everyone reads the trace at info level.

**Fix:** Return search results plus a degradation flag (`fts_only: bool`,
`semantic_failed: bool`). Render a status hint in the UI when degraded.
Alternatively, escalate from `warn!` to `error!` and emit metrics.

### H4. Detached search task leaks results across rapid re-queries

**Where:** `crates/u-forge/src/search_panel.rs:236`.

**What:** `do_search` does `cx.spawn(...).detach()` with no task handle stored.
If the user types quickly (e.g. in an autocomplete pattern) two tasks are
in flight; whichever finishes second clobbers the panel's `results` field.
The first task's output may also overwrite the second's if the underlying
embedding worker is fast on the second call but slow on the first.

**Why it matters:** Stale results shown without warning. The pattern violates
ARCHITECTURE.md's documented rule about owning task handles.

**Fix:** Store `Option<gpui::Task<()>>` on the panel; replace it on each
search; the previous task's drop cancels it. Apply the same fix to
`PathPickerModal::browse` (`path_picker.rs:97`) which has the identical
shape.

### H5. EWMA seed of 0 destabilises embed routing during warmup

**Where:** `crates/u-forge-core/src/queue/workers.rs:175-181` and
`crates/u-forge-core/src/queue/weighted.rs:176-185`.

**What:** New embed workers seed `ewma_us` to 0. The dispatcher's cost
function returns `pending` (cheapest) until the first job completes, after
which it returns `(pending+1) * ewma`. The first three jobs may oscillate
between workers as each transitions from `0` to a real EWMA, producing
non-monotone routing decisions during warmup.

**Why it matters:** Cold-start latency variance. Visible as "first few
embeddings are slow on a fresh launch" but otherwise invisible. Becomes more
significant if heterogeneous devices (NPU + GPU + CPU) are present.

**Fix:** Seed `ewma_us` with a conservative default per device class — e.g.
`{ npu: 30_000, gpu: 50_000, cpu: 200_000 }` microseconds. The values rapidly
converge to truth; a non-zero seed only matters for the first few jobs.

### H6. Agent does not validate tool arguments against the declared schema

**Where:** `crates/u-forge-agent/src/lib.rs` (tool definitions and the
`prompt_stream` loop near `:1099-1101`).

**What:** Tool argument schemas come from `JsonSchema` derive, but the agent
trusts the LLM-supplied JSON and lets `serde_json::from_value` either coerce
or fail at call time. Failure surfaces as a generic deserialisation error
that the LLM cannot easily correct because the validation message doesn't
identify the offending field.

**Why it matters:** Multi-turn loops burn tokens on tools that fail in ways
the LLM cannot reason about. Switching to a smaller LLM (less reliable JSON
output) makes this much worse.

**Fix:** Validate args against the JSON Schema before dispatch and return a
`ToolError` whose message names the failing field and gives the schema
fragment. The `jsonschema` crate handles this in 4 lines.

### H7. `chat_panel` epoch / cancel-flag interaction has a brief two-poller window

**Where:** `crates/u-forge/src/app_view/mod.rs:655-701`.

**What:** When the user fires a second embedding plan while the first is still
in flight, the new plan bumps the epoch *and* installs a fresh cancel flag.
The old poller is supposed to exit on the epoch mismatch, but its check
happens inside the next `this.update(cx, ...)` tick — there is a sub-second
window where both pollers are alive and racing on the same status string.
The mutation is atomic (it's inside an `update` closure), so no crash; but
the displayed status alternates between the two plans until the old poller
yields.

**Why it matters:** Visible UI flicker; users perceive it as instability.

**Fix:** Take the epoch as the only cancellation signal and drop the
parallel `Arc<AtomicBool>` cancel flag (it duplicates the epoch check). Or
gate runs with an "embedding_in_flight" atomic and reject overlapping plans
with a status message.

### H8. Lemonade catalog discovery fails atomically — one bad endpoint blocks all init

**Where:** `crates/u-forge-core/src/lemonade/catalog.rs:131-139`.

**What:** `LemonadeServerCatalog::discover` uses `tokio::try_join!` over
`/models`, `/system-info`, `/health`. A 5xx on any of the three fails the
whole discovery. `/health` in particular is expected to return `{
all_models_loaded: [] }` on a fresh server, but a transient 503 from any
endpoint takes the whole UI's "connect" flow with it.

**Why it matters:** Brittle startup. Silent retry would mask the issue, but
exposing the per-endpoint failure would let the user diagnose ("models is
fine but health is hanging — restart Lemonade").

**Fix:** Use `tokio::join!` (no early-exit), then assemble the catalog from
whichever endpoints succeeded, recording failures on the catalog struct so
the UI can surface them.

---

## Medium — correctness, performance, or developer-experience issues

### M1. `save_schema` is `async` but does no async work

**Where:** `crates/u-forge-core/src/schema/manager.rs:63-70`. Already noted
in `ARCHITECTURE.md` "Design Decisions" as known.

**What:** Function body is fully synchronous; callers `.await` it pointlessly.

**Fix:** Drop `async`, follow the compile errors, remove the `.await` from
each call site (4 known).

### M2. AppConfig accepts unknown TOML keys silently

**Where:** `crates/u-forge-core/src/config.rs:597` (the
`toml::from_str(&text)?` call) and the `AppConfig` struct lacking
`#[serde(deny_unknown_fields)]`.

**What:** A typo like `embeding.high_quality_embedding = true` is parsed
and ignored. Defaults take over, the user thinks they enabled HQ, nothing
warns them.

**Fix:** Add `#[serde(deny_unknown_fields)]` to `AppConfig` and any nested
section structs. Add a unit test that a malformed config returns an error.

### M3. Test helper error message names the wrong probe port (resolved)

**Where:** `crates/u-forge-core/src/test_helpers.rs:45`.

**What:** Message reads "tried localhost:8000 and LEMONADE_URL". Actual
probe is `localhost:13305` (see `lemonade/mod.rs:108`, the
`resolve_lemonade_url` implementation).

**Fix:** Update the message string.

### M4. Schema injection into agent system prompt is unbounded

**Where:** `crates/u-forge-agent/src/lib.rs:937-954`.

**What:** `graph.schema_prompt_summary_all()` is concatenated with no size
check. Large worlds can push the system prompt to 15-20% of a 128k window;
on a 7B model it can run the whole context dry.

**Fix:** Add a token budget on the schema summary, log when truncated, and
prefer types referenced by recent traffic.

### M5. Tool turn limit has no token / budget guard

**Where:** `crates/u-forge-agent/src/lib.rs` (`max_tool_turns` default 5,
`:898`, `:1077`).

**What:** Five iterations × hybrid search × multi-tool calls can burn through
hundreds of thousands of tokens with no circuit breaker. There's no
input-token budget, no detection of repeating tool calls, and no convergence
check.

**Fix:** Track cumulative tokens; bail when crossing a configurable
threshold. Optionally hash tool calls to detect repeats and force the LLM
to break out.

### M6. No test for the embedding-dimension mismatch error path (resolved)

**Where:** `crates/u-forge-core/src/graph/storage.rs` (the
`check_or_init_embedding_dims` guard) and the test files.

**What:** The `EmbeddingDimensionMismatch` thiserror struct exists, the guard
runs at open time, but no test verifies that opening a DB with a different
compiled-in dimension actually raises this error. A regression that
short-circuits the guard would not be caught.

**Fix:** Add a test that creates a DB, manually overwrites
`schema_metadata.chunks_vec_dims` to a wrong value, reopens, and asserts the
error.

### M7. Layout runs all 200 iterations even on tiny graphs

**Where:** `crates/u-forge-graph-view/src/layout.rs:58`.

**What:** No early-exit on convergence. A 3-node graph still pays the full
200-iteration cost.

**Fix:** Track `max_disp` per iteration; break when `max_disp <
convergence_threshold` after at least N iterations.

### M8. `cosmic-text-patched` declares `[profile.test]` in a non-root crate

**Where:** `crates/cosmic-text-patched/Cargo.toml:186-188`.

**What:** Cargo prints a workspace warning every build because profile
overrides outside the root are ignored.

**Fix:** Move `[profile.test]` to the workspace `Cargo.toml` if the override
is wanted; otherwise delete the block.

### M9. Examples aren't checked in CI (and CI doesn't exist)

**Where:** `crates/u-forge-core/examples/convert_memorymesh.rs`,
`.github/workflows/` does not exist.

**What:** Examples can rot silently because nothing exercises them. There's
no CI at all, so build+test+clippy regressions only get caught on the
maintainer's local box.

**Fix:** Add a minimal `.github/workflows/test.yml` running
`cargo build --workspace`, `cargo check --examples --all`,
`cargo test --workspace -- --test-threads=1`, and
`cargo clippy --workspace --no-deps -- -D warnings` (or whatever bar fits).

### M10. Action-bar / list-state interactions are correct but underdocumented

**Where:** `crates/u-forge/src/chat_panel.rs:242-258, 271, 286, 343,
785, 815, 838`.

**What:** The split between `ListState::reset()` (full structural change) and
`splice_appended()` (streaming append) is intentional and right, but every
new code path has to remember which to call. There's no helper enforcing
the rule.

**Fix:** Two named helpers — `replace_messages()` calling `reset` and
`append_message()` calling `splice_appended`. Make the raw mutation private.

### M11. Missing `chunks_vec_hq` rebalance when standard vec is populated but HQ isn't

**Where:** Cross-cutting between `graph/fts.rs` upserts and `search/mod.rs`
RRF.

**What:** Per-node re-chunk does both standard and HQ embeddings sequentially.
If the HQ provider is unavailable on first run but appears later (e.g. user
downloads the HQ model after starting the app), the standard index is
populated but the HQ index is not — and there is no "embed-only-HQ-for-the-
gap" code path. `EmbeddingPlan::embed_all` does a bulk sweep but doesn't
distinguish between "no embedding at all" and "missing HQ specifically".

**Fix:** Extend the embedding plan with a `BackfillHq` variant that targets
chunks present in `chunks_vec` but missing from `chunks_vec_hq`.

### M12. `node_at_position` uses `partial_cmp`-with-default which masks NaN

**Where:** `crates/u-forge-graph-view/src/snapshot.rs:109`.

**What:** `.min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))`
treats NaN distances as ties, so a NaN node can "win" hit-testing. This
becomes reachable if C5 fires.

**Fix:** Filter out non-finite distances before `min_by`, or fix C5 at
source.

### M13. Embed retry uses lockstep delays — thundering herd on recovery

**Where:** `crates/u-forge-core/src/queue/workers.rs:143-155`.

**What:** Three workers retrying after a Lemonade restart all sleep exactly
100ms, 200ms — synchronised by construction.

**Fix:** Add jitter: `delay_ms + (rand_u64 % (delay_ms / 2))`.

---

## Low / Nits

- **L1.** `EdgeType` newtype was claimed to have legacy enum variants in stale
  comments; verified — no legacy variants remain in `types.rs`. Outdated
  cross-references in `.rulesdir/rust-patterns.mdc:91-97` mention "deprecated
  enum variants … only for backward compatibility" but no such variants exist
  in the code. Trim the doc.
- **L2.** `fts5_sanitize` is duplicated between `u-forge-core/src/search/
  sanitize.rs` and `u-forge-agent/src/lib.rs:113-128`. Re-export from core
  and import in agent.
- **L3.** Several UI sort sites use `sort_by(|a, b|
  a.1.to_lowercase().cmp(&b.1.to_lowercase()))` where `sort_by_key(|a|
  a.1.to_lowercase())` is the clippy-suggested cheaper form. Files:
  `node_editor/render.rs:1034`, `node_editor/mod.rs:635`,
  `node_panel.rs:85,90`.
- **L4.** `additional_params()` rebuilds a `serde_json::Map` per agent
  build (`u-forge-agent/src/lib.rs:971-1002`); cache it on `GraphAgent`.
- **L5.** Read-only `TextFieldView` still installs a blink task even though
  `cursor_visible` is forced false. Skip the blink task when read-only.
- **L6.** Magic constants in layout: `CELL_SIZE = 300.0` lacks a comment
  explaining `2.5 × IDEAL_LENGTH`. Either derive (`const CELL_SIZE: f32 =
  IDEAL_LENGTH * 2.5;`) or document the ratio.
- **L7.** `GraphEvent` carries no delta information (`observable.rs:14-22`),
  forcing subscribers to refetch the whole node on `NodeUpdated`. If
  per-property highlighting becomes a feature, this becomes a real cost.
- **L8.** Broadcast channel capacity is fixed at 64 (`observable.rs:36`).
  When a `Lagged` is delivered, subscribers fall back to a full snapshot
  rebuild without telemetry; under bulk imports this is the silent cause of
  occasional UI hitches.

---

## Dead code / orphaned scaffolding

- **D1. `u-forge-ts-runtime` is a literal stub.** `crates/u-forge-ts-runtime/
  src/lib.rs` is three lines of comments; `Cargo.toml` doesn't even depend on
  `deno_core`. Anything linking against the crate compiles but does nothing.
  See "Architecture friction" §F2 for the implications.
- **D2.** `crates/u-forge-core/examples/convert_memorymesh.rs` is the only
  example; it isn't built in CI (M9) so its drift status is unknown today.
  A sweep is needed.
- **D3.** Comments in `.rulesdir/rust-patterns.mdc:74-79` reference removed
  symbols (`FastEmbedProvider`, `EmbeddingManager`, `TranscriptionManager`,
  `DeviceWorker`, `hardware/`). Verified absent from the code; the doc is
  doing its job, but cross-check periodically.
- **D4.** No usage of `selection_model.rs` outside of the UI crate itself
  was flagged by the UI agent — review whether anything depends on its
  public surface or it can be made `pub(crate)`.

---

## Architectural friction — would make future enhancements painful

These are not bugs today, but the code commits to assumptions that will
become expensive to undo. List them so design choices for new features can
account for them.

### F1. `properties` as a single JSON blob blocks indexed property queries

`nodes.properties` is a JSON string. Filtering inside it requires
deserialising to Rust at the application layer or `json_extract` per-row at
the SQL layer. Adding "find all characters where `level >= 5`" requires
either:
- Migrating to a typed property column model (breaking change), or
- Adding SQLite expression indexes per property (workable but not currently
  supported by the schema).

If property-level filtering becomes a UI feature, expect a multi-PR
migration.

### F2. `u-forge-ts-runtime` skeleton has already locked in a serialisation contract

The crate depends on `u-forge-core` and `serde_json`, which presumes
TypeScript values are JSON-serialisable for graph storage. Real `deno_core`
isolates expose `v8::Local<v8::Value>` handles that aren't trivially
serialisable; bridging them costs a non-trivial design pass. Document the
trade-off in `feature_TS-Agent-Sandbox.md` before committing further to the
shape.

### F3. Multi-tenant / multi-user is unmodeled

`KnowledgeGraphStorage` is identity-agnostic. Adding per-user permissions
requires adding `user_id` columns to nodes/edges/chunks/positions and
threading the predicate through every query — multi-PR effort. Worth
factoring identity into the storage trait now if it's on the roadmap.

### F4. Undo/redo is not implementable without a journal

`INSERT OR REPLACE` on nodes and `DELETE FROM nodes WHERE id = ?` (with
cascade) are destructive. There is no audit log. Adding undo without a
journal table is impossible; with a journal, every mutation gains a side
write. Consider before committing more code that assumes destructive
in-place edits.

### F5. Embedding dimension space is hard-coded at two sizes (partially resolved)

Adding a third (e.g. 1024-dim Lemonade alternative or 8192-dim future
research model) means a new constant, a new table, new triggers, and search
pipeline branching. The current code has search hard-coded for "standard or
HQ", not "any registered space". A small refactor of the search merge into
"per registered embedding space, embed → ANN → contribute to RRF" makes the
addition trivial; today it doesn't.

### F6. Provider trait surface is single-vendor

`EmbeddingProvider`/`TranscriptionProvider` have only Lemonade impls. Adding
Ollama, OpenAI direct, or local fastembed needs no trait change but does
need
- `ProviderFactory` extended with non-Lemonade builds
- `ModelSelector` reworked beyond Lemonade catalog input
- HTTP client abstraction (`LemonadeHttpClient` is provider-specific).

The trait is the right shape; the factory and selector are the friction.

### F7. Job cancellation does not exist

Once submitted to the queue, a job cannot be cancelled. The chat path has a
"stop" button via `stream_task` cancellation, but embed/transcribe/rerank do
not. Long-running batch flows (e.g. "Embed All" on a large graph) cannot be
interrupted cleanly.

**Fix sketch:** Wrap each job with a `CancellationToken`; workers check before
each retry attempt.

### F8. No metrics export

Heavy use of `tracing::debug!` is good but not export-friendly. Adding the
`metrics` crate and emitting queue depth, job latency histograms, and GPU
contention durations would unblock both performance work and operational
monitoring.

### F9. Window focus / blur is unhandled

Modals, dropdowns, and context menus are not dismissed on app blur. Users
returning from Alt+Tab find stale overlays. A single `on_focus_change` on
`AppView::render` could broadcast a "dismiss overlays" signal to children.

---

## Documentation drift / prescriptive plans in shared docs

- `ARCHITECTURE.md:313` already calls out `save_schema` as known-misleading
  async — it should be fixed (M1).
- `.rulesdir/rust-patterns.mdc:91-97` mentions deprecated `EdgeType` enum
  variants that no longer exist in code (L1).
- `ARCHITECTURE.md:90-92` documents `chunks_vec` cleanup via
  `chunks_vec_ad` and is silent on `chunks_vec_hq_ad`, even though that
  trigger does exist in `graph/storage.rs:121-123`. Add a one-line mention.
- `.rules` Anti-Pattern #4 is being violated by C4. Either fix the code or
  amend the rule.

---

## Punch-list (turn into PRs)

Roughly ordered by impact-per-effort:

1. **M3** — fix the test-helper error string (1 line).
2. **M2** — `#[serde(deny_unknown_fields)]` on `AppConfig` (+ test).
3. **M8** — move `[profile.test]` out of cosmic-text-patched.
4. **L3** — clippy auto-fix the `sort_by_key` sites.
5. **L1, L6, D3** — doc trim for stale rules and undocumented constants.
6. **C5** — collision jitter in layout.
7. **C6** — switch R-tree update to bulk_load(survivors).
8. **C2 / H1** — fail-fast on cross-type name ambiguity in import; switch
   agent's `resolve_node` error to a type-summarised message.
9. **C1, H2** — wire `validate_and_coerce_properties` into ingest;
   add required-property checking; emit `MissingRequired` issues.
10. **H4** — store + cancel the search task; same shape for path-picker
    browse.
11. **C3** — wrap LLM completion in `tokio::time::timeout` and release the
    GPU guard before propagating.
12. **C4** — move `paint_entity.update` out of the canvas paint closure.
13. **H6** — JSON Schema validation on agent tool args.
14. **H7** — collapse epoch + cancel flag for embedding plan.
15. **H8** — change catalog discovery to `tokio::join!` + per-endpoint
    failure tracking.
16. **M9** — add CI.
17. **H3, H5, M5, M11** — observability and policy fixes that need
    discussion before coding.

The first six are mechanical and isolated; the remainder are design
conversations.
