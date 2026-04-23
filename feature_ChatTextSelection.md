# feature_ChatTextSelection — Selectable / copyable message text

Lets the user highlight arbitrary ranges of text inside chat messages and
copy them to the clipboard, plus provides a one-click copy-whole-message
affordance. Sequenced **after** `feature_ChatPolish02.md` — the connect/
send/stop/retry/delete work in that file is higher-impact and a strict
prerequisite to landing paste-driven QA of this feature.

Canonical test command: `cargo test --workspace -- --test-threads=1`.

---

## Current state

`crates/u-forge-ui-gpui/src/chat_message.rs` renders message bodies with
plain `div().child(SharedString)` elements. GPUI's default text element has:

- no caret, no hit-testing for mid-word clicks,
- no drag selection,
- no clipboard integration.

Nothing copies today. Ctrl+C is a no-op.

`crates/u-forge-ui-gpui/src/text_field.rs` already owns the bulk of the
text infrastructure we need:

- Canvas-based paint via `window.text_system().shape_text(...)`
  (`text_field.rs:581-614`).
- `TextFieldLayout` caches the shaped layout and byte-offset-per-line index.
- `cursor_for_click(local_x, local_y) -> byte_offset` — exact glyph
  hit-testing (`text_field.rs:133-164`).
- `cursor_pos_from_layout(byte) -> (Pixels, Pixels)` — inverse mapping
  for visual selection rendering (`text_field.rs:168-201`).
- A `selection: Option<Range<usize>>` field that's already structurally
  present but only populated by `replace_and_mark_text_in_range`.

This is ~70% of the plumbing for read-only drag-selectable text.

Clipboard API (confirmed in `gpui-0.2.2`):
`cx.write_to_clipboard(gpui::ClipboardItem::new_string(text))`.

---

## Phases

Two phases. Phase A is a same-day landable win. Phase B is the real
feature and can ship immediately after A.

### Phase A — Per-message Copy button

**Intent:** Every message gets a 📋 icon that copies the full message
text to the system clipboard.

**Plan**

1. Extend `ChatMessageView::render_text` (chat_message.rs:151-185) with a
   header-row layout containing the existing role label on the left and a
   small clipboard icon on the right.
2. Icon button: 14×14 div, `rgba(0x6c708688)` idle, hover to
   `rgba(0xcdd6f4ff)`, character `"📋"` (or `"⎘"` if the monospace font
   doesn't have the emoji glyph — test both).
3. Handler:
   ```rust
   on_mouse_down(MouseButton::Left, cx.listener(|this, _, _window, cx| {
       cx.write_to_clipboard(gpui::ClipboardItem::new_string(this.text.to_string()));
   }))
   ```
4. Apply the same treatment to `render_thinking` and `render_tool_call`,
   placing the copy button next to the existing chevron. For tool calls,
   copy the full assembled `"name\nArgs:\n<args>\nResult:\n<result>"`
   text so pasting captures the whole block.
5. Optional "copied!" toast: skip — not worth the state churn. The user
   sees the paste land immediately.

**Files touched (Phase A)**
- `crates/u-forge-ui-gpui/src/chat_message.rs` — three render methods
  get a copy button.

**Risk:** negligible. Self-contained, no persistence, no async.

**Time estimate:** 30 min including manual verification.

---

### Phase B — In-message drag selection + Ctrl+C

**Intent:** Click-and-drag inside any message highlights text. Ctrl+C (or
Cmd+C on Mac) copies the highlighted substring.

**Design choice — extend vs duplicate**

Option B1 (recommended): extend `TextFieldView` with a `read_only: bool`
flag. Reuses the canvas, layout cache, IME handler, and paint logic.
Cost: threads a flag through ~6 call sites and suppresses key handlers
that mutate content.

Option B2: introduce a new `SelectableText` element. Cleaner boundary,
but duplicates `shape_text` / layout-cache / click-mapping code. The
paint closure alone is ~170 lines.

**Pick B1.** Duplication is the bigger long-term cost. The "read-only"
flag is cheap to maintain.

**Plan**

1. Add to `TextFieldView`:
   ```rust
   read_only: bool,
   drag_anchor: Option<usize>,  // byte offset where current drag started
   ```
   `drag_anchor` is `Some` while the left mouse button is held after a
   down event inside the field. `None` otherwise.

2. Constructor: new `TextFieldView::new_read_only(text: &str, cx: &mut Context<Self>) -> Self`
   that sets `read_only = true`, `multiline = true` (messages can be
   multi-paragraph), disables the blink task (no caret needed), and
   populates `content` directly.

3. Paint closure changes (`text_field.rs:523-692`):
   - Before painting text: if `selection.is_some()` and non-empty, paint
     highlight quads under the selected glyphs. One quad per visual line
     of the selection. Compute start-X / end-X per wrapped line using
     `wl.position_for_index` against the selection endpoints clamped to
     that line's byte range. Color: `rgba(0x585b7055)` (subtle blue-grey
     overlay).
   - Skip cursor painting when `read_only = true` — the caret is
     meaningless for read-only text.
   - Keep the `handle_input` install so IME still works for editable
     instances. In read-only mode, skip the install to reduce per-paint
     work and avoid stealing focus from other fields.

