# Active Plan Ledger

Last reconciled: 2026-08-04 against `main` after PR #33.

Source code is authoritative. Active implementation work is recorded only in
`bug_*.md` and `feature_*.md`; the older phase plans are retained under
`archive/2026-04-audit/` as an audit snapshot, not as executable instructions.

## Approved direction

- Alpha work prioritizes correctness, Zed-like panel structure and behavior,
  and Lemonade lifecycle reliability.
- The graph remains the one intentionally distinct center view.
- The current u-forge palette remains, behind semantic UI tokens. Zed is a
  behavioral reference, not a source-code dependency.
- Chat sessions remain persisted and the latest session resumes, but Zed's new
  history/archive navigation is outside the parity target.
- Lemonade control-plane and direct-chat deviations remain custom HTTP;
  compatible endpoints continue to use OpenAI-compatible libraries.
- A reasoning-mode change requires an effective model reload and serialized
  execution, not only an `enable_thinking` request field.

## Active plans

| Plan | Priority | Purpose |
|------|----------|---------|
| [Alpha correctness](bug_AlphaCorrectness.md) | P0 | Import integrity, graph invariants, search outcomes, GPUI lifecycle fixes, and maintenance debt. |
| [Lemonade runtime](feature_LemonadeRuntime.md) | P0 | Shared connection/auth, partial catalog discovery, effective reasoning profiles, capacity-aware selection, and coordinated chat transports. |
| [Zed UI parity](feature_ZedUiParity.md) | P0/P1 | Semantic components, behavioral docks, focus/actions, tabs, menus, status UI, and parity verification. |
| [Inference lifecycle](feature_InferenceLifecycle.md) | P1 | Real cancellation and evidence-led queue observability/tuning. |
| [Agent budgets](feature_AgentBudgets.md) | P1 | Schema prompt limits, cumulative budgets, and repeated-tool circuit breaking. |

The active plans are independently reviewable, but their implementation order
is: correctness foundations → Lemonade runtime → Zed UI foundation/adaptation →
inference lifecycle → agent budgets. Small maintenance tasks may land alongside
the first compatible change.

## Parked decisions

These are deliberately not active plans. Reopen only when the named product or
usage trigger exists.

| Topic | Current decision | Reopen trigger |
|-------|------------------|----------------|
| TypeScript sandbox | Design gate; see `feature_TS-Agent-Sandbox.md` | Alpha foundations are stable and the sandbox threat model/API are reapproved. |
| Multi-user identity | Single-user, local-first | A concrete account/workspace-sharing product requirement. |
| Generic provider abstraction | Preserve capability traits and Lemonade-specific control plane | A second real backend is ready to implement. |
| Typed/indexed properties | Keep JSON properties | A specified filtering/query UX with measured query needs. |
| General embedding-space registry | Keep standard and HQ lanes | A third incompatible embedding space is required. |
| Undo/redo journal | Not scheduled | Undo/redo is approved as an Alpha or post-Alpha feature. |
| Worker seed/jitter tuning | Measure first | Queue telemetry or benchmarks demonstrate a warm-up/retry problem. |

## Archived audit reconciliation

The archived plans contain useful history but also stale or disproven claims:

- Import ambiguity remains real in both the in-session name map and persisted
  fallback. Strict schema import and atomic validated writes already exist.
- The layout plan's original coincident-node and NaN-selection explanations do
  not match current control flow. The retained work is saved/unsaved placement,
  viewport invariants, measured convergence, and robust spatial rebuilds.
- Lemonade's timeout and all-or-nothing catalog findings remain, but a blanket
  five-second completion timeout is not acceptable for local models.
- Search task ownership, list-state helpers, CI, dimension mismatch coverage,
  and HQ backfill are already implemented.
- Search runtime failures still do not reach the UI even though a structured
  response shell exists.
- The vendored `cosmic-text` test profile is intentional and valid.
- Provider generalization, multi-tenancy, and storage redesign were speculative
  future work, not Alpha defects.

## Verification baseline

At reconciliation time `make fmt-check`, `make check`, and `make test` pass
without a Lemonade server. Project crates have no clippy warnings; inherited
warnings remain in the separately tested vendored `cosmic-text` crate.
