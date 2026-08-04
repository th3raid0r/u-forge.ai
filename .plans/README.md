# Plan Status Ledger

Last reconciled: 2026-08-04, after PR #33 (`topic/refactor-and-clean-pass`)
was merged into `main`.

The phase numbers describe sequencing from the 2026-04-24 audit; they are not
an automatically approved roadmap. A plan is complete only when all of its
named findings and verification requirements are satisfied. Adjacent work is
recorded without treating it as closure.

| Plan | Status | Reconciled result |
|------|--------|-------------------|
| Phase 1 — Import Validation | Partial | Strict import was already present; all loaded schemas now also govern `KnowledgeGraph` object/edge writes and atomic import batches. C2 and H1 remain open. |
| Phase 1 — Layout & Spatial | Open | Graph fitting landed, but C5, C6, M7, and M12 remain open. |
| Phase 1 — Lemonade Stability | Open | Runtime-profile reload coordination landed; C3 and H8 remain open. |
| Phase 1 — Mechanical Sweep | Partial | M3 and L3 are complete; remaining findings are open. |
| Phase 1 — UI Async Hygiene | Partial | SearchPanel now owns/cancels stale searches; the PathPicker half of H4 and C4, H7, M10 remain open. |
| Phase 2 — Agent Budgets | Open | M4 and M5 remain open. |
| Phase 2 — CI & Test Coverage | Partial | M9 and M6 are complete; D2 example coverage remains open. |
| Phase 2 — Search Observability | Partial | H3 has a core response contract and fingerprint-aware fallback, but runtime failures and UI messaging are incomplete. M11 remains open. |
| Phase 2 — Worker Tuning | Open | H5 and M13 remain open. |
| Phase 3 — Job Cancellation | Open | Search task cancellation is local UI lifecycle management, not F7 queue cancellation. F7 and F9 remain open. |
| Phase 3 — Multi-Tenant Identity | Open | F3 remains open. |
| Phase 3 — Provider & Metrics | Partial | Typed committed graph changes partially address L7. F6, F8, L8, D4, and property-level L7 deltas remain open. |
| Phase 3 — Storage Evolution | Partial | Fixed vector lanes now validate dimensions and provider fingerprints; the F5 registry and F1/F4 remain open. |
| Phase 3 — TS Runtime Design | Open | The new Lemonade LLM runtime coordinator is unrelated to the TypeScript sandbox. F2 remains open. |

## Merged refinement work outside the original finding boundaries

- CI delegates the canonical formatting, check, clippy, and test targets to the
  root `Makefile`.
- `GraphMutation` and `GraphChange` establish a schema-authoritative mutation
  and notification boundary. The GPUI shell coalesces committed changes into
  snapshot refreshes and clears stale selection after deletion.
- Lemonade model id, load options, and reasoning mode form one serialized
  runtime profile. Changing reasoning mode forces `/load`, while chat remains
  available when embedding providers are absent.
- Standard and HQ vector lanes persist provider fingerprints. Semantic search
  refuses mismatched or unidentified populated lanes; hybrid search can fall
  back to FTS5.
- Zed-aligned panel metadata/contracts and a graph-specific Fit Graph action
  landed. Persisted chat-history navigation remains available in the chat
  header. These are not substitutes for the remaining GPUI correctness
  findings.
