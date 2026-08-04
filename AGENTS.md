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

Schemas are strict and authoritative. Data import must not infer or widen a
schema from JSONL input. Unknown object types, edge types, fields, endpoint
pairs, and missing required properties are import diagnostics. Widening the
accepted shape requires an explicit schema change.

## Workflow

Before editing, inspect the source and matching `.rulesdir` guidance. Prefer
narrow changes that preserve existing module boundaries. Use `rg` for searches.
Keep prescriptive implementation plans out of shared descriptive docs.

Run `make fmt-check`, `make check`, and `make test` before handing off a
substantial change. `make clippy` is the strict lint target and excludes the
vendored `cosmic-text` workspace member while still checking project crates.
