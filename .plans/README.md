# Plan Ledger

Last reconciled: 2026-08-24 on `feature/inference-queue-lifecycle`.

Source code and verified behavior are authoritative. This directory contains
only active or deliberately parked product work; completed checklists are audit
records under `archive/` and are not current implementation instructions.

## Remediation

| Plan | State | Progress |
|------|-------|----------|
| [Adaptive Agent Context Optimization](feature_agent-context-optimization.md) | Planned | Impact rank 3; removes rollback residue and replaces per-source budgets with pressure-triggered compaction. Covers `AGENT-02`, `AGENT-03`, `AGENT-05`, `AGENT-10`, and `ALLOW-05`. |
| [Typed Chat Run Lifecycle](feature_chat-run-lifecycle.md) | Planned | Impact rank 4; covers `AGENT-01`, `AGENT-04`, `AGENT-09`, `UI-08`, and the `ALLOW-10` acceptance outcome. |
| [Catalog-Derived Lemonade Setup and Activation](feature_lemonade-setup-activation.md) | Planned | Impact rank 5; covers `UI-04`, `UI-01`, `UI-05`, and `LEMON-08`. |

The source inventory and full surfaced/planned/remediated candidate ledger live in
[`v0.1.1_analysis.md`](v0.1.1_analysis.md).

## Completed remediation

| Plan | Completed | Outcome |
|------|-----------|---------|
| [Staged Hybrid Search Pipeline](archive/feature_hybrid-search-pipeline.md) | 2026-08-24 | Remediated `CORE-01` and `AGENT-07`; removed `ALLOW-11`; preserved the public search and cancellation contracts. |
| [Unified Inference Queue Lifecycle](archive/feature_inference-queue-lifecycle.md) | 2026-08-24 | Remediated `CORE-05`, `CORE-09`, and `CORE-11`; removed `ALLOW-08`; unified submission and terminal accounting while preserving capability-specific execution. |

## Active design gate

| Plan | State | Purpose |
|------|-------|---------|
| [TypeScript agent sandbox](feature_TS-Agent-Sandbox.md) | Design gate | Approve a pinned `deno_core` API, threat model, resource controls, and v1 op surface before runtime dependencies or implementation land. |

No sandbox crate exists before design approval. The completed inference
lifecycle is its future cancellation boundary: sandbox-owned AI operations use
an explicit parent `CancellationToken` and the typed queue job outcomes.

## Completed Alpha work

The August completion briefs live in
[`archive/2026-08-alpha/`](archive/2026-08-alpha/README.md):

| Brief | Completed | Implemented outcome |
|-------|-----------|---------------------|
| Alpha correctness | 2026-08-04 | Strict import diagnostics, finite graph/layout invariants, structured search degradation, GPUI lifecycle fixes, and maintenance closure. |
| Lemonade runtime | 2026-08-05 | Pinned private runtime, shared connection/auth, partial discovery, effective runtime profiles, setup management, and coordinated chat transports. |
| Zed-structured workspace | 2026-08-07 | Semantic components, behavioral docks, focus/actions, Details tabs, menus, status UI, and the DM-oriented World Canvas. |
| Inference lifecycle | 2026-08-09 | Typed cancellable jobs, parent cancellation, explicit terminal outcomes, queue telemetry, and evidence-retained routing/retry constants. |
| Agent budgets | 2026-08-09 | Initial model-reconciled budget controls landed; conservative constraints were later partially rolled back after disrupting tool use. Adaptive context optimization is now planned above. |
| Linux client decorations | 2026-08-09 | Negotiated GPUI client chrome, native window interactions, tiling-aware geometry, persisted control placement, and supported-session validation. |
| [v0.1.1 audit remediation](v0.1.1_audit-remediation.md) | Complete | Items 1–11 are implemented and verified on `chore/v0.1.1-audit-remediation`. |

These features were sometimes implemented with narrower or different mechanics
than their original proposals. The archived briefs record final outcomes; the
current contracts live in source, `ARCHITECTURE.md`, and `.rulesdir/`.

## Parked decisions

These topics have no active implementation approval.

| Topic | Current boundary | Reopen trigger |
|-------|------------------|----------------|
| Guided import presentation | Raw schema and data import remain explicit menu workflows | A separately approved DM-oriented import experience. |
| Timeline and map center items | World Canvas currently contains Connections | An approved interaction and persistence design for either item. |
| Multi-user identity | Single-user, local-first storage | A concrete account or workspace-sharing requirement. |
| Generic provider control plane | Capability traits remain; discovery and management are Lemonade-specific | A second real backend is ready to implement. |
| Typed/indexed properties | Node properties remain schema-validated JSON | A specified filtering/query UX with measured index needs. |
| General embedding-space registry | Standard and HQ lanes remain explicit | A third incompatible embedding space is required. |
| Undo/redo journal | Mutations commit directly without a journal | Undo/redo receives product approval. |
| Worker seed/jitter tuning | Existing EWMA fallback and bounded retry schedule remain | Queue telemetry or benchmarks demonstrate a warm-up or retry problem. |

## Historical audit

[`archive/2026-04-audit/`](archive/2026-04-audit/README.md) preserves the
original bug-finding report and phase plans. Its line numbers, open/closed
labels, proposed fixes, and execution order describe the April tree and are not
authoritative for current work.

## Verification baseline

CI runs `make fmt-check`, `make check`, `make clippy`, and `make test-ci`.
Local full verification uses `make test`, which owns one checksum-pinned
embedded Lemonade runtime for the complete serial suite.
