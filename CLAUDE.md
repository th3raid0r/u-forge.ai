# u-forge.ai — Claude Code Context

Read `.rules` first for every task. It contains the anti-patterns and task-based
routing. Rule files in `.rulesdir/` have more detail by topic. `ARCHITECTURE.md`
has the workspace layout, SQLite schema, inference design, and design decisions.

Use [GitHub Issues](https://github.com/th3raid0r/u-forge.ai/issues) for plans, bug
reports, audit findings, and follow-ups. For issue intake, implementation, and
review, follow [`.rulesdir/development-workflow.mdc`](.rulesdir/development-workflow.mdc).
Do not maintain a second backlog in repository files.

**Canonical test command:** `make test`

## Project summary

Local-first TTRPG worldbuilding tool with an AI-powered knowledge graph. Written
in Rust. Native desktop UI built on GPUI (Zed editor's GPU-accelerated framework).
Uses SQLite (FTS5 + sqlite-vec ANN) for storage, Lemonade Server for all AI
inference (embedding, STT, TTS, LLM, reranking). The `KnowledgeGraph` facade is
intentionally decoupled from AI — it works without a running server. AI
capabilities are opt-in via `InferenceQueue`.
