# Phase 3 — Multi-Tenant Identity Model

**Status (2026-08-03): Open.** Retained from the audit for follow-up. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Source findings:** F3

**Why Phase 3:** Adding identity to `KnowledgeGraphStorage` is a
multi-PR refactor that threads `user_id` (or equivalent) through every
storage call. It is gated entirely on a product decision: is multi-user
on the roadmap? If yes, refactor before more code accretes; if no,
documenting the limitation is the entire plan.

**Branch name suggestion (design):**
`design/phase3-multi-tenant-identity`

---

## Scope

| ID | What | Where |
|----|------|-------|
| F3 | Multi-tenant / multi-user is unmodeled | `crates/u-forge-core/src/graph/storage.rs` (and every consumer) |

---

## Suggested approach

### Step 1 — product decision (asked of the user)

Multi-tenant is invasive. Before any code:

1. Is multi-user on the roadmap at all?
2. If yes, what does "user" mean — a real human authenticated identity,
   a workspace-scoped project, or both?
3. Sharing model: private / read-only-share / collaborative-edit.
4. Scope of isolation: per-graph databases (file-level isolation) vs.
   per-user namespacing within one DB.

**If the answer is "no" or "not yet":** stop here. Document the
limitation in `ARCHITECTURE.md` and `README.md`, then close this plan.

**If the answer is "yes":** proceed.

### Step 2 — design document

Produce a feature doc covering:

1. **Identity primitive.** `UserId` newtype; whence is it sourced.
2. **Storage shape.** New `user_id` columns on:
   - `nodes`, `edges`, `chunks`, `positions`,
   - any other table that stores user-authored content.
3. **Predicate threading.** Every query gains a `user_id` filter.
   Helper trait or extension method?
4. **Sharing primitive.** ACL table or per-resource visibility flag.
5. **Migration.** Existing single-user databases need a backfill
   (everyone becomes a default `UserId`).
6. **API surface.** How does the UI / agent acquire identity? Login
   flow? Local-only mode for the existing single-user case?

### Step 3 — implementation, in stages

1. **Foundations:** introduce `UserId` and its persistence; add the
   columns and migration; existing tests pass with a default user.
2. **Predicate threading:** add the filter to every query. Write a
   contract test to assert that no query returns cross-user data.
3. **ACL layer:** introduce sharing primitives.
4. **UI surface:** identity selection, sharing controls.

---

## Testing instructions

Once code lands:

- Canonical command: `cargo test --workspace -- --test-threads=1`.
- Contract tests: for every storage method, assert isolation between
  two synthetic users (A's data is invisible to B).
- Migration tests: open a single-user DB, run the migration, assert
  the default user inherits all data and queries still work.
- Sharing tests: A shares X with B; assert B can read X but not
  unrelated data.

Manual verification:

- Run the app under user A; create content; switch to user B (or open
  as B from a fresh session); confirm A's content is invisible.

---

## Documentation fold-in

- **`feature_*.md`** — design doc lives here.
- **`ARCHITECTURE.md`** — substantial additions: identity model,
  migration plan, ACL layer.
- **`README.md`** — update positioning. The project is currently
  described as local-first single-user; multi-tenant changes that.
- **`bugfinding.md`** — leave alone.

If multi-tenant is *not* pursued, instead add a one-paragraph "single-
user-only" note to `ARCHITECTURE.md` explaining the limitation and the
data shape it would take to add it later.

---

## User input prompts

The whole plan is a user-input gate.

1. **Yes / no decision.** Is multi-tenant on the roadmap?
2. If yes, the questions in Step 1 above.
3. If no, confirm the user wants the limitation documented.

---

## Commit & push

If multi-tenant is pursued:

- Design PR first.
- Per-stage implementation PRs.
- Each PR is independently reviewable.

If documenting only:

- Single small PR with the limitation note.

---

## Out of scope

- Cloud sync / server-side replication (out of project scope per
  `CLAUDE.md` "local-first" framing — confirm with user).
- Authentication infrastructure (separate concern — pluggable).
- Provider abstraction (F6 — separate plan).
