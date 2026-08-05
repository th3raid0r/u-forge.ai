# Phase 3 — Job Cancellation & Window Lifecycle

**Status (2026-08-04): Open.** SearchPanel now owns and replaces its GPUI search task, but this is local stale-result prevention rather than F7: queued embedding, transcription, TTS, generation, and reranking jobs still have no public cancellation token or handle. F9 window-blur dismissal also remains open. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Source findings:** F7, F9

**Why Phase 3:** Job cancellation requires threading a cancellation token
through every queue worker and every provider call. Window focus/blur is
a small UI feature on its own but pairs naturally because both are about
"the user's current intent should be observable to running work".

**Branch name suggestion (design):**
`design/phase3-job-cancellation`

---

## Scope

| ID | What | Where |
|----|------|-------|
| F7 | Submitted jobs cannot be cancelled | `crates/u-forge-core/src/queue/` (workers, dispatch); chat already has stream cancellation but embed/transcribe/rerank do not |
| F9 | Window focus/blur not handled — modals/dropdowns persist on Alt+Tab | `crates/u-forge-ui-gpui/src/app_view/` (`render` is the natural hook) |

---

## Suggested approach

### F7 — cancellation tokens through the queue

1. Add a `CancellationToken` type (use `tokio_util::sync::CancellationToken`
   if it's already a workspace dep; otherwise roll a small one).
2. Each `Job` carries a token cloned from a parent.
3. Workers check the token:
   - Before each retry attempt.
   - Between provider call setup and dispatch.
   - During streaming reads (poll the token alongside the stream).
4. Public API: `JobHandle::cancel()` cancels a specific job;
   `EmbeddingPlan::cancel()` cancels all jobs in the plan.
5. UI surface: "Embed All" gets a stop button; transcription and rerank
   get matching controls.

Implementation note: existing chat `stream_task` cancellation should be
re-expressed in terms of this token to unify the model.

### F9 — focus / blur dismissal

1. Hook `on_focus_change` (or the GPUI equivalent) on `AppView::render`.
2. On blur: emit a `DismissOverlays` event/observer.
3. Children (modals, dropdowns, context menus) subscribe and dismiss
   themselves.
4. Optional: distinguish "blur because user opened a child window" from
   "blur because Alt+Tab" if GPUI exposes the difference.

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted tests:

- **F7 unit:** submit a long-running embed job; cancel it; assert the
  worker observes the cancellation and exits cleanly without writing
  partial state.
- **F7 plan-level:** submit `EmbeddingPlan::embed_all`; cancel
  mid-execution; assert no further jobs start and in-flight jobs stop
  promptly.
- **F7 retry interaction:** force a job into retry; cancel during the
  backoff sleep; assert the worker exits the sleep and the job is not
  retried.

Manual verification:

- Trigger "Embed All" on a large graph; press the new stop control;
  confirm no further jobs land in the DB.
- Open a context menu, Alt+Tab away, return; confirm the menu has been
  dismissed.

---

## Documentation fold-in

- **`ARCHITECTURE.md`** — queue section: document the cancellation token
  and where workers check it. Note the unification of chat-stream
  cancellation under the same token.
- **`ARCHITECTURE.md`** — UI section: document the `DismissOverlays`
  contract on app blur.
- **`.rulesdir/`** — add to GPUI rules: "child overlays must subscribe
  to `DismissOverlays` and tear themselves down on blur."
- **`bugfinding.md`** — leave alone.

---

## User input prompts

Pause and ask before:

1. **Cancellation cascade.** When the user cancels a plan, do
   *currently-streaming* tokens flush to the user, or are they
   discarded? UX call.
2. **Window-blur threshold.** Some users dislike modals dismissing when
   they Alt+Tab to copy a path from another window. Ask whether blur
   should dismiss immediately, after a delay, or never (only on focused
   click outside).
3. **Existing chat cancellation.** Ask whether to refactor the chat
   `stream_task` cancellation to use the new token, or leave it
   parallel for now (lower-risk).

---

## Commit & push

This is multi-PR work. Likely split:

1. `feat(queue): cancellation tokens on jobs and plans (F7)` — core
   plumbing only.
2. `feat(ui): stop control for embedding plans (F7)` — UI surface.
3. `feat(ui): dismiss overlays on window blur (F9)` — small standalone.

Push and PR each separately.

---

## Out of scope

- Pausing jobs (cancel only, no pause/resume).
- Persistent cancellation (jobs cannot be cancelled across app restart;
  unfinished jobs simply don't resume).
- Storage evolution (F1, F4, F5 — separate plan).