4. Mouse handlers (extend existing ones at `text_field.rs:719-748`):
   - `on_mouse_down(Left)`:
     - In editable mode: existing behavior (set cursor, clear selection).
     - In read-only mode: set `selection = Some(byte..byte)`, set
       `drag_anchor = Some(byte)`, focus self for Ctrl+C routing.
   - **New** `on_mouse_move` (listener): if `drag_anchor.is_some()`,
     compute hit-tested byte offset from current pointer position, set
     `selection = Some(anchor..new)`, `cx.notify()`.
   - **New** `on_mouse_up(Left)` (listener): if `drag_anchor.is_some()`,
     drop it. If `selection.start == selection.end`, clear selection.

   Mouse-move throttling: if paint cost climbs with long messages, coalesce
   updates to one per frame by checking `window.refresh_requested()` — but
   follow `.rules` rule 4: **measure first**. Don't throttle speculatively.

5. Keyboard handler (extend `text_field.rs:749-839`):
   - New arm for `"c"` when `event.keystroke.modifiers.platform` (Cmd on
     Mac) or `.control` (Linux/Win) is true:
     - If `selection` is `Some` and non-empty, slice the selected substring
       out of `content` and write to clipboard.
     - Swallow the event (don't fall through to IME).
   - In read-only mode, also swallow: backspace, delete, and
     `replace_text_in_range` IME inserts — these would otherwise mutate
     content. Simplest implementation: add an early-return guard in each
     mutating key branch and in `replace_text_in_range`.

6. Focus model:
   - Read-only `TextFieldView`s are focusable so Ctrl+C routes to them.
   - Click inside → focus. Click elsewhere → lose focus (standard GPUI
     focus behavior).
   - Only one message selected at a time: clicking in message B clears
     the selection in message A (because A loses focus). This matches
     intuitive multi-widget selection UX and avoids a global selection
     registry.

7. Chat message integration:
   - `ChatMessageView::render_text` creates a child
     `Entity<TextFieldView>` in its constructor (`new_text`,
     `new_tool_call`, `from_stored`) and stores it in a new field
     `body: Entity<TextFieldView>`.
   - `append_text` / `append_error` / `set_tool_result` become:
     `self.body.update(cx, |tf, cx| tf.set_content(&new_text, cx))`.
     Since `set_content` currently resets cursor / scroll, introduce
     a `TextFieldView::replace_content_preserving_selection` variant
     used during streaming — otherwise each token nukes an active
     selection.

   **Trade-off:** allocating one `Entity<TextFieldView>` per message is
   more expensive than the current plain-div rendering. Each entity adds
   ~1 paint-closure and ~1 layout cache. With hundreds of messages in
   long sessions this is the main perf risk. Mitigation: the message
   list is already virtualized (`ListState`), so off-screen messages
   don't paint. Validate with the perf overlay (`Ctrl+Shift+P`) on a
   500-message session before declaring victory.

8. Thinking and ToolCall bodies:
   - Apply the same treatment to the expanded body of collapsible blocks.
     Collapsed blocks skip the `TextFieldView` entirely (keep them cheap).

**Files touched (Phase B)**
- `crates/u-forge-ui-gpui/src/text_field.rs` — `read_only` flag,
  `drag_anchor`, selection paint, mouse-move / mouse-up handlers,
  Ctrl+C handler, `new_read_only`, `replace_content_preserving_selection`.
- `crates/u-forge-ui-gpui/src/chat_message.rs` — each message owns a
  `body: Entity<TextFieldView>`; `append_text` routes through it.

**Out of scope for Phase B**
- **Cross-message selection.** Selection stays per-message. Dragging
  from message A into message B cancels A's selection when B focuses.
  A session-wide selection model would require a coordinator plus
  coordinated paint; revisit only if explicitly asked.
- **Double-click to select word / triple-click to select line.** Common
  but not in scope. Easy follow-up once single-click-drag works.
- **Selection via keyboard (Shift+Arrow).** Out of scope — would need
  caret semantics in read-only mode, which complicates the state machine.

---

## Testing

Unit-level: none — rendering/mouse behavior isn't covered by the Rust
test suite. Manual, via `cargo run -p u-forge-ui-gpui`:

**Phase A**
- Click copy icon on a user message, paste into external editor — text
  matches verbatim.
- Click copy icon on an assistant message with newlines — newlines
  survive paste.
- Click copy icon on a tool call → paste includes name, args, result.

**Phase B**
- Drag across part of an assistant message — highlight appears, tracks
  the pointer, stops at release.
- Ctrl+C with active selection → paste elsewhere yields the substring.
- Click inside message A, drag, release. Click into message B — A's
  highlight clears, B gets a cursor/anchor.
- Selection survives scrolling a long message into and out of view
  (virtualized list recycle test).
- Streaming: start a long response, select a range of already-streamed
  text. New tokens arriving must not clear the selection.
- Perf: `Ctrl+Shift+P` overlay on a session with 500 messages — drag
  selection in the last message; frame time ≤ 16 ms.

---

## Implementation order (within this feature)

1. Phase A alone — ship and confirm it's enough UX for the user's daily
   workflow.
2. Phase B only if the user explicitly asks after using Phase A.

Gate on user feedback between phases.
