# Phase 2 — Search Observability & HQ Backfill

**Status (2026-08-04): Partially implemented.** H3 now has a core `SearchResponse` with capability flags and degradation reasons, and hybrid search falls back to FTS5 when an embedding lane is unavailable or fails fingerprint validation. It is not complete: an embedding request that fails after capability detection is still only logged, and SearchPanel currently discards the response metadata instead of rendering a degradation hint. M11 remains open. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Reconciliation note:** The implemented response uses
`degraded_reasons: Vec<String>` rather than the originally proposed
`fts_only`/`semantic_failed` fields so it can describe standard semantic, HQ,
and reranking capability independently. Preserve that broader contract when
finishing H3.

**Source findings:** H3, M11

**Why Phase 2:** Both findings shape user-visible search quality but
neither is a sharp data-loss bug. They benefit from Phase 1's import
validation and Lemonade stability landing first (so degraded search has a
healthier upstream to compare against). Both also need a small product
discussion before coding.

**Branch name suggestion:** `feat/phase2-search-observability`

---

## Scope

| ID | What | Where |
|----|------|-------|
| H3 | Search silently degrades to FTS-only on embedding failure | `crates/u-forge-core/src/search/mod.rs` (embed → ANN stage) |
| M11 | No `BackfillHq` plan variant when standard vec is populated but HQ isn't | `crates/u-forge-core/src/queue/` (`EmbeddingPlan`); per-node re-chunk path in `graph/fts.rs` |

---

## Suggested approach

### H3 — degradation flag in search results

- Extend the search result struct (whatever the public type returned by
  `search/mod.rs` is) with `fts_only: bool` and `semantic_failed:
  Option<String>` (carrying the upstream error so the UI can surface it).
- The current `warn!` path becomes: set `semantic_failed = Some(...)`,
  return FTS-only results, escalate the log to `error!` (or emit a
  metric, if M9/F8 lands first).
- Plumb the flag through to the UI; the search panel renders a small
  status hint like "semantic search unavailable — showing keyword
  results only".
- Keep the existing alpha-controlled "deliberately FTS-only" path
  distinguishable from the failure path.

### M11 — `BackfillHq` embedding plan variant

- Audit `EmbeddingPlan` for the existing variants. Add `BackfillHq` (or
  rename for clarity) that targets chunks present in `chunks_vec` but
  missing from `chunks_vec_hq`.
- Implementation strategy: a SQL query that does a `LEFT JOIN` between
  `chunks_vec` (or `chunks` + standard embedding presence) and
  `chunks_vec_hq` to find the gap.
- Hook into the existing per-node re-chunk so HQ-only backfill is a
  separate code path from "embed everything".
- Verify the new variant respects the `chunks_vec_hq_ad` cleanup trigger
  (already exists per `graph/storage.rs:121-123`).

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted tests:

- **H3 unit:** mock the embedding queue so `embed(query)` fails; assert
  the search result carries `fts_only = true` and `semantic_failed =
  Some(err_message)`.
- **H3 alpha=1.0:** assert that explicit FTS-only mode still returns
  `fts_only = true` but `semantic_failed = None`.
- **M11:** populate `chunks_vec` for a node, leave `chunks_vec_hq` empty,
  run the new `BackfillHq` plan, assert `chunks_vec_hq` is populated.
- **M11 idempotency:** running `BackfillHq` twice on a fully-populated DB
  should be a no-op.

Manual verification (if a fully-running stack is available):

- Stop the embedding worker mid-search; observe the UI surface the
  degradation hint.
- Toggle the HQ embedding model on/off mid-session; trigger a backfill
  and confirm chunks fill in.

If unable to run the stack, document the gap in the PR description.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — search pipeline section: document the
  degradation contract (fts_only / semantic_failed flags). Document the
  `BackfillHq` plan variant and how it fits the embedding-plan taxonomy.
- **`README.md`** — if it lists supported search modes, add a note about
  graceful degradation.
- **`.rulesdir/`** — if there is a search-rules file, mention that
  callers should check `fts_only` before claiming hybrid coverage.
- **`bugfinding.md`** — leave alone.

---

## User input prompts

Pause and ask before:

1. **H3 logging policy.** Audit suggests escalating `warn!` to `error!`.
   Ask the user whether to do that, or whether they prefer adding a
   metrics counter (which depends on F8 in Phase 3) and keeping the log
   at `warn!`. The right answer depends on whether the user has
   downstream alerting.
2. **H3 UI surface.** The status hint placement is a UX decision —
   inline in the result list, in a header banner, or as a toast. Ask.
3. **M11 trigger.** Should backfill run automatically when an HQ provider
   appears, or only on explicit user action? Auto is more convenient;
   manual avoids surprise GPU usage.

---

## Commit & push

When tests pass:

1. Two commits is fine:
   - `feat(search): expose degradation flags when semantic search fails (H3)`
   - `feat(queue): BackfillHq embedding plan variant for HQ index gaps (M11)`
2. Push and open a PR.

---

## Out of scope

- Refactoring the search merge to support arbitrary embedding spaces
  (that is F5, Phase 3).
- Adding metrics infrastructure (F8, Phase 3) — H3 stops at a flag, not
  a metric.
- Reranking changes.
