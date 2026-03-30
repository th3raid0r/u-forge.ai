# u-forge.ai — Claude Code Context

Read `.rules` first for every task. Rule files in `.rulesdir/` have more detail by topic.

**Canonical test command:** `cargo test --workspace -- --test-threads=1`

---

## Project summary

Local-first TTRPG worldbuilding tool with an AI-powered knowledge graph. Written in Rust. Uses SQLite (FTS5 + sqlite-vec ANN) for storage, Lemonade Server for all AI inference (embedding, STT, TTS, LLM, reranking). The `KnowledgeGraph` facade is intentionally decoupled from AI — it works without a running server. AI capabilities are opt-in via `InferenceQueue`.

---

## Key Anti-Patterns

See `.rules` for the full list. Summary:

1. **Env vars are overrides, not requirements.** `cargo run --manifest-path crates/u-forge-core/Cargo.toml --example cli_demo` must work with zero env vars set. Always try `http://localhost:8000/api/v1` first.
2. **Fetch live state before assuming capabilities.** Use `SystemInfo::fetch()` and `LemonadeModelRegistry::fetch()`.
3. **Docs are indexes, not mirrors.** Don't duplicate what source comments already say.

---

## Important Resources

| Resource | Purpose |
|---|---|
| `.rules` | Routing guide + anti-patterns — read first for every task |
| `.rulesdir/project-structure.mdc` | Module map, architecture, integration points |
| `.rulesdir/rust-patterns.mdc` | Error handling, traits, async, storage patterns |
| `.rulesdir/testing-debugging.mdc` | TempDir setup, skip guards, CI commands |
| `.rulesdir/development-workflow.mdc` | Build/run commands, provider addition, Lemonade setup |
| `.rulesdir/environment-config.mdc` | Env vars, auto-discovery order |
| `.rulesdir/schema-system.mdc` | Schema format, validation, property types |
| `.rulesdir/json-data-formats.mdc` | JSONL import format, two-pass pipeline |
| `ARCHITECTURE.md` | Full module map, workspace layout, SQLite schema, hardware design, design decisions |
| `feature_UI.md` | GPUI native UI — graph view model, spatial indexing, GPUI prototype |
| `feature_TS-Agent-Sandbox.md` | deno_core TypeScript agentic sandbox with restricted op surface |

---

## Work in Progress

### Active features

**Parallel tracks (now unblocked — workspace split is complete):**
- **`feature_UI.md`** — Native GPUI graph visualization: `u-forge-graph-view` view model + GPUI prototype. Skeleton crate at `crates/u-forge-ui-gpui/`.
- **`feature_TS-Agent-Sandbox.md`** — Embedded V8 TypeScript sandbox: `u-forge-ts-runtime` with `deno_core` ops and `.d.ts` contract. Skeleton crate at `crates/u-forge-ts-runtime/`.
