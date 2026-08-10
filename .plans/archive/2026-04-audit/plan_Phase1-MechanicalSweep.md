# Phase 1 — Mechanical Sweep

**Status (2026-08-03): Partially implemented.** M3 and L3 are resolved, and project crates pass strict clippy. The remaining items are still open. Paths and symbols are authoritative; line references below describe the audit snapshot.

**Source findings:** M1, M2, M3, M8, L1, L2, L3, L5, L6, D3
(see `bugfinding.md` "Punch-list" items 1–5).

**Why this is its own branch:** Every item here is an isolated one-file or
one-line change. They share no logic but cluster well as a single PR because
each fix is too small to merit its own review cycle. Land them together to
clear the noise floor before the substantive Phase 1 work touches the same
files.

**Branch name suggestion:** `chore/phase1-mechanical-sweep`

---

## Scope

| ID | One-line description | File pointer |
|----|----------------------|--------------|
| M1 | Drop `async` from `save_schema`; remove `.await` at all (4) call sites | `crates/u-forge-core/src/schema/manager.rs:63-70` |
| M2 | Add `#[serde(deny_unknown_fields)]` to `AppConfig` and nested sections; add a unit test that an unknown TOML key fails | `crates/u-forge-core/src/config.rs:597` |
| M3 | Fix probe-port string in test helper error | `crates/u-forge-core/src/test_helpers.rs:45` |
| M8 | Remove or relocate `[profile.test]` from non-root crate | `crates/cosmic-text-patched/Cargo.toml:186-188` |
| L1 | Trim stale `EdgeType` deprecated-variant references | `.rulesdir/rust-patterns.mdc:91-97` |
| L2 | De-duplicate `fts5_sanitize`: keep one in core, re-export, import in agent | `crates/u-forge-core/src/search/sanitize.rs` ↔ `crates/u-forge-agent/src/lib.rs:113-128` |
| L3 | Apply clippy `sort_by_key` suggestion | `crates/u-forge/src/node_editor/render.rs:1034`, `node_editor/mod.rs:635`, `node_panel.rs:85,90` |
| L5 | Skip blink task installation when `read_only == true` | `crates/u-forge/src/text_field.rs` (search for blink task setup) |
| L6 | Either derive `CELL_SIZE` from `IDEAL_LENGTH` or document the 2.5× ratio | `crates/u-forge-graph-view/src/layout.rs` |
| D3 | Cross-check `.rulesdir/rust-patterns.mdc:74-79` for removed-symbol references; trim if stale | `.rulesdir/rust-patterns.mdc:74-79` |

---

## Suggested order

1. M3 first — smallest possible diff, useful to confirm test pipeline is green.
2. M8 next — silences a workspace warning that pollutes every subsequent build.
3. M1 — touch graph plumbing only after the easy wins land.
4. M2 — add the `deny_unknown_fields` test alongside.
5. L2 — affects two crates; do it on a clean tree.
6. L3, L5, L6, L1, D3 — the doc/clippy/cosmetic batch.

---

## Testing instructions

Canonical command (project rule, see `CLAUDE.md`):

```
cargo test --workspace -- --test-threads=1
```

Targeted checks per item:

- **M2:** add a unit test under `crates/u-forge-core/src/config.rs` (or a
  sibling tests module) that round-trips a TOML with an unknown key and
  asserts the error path. Must run as part of `cargo test --workspace`.
- **M1:** `cargo build --workspace` should reveal every `.await` site that
  needs adjustment after dropping `async`. Confirm the tests still pass.
- **M8:** `cargo build --workspace` should no longer warn about the
  non-root profile override.
- **L3:** `cargo clippy --workspace --no-deps -- -D warnings` to confirm
  clippy is now clean for those sites.
- **L1, L6, D3:** documentation only; verify `grep -rn` no longer finds
  stale references.

If any change touches behaviour you cannot exercise from a unit test (e.g.
the read-only blink task in L5), say so explicitly in the PR description
rather than claiming success.

---

## Documentation fold-in

After landing the code changes, update:

- `ARCHITECTURE.md:313` — remove the "save_schema is misleading async" note
  once M1 is done.
- `.rulesdir/rust-patterns.mdc` — apply the L1 and D3 trims; if any other
  symbol references are stale, fix in the same pass.
- If M1 changes the call shape for any consumer, update any code-snippet
  example in `ARCHITECTURE.md` that still shows `.await`.
- Note the new `deny_unknown_fields` behaviour in any config-related doc
  (e.g. `README.md` if it documents `u-forge.toml`).

---

## User input prompts

You should pause and ask the user before proceeding when:

1. **M8 placement decision** — ask whether to *delete* the
   `[profile.test]` block (safe default) or *relocate* it to the workspace
   root `Cargo.toml` (preserves the override, but the user may not actually
   want it). Quote the current contents in your prompt so they can decide.
2. **L1 / D3 doc trim** — if the cross-references are not just stale but
   describe a behaviour the user might still want documented elsewhere, ask
   before deleting.

For everything else in this plan, proceed without prompting.

---

## Commit & push

When all in-scope items are landed and tests pass:

1. Stage the changes by file (avoid `git add -A`).
2. Create a single commit titled e.g.
   `chore: phase 1 mechanical sweep (M1-M3, M8, L1-L6, D3)` whose body
   bullet-lists each finding addressed.
3. Push the branch to origin and open a PR. Do not merge unattended — wait
   for user review.

---

## Out of scope

Anything that requires logic changes beyond what is listed (no behavioural
refactors, no new abstractions). If a fix turns out to need design
discussion, drop it from this PR and add a note pointing at the relevant
Phase 2 / Phase 3 plan.
