# Active Plan Ledger

Last reconciled: 2026-08-07 against `main` after PR #40.

Source code is authoritative. Active implementation checklists are recorded in
`bug_*.md` and `feature_*.md`. A decision-complete `plan_*.md` may accompany a
large checklist when later sessions need more implementation detail than the
tracker should duplicate. Older phase plans remain under
`archive/2026-04-audit/` as an audit snapshot, not executable instructions.

## Approved direction

- Alpha work prioritizes correctness, Zed-like panel structure and behavior,
  and Lemonade lifecycle reliability.
- The graph remains the one intentionally distinct center view.
- The current u-forge palette remains, behind semantic UI tokens. Zed is a
  behavioral reference, not a source-code dependency.
- Chat sessions remain persisted, the latest session resumes, and the existing
  header history navigation remains; Zed's newer history/archive interface is
  outside the parity target.
- Lemonade control-plane and direct-chat deviations remain custom HTTP;
  compatible endpoints continue to use OpenAI-compatible libraries.
- Lemonade v11.5.1 request-scoped thinking control is the default. The
  llama.cpp reload workaround remains available through a global TOML strategy
  until supported models have been validated; either path stays serialized
  through complete inference.
- Packaged Ubuntu x64 builds own a private pinned embeddable `lemond` by
  default. `LEMONADE_URL` explicitly selects an external instance.
- The repository remains source-first with no published binary release. The
  first binary distribution is gated on inference lifecycle, agent budgets,
  and GNOME-compatible client-side window decorations being complete and
  verified.

## Active plans

| Plan | Priority | Purpose |
|------|----------|---------|
| [Inference lifecycle](feature_InferenceLifecycle.md) | P1 | Real cancellation and evidence-led queue observability/tuning. |
| [Agent budgets](feature_AgentBudgets.md) | P1 | Schema prompt limits, cumulative budgets, and repeated-tool circuit breaking. |
| [GNOME client-side decorations](feature_GnomeClientSideDecorations.md) | P1 | Complete Linux window chrome when the compositor delegates decorations to the client. |

Inference lifecycle must precede agent budgets where their cancellation and
completion outcomes meet. Client-side decorations are independent and may land
alongside either feature. All three are first-binary-release gates.

## Completed Alpha foundations

| Plan | Completed | Outcome |
|------|-----------|---------|
| [Alpha correctness](bug_AlphaCorrectness.md) | 2026-08-04 | Strict import integrity, finite graph invariants, structured search outcomes, GPUI lifecycle corrections, and maintenance closure. |
| [Lemonade runtime](feature_LemonadeRuntime.md) ([detailed plan](plan_LemonadeRuntime.md)) | 2026-08-05 | Private pinned runtime, managed setup, shared connection/auth, live effective profiles, and coordinated chat transports. |
| [Zed UI parity](feature_ZedUiParity.md) | 2026-08-07 | Semantic components, behavioral docks, focus/actions, Details tabs, menus, status UI, and the DM-oriented World Canvas workspace. Guided import remains separately deferred. |

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

The archived plans contain useful history but predate the completed Alpha
foundations:

- Ambiguous import endpoints, required-property consistency, and agent
  ambiguity diagnostics were resolved by the Alpha correctness work.
- Saved/unsaved placement, finite viewport invariants, measured convergence,
  and robust spatial rebuilding are implemented; the archived layout plan's
  earlier coincident-node and NaN-selection explanations remain disproven.
- Partial Lemonade discovery, bounded timeout classes, live profile authority,
  and full-stream execution leases replaced the archived all-or-nothing and
  blanket-timeout assumptions.
- Search task ownership, stage-outcome propagation through the UI, list-state
  helpers, CI, dimension mismatch coverage, and HQ backfill are implemented.
- The vendored `cosmic-text` test profile is intentional and valid.
- Provider generalization, multi-tenancy, and storage redesign were speculative
  future work, not Alpha defects.

## Verification baseline

The completed feature briefs record their canonical verification. CI runs
`make fmt-check`, `make check`, `make clippy`, and `make test-ci`; local full
verification uses `make test` with one owned pinned Lemonade runtime.
