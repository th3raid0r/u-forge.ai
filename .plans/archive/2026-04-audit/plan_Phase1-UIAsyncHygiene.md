# Phase 1 — UI Async & Paint Hygiene

**Status (2026-08-04): Partially implemented.** The SearchPanel half of H4 is complete: it owns its `gpui::Task`, cancels the prior task, and rejects stale generations. `PathPickerModal::browse` still detaches its task. C4, H7, and M10 also remain open; the embedding plan still uses both an epoch and an atomic cancel flag, and chat list mutation still lacks the proposed named helpers. Paths and symbols are authoritative; line references below describe the 2026-04-24 snapshot.

**Adjacent work:** PR #33 also coalesces committed core graph changes into one
snapshot refresh per frame-sized window and adds panel contracts. Those changes
improve UI consistency but do not close the remaining findings in this plan.

**Source findings:** C4, H4, H7, M10

**Why this is its own branch:** All four findings are GPUI-side and
cluster around the same theme: hold async/state correctly so paint stays
fast, results don't clobber each other, and the chat panel's epoch logic
stops racing. They all live under `crates/u-forge-ui-gpui/`.

**Branch name suggestion:** `fix/phase1-ui-async-hygiene`

---

## Scope

| ID | What | Where |
|----|------|-------|
| C4 | `paint_entity.update(...)` called inside canvas paint closure | `crates/u-forge-ui-gpui/src/text_field.rs:808-810`, `:833-840` |
| H4 | Detached search task leaks results across rapid re-queries | `crates/u-forge-ui-gpui/src/search_panel.rs:236`, `path_picker.rs:97` |
| H7 | Chat embedding-plan epoch + cancel-flag two-poller window | `crates/u-forge-ui-gpui/src/app_view/mod.rs:655-701` |
| M10 | List-state helpers for `chat_panel` reset/append | `crates/u-forge-ui-gpui/src/chat_panel.rs:242-258, 271, 286, 343, 785, 815, 838` |

---

## Suggested approach

### C4 — move measurement out of paint

- Identify the values written during paint: `shaped_layout`, `field_origin_x/y`,
  `measured_line_h`, `content_height`, `visible_height`, `visible_width`.
- Move these writes into a prepaint hook (`on_before_paint` or equivalent
  in GPUI). The values are needed by event handlers (click hit-testing,
  scroll), so they continue to live on the entity but are written *before*
  paint, not during.
- Standard pattern (per the audit): event handlers read the *previous*
  frame's measurements. Confirm this matches Zed's editor pattern; review
  `.rulesdir/gpui-patterns.mdc` and `.rules` Anti-Pattern #4 before
  designing.
- Single-frame layout lag is acceptable; document it in a code comment if
  it surprises a future reader.

### H4 — own the task handle

- In `SearchPanel`, store an `Option<gpui::Task<()>>` field.
- In `do_search`, replace the existing `cx.spawn(...).detach()` with
  assignment to the field. Dropping the previous task on assignment
  cancels it.
- Apply identical refactor to `PathPickerModal::browse` (`path_picker.rs:97`).
- Verify the underlying queue work is actually cancellable on task drop;
  if not, add a small "stale check" inside the spawned task that
  short-circuits when a sequence number changes (sequence number stored
  on the panel, incremented per `do_search`).

### H7 — single cancellation source

- Audit the dual-mechanism: epoch counter + `Arc<AtomicBool>` cancel flag.
- Recommended path (per audit): take epoch as the single source of truth;
  drop the cancel flag.
- Alternative path: gate runs with an `embedding_in_flight: AtomicBool`
  and reject overlapping plans with a status message.
- Pick one and document why. Whichever is chosen, ensure no two pollers
  can write `status` on the same entity in the same frame.

### M10 — named helpers

- Add `replace_messages(...)` and `append_message(...)` on `ChatPanel`.
- `replace_messages` calls the existing `ListState::reset()` path.
- `append_message` calls `splice_appended()`.
- Convert the listed call sites (`:242-258, 271, 286, 343, 785, 815, 838`)
  to use the new helpers.
- Make the raw mutation private if possible; if GPUI requires a public
  surface, document the rule in a doc comment on the helpers.

---

## Testing instructions

Canonical command:

```
cargo test --workspace -- --test-threads=1
```

Targeted tests are tricky in GPUI — there's no headless test harness for
the UI crate by default. Where possible add unit tests for pure logic
(e.g. M10 helpers' precondition checks, H4's stale-sequence shortcut).

Manual verification — required for this plan, since UI behaviour is the
target:

- **C4:** open a chat with several long messages and the node editor
  open. Type rapidly in the chat input; watch frame timing if a profiler
  is available, or watch for visible jank. Expectation: no per-frame
  layout amplification.
- **H4:** open the search panel; type a long query character-by-character
  with sub-100ms gaps. Confirm the final query's results are displayed,
  not a stale partial result.
- **H4 (path picker):** browse rapidly through directories; confirm the
  visible listing matches the *current* directory, not a stale earlier
  one.
- **H7:** trigger an embedding plan, then immediately trigger another
  before the first finishes. Watch the status string — it should not
  alternate between the two plans' progress.
- **M10:** purely a refactor; verify chat behaviour is unchanged
  (streaming responses still append, non-streaming still resets cleanly).

If you cannot run the GPUI app in your environment, **say so explicitly**
in the PR description rather than claiming the manual checks were done
(per `CLAUDE.md` rules).

---

## Documentation fold-in

- **`.rulesdir/gpui-patterns.mdc`** — if it does not already, add a rule
  about owning task handles (closes H4's pattern) and about the prepaint
  measurement pattern (closes C4's pattern). Cross-reference `.rules`
  Anti-Pattern #4.
- **`.rules`** — Anti-Pattern #4 currently says "no paint-time mutation".
  Verify this still reads correctly after C4 lands; if a code example
  exists, update it.
- **`ARCHITECTURE.md`** — task-handle rule is documented; verify it
  still matches reality.
- **`bugfinding.md`** — leave alone.

---

## User input prompts

Pause and ask before:

1. **H7 design choice.** Single epoch vs. `embedding_in_flight` gate.
   The first is cleaner; the second is more explicit. Ask which the
   user prefers — they map to different UX (silent override vs. visible
   "already running" message).
2. **C4 layout-lag acceptability.** A one-frame lag in measurements is
   the standard pattern but may produce a single-frame visual artifact
   on extreme resizes. Confirm with the user that this is acceptable.
3. **M10 visibility.** Ask whether the raw mutation should become
   `pub(crate)`, `private`, or remain `pub` (GPUI sometimes forces
   public; verify before deciding).

---

## Commit & push

When tests + manual verification pass:

1. Either one combined commit or four small ones — your call. If small:
   - `refactor(ui): own search/path-picker task handles (H4)`
   - `fix(ui): collapse epoch + cancel flag for embedding plan (H7)`
   - `fix(ui): move text-field measurement out of paint (C4)`
   - `refactor(ui): named replace/append helpers on chat list (M10)`
2. Push and open a PR.

---

## Out of scope

- New UI components.
- Visual redesigns.
- Window focus/blur handling (F9, Phase 3).
- Any GPUI version bump.
