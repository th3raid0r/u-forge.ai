# feature_ChatPolish02 — Chat UI quality-of-life polish

Three features for the GPUI chat panel. All changes live in
`crates/u-forge-ui-gpui/src/` (primarily `chat_panel.rs` and `chat_message.rs`).
None touch the knowledge graph or inference pipeline.

Text selection / copy-from-messages is tracked separately in
**`feature_ChatTextSelection.md`** and is sequenced **after** this feature
completes.

Canonical test command: `cargo test --workspace -- --test-threads=1`.

---

## Feature A — Retry button on every assistant message ✓ DONE

### Intent
Each assistant message shows a "⟳ Retry" button. Clicking it re-runs the
immediately preceding user turn, replacing this assistant message (and
anything after it — thinking blocks, tool calls, later turns).

### Status
**Implemented.** All tests pass (`cargo test --workspace -- --test-threads=1`).

Key changes landed:
- `chat_message.rs`: `RetryRequested(EntityId)` event + `EventEmitter` impl;
  `render_text` takes `cx` and appends a `⟳` footer button on assistant
  messages that emits `RetryRequested(cx.entity_id())` on click.
- `chat_panel.rs`: `msg_subscriptions: Vec<gpui::Subscription>` parallel to
  `messages`; `send_with_text` extracted from `do_send`; `retry_message`
  finds the preceding user turn, truncates `messages` + `msg_subscriptions`
  to that index, resets `list_state`, and calls `send_with_text`.
  Subscriptions created on push, rebuilt on `load_session`, cleared on
  `new_session` / `delete_session`.

### Original current state (pre-implementation)
`ChatPanel::do_send` pushed a `User` message then streamed the response.
No retry existed. Messages stored as `Vec<Entity<ChatMessageView>>`.

### Plan

1. Add a `retry_message(&mut self, msg_entity_id: gpui::EntityId, cx: &mut Context<Self>)`
   method on `ChatPanel`:
   - Abort if `self.streaming` — log via `tracing::debug!` and return, same
     pattern as `new_session`/`load_session`/`delete_session`.
   - Find `msg_idx` by searching `self.messages` for the entity with a
     matching id. If not found, return.
   - Walk backwards from `msg_idx - 1` to find the nearest preceding
     `ChatMessageRole::User` message. Record its text.
   - If none found, return (guards against corrupt state).
   - Truncate `self.messages` to remove everything from and including
     the User message we're about to re-send — we'll push it back fresh
     so `save_current_session` sees a clean tail.
   - Call `self.list_state.reset(self.messages.len())` — truncation
     invalidates cached item measurements past the cut point.
   - Call `self.send_with_text(user_text, cx)` (new helper, see below).

2. Extract a `fn send_with_text(&mut self, text: String, cx: &mut Context<Self>)`
   helper that contains the current `do_send` body from "Clear input..."
   onward. `do_send` becomes:
   ```rust
   fn do_send(&mut self, cx: &mut Context<Self>) {
       let text = self.input_field.read(cx).content.trim().to_string();
       if text.is_empty() || self.streaming { return; }
       self.input_field.update(cx, |field, cx| field.set_content("", cx));
       self.send_with_text(text, cx);
   }
   ```
   Retry reuses `send_with_text` without going through the input field.

3. Add a footer row to `ChatMessageView::render_text` when
   `role == Assistant` containing the retry button. 14×14 `div`, text
   `"⟳"`, `rgba(0x6c708688)` idle, hover to `rgba(0xcdd6f4ff)`.
4. The retry button needs to tell the parent `ChatPanel` which message to
   retry. The message entity doesn't know its own index and doesn't know
   about `ChatPanel`. Use event emission:
   - Add `pub(crate) struct RetryRequested(pub gpui::EntityId);` to
     `chat_message.rs`.
   - `impl EventEmitter<RetryRequested> for ChatMessageView`.
   - The button handler emits `cx.emit(RetryRequested(cx.entity_id()))`.
