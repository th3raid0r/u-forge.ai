# Phase 3 — Storage Evolution

**Status (2026-08-03): Partially implemented.** SQLite's two vector lanes are now dimension-configurable and guarded by persisted metadata, but the general embedding-space registry proposed by F5—and F1/F4—remain open. Paths and symbols are authoritative; line references below describe the audit snapshot.

**Source findings:** F1, F4, F5

**Why Phase 3:** Each item is multi-PR architectural work that locks in
contracts shaping every other feature. They benefit from being designed
together because a journal table (F4) and embedding-space registry (F5)
both touch the same `schema_metadata` and migration tooling that any
property-column move (F1) would also touch.

**This plan is an architecture brief, not a coding plan.** A subagent
expanding it should produce a design document first and a multi-PR
implementation plan second.

**Branch name suggestion (design doc only):**
`design/phase3-storage-evolution`

---

## Scope

| ID | What | Constraint |
|----|------|------------|
| F1 | `properties` JSON blob blocks indexed property queries | `nodes.properties` is currently a JSON string |
| F4 | Undo/redo not implementable without a journal | All node mutations are destructive |
| F5 | Embedding dimension space hard-coded to 768 / 4096 | `chunks_vec` and `chunks_vec_hq` are siblings, not a registry |

---

## Suggested approach

### Step 1 — write a design document

Before any code, produce a design document under `feature_*` (matching
the existing `feature_TS-Agent-Sandbox.md` shape) covering:

1. **Goals and non-goals** for each finding.
2. **Migration strategy.** Existing user databases must continue to
   open. Decide between in-place migrations versus export/import.
3. **Schema sketch.** New tables / columns / triggers, in SQL, with
   commentary.
4. **Backward compatibility.** Which APIs change shape; deprecation
   plan.
5. **Performance impact.** Especially for F1 — typed columns can
   improve query performance but complicate ingest.

### Step 2 — F1: typed property columns or expression indexes

Two options:

- **(a) Typed columns:** introduce per-type secondary tables keyed by
  `node_id`, each storing a typed slice of the schema. Heavy migration,
  schema-driven DDL.
- **(b) SQLite expression indexes** on selected JSON paths:
  `CREATE INDEX … ON nodes(json_extract(properties, '$.level'))`. Lower
  effort, narrower benefit.

Audit recommends thinking about (a) but starting with (b) where
property-level filtering is a near-term UI feature.

### Step 3 — F4: journal table for undo/redo

- Add `mutation_journal(id, ts, op, before_blob, after_blob, actor)`.
- Every node/edge mutation writes a journal row in the same transaction.
- Undo replays the inverse of `before_blob`.
- Decide whether the journal is bounded (size cap, age cap) or
  permanent (full audit trail).

### Step 4 — F5: registered embedding spaces

- Replace the two-sibling `chunks_vec` / `chunks_vec_hq` model with a
  registry table: `embedding_spaces(name, dims, table_name, …)`.
- Search RRF iterates registered spaces instead of branching on
  "standard or HQ".
- New spaces become a row insert + a generated table; no code changes
  required for additional dimensions.

---

## Testing instructions

This is design-phase work. Once code starts:

- Canonical command: `cargo test --workspace -- --test-threads=1`.
- Add migration tests: open a DB at the old schema, run the migration,
  assert the new schema is correct and existing data is preserved.
- Add property-query tests for F1 covering equality, range, and
  presence/absence filters.
- Add undo/redo tests for F4: mutation, undo, redo, mutate-after-undo
  invalidates redo stack.
- Add a third-space test for F5: register a hypothetical 1024-dim
  space; embed; search; assert results.

Manual verification: import a real-size graph (`data/`), exercise the
new query paths, and confirm performance is acceptable.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — substantial updates: SQLite schema section
  needs the new tables; design-decision section needs the rationale for
  each change.
- **`feature_*.md`** — produce a per-finding feature doc (or one
  combined doc) detailing the migration plan.
- **`README.md`** — note that databases require a one-time migration on
  first open after upgrade.
- **Migration runbook** — produce a separate runbook for users with
  existing graphs explaining the migration steps.

---

## User input prompts

This whole plan exists to gather user input. Specifically ask:

1. **Priority among F1 / F4 / F5.** They are independent; the user may
   want to ship one before the others.
2. **F1 strategy.** Typed columns vs. expression indexes. The choice
   has UX consequences (which queries are fast).
3. **F4 retention policy.** Bounded journal or permanent audit trail?
   Affects storage growth and undo distance.
4. **F5 scope.** Register-time-only, or hot-pluggable at runtime?
5. **Backward compatibility.** Confirm whether existing user databases
   must migrate cleanly, or whether a re-import is acceptable.
6. **Sequencing.** Land the design doc as a PR first, get feedback,
   *then* code? Recommended.

---

## Commit & push

Design phase:

1. Commit the design document on a `design/` branch.
2. Open a PR for review of the design only (no code).
3. After design approval, follow up with implementation PRs per
   finding.

Implementation phase: each finding gets its own PR(s). Do not collapse
all three into one mega-PR.

---

## Out of scope

- Replacing SQLite with another store.
- Multi-tenant identity (F3 — separate plan).
- Job cancellation infrastructure (F7 — separate plan).
- Provider abstraction (F6 — separate plan).
