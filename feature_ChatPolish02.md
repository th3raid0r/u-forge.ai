# feature_ChatPolish02 — Chat UI quality-of-life polish

Three features for the GPUI chat panel. All changes live in
`crates/u-forge-ui-gpui/src/` (primarily `chat_panel.rs` and `chat_message.rs`).
None touch the knowledge graph or inference pipeline.

Text selection / copy-from-messages is tracked separately in
**`feature_ChatTextSelection.md`** and is sequenced **after** this feature
completes.

Canonical test command: `cargo test --workspace -- --test-threads=1`.

---

## Feature A — Retry button on assistant and user messages ✓ DONE

### Intent
Every user and assistant message shows a "⟳" retry button. Clicking it
re-runs the nearest user turn at or before that message, replacing it and
everything after it (thinking blocks, tool calls, later turns).

User-message retry is especially useful after deleting an assistant response:
the remaining user message gets a retry button so generation can be
re-triggered without retyping.

### Status
**Implemented and refactored.** All tests pass (`cargo test --workspace -- --test-threads=1`).

Key changes from initial implementation through final refactor:
- `send_with_text` extracted from `do_send`; `retry_message` finds the
  correct user turn, truncates `messages` to that index, resets `list_state`,
  calls `send_with_text`.
- `retry_message` extended: if the clicked message IS a `User` role, it
  uses it directly instead of walking backwards. This enables retry on
  dangling user messages (e.g. after deleting an assistant response).
- **Retry button moved to render-site** (see "Render-site action bar" in
  Feature C). `RetryRequested` event, `EventEmitter` impl, and
  `msg_subscriptions` vector were all removed — the list item builder
  renders the button directly and calls `retry_message` via entity update.

### Original current state (pre-implementation)
`ChatPanel::do_send` pushed a `User` message then streamed the response.
No retry existed. Messages stored as `Vec<Entity<ChatMessageView>>`.

### Plan (historical — superseded by action bar refactor)

The initial design used GPUI event emission:
1. `RetryRequested(EntityId)` event emitted from `ChatMessageView`.
2. `ChatPanel` subscribed per-message via `msg_subscriptions: Vec<gpui::Subscription>`.
3. Retry button rendered inside `ChatMessageView::render_text` for assistant only.

This was replaced by the render-site action bar (see Feature C) which
eliminated the subscription machinery entirely and extended retry to user
messages at the same time.

**Edge cases**
- Retry on the *last* assistant message: behaves as a regenerate.
- Retry a dangling user message (assistant deleted): uses that message directly.
- Retry when the provider isn't configured: `send_with_text` handles this.
- Retry during streaming: suppressed via the `self.streaming` guard.
- Retry across a Thinking block: Thinking blocks are filtered from
  `raw_history` in `send_with_text`, so truncating past them is safe.
- Retry across tool calls: same reasoning — tool call messages aren't in
  `raw_history`.

### Files touched
- `crates/u-forge-ui-gpui/src/chat_message.rs` — retry button removed
  from `render_text`; `RetryRequested` event and `EventEmitter` impl
  removed; `msg_subscriptions` machinery eliminated.
- `crates/u-forge-ui-gpui/src/chat_panel.rs` — `retry_message` extended
  for user-role messages; `send_with_text` extraction; subscription vector
  removed; retry button moved to render-site action bar (see Feature C).

---

## Feature B — Three-state send button: Connect / Send / Stop ✓ DONE