5. `ChatPanel` subscribes on each push:
   ```rust
   let sub = cx.subscribe(&msg, |this: &mut ChatPanel, _src, ev: &RetryRequested, cx| {
       this.retry_message(ev.0, cx);
   });
   // stored so it isn't dropped
   ```
   Subscriptions stored in a `Vec<gpui::Subscription>` on `ChatPanel`
   (alongside the existing `submit_sub`). Cleared on session switch.

**Edge cases**
- Retry on the *last* assistant message: behaves as a regenerate.
- Retry when the provider isn't configured: the existing "Chat unavailable"
  fallback in `do_send` (now `send_with_text`) handles this.
- Retry during streaming: suppressed via the `self.streaming` guard.
- Retry across a Thinking block: Thinking blocks are filtered out of
  `raw_history` in `send_with_text` (`chat_panel.rs:281-297`), so removing
  them doesn't change the effective history. Safe to truncate.
- Retry across tool calls: same reasoning — tool call messages aren't in
  `raw_history`. Safe to truncate.

### Files touched
- `crates/u-forge-ui-gpui/src/chat_message.rs` — `RetryRequested` event,
  `EventEmitter` impl, retry button in `render_text` for Assistant.
- `crates/u-forge-ui-gpui/src/chat_panel.rs` — new `retry_message`,
  `send_with_text` extraction, per-message subscription vector.

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

## Feature C — Inline delete button on the last message ("Back button")

### Intent
A small 🗑 icon appears on **whichever message is currently the tail** of
the session — user, assistant, thinking, or tool call. One click deletes
that single message. After deletion, the icon re-renders on the new tail,
so the user can chain-click to walk backwards through history.

This is an undo / "back button" for conversation tail, used to trim
before a retry (or instead of one).

### Current state
`delete_session` exists (`chat_panel.rs:714`) but deletes the entire
session. No per-message delete. The last-message slot has no affordance.

### Design notes (from user direction)

- **Inline**, not a toolbar button.
- Shown **only on the last message**, not all of them.
- **Any role** — user / assistant / thinking / tool call — is deletable.
- **Re-evaluates after deletion.** The new tail picks up the button.
- Chainable: click repeatedly to delete all messages.

### Plan

1. Add `fn delete_message_at(&mut self, ix: usize, cx: &mut Context<Self>)`
   on `ChatPanel`:
   - Abort if `self.streaming` — same suppression pattern as other
     mutators. (The last message during streaming is the in-flight
     assistant response; allowing delete mid-stream would race with
     `append_text`.)
   - Bounds-check: `if ix >= self.messages.len() { return; }`.
   - `self.messages.remove(ix);`
   - `self.list_state.reset(self.messages.len())` — truncation (or
     mid-list removal) invalidates cached measurements.
   - Drop any subscriptions the deleted entity owned (see step 3 below).
   - `self.save_current_session(cx);` — persistence stays in sync.
   - `cx.notify();`.

   **No group-pop behavior.** Each click deletes exactly one message —
   user's explicit "back button" semantics. A Thinking block followed by
   an Assistant takes two clicks to clear; that's intentional.

