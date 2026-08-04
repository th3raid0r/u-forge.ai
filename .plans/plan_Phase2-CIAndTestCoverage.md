# Phase 2 — CI & Test Coverage

**Status (2026-08-03): Partially implemented.** M6 is resolved with dimension-mismatch coverage for configured SQLite vector lanes. CI and example coverage remain open. Paths and symbols are authoritative; line references below describe the audit snapshot.

**Source findings:** M9, M6, D2

**Why Phase 2:** Wait for Phase 1 to land so the new CI doesn't trip on
known-fixed-but-not-yet-merged issues. Adding CI also implies a tightening
of the build bar (clippy, examples, all-features), which should be done
once Phase 1 fixes are settled.

**Branch name suggestion:** `chore/phase2-ci-and-tests`

---

## Scope

| ID | What | Where |
|----|------|-------|
| M9 | No CI; build/test/clippy regressions only caught locally | `.github/workflows/` (does not exist) |
| M6 | No test for the embedding-dimension mismatch error path | `crates/u-forge-core/src/graph/storage.rs` (`check_or_init_embedding_dims` guard) |
| D2 | `examples/convert_memorymesh.rs` not exercised; drift status unknown | `crates/u-forge-core/examples/` |

---

## Suggested approach

### M9 — minimal `.github/workflows/test.yml`

Create the workflow with these jobs (or one matrixed job):

1. **Build:** `cargo build --workspace`
2. **Test:** `cargo test --workspace -- --test-threads=1` (canonical
   command per `CLAUDE.md`).
3. **Examples build:** `cargo check --examples --all`
4. **Clippy:** `cargo clippy --workspace --no-deps -- -D warnings`
5. **Format:** `cargo fmt --all -- --check` (only if the project already
   uses `cargo fmt` consistently — check first).

Choose the runner size carefully — GPUI builds may need extra RAM. If
the `cosmic-text-patched` crate has system dependencies, document them
in the workflow with `apt-get install` or equivalent.

Cache the cargo registry and target directory. Use `Swatinem/rust-cache`
or equivalent.

Run on `push` to `main` and on every `pull_request`.

### M6 — embedding-dimension mismatch test

Add a test under `crates/u-forge-core/` (or wherever the existing
storage tests live):

1. Create a temp DB with the configured dimension (768 or 4096).
2. Connect directly to SQLite, overwrite
   `schema_metadata.chunks_vec_dims` to a known-wrong value (e.g. 999).
3. Drop the connection.
4. Reopen via the public API (`KnowledgeGraphStorage::open` or whatever
   it is).
5. Assert the returned error is `EmbeddingDimensionMismatch` with the
   expected fields.

This locks the guard behaviour against accidental short-circuits.

### D2 — `convert_memorymesh.rs` drift sweep

1. Run `cargo check --example convert_memorymesh` from a clean tree.
2. If it builds, run it against representative input under `data/` or
   `demo_data/` (whichever it is designed for) and confirm output.
3. Document required arguments / inputs in a top-of-file doc comment if
   missing.
4. Once CI from M9 is in place, the example is automatically built.

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Plus:

- `cargo check --examples --all` to validate example builds.
- `cargo clippy --workspace --no-deps -- -D warnings` if you want to
  match what CI will run.

After the workflow file lands, push it and verify the actions run green
on the PR. If anything fails, fix it before merging.

---

## Documentation fold-in

- **`README.md`** — add a "CI" badge and a one-line description of the
  pipeline.
- **`CLAUDE.md`** — note that CI runs the canonical test command on
  every PR.
- **`ARCHITECTURE.md`** — under "Design Decisions" or similar, mention
  the embedding-dimension check is now test-locked.
- **`bugfinding.md`** — leave alone.

---

## User input prompts

Pause and ask before:

1. **Runner size / cost.** GitHub Actions free tier may be tight for a
   workspace with GPUI. Confirm the user is OK with the runtime cost or
   ask whether to scope the matrix narrower (e.g. test on Ubuntu only,
   skip macOS).
2. **Format check.** Ask whether `cargo fmt --check` should be enforced
   in CI. If the codebase has historical inconsistencies, this can
   produce a noisy first PR.
3. **Required system dependencies.** If the build needs system libs
   (likely for GPUI / cosmic-text), confirm the apt-get list with the
   user — they know what their dev environment installs.
4. **D2 scope.** If the example is broken, ask whether to fix it now or
   delete it as dead scaffolding.

---

## Commit & push

When the workflow runs green on a draft PR:

1. Three commits, in order:
   - `test(core): lock embedding-dimension mismatch error path (M6)`
   - `chore(examples): refresh convert_memorymesh and document inputs (D2)`
   - `ci: add build + test + clippy + examples workflow (M9)`
2. Push and open a PR. Do not merge until CI itself is green.

---

## Out of scope

- Adding new test infrastructure (proptest, criterion benches, fuzzing).
- Restructuring crate-level test layout.
- Coverage tooling (codecov, etc.).
- Release pipelines / publishing crates.
