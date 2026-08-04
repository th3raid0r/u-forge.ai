# Phase 2 — Worker EWMA & Retry Tuning

**Status (2026-08-03): Open.** Retained from the audit for follow-up. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Source findings:** H5, M13

**Why Phase 2:** Both are observability-adjacent tuning fixes. They are
small in scope but benefit from CI (M9) and metrics (F8 in Phase 3) to
verify the impact. Doable independently as soon as Phase 1's lemonade
stability work lands.

**Branch name suggestion:** `fix/phase2-worker-tuning`

---

## Scope

| ID | What | Where |
|----|------|-------|
| H5 | EWMA seed of 0 destabilises embed routing during warmup | `crates/u-forge-core/src/queue/workers.rs:175-181`, `crates/u-forge-core/src/queue/weighted.rs:176-185` |
| M13 | Embed retry uses lockstep delays — thundering herd on recovery | `crates/u-forge-core/src/queue/workers.rs:143-155` |

---

## Suggested approach

### H5 — non-zero EWMA seed per device class

- Introduce per-device-class default seeds. Audit suggests:
  - NPU: 30,000 µs
  - GPU: 50,000 µs
  - CPU: 200,000 µs
- Where the device class is known at worker construction, seed from a
  static lookup. If unknown, fall back to a conservative middle value
  (e.g. 100,000 µs).
- Verify the dispatcher's cost function `(pending+1) * ewma` becomes
  monotone from job 1, eliminating cold-start oscillation.

### M13 — retry jitter

- Replace `delay_ms` constants with `delay_ms + (rand_u64 % (delay_ms / 2))`.
- Use a per-worker RNG seeded at construction to keep jitter
  deterministic within a worker but uncorrelated across workers.
- Apply consistently across the retry sequence (100ms, 200ms, …).

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted tests:

- **H5 unit:** construct a queue with multiple workers; submit several
  jobs in quick succession before any have completed; assert routing
  decisions match the seeded-EWMA expectation rather than the
  zero-seeded oscillation pattern. (May require exposing the cost
  function for testing.)
- **M13 unit:** construct two workers, force them both into retry; record
  the actual delay used; assert the deltas are not identical. Can use a
  deterministic seed for reproducibility.

Manual verification: hard to reproduce without a multi-worker setup.
Document the gap if you can't run it.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — queue/worker section: document the per-device
  EWMA seed table and rationale. Mention retry jitter and the
  per-worker RNG.
- **`bugfinding.md`** — leave alone.

---

## User input prompts

Pause and ask before:

1. **Seed values.** Audit's NPU/GPU/CPU values are reasonable but
   workload-dependent. Confirm or substitute.
2. **RNG choice.** `fastrand`, `rand`, or `SmallRng` — pick whatever the
   project already uses. If none is in workspace dependencies, ask the
   user before adding.

---

## Commit & push

1. Single commit:
   `fix(queue): seed EWMA per device class; jitter retry delays (H5, M13)`.
2. Push and open a PR.

---

## Out of scope

- Replacing the EWMA cost function with a different scheduler.
- Job cancellation (F7, Phase 3).
- Metrics export (F8, Phase 3).
