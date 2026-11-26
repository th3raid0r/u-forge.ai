# u-forge.ai — Agent Context

Read `.rules` first for every task. It contains active anti-patterns and
task-based routing. Read only the relevant files in `.rulesdir/`, and use
`ARCHITECTURE.md` for workspace and subsystem context.

The canonical full test target is `make test`. Prefer the root `Makefile` for
repeatable build, check, formatting, clippy, and test commands. Do not use a
single-test-name filter as final verification.

## Project summary

u-forge.ai is a local-first TTRPG worldbuilding tool written in Rust. Its native
desktop UI uses GPUI. Knowledge graph persistence uses SQLite with FTS5 and
sqlite-vec ANN indexes. Lemonade Server provides optional embedding, STT, TTS,
LLM, and reranking capabilities through `InferenceQueue`; `KnowledgeGraph`
itself must remain usable without a running inference server.

Persisted schemas are strict and authoritative. The desktop import flow requires
them; unknown object types, edge types, fields, endpoint pairs, and missing
required properties become import diagnostics. The low-level `DataIngestion`
API retains a schema-less compatibility mode for an empty graph, so callers
that require strict import must load schemas first. Import never infers or
mutates a persisted schema from JSONL records.

## Workflow

Track plans, bugs, and audit findings in
[GitHub Issues](https://github.com/th3raid0r/u-forge.ai/issues), not in local
plan files. For tracked work, read the issue, its latest decisions and any
linked PR before editing. Follow the issue intake, audit, and PR workflow in
[`.rulesdir/development-workflow.mdc`](.rulesdir/development-workflow.mdc).

Inspect the source and matching `.rulesdir` guidance before making narrow
changes that preserve existing module boundaries. Use `rg` for searches.

Run `make fmt-check`, `make check`, and `make test` before handing off a
substantial change. `make clippy` is the strict lint target for project crates.
The vendored `cosmic-text` patch is excluded from workspace resolution but is
formatted and tested separately by the canonical Makefile targets.
