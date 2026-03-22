# u-forge.ai — Claude Code Context

Read `.rules` first for every task. Rule files in `.rulesdir/` have more detail by topic.

**Canonical test command:** `cargo test -- --test-threads=1`

---

## Project summary

Local-first TTRPG worldbuilding tool with an AI-powered knowledge graph. Written in Rust. Uses SQLite (FTS5 + sqlite-vec ANN) for storage, Lemonade Server for all AI inference (embedding, STT, TTS, LLM, reranking). The `KnowledgeGraph` facade is intentionally decoupled from AI — it works without a running server. AI capabilities are opt-in via `InferenceQueue`.

---

## Key Anti-Patterns

See `.rules` for the full list. Summary:

1. **Env vars are overrides, not requirements.** `cargo run --example cli_demo` must work with zero env vars set. Always try `http://localhost:8000/api/v1` first.
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
| `ARCHITECTURE.md` | Full module map, SQLite schema, hardware design, design decisions |

---

## Work in Progress

*No active work items.*
