# feature_ChatTextSelection — Selectable / copyable message text

Lets the user highlight arbitrary ranges of text inside chat messages and
copy them to the clipboard, plus provides a one-click copy-whole-message
affordance. Sequenced **after** `feature_ChatPolish02.md` — the connect/
send/stop/retry/delete work in that file is higher-impact and a strict
prerequisite to landing paste-driven QA of this feature.

Canonical test command: `cargo test --workspace -- --test-threads=1`.

---

## Status: Phase A + Phase B + Phase C shipped. Feature complete.

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

**Phase C — Built-in right-click context menu on every `TextFieldView`** ✓
- Bug 1 and Bug 2 from the prior session are both fixed as side effects of
  moving the context menu into the widget itself. The chat-panel-level
  overlay still exists for ToolCall rows (which have no body) and for
  right-clicks on empty padding around message bodies.
- `TextFieldView` gains `context_menu_pos: Option<Point<Pixels>>`.
- Right-click (`on_mouse_down(MouseButton::Right, ...)`) focuses the field,
  captures the click position, calls `cx.stop_propagation()`, and sets
  `context_menu_pos`. An "empty menu" case (read-only + no selection) is
  detected inline: the handler returns early without stopping propagation,
  so outer containers (chat message rows) can still open their own
  "copy whole message" menu.
- Menu items adapt to state:
  - **Copy** is shown only when there is a non-empty selection.
  - **Paste** is shown only when the field is editable.
  - A separator is shown between them only when both are visible.
- Menu is rendered via `deferred(anchored().position(pos))` inside the
  field's own element tree, so it floats above siblings at the exact
  click coords.
- Dismissal is comprehensive:
  - `on_mouse_down_out` (capture phase) — clicking anywhere else in the
    window closes the menu *before* the click is handled elsewhere, so
    right-clicking a different field never leaves a stale menu behind.
  - Any left-click inside the field dismisses the menu.
  - Losing focus dismisses (handled in the focus-transition block of
    `render`).
  - `Escape` in `on_key_down` dismisses (checked before Ctrl shortcuts).
  - Clicking Copy/Paste dismisses and stops propagation.

**Phase C — Chat panel bug fixes** ✓
- Fix for the stale-capture class of bugs that affected the chat panel's
  action bar ⎘ button and the legacy per-row right-click handler: both
  were previously capturing `msg.read(cx).copy_text_for_context(cx)` at
  render time. Because selection changes in a message body's
  `TextFieldView` notify only that entity (not the `ChatPanel`), the
  captured string could be stale by the time the user clicked. Both
  handlers now clone the `Entity<ChatMessageView>` itself and call
  `read(cx).copy_text_for_context(cx)` at click time.

---

## Remaining work

- [ ] Double-click to select word (out of scope but easy follow-up)
- [ ] Shift+Arrow keyboard selection (out of scope)
- [ ] Cross-message selection (out of scope)
- [ ] Perf validation: `Ctrl+Shift+P` on a 500-message session

---

## Files touched

- `crates/u-forge-ui-gpui/src/text_field.rs` — Phase B: `read_only`,
  `drag_anchor`, `text_color`, `new_read_only`,
  `replace_content_preserving_selection`, `selected_text`,
  `insert_at_cursor`, selection paint, mouse-move/up handlers, Ctrl+C/V/A.
  Phase C: `context_menu_pos`, right-click handler with stop-propagation,
  `on_mouse_down_out` dismiss, Escape handler, focus-loss dismiss, inline
  `deferred(anchored)` menu with adaptive Copy/Paste items.
- `crates/u-forge-ui-gpui/src/chat_message.rs` — `body: Option<Entity<TextFieldView>>`,
  updated constructors, `append_text`/`append_error` routing,
  `copy_text_for_context`, `copy_text`.
- `crates/u-forge-ui-gpui/src/chat_panel.rs` — action bar copy button
  (Phase C: reads live state via cloned `msg` entity), right-click handler
  on message rows (Phase C: same fix), `ContextMenuState`, context menu
  overlay, `insert_at_cursor` paste action, right-click on input area.
