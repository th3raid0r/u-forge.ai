# feature_ChatTextSelection — Selectable / copyable message text

Lets the user highlight arbitrary ranges of text inside chat messages and
copy them to the clipboard, plus provides a one-click copy-whole-message
affordance. Sequenced **after** `feature_ChatPolish02.md` — the connect/
send/stop/retry/delete work in that file is higher-impact and a strict
prerequisite to landing paste-driven QA of this feature.

Canonical test command: `cargo test --workspace -- --test-threads=1`.

---

## Status: Phase A + Phase B implemented. Two known bugs remain.

### What's shipped

**Phase A — Copy button in action bar** ✓
- `⎘` copy button added to the action bar on every User/Assistant message
  (alongside the existing retry/delete buttons). Copies the full message
  text or, if a selection is active in the body, the selected substring.
- `copy_text()` on `ChatMessageView` assembles ToolCall bodies as
  `"name\nArgs:\n…\nResult:\n…"` for the right-click copy path.

**Phase A — Right-click context menu** ✓ (partial — see known bugs)
- Right-clicking any message row opens a small `anchored()` overlay with
  **Copy** and **Paste** items.
- **Copy** captures `copy_text_for_context(cx)` at right-click time:
  returns the active selection if any, otherwise the full message text.
- **Paste** reads the clipboard and calls `insert_at_cursor` on the chat
  input `TextFieldView`.
- Menu dismisses on left-click in the message area or input area (same
  dismiss mechanism as the history/model dropdowns).

**Phase B — Drag selection in all text fields** ✓
- `TextFieldView` extended with `read_only: bool`, `drag_anchor: Option<usize>`,
  `text_color: Hsla`.
- `new_read_only(text, color, cx)` constructor — no border/bg/caret/IME,
  full content height (no `TEXT_FIELD_MAX_H` cap).
- Selection highlight painted as quads behind text per visual row,
  handles word-wrapped lines. Color `rgba(0x585b7088)`.
- `on_mouse_down` sets `drag_anchor`; `on_mouse_move` extends selection;
  `on_mouse_up` / `on_mouse_up_out` finalizes.
- `Ctrl+C` copies selection in any mode (read-only or editable). ✓ **verified working**
- `Ctrl+V` pastes at cursor in editable mode only. ✓ **verified working**
- `Ctrl+A` selects all in any mode.
- All mutation keys (`backspace`, `delete`, `enter`, etc.) suppressed in
  read-only mode.
- `replace_content_preserving_selection` used during streaming so active
  selections survive token deltas.
- `selected_text()` returns the selected substring or None.

**Phase B — Chat message bodies use `TextFieldView`** ✓
- `ChatMessageView` gains `body: Option<Entity<TextFieldView>>`.
- User/Assistant/Thinking messages create a read-only body on construction.
- ToolCall messages keep plain div rendering (`body: None`).
- `append_text` / `append_error` update the body via
  `replace_content_preserving_selection`.
- `render_text` and `render_thinking` render the body entity; plain-div
  fallback for the None case.
- Constructors (`new_text`, `new_tool_call`, `from_stored`) now take
  `cx: &mut Context<Self>` to create the nested entity.

**`insert_at_cursor` on `TextFieldView`** ✓
- Inserts (or replaces selection) at the cursor position and emits
  `TextChanged`. Used by the Paste action in the context menu.

---

## Known bugs (next session)

### Bug 1 — Right-click Copy does nothing in the chat panel

**Symptom:** Opening the context menu and clicking "Copy" has no visible
effect; nothing lands in the clipboard.

**Likely cause:** `copy_text_for_context` is called at right-click time
(inside the list builder closure, `cx: &mut App`). At that moment the
body's `TextFieldView` may not yet have focus or the selection might not
be readable via the shared `App` context in the same frame as the
right-click event. The captured `ctx_text` in `ContextMenuState` may end
up as an empty string.

**Investigation starting point:**
- Add a `tracing::debug!` in `copy_text_for_context` to log what text is
  captured at right-click time.
- Check whether `body.read(cx).selected_text()` returns `Some` during
  a right-click when a selection is visually active.
- Verify that the `ctx_copy_listener` closure is actually being reached
  (the menu item fires its `on_mouse_down`).

### Bug 2 — Right-click context menu only appears on chat message rows

**Symptom:** Right-clicking outside the chat message list (e.g. on the
header area, input field, node editor, etc.) does not open a context menu.

**Root cause:** The right-click handler is only registered on
`("msg-row", ix)` divs inside the chat panel's list builder. There is no
global right-click handler.

**Design decision needed:** A global context menu would need to be owned
by `AppView` (or a new coordinator), with a way to determine the "active
selectable" under the cursor and what text it contains. This requires
either:
- A GPUI focus-based approach: the focused `TextFieldView` is always the
  selection owner; right-click anywhere routes to it.
- A mouse-position hit-test approach: walk the element tree to find the
  `TextFieldView` under the pointer.

The simplest fix: register `on_mouse_down(Right, ...)` on the input field
wrapper in `chat_panel.rs` render so at least the input box gets a context
menu. Broader global coverage is a follow-up.

---

## Remaining work

- [ ] Fix Bug 1 (Copy in context menu)
- [ ] Fix Bug 2 (right-click coverage beyond message rows)
- [ ] Double-click to select word (out of scope but easy follow-up)
- [ ] Shift+Arrow keyboard selection (out of scope)
- [ ] Cross-message selection (out of scope)
- [ ] Perf validation: `Ctrl+Shift+P` on a 500-message session

---

## Files touched

- `crates/u-forge-ui-gpui/src/text_field.rs` — `read_only`, `drag_anchor`,
  `text_color`, `new_read_only`, `replace_content_preserving_selection`,
  `selected_text`, `insert_at_cursor`, selection paint, mouse-move/up
  handlers, Ctrl+C/V/A.
- `crates/u-forge-ui-gpui/src/chat_message.rs` — `body: Option<Entity<TextFieldView>>`,
  updated constructors, `append_text`/`append_error` routing,
  `copy_text_for_context`, `copy_text`.
- `crates/u-forge-ui-gpui/src/chat_panel.rs` — action bar copy button,
  right-click handler on message rows, `ContextMenuState`, context menu
  overlay, `insert_at_cursor` paste action.
