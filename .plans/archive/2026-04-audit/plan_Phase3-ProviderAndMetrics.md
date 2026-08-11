# Phase 3 — Provider Abstraction & Metrics Export

**Status (2026-08-04): Partially implemented.** L7 now has a shared core `GraphChange` stream emitted after commits, with object ids and complete edge endpoint/type data. The UI consumes and coalesces it, but object updates do not carry changed-property keys and the UI still rebuilds a snapshot. L8 is only adjacent: capacity changed from 64 to a fixed 256 and lag implies a rebuild, but capacity is not configurable and lag has no telemetry. F6, F8, and D4 remain open. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Reconciliation note:** `u-forge-graph-view::GraphEvent` is now an alias for
the core `GraphChange`; future L7/L8 work should extend that core contract
instead of recreating a second observable graph event type.

**Source findings:** F6, F8, L7, L8, D4

**Why Phase 3:** Provider abstraction (F6) is multi-PR architectural
work; metrics export (F8) unblocks performance work but needs
infrastructure choices; the smaller observability nits (L7, L8) and the
dead-code review (D4) ride along because they live in the same
"observability and surface area" theme.

**Branch name suggestion (design):**
`design/phase3-provider-and-metrics`

---

## Scope

| ID | What | Where |
|----|------|-------|
| F6 | Provider trait surface is single-vendor (Lemonade only) | `crates/u-forge-core/src/lemonade/` (factory, selector, http client) |
| F8 | No metrics export | Cross-cutting; `tracing::debug!` is the current proxy |
| L7 | `GraphEvent` carries no delta info — subscribers refetch full node | `crates/u-forge-core/src/graph/observable.rs:14-22` |
| L8 | Broadcast channel capacity fixed at 64; `Lagged` falls back without telemetry | `crates/u-forge-core/src/graph/observable.rs:36` |
| D4 | `selection_model.rs` may be UI-internal only — review public surface | `crates/u-forge/src/selection_model.rs` |

---

## Suggested approach

### F6 — generalise the factory and selector

The `EmbeddingProvider` and `TranscriptionProvider` traits are the right
shape; the friction is in:

1. **`ProviderFactory`** — currently only constructs Lemonade. Refactor
   to take a `ProviderKind` enum (`Lemonade`, `Ollama`, `OpenAi`, etc.)
   and a credential / endpoint config. Each kind is a feature gate.
2. **`ModelSelector`** — currently consumes a Lemonade catalog.
   Generalise to consume a `Vec<ModelDescriptor>` produced by whichever
   provider kind is active.
3. **`LemonadeHttpClient`** — extract a generic `ProviderHttpClient`
   trait; keep Lemonade as one impl. Watch for Lemonade-specific
   auth/headers.

Recommended sequencing: trait surface first, then a stub second-vendor
impl (e.g. Ollama, since it's local), then UI surface.

### F8 — `metrics` crate integration

1. Add the `metrics` and `metrics-exporter-prometheus` crates (or an
   equivalent — confirm with user first).
2. Emit:
   - Queue depth gauge per worker class.
   - Job latency histograms per provider + job kind.
   - GPU contention duration histogram.
   - Embedding-failure counter (closes the H3 observability gap).
3. Expose a Prometheus endpoint behind a config flag (or write to a
   local file for users without Prometheus).
4. Document the metric names in a stable contract.

### L7 — delta in `GraphEvent`

- Add a `changed_properties: Option<Vec<String>>` (or similar) to
  `NodeUpdated`.
- Subscribers that care about specific properties can short-circuit
  full refetch.
- For `EdgeAdded` / `EdgeRemoved`, include the edge endpoints.
- Bump the broadcast version contract; document.

### L8 — broadcast capacity + lag telemetry

- Make capacity configurable (default still 64).
- On `Lagged`: emit a metric (`graph.events.lagged`) and a `warn!` log.
- Document the recovery path (full snapshot rebuild) so it's not silent.

### D4 — `selection_model.rs` surface review

- Search the workspace for usages outside `u-forge`.
- If none, change the visibility to `pub(crate)`.
- Document the decision in a doc comment on the module.

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted tests:

- **F6:** unit test the `ProviderFactory` for each registered kind;
  contract test the trait via a mock impl that asserts the correct
  methods are called.
- **F8:** unit test that the metrics emission paths run (don't assert on
  the exporter; that's integration territory).
- **L7:** test that `NodeUpdated` carries delta when only a subset of
  properties changed; subscribers can act on that delta without a
  refetch.
- **L8:** test `Lagged` emission produces a metrics counter increment
  and a warn log.
- **D4:** `cargo build --workspace` after the visibility change must
  still build.

Manual verification:

- F8: scrape the metrics endpoint; verify the documented metrics appear
  with sensible values.
- F6: switch the configured provider kind to a stub Ollama impl;
  confirm the app initialises and surfaces a clear error if the stub
  isn't fully implemented.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — provider section: redesigned factory/selector
  surface; how to register a new provider. Metrics section: list every
  emitted metric, its type, and labels.
- **`README.md`** — note the supported provider kinds, even if only
  Lemonade is fully implemented at first.
- **`u-forge.toml`** — document the provider kind selection and metrics
  endpoint configuration.
- **`.rulesdir/`** — note the metric-naming convention for new
  observability work.
- **`bugfinding.md`** — leave alone.

---

## User input prompts

Pause and ask before:

1. **Second provider choice.** Audit suggests Ollama as a natural
   second; OpenAI direct is also possible. Ask the user which to stub
   first.
2. **Metrics framework.** `metrics` + Prometheus is the suggested
   default; alternatives include OpenTelemetry. Ask the user.
3. **Metrics endpoint exposure.** Local-only file? HTTP endpoint?
   Optional binding behind a config flag?
4. **L7 delta granularity.** Full property delta is information-rich
   but inflates events. Ask whether to ship full delta or just a list
   of changed property keys.
5. **L8 capacity default.** Keep 64? Bump to 256? Make it auto-tune
   with subscriber count?
6. **D4 — verify scope.** If `selection_model.rs` *is* used outside
   the UI crate, document why before removing visibility.

---

## Commit & push

Multi-PR plan:

1. `feat(graph): delta in GraphEvent + Lagged telemetry (L7, L8)` —
   smallest, ship first.
2. `chore(ui): tighten selection_model visibility (D4)` — trivial.
3. `feat(metrics): emit queue, job, GPU contention metrics (F8)` —
   medium; needs design sign-off.
4. `feat(provider): generalise factory + selector + http client (F6)` —
   largest; multi-step.

Each PR pushed and reviewed independently.

---

## Out of scope

- Tracing / span export (separate concern from metrics).
- Distributed tracing.
- Logging rework — `tracing` stays.
- Storage evolution (F1, F4, F5 — separate plan).
- Job cancellation (F7 — separate plan).
