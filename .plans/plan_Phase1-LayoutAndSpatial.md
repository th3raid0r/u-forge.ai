# Phase 1 — Layout & Spatial Stability

**Status (2026-08-03): Open.** Retained from the audit for follow-up. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Source findings:** C5, C6, M7, M12 (graph-view), plus L6 if not folded
into the mechanical sweep.

**Why this is its own branch:** All four findings live in
`crates/u-forge-graph-view/`. They share data structures (R-tree, layout
positions, snapshot) and one bug (C5) feeds another (M12). Coupling the
fixes prevents partially-applied changes that paper over symptoms.

**Branch name suggestion:** `fix/phase1-layout-spatial`

---

## Scope

| ID | What | Where |
|----|------|-------|
| C5 | Force-directed layout produces NaN/Inf when nodes coincide | `crates/u-forge-graph-view/src/layout.rs:84-94` |
| C6 | R-tree update relies on exact-position match for removal | `crates/u-forge-graph-view/src/snapshot.rs:316-340` |
| M12 | `node_at_position` masks NaN with `partial_cmp().unwrap_or(Equal)` | `crates/u-forge-graph-view/src/snapshot.rs:109` |
| M7 | Layout runs all 200 iterations even on tiny graphs | `crates/u-forge-graph-view/src/layout.rs:58` |
| (opt) L6 | Document or derive `CELL_SIZE` constant | `crates/u-forge-graph-view/src/layout.rs` |

Also include the C5-related defensive clamp in
`Viewport::screen_to_world` at `crates/u-forge-ui-traits/src/lib.rs:78-85`
(prevents a broken zoom from cascading NaN through hit-testing).

---

## Suggested approach

1. **C5 — collision jitter.** Replace the existing `dist_sq < 0.01`
   continue-guard with a small `MIN_SEPARATION_SQ` constant; when triggered,
   apply a deterministic nudge to one node's position, then `continue`.
   Reference snippet in `bugfinding.md` §C5. Confirm the nudge is
   deterministic so re-importing the same graph reproduces the same layout.
2. **M12 — NaN filter.** Filter out non-finite distances before `min_by`.
   Even with C5 fixed this is a cheap safety net for any future regression.
3. **C6 — R-tree bulk_load.** Switch from incremental `remove(&entry)` to
   the bulk_load(survivors) pattern shown in `bugfinding.md`. Verify the
   filter set is correct (`removed: HashSet<ObjectId>`).
4. **M7 — convergence early-exit.** Track `max_disp` per iteration; break
   when below a threshold after a minimum N iterations (e.g. 20). Pick the
   threshold conservatively so tiny graphs stop early but large graphs
   still relax fully.
5. **Viewport::screen_to_world clamp.** Defensive: if zoom is non-finite or
   zero, return the viewport centre rather than producing NaN.

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted unit tests to add (or extend) under `crates/u-forge-graph-view/`:

- **C5:** test that places two nodes at identical positions, runs one
  layout step, and asserts both positions are finite.
- **C6:** test that builds a snapshot, removes nodes whose positions are
  *not* identical to the previously-stored R-tree entries (simulate a
  drag), and asserts the R-tree no longer contains them.
- **M7:** test that a 3-node graph converges in fewer than 200 iterations
  (count the loop, expose via a return value or test-only field).
- **M12:** test that injecting a NaN position into the spatial index does
  not cause it to "win" hit-testing.

UI-level: there is no automated UI test harness, so manually verify by:
- opening the demo data graph,
- dragging two nodes into the same spot,
- triggering a layout pass,
- confirming no node visually disappears, no panic, no warning in logs.

If you cannot run the GPUI app in this environment, say so explicitly
rather than claiming UI verification.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — if there is a "Graph layout" section, add a
  note about the NaN-defensive contract: "layout positions are guaranteed
  finite; the spatial index assumes finite positions and rejects others
  defensively." Also update with the new convergence behaviour from M7
  (mention the early-exit threshold).
- **`.rulesdir/`** — if a graph-view rules file exists, add a one-liner
  about the bulk_load(survivors) idiom over `RTree::remove`.
- **`bugfinding.md`** — leave alone (it is the audit log).

---

## User input prompts

Ask the user before:

1. **M7 threshold choice** — the convergence threshold and minimum
   iteration count are policy. Propose `min_iterations = 20`,
   `convergence_threshold = 0.5` (in layout units) and ask for approval or
   alternative.
2. **C5 nudge direction** — `Vec2::new(0.5, 0.0)` is a defensible default,
   but if the user prefers a deterministic-but-pseudo-random nudge (hash
   of node id), ask which they want.

For C6 and M12 proceed without prompting; the changes have a clear right
answer.

---

## Commit & push

After all in-scope items pass tests:

1. Single commit, e.g.
   `fix(graph-view): NaN-proof layout, bulk_load R-tree, convergence early-exit (C5, C6, M7, M12)`.
2. The commit body should explicitly mention which findings from
   `bugfinding.md` it closes.
3. Push the branch to origin and open a PR. Wait for review before
   merging.

---

## Out of scope

- Layout algorithm replacement (sticking with force-directed).
- R-tree replacement (`rstar` stays).
- Performance tuning beyond M7's early-exit.
- Anything in `snapshot.rs` outside of the spatial-index update path.
