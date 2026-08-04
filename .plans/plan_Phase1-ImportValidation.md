# Phase 1 — Import Validation & Edge Resolution Correctness

**Status (2026-08-03): Partially implemented.** Strict schema-backed import now drops undeclared properties, skips unknown types, enforces required properties at the import boundary, validates edge types/endpoints, and emits JSONL diagnostics. Cross-type name ambiguity (C2) and broader schema-manager coercion policy remain open. Paths and symbols are authoritative; line references below describe the audit snapshot.

**Source findings:** C1, C2, H1, H2

**Why this is its own branch:** All four findings are about the JSONL
ingest path silently accepting bad data. They share the schema-manager
validator and the cross-type name resolution code. Fixing them together
prevents inconsistent partial fixes (e.g. wiring up the validator without
adding required-property checks).

**Branch name suggestion:** `fix/phase1-import-validation`

---

## Scope

| ID | What | Where |
|----|------|-------|
| C1 | Property validation skipped during JSONL import | `crates/u-forge-core/src/ingest/data.rs:326-348`, `crates/u-forge-core/src/schema/manager.rs:459-548` |
| H2 | `validate_and_coerce_properties` doesn't enforce `required` properties | `crates/u-forge-core/src/schema/manager.rs:459-548` |
| C2 | Cross-type name collisions silently pick wrong node during edge resolution | `crates/u-forge-core/src/ingest/data.rs:245-271` |
| H1 | Agent's `resolve_node` returns unhelpful "5 of 47 matches" message | `crates/u-forge-agent/src/lib.rs:713-741` |

---

## Suggested approach

1. **H2 first — required-fields pass.**
   - Add `PropertyIssue::MissingRequired { key: String }` variant.
   - Top of `validate_and_coerce_properties`: iterate
     `ObjectTypeSchema::required_properties`, emit an issue for each
     missing key.
   - Make this addition first because C1 needs the issue list to be
     correct before wiring it into the import.

2. **C1 — wire validator into import.**
   - In `add_properties_to_builder`, call
     `schema_manager.validate_and_coerce_properties(object_type, &mut props)`.
   - Decide policy per `PropertyIssue`:
     - `UnknownProperty` → warn + retain (or drop, ask user — see prompts).
     - `TypeMismatch` → coerce when possible; otherwise warn or fail.
     - `InvalidEnumValue` → fail the import with a clear error.
     - `MissingRequired` → fail the import.
   - Surface a per-record error/warning aggregate so the user can fix
     their JSONL in one pass instead of fix-rerun cycles.

3. **C2 — cross-type ambiguity = fail-fast.**
   - In `resolve_node_id`, when `find_by_name_only` returns multiple
     matches, return an error listing all matches (id + type). Drop the
     "use first match" silent fallback.
   - Recommend in the error that the import format use `type:name` to
     qualify references.

4. **H1 — agent message improvement.**
   - In `u-forge-agent`'s `resolve_node`, replace the "5 of n" listing
     with a type-summarised message when n > 10:
     `"matched 47 nodes across types: character (12), event (30), faction (5) — narrow by type or pass a UUID"`.
   - When n ≤ 10, list all (current code lists 5; bump to 10).

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted tests to add:

- **H2:** unit test in `schema/manager.rs` with a schema declaring
  `required: ["name", "description"]`; assert `MissingRequired` issue is
  raised when description is omitted.
- **C1:** integration-style test under
  `crates/u-forge-core/tests/` (if one exists; otherwise inside
  `ingest/data.rs`'s tests) that imports a JSONL where:
  (a) a property is misspelled (expect warn + drop or warn + retain
   per chosen policy);
  (b) a `Number` property is supplied as `String("42")` (expect coerced);
  (c) a `required` property is missing (expect import to fail with a
   precise error).
- **C2:** test that imports JSONL with two nodes named "Gandalf" of
  different `object_type`s and a third record with an unqualified edge
  reference; assert the import fails with all matches listed.
- **H1:** unit test on `resolve_node` that simulates 47 matches and
  asserts the type-summarised message format.

Manual verification: run the demo data ingest under `data/` or
`demo_data/`; confirm import still succeeds for clean data and fails with
a usable error message for the test cases above.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — under the SQLite schema or ingest section, add
  a paragraph describing the new validation contract: "all imports go
  through `validate_and_coerce_properties`; `MissingRequired` and
  `InvalidEnumValue` are hard failures, `TypeMismatch` is coerced when
  possible, `UnknownProperty` is a warning."
- **`README.md`** — if it documents the JSONL import format, mention the
  recommended `type:name` qualifier for cross-type references.
- **`.rulesdir/`** — if there is an ingest or schema rules file, note the
  fail-fast rule on cross-type ambiguity.
- **`bugfinding.md`** — leave alone.
- Demo / sample data under `data/` and `demo_data/` may need updates if
  the new validation is stricter than what's there. Run the import
  against them and fix any newly-revealed schema drift.

---

## User input prompts

You should pause and ask the user before:

1. **C1 unknown-property policy.** The audit suggests "warn on
   UnknownProperty"; ask whether the user wants to *drop* unknown
   properties or *retain* them (warning either way). The choice has
   downstream consequences — a typo is preserved in the JSON blob if
   retained, lost if dropped.
2. **C2 reference-format expectation.** Ask whether to:
   (a) require `type:name` everywhere and fail any unqualified reference
   (stricter, more invasive — may require updating `data/` samples), or
   (b) keep unqualified references as a fast path that *only* fails when
   ambiguous (more compatible).
   The audit recommends (b); confirm before coding.
3. **C2 demo-data updates.** If running the new validator against
   `data/` or `demo_data/` reveals errors, ask whether to clean the data
   in this PR or in a follow-up.

---

## Commit & push

After in-scope items pass:

1. Consider splitting into two commits if the diff is large:
   - `fix(core): wire validator into JSONL import; require fields; coerce types (C1, H2)`
   - `fix(core,agent): fail-fast on cross-type name ambiguity; type-summarised matches (C2, H1)`
2. Push the branch and open a PR. Do not merge unattended.

---

## Out of scope

- Migrating the `properties` JSON-blob to typed columns (that is F1,
  Phase 3).
- Restructuring `JsonDataIngestion` more broadly.
- Changing how schemas are loaded or stored.