### Intent
The bottom-right action button reflects the app's chat state:
- **"Connect"** — rendered when `self.chat_provider.is_none()` (Lemonade
  init hasn't succeeded yet, or the server was unreachable at startup).
- **"Send"** — rendered when provider exists and `!self.streaming`.
- **"Stop"** — rendered when `self.streaming` is true; clicking cancels
  the in-flight stream.

### Status
DONE. Implemented on feature_ChatPolish02 branch.

- `stream_task: Option<gpui::Task<()>>` field stores the active stream handle;
  `stop_stream()` drops it, appends `\n[Cancelled]`, then calls `finalize_stream`.
- `ConnectRequested` event emitted by `do_send` (and the Connect button) when no
  provider/agent is present; AppView subscribes, sets `connecting = true`, and
  re-invokes `do_init_lemonade`. On failure `set_connect_failed` is called and
  the error renders in red below the input row.
- Button is now four-state (Connect / Connecting… / Send / Stop) with width
  pinned to 88 px to prevent input-row reflow.
- `do_send` is guarded: streaming → no-op; no provider → emit ConnectRequested
  (or no-op if already connecting).

### Original current state (pre-implementation)
- The button (`chat_panel.rs:1133-1164`) shows "Send" always; when streaming
  it just greys out and becomes a no-op.
- `do_init_lemonade` runs exactly once at startup (`app_view/mod.rs:284`).
  There's no way for the user to retry connection.
- Cancellation doesn't exist. `cx.spawn(async move |this, cx| { … }).detach()`
  drops the task handle, so the stream can't be stopped.

### Plan

**B1 — Cancellation plumbing**

1. Replace the current detached spawn with a stored handle. On `ChatPanel`:
   ```rust
   stream_task: Option<gpui::Task<()>>,
   ```
2. At the start of both the agent path and direct path in `send_with_text`,
   assign `self.stream_task = Some(cx.spawn(async move |this, cx| { … }));`
   (no `.detach()`). Storing the `Task<()>` keeps it alive and lets us drop
   it to cancel.
3. On `finalize_stream`, set `self.stream_task = None`.
4. Implement `fn stop_stream(&mut self, cx: &mut Context<Self>)`:
   - Drop `self.stream_task.take()` — this drops the outer GPUI spawn,
     which drops the `mpsc::Receiver` captured inside, which causes the
     next `tx.send(...).await` inside `prompt_stream`
     (`agent/src/lib.rs:1086` and siblings) to return `Err`, breaking the
     `'stream` loop — this is already documented in the agent code.
   - Append a `"\n[Cancelled]"` marker to the current streaming assistant
     message (if any) so the user sees it stopped.
   - Call `self.finalize_stream(cx)` to persist the partial response and
     reset state.

**B2 — Connect action**

`ChatPanel` shouldn't call `do_init_lemonade` directly — that's AppView's
job and pulls in `LemonadeServerCatalog`, `ProviderFactory`, `GraphAgent`,
etc.

1. Add an event `pub(crate) struct ConnectRequested;` to `chat_panel.rs`
   and `impl EventEmitter<ConnectRequested> for ChatPanel`.
2. In `app_view/mod.rs`, alongside the existing chat-panel wiring,
   subscribe to `ConnectRequested` and call `self.do_init_lemonade(cx)`.
3. Add a `connecting: bool` flag on `ChatPanel` so the button can show
   "Connecting…" while init is in flight. Set to `true` when emitting the
   event; AppView flips it back to `false` on success/failure via a new
   public method `ChatPanel::set_connecting(&mut self, b: bool)`.
4. Extend `set_provider` to imply `connecting = false`. Add
   `set_connect_failed(&mut self, msg: &str)` that flips `connecting =
   false` and stashes a brief user-visible error string to render under
   the button.

**B3 — Button rendering**

Replace the existing send-button element with a match on state derived
from `(self.chat_provider.is_some(), self.streaming, self.connecting)`:

| State       | Label         | Bg color                        | Handler                 |
|-------------|---------------|---------------------------------|-------------------------|
| Connect     | "Connect"     | `rgb(0xf9e2af)` (yellow accent) | emit `ConnectRequested` |
| Connecting  | "Connecting…" | `rgb(0x45475a)` (greyed)        | no-op                    |
| Send        | "Send"        | `rgb(0x89b4fa)` (blue)          | `do_send`                |
| Stop        | "Stop"        | `rgb(0xf38ba8)` (red accent)    | `stop_stream`            |

Button width: pick the widest label ("Connecting…") and pin the button
width so the input row doesn't reflow per state change. Estimate: 72 px.

**B4 — Enter key**

The Enter-to-submit path calls `do_send` directly from the `TextSubmit`
subscription. Guard `do_send`'s entry: if `!has_provider`, emit
`ConnectRequested` instead; if `streaming`, no-op. This way Enter also
drives the state machine.

### Files touched
- `crates/u-forge-ui-gpui/src/chat_panel.rs` — button rendering,
  `stream_task` field, `stop_stream`, `ConnectRequested` event,
  `connecting` flag, `set_connecting`, `set_connect_failed`.
- `crates/u-forge-ui-gpui/src/app_view/mod.rs` — subscribe to
  `ConnectRequested`, wire `connecting` state around `do_init_lemonade`
  completion, verify second invocation of `do_init_lemonade` is safe
  (inspect for resource leaks in `InferenceQueue` / `GraphAgent` rebuild
  paths).

### Risks
- `do_init_lemonade` rebuilding providers a second time might duplicate
  `InferenceQueue` workers or leak the old `GraphAgent`. Verify that
  reassigning `view.state.inference_queue = Some(queue)` drops the old
  queue cleanly. If not, add an explicit teardown path before re-init.
- Dropping the GPUI Task cancels the *outer* async stream consumer, but
  the tokio task inside `prompt_stream` keeps running until its next
  `tx.send().await`. For a typical streamed LLM response this is ≤1
  token's worth of latency (usually <100 ms). Acceptable.

---

## Feature C — Inline delete button on the last message ("Back button") ✓ DONE

### Intent
A small 🗑 icon appears on **whichever message is currently the tail** of
the session — user, assistant, thinking, or tool call. One click deletes
that single message. After deletion, the icon re-renders on the new tail,
so the user can chain-click to walk backwards through history.

This is an undo / "back button" for conversation tail, used to trim
before a retry (or instead of one).

### Status — DONE (refactored)
- `delete_message_at` added to `ChatPanel`.
- **Render-site action bar** replaces both the original `del-row` and the
  per-entity retry button from Feature A. The list item builder conditionally
  appends a shared action bar below each message containing `[⟳] [x]`
  buttons. Retry shows for User/Assistant when not streaming; delete (`x`)
  shows only on the last message when not streaming.
- Action bar background matches the bubble above it per role.
- `msg_subscriptions.remove(ix)` removed — subscription vector eliminated.
- `list_state.reset(len)` + `save_current_session` called on every delete.
- `cargo check -p u-forge-ui-gpui` and full workspace tests pass clean.

### Current state (pre-implementation)
`delete_session` existed (`chat_panel.rs:714`) but deleted the entire
session. No per-message delete. The last-message slot had no affordance.

### Design notes (from user direction)

- **Inline**, not a toolbar button.
- Shown **only on the last message**, not all of them.
- **Any role** — user / assistant / thinking / tool call — is deletable.
- **Re-evaluates after deletion.** The new tail picks up the button.
- Chainable: click repeatedly to delete all messages.

### Plan (as implemented)

1. `fn delete_message_at(&mut self, ix: usize, cx: &mut Context<Self>)` on `ChatPanel`:
   - Abort if `self.streaming`.
   - Bounds-check `ix`.
   - `self.messages.remove(ix)`.
   - `self.list_state.reset(self.messages.len())`.
   - `self.save_current_session(cx)` + `cx.notify()`.

   **No group-pop behavior.** Each click deletes exactly one message.
   A Thinking block followed by an Assistant takes two clicks; intentional.

2. **Render-site action bar** — both retry and delete live in the list item
   builder in `chat_panel.rs`, not inside `ChatMessageView`. On each render
   the builder reads `role`, `is_last`, and `is_streaming` from the panel,
   then conditionally appends an action bar:
   - `can_retry`: `User` or `Assistant` role, not streaming → shows `⟳`.
   - `show_delete`: `is_last && !is_streaming` → shows `x`.
   - Action bar only rendered when at least one button is visible.
   - Background color matches the bubble above it:
     User → `rgb(0x313244)`, Thinking → `rgb(0x181825)`,
     Assistant/ToolCall → `rgb(0x1e1e2e)`.
   - Retry calls `retry_message(msg_entity_id, cx)` directly via entity
     update — no event emission needed.
   - Delete calls `delete_message_at(last, cx)` via entity update.

   This eliminates `RetryRequested`, `EventEmitter<RetryRequested>`, and
   the entire `msg_subscriptions: Vec<gpui::Subscription>` vector that
   Feature A originally introduced.

**Edge cases**
- Deleting down to zero messages: session becomes empty;
  `save_current_session` still writes an empty message list; session
  title was derived from the first user message and stays until the
  session itself is deleted or a new user message is added. Acceptable.
- Deleting during streaming: button is not rendered when `streaming`.
- Undo: out of scope. Deletion is persisted immediately.

### Files touched
- `crates/u-forge-ui-gpui/src/chat_panel.rs` — `delete_message_at`,
  render-site action bar in the list item builder (retry + delete),
  `msg_subscriptions` vector removed entirely.
- `crates/u-forge-ui-gpui/src/chat_message.rs` — retry button and
  `RetryRequested` event removed; `render_text` no longer takes `cx`.

---

## Bug fixes (ChatPolish02-D)

Small UI bugs addressed in the same branch after features A–C landed.
All changes are in `chat_panel.rs` unless noted.

### D1 — History dropdown: text overflow + fade + delete button visibility ✓ DONE

**Problem.** Session titles in the history dropdown ran to the edge of the
panel, carrying the `✕` delete button off-screen or making it hard to see.

**Solution.**
- Row: `relative()`, `w_full()`, `overflow_x_hidden()` to constrain width.
- Title div: `flex_grow/shrink`, `min_w_0()`, `overflow_x_hidden()` to clip text.
- Gradient fade: row-level `absolute()` child, `right(px(26.0))` (= 8 px
  padding + 18 px button), `w(px(28.0))`. Both colour stops share the same
  hue — only alpha differs — so the gradient is invisible over empty space
  and only appears where text overflows beneath it.
- Delete button: `flex_none` sibling, always visible at the right edge.
- Row backgrounds converted to pre-composited opaque values so gradient
  end colours match exactly:
  - default:  `rgb(0x313244)` (dropdown bg)
  - selected: `rgb(0x3c3d50)` (= `rgba(0x45475a88)` over `#313244`)
  - hovered:  `rgb(0x393a4d)` (= `rgba(0x45475a66)` over `#313244`)
- `hovered_history_ix: Option<usize>` field added to `ChatPanel`; wired
  via `on_hover` on each row so the gradient end colour updates on hover.
- `linear_gradient`, `linear_color_stop` added to gpui imports.

### D2 — History dropdown hover gradient: directional bug ⚠ DEFERRED

**Symptom.** When the cursor *descends* into a history row from above, the
gradient colour fails to update to the hovered value. Entering from the
side or bottom works correctly.

**Root cause (hypothesis).** `on_hover` callbacks in GPUI appear to fire
only when the row's own hitbox is the top-most entry point. A `relative()`-
positioned child div (title, previously) creates a separate stacking
context that paints on top of the row, so top-entry hits the child's
hitbox first and the row's `on_hover` never transitions. Moving `relative()`
from the title to the row (so the title is `position: static`) did not
fully resolve the issue — the GPUI hitbox/stacking system may have a deeper
interaction with how list items register hover when traversed vertically.

**Workaround.** None applied. The gradient colour is slightly wrong on
top-entry hover but corrects itself as soon as the cursor moves within the
row. Visually minor — the row background itself switches via `.hover()` CSS
styling (which is unaffected), only the gradient lags.

**To investigate later.**
- Whether GPUI's virtualized `list()` element interferes with child hover
  detection across item boundaries.
- Whether `on_hover` has a known limitation with top-to-bottom cursor
  traversal in stacked-item layouts.
- Potential alternative: track hover via `on_mouse_move` on the dropdown
  container and derive the hovered index from `event.position.y` and item
  height (24 px), bypassing per-item `on_hover` entirely.

---

## Cross-cutting concerns

### Stream cancellation ordering (Features A, B, C)
`retry_message`, `stop_stream`, `delete_message_at`, `new_session`,
`load_session`, `delete_session` — all of these mutate `self.messages`.
The existing three already guard on `self.streaming`. Apply the same
guard to the new ones. Stop is the *only* new operation that's
*supposed* to fire during streaming.

### Persistence
Every mutation that changes `self.messages` should call
`save_current_session(cx)` at the end so chat history stays consistent.
Retry calls it implicitly via `finalize_stream` after the new stream
completes. Delete calls it explicitly. Stop calls it via
`finalize_stream`.

### Virtualized list (`ListState`) invalidation
Summary from `splice_appended` doc (`chat_panel.rs:219`):
- Append-only: use `splice(len..len, 1)` — preserves prior measurements.
- Wholesale replace / truncate / remove-mid / reorder: use `reset(len)`
  — invalidates all.

Feature A (retry truncates) and Feature C (delete-at-ix removes) both
need `reset(self.messages.len())`. Feature B doesn't touch the list
structure.

### Subscription storage ✗ ELIMINATED
The `msg_subscriptions: Vec<gpui::Subscription>` vector planned in Feature A
was removed when retry moved to render-site (Feature C refactor). The list
item builder reads panel state directly and calls `retry_message` via entity
update — no per-message subscriptions needed. Future per-message actions
should follow the same render-site pattern rather than reintroducing a
subscription vector.

---

## Implementation order

1. **Feature B1 (cancellation plumbing) first** — establishes the
   `stream_task` field. Foundational for the other two.
2. **Feature C (inline delete)** — smallest surface area; validates the
   list-wrapper approach. Doesn't depend on A.
3. **Feature B2–B4 (Connect / Stop button states + wiring)** — depends
   on B1.
4. **Feature A (retry)** ✓ DONE — `send_with_text` extracted; `retry_message`
   implemented and extended to handle user-role messages. Subscription vector
   approach superseded by render-site action bar (see Feature C).

## Testing

- `cargo check -p u-forge-ui-gpui` — type check.
- `cargo test --workspace -- --test-threads=1` — canonical regression.
- Manual: launch `cargo run -p u-forge-ui-gpui`:
  - With Lemonade off: button reads "Connect"; clicking triggers init;
    shows "Connecting…" then flips to "Send" on success or stays
    "Connect" with an error on failure.
  - Send a message; mid-stream click "Stop" — response halts,
    "[Cancelled]" marker appears, button returns to "Send".
  - Retry an assistant message mid-session: subsequent turns disappear,
    new response streams in.
  - Delete the last assistant message: remaining user message shows `⟳`;
    clicking it re-submits without retyping.
  - Inline delete on the tail: message vanishes, `x` re-appears on the
    new tail; chain-click through multiple messages of varying roles;
    confirm session empties cleanly.
  - Action bar background matches the bubble above it for each role.
- Persistence: all above operations should survive an app restart via
  the `chat_history.db` session replay.