2. Button placement — **render-site, not per-entity**. Wrap each list
   item in the virtualized list builder (`chat_panel.rs:841-851`) with
   a container that conditionally adds the delete button when
   `ix == panel.messages.len() - 1`:
   ```rust
   move |ix, _window, cx: &mut App| {
       let panel = list_entity.read(cx);
       let Some(msg) = panel.messages.get(ix).cloned() else {
           return div().into_any_element();
       };
       let is_last = ix + 1 == panel.messages.len();
       let is_streaming = panel.streaming;
       let entity = list_entity.clone();

       let mut row = div()
           .id(("msg-row", ix))
           .flex()
           .flex_col()
           .w_full()
           .child(msg);

       if is_last && !is_streaming {
           row = row.child(
               div()
                   .id(("del-row", ix))
                   .flex()
                   .flex_row()
                   .justify_end()
                   .px_2()
                   .py(px(2.0))
                   .child(
                       div()
                           .id(("del", ix))
                           .flex()
                           .items_center()
                           .justify_center()
                           .w(px(18.0))
                           .h(px(18.0))
                           .rounded(px(2.0))
                           .text_xs()
                           .text_color(rgba(0x6c708688))
                           .cursor_pointer()
                           .hover(|s| s.text_color(rgba(0xf38ba8ff))
                                       .bg(rgba(0x45475a66)))
                           .on_mouse_down(
                               MouseButton::Left,
                               move |_, _, cx: &mut App| {
                                   entity.update(cx, |this, cx| {
                                       let last = this.messages.len().saturating_sub(1);
                                       this.delete_message_at(last, cx);
                                   });
                               },
                           )
                           .child("🗑"),
                   ),
           );
       }
       row.into_any_element()
   }
   ```

   This keeps the button **outside** `ChatMessageView`. No per-message
   `is_last` bookkeeping, no re-notify dance when the tail shifts — the
   list automatically re-renders items when their data changes, and the
   wrapper div recomputes `is_last` on each render.

3. Subscription cleanup for Feature A:
   When Feature A adds `Vec<gpui::Subscription>` on `ChatPanel` (one per
   message for retry), `delete_message_at` must also remove the
   corresponding subscription. Since `gpui::Subscription` doesn't carry
   a natural key, store them in a parallel `Vec<gpui::Subscription>`
   indexed the same as `self.messages`, and `subs.remove(ix)` alongside
   `self.messages.remove(ix)`. Dropping a `Subscription` unsubscribes.

**Edge cases**
- Deleting down to zero messages: session becomes empty;
  `save_current_session` still writes an empty message list; session
  title was derived from the first user message and stays until the
  session itself is deleted or a new user message is added. Acceptable.
- Deleting during streaming: button is not rendered when `streaming`.
- Undo: out of scope. Deletion is persisted immediately.

### Files touched
- `crates/u-forge-ui-gpui/src/chat_panel.rs` — `delete_message_at`,
  wrapper div in the virtual-list item builder, subscription-vector
  sync.

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

### Subscription storage
Feature A introduces one subscription per message (for retry events).
Keep a parallel `Vec<gpui::Subscription>` on `ChatPanel`:
- Push alongside `self.messages.push(msg)`.
- `remove(ix)` alongside `self.messages.remove(ix)`.
- `clear()` on session switch (alongside `self.messages.clear()`).
- On wholesale session load, rebuild from the new message list.

This same vector carries any future per-message subscription (e.g. a
future "edit message" event).

---

## Implementation order

1. **Feature B1 (cancellation plumbing) first** — establishes the
   `stream_task` field. Foundational for the other two.
2. **Feature C (inline delete)** — smallest surface area; validates the
   list-wrapper approach. Doesn't depend on A.
3. **Feature B2–B4 (Connect / Stop button states + wiring)** — depends
   on B1.
4. **Feature A (retry)** ✓ DONE — `send_with_text` extracted; `retry_message`
   implemented; `Vec<Subscription>` pattern in place. Feature C's delete
   path (`delete_message_at`) must also call
   `self.msg_subscriptions.remove(ix)` when it lands.

Note on order vs dependency: C's subscription-cleanup concern (step
C.3) is only meaningful once A lands. Land C first *without* the
subscription-vector hook (there's nothing to clean up yet), then when A
adds the vector, extend C's delete path in the same PR.

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
  - Inline delete on the tail: message vanishes, button re-appears on
    the new tail; chain-click through multiple messages of varying
    roles; confirm session empties cleanly.
- Persistence: all above operations should survive an app restart via
  the `chat_history.db` session replay.
