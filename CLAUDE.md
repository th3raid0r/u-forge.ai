# u-forge.ai — Claude Code Context

Read `.rules` first for every task. It contains the anti-patterns and task-based
routing. Rule files in `.rulesdir/` have more detail by topic. `ARCHITECTURE.md`
has the workspace layout, SQLite schema, inference design, and design decisions.

**Canonical test command:** `cargo test --workspace -- --test-threads=1`

## Project summary

Local-first TTRPG worldbuilding tool with an AI-powered knowledge graph. Written
in Rust. Native desktop UI built on GPUI (Zed editor's GPU-accelerated framework).
Uses SQLite (FTS5 + sqlite-vec ANN) for storage, Lemonade Server for all AI
inference (embedding, STT, TTS, LLM, reranking). The `KnowledgeGraph` facade is
intentionally decoupled from AI — it works without a running server. AI
capabilities are opt-in via `InferenceQueue`.
