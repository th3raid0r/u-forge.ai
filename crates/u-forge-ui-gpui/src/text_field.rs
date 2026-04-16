use std::ops::Range;

use gpui::{
    canvas, div, fill, font, point, prelude::*, px, rgba, size, App, Bounds, Context,
    ElementInputHandler, EntityInputHandler, FocusHandle, Focusable, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString,
    TextAlign, TextRun, UTF16Selection, Window, WrappedLine,
};

/// Single-line text field height (px).
pub(crate) const TEXT_FIELD_MIN_H: f32 = 28.0;
/// Maximum text field height before scrolling kicks in (px).
pub(crate) const TEXT_FIELD_MAX_H: f32 = 120.0;
/// Vertical padding inside text fields (px).
pub(crate) const TEXT_FIELD_PAD_Y: f32 = 4.0;

// ── Text field widget ────────────────────────────────────────────────────────

/// Event emitted by `TextFieldView` when its content changes.
pub(crate) struct TextChanged(pub(crate) String);

/// Event emitted by `TextFieldView` when Enter is pressed in a single-line field.
#[allow(dead_code)]
pub(crate) struct TextSubmit(pub(crate) String);

/// Cached shaped text layout from the most recent paint, used for click-to-cursor mapping.
/// One WrappedLine per explicit '\n'-delimited line, paired with the byte offset where
/// that line starts in the content string. All fields use wrapped text for dynamic sizing.
struct TextFieldLayout(Vec<(usize, WrappedLine)>);

/// A minimal editable text field built on GPUI's `EntityInputHandler`.
///
/// Handles basic cursor movement, character insertion (via platform IME),
/// backspace, delete, home/end, and optional multiline editing.
pub(crate) struct TextFieldView {
    pub(crate) content: String,
    /// Cursor position as a UTF-8 byte offset into `content`.
    cursor: usize,
    /// Optional selection range (start..end) in UTF-8 byte offsets.
    selection: Option<Range<usize>>,
    /// IME marked (composing) text range.
    marked_range: Option<Range<usize>>,
    pub(crate) focus: FocusHandle,
    multiline: bool,
    /// When true on a multiline field, Enter emits `TextSubmit` and Shift+Enter inserts a newline.
    pub(crate) submit_on_enter: bool,
    placeholder: String,
    /// Whether the cursor is currently visible (used for blinking).
    cursor_visible: bool,
    /// Epoch counter — incremented on every reset or stop to cancel stale blink timers.
    blink_epoch: usize,
    /// Tracks the focused state from the previous render, so we can detect changes.
    was_focused: bool,
    /// Actual line height measured from font metrics, updated each paint.
    measured_line_h: f32,
    /// Element origin X in window coordinates, updated each paint.
    field_origin_x: f32,
    /// Element origin Y in window coordinates, updated each paint.
    field_origin_y: f32,
    /// Cached shaped layout from the most recent paint frame — used for click mapping.
    shaped_layout: Option<TextFieldLayout>,
    /// Total content height (in pixels) from the most recent paint — used to size the field div.
    content_height: f32,
    /// Visible height of the text area (bounds height minus padding), updated each paint.
    visible_height: f32,
    /// Vertical scroll offset (multiline only).
    scroll_offset: f32,
    /// Horizontal scroll offset (single-line mode only).
    h_scroll_offset: f32,
    /// Visible width of the text area (bounds width minus padding), updated each paint.
    visible_width: f32,
}

impl TextFieldView {
    pub(crate) fn new(multiline: bool, placeholder: &str, cx: &mut Context<Self>) -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            selection: None,
            marked_range: None,
            focus: cx.focus_handle(),
            multiline,
            submit_on_enter: false,
            placeholder: placeholder.to_string(),
            cursor_visible: true,
            blink_epoch: 0,
            was_focused: false,
            measured_line_h: 19.0,
            field_origin_x: 0.0,
            field_origin_y: 0.0,
            shaped_layout: None,
            content_height: 0.0,
            visible_height: 0.0,
            scroll_offset: 0.0,
            h_scroll_offset: 0.0,
            visible_width: 0.0,
        }
    }

    /// Show cursor immediately and restart the blink cycle with a 500 ms head-start,
    /// matching Zed's `pause_blinking` → `blink_cursors` pattern.
    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.blink_tick(epoch, cx)).ok();
            }
        })
        .detach();
    }

    /// Toggle cursor visibility and reschedule; cancelled automatically when epoch
    /// no longer matches (i.e. `reset_blink` or `stop_blinking` was called).
    fn blink_tick(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if epoch != self.blink_epoch {
            return;
        }
        self.cursor_visible = !self.cursor_visible;
        cx.notify();
        self.blink_epoch += 1;
        let next_epoch = self.blink_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.blink_tick(next_epoch, cx)).ok();
            }
        })
        .detach();
    }

    /// Stop blinking and hide the cursor (called when focus is lost).
    fn stop_blinking(&mut self) {
        self.cursor_visible = false;
        self.blink_epoch += 1; // invalidates any pending blink_tick timers
    }

    /// Map a click position (local to text area, after subtracting element origin
    /// and padding) to a UTF-8 byte offset into `self.content`.
    /// Uses the cached shaped layout from the most recent paint for exact glyph mapping.
    fn cursor_for_click(&self, local_x: f32, local_y: f32) -> usize {
        let line_h = px(self.measured_line_h);
        match &self.shaped_layout {
            Some(TextFieldLayout(lines)) => {
                // Determine which explicit line was clicked by accumulating
                // the visual height of each WrappedLine.
                let mut y_acc = px(0.0);
                for (byte_start, wl) in lines {
                    let visual_rows = (wl.wrap_boundaries().len() + 1) as f32;
                    let line_visual_h = line_h * visual_rows;
                    if px(local_y) < y_acc + line_visual_h
                        || *byte_start == lines.last().map(|(b, _)| *b).unwrap_or(0)
                    {
                        let rel_y = px(local_y) - y_acc;
                        let pos = point(px(local_x), rel_y);
                        let idx = wl
                            .closest_index_for_position(pos, line_h)
                            .unwrap_or_else(|i| i);
                        return byte_start + idx;
                    }
                    y_acc += line_visual_h;
                }
                self.content.len()
            }
            None => {
                // No layout yet (first frame); clamp to content length.
                self.content
                    .len()
                    .min((local_x / self.measured_line_h.max(1.0) * 2.0) as usize)
            }
        }
    }

    /// Get cursor pixel position (x, y) relative to text area origin for a given
    /// byte offset, using the cached shaped layout. Falls back to (0,0) if no layout.
    fn cursor_pos_from_layout(&self, byte_offset: usize) -> (Pixels, Pixels) {
        let line_h = px(self.measured_line_h);
        match &self.shaped_layout {
            Some(TextFieldLayout(lines)) => {
                let mut y_acc = px(0.0);
                for (byte_start, wl) in lines {
                    let line_len = wl.len();
                    if byte_offset >= *byte_start && byte_offset <= *byte_start + line_len {
                        let local_idx = byte_offset - byte_start;
                        if let Some(pos) = wl.position_for_index(local_idx, line_h) {
                            return (pos.x, y_acc + pos.y);
                        }
                        return (wl.width(), y_acc);
                    }
                    let visual_rows = (wl.wrap_boundaries().len() + 1) as f32;
                    y_acc += line_h * visual_rows;
                }
                (px(0.0), y_acc)
            }
            None => (px(0.0), px(0.0)),
        }
    }

    /// Adjust vertical scroll_offset so the cursor is visible (multiline only).
    fn ensure_cursor_visible(&mut self, cursor_y: f32, visible_h: f32) {
        if visible_h <= 0.0 {
            return;
        }
        let line_h = self.measured_line_h;
        let cursor_bottom = cursor_y + line_h;
        if cursor_y < self.scroll_offset {
            self.scroll_offset = cursor_y;
        } else if cursor_bottom > self.scroll_offset + visible_h {
            self.scroll_offset = cursor_bottom - visible_h;
        }
        self.scroll_offset = self.scroll_offset.max(0.0);
    }

    /// Adjust horizontal h_scroll_offset so the cursor is visible (single-line only).
    fn ensure_cursor_visible_h(&mut self, cursor_x: f32, visible_w: f32) {
        if visible_w <= 0.0 {
            return;
        }
        // Keep a small margin so the cursor isn't flush against the edge.
        let margin = 8.0_f32;
        if cursor_x < self.h_scroll_offset + margin {
            self.h_scroll_offset = (cursor_x - margin).max(0.0);
        } else if cursor_x + margin > self.h_scroll_offset + visible_w {
            self.h_scroll_offset = cursor_x + margin - visible_w;
        }
        self.h_scroll_offset = self.h_scroll_offset.max(0.0);
    }

    /// Scroll to keep the current cursor visible, using cached layout from the
    /// previous paint frame. Called from key/mouse handlers — NOT from paint.
    fn scroll_to_cursor(&mut self) {
        if self.multiline {
            let (_, cy) = self.cursor_pos_from_layout(self.cursor);
            self.ensure_cursor_visible(f32::from(cy), self.visible_height);
        } else {
            let (cx, _) = self.cursor_pos_from_layout(self.cursor);
            self.ensure_cursor_visible_h(f32::from(cx), self.visible_width);
        }
    }

    pub(crate) fn set_content(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.to_string();
        self.cursor = 0;
        self.selection = None;
        self.scroll_offset = 0.0;
        self.h_scroll_offset = 0.0;
        self.reset_blink(cx);
        cx.notify();
    }

    /// Delete the current selection, returning true if something was deleted.
    fn delete_selection(&mut self) -> bool {
        if let Some(sel) = self.selection.take() {
            let start = sel.start.min(sel.end);
            let end = sel.start.max(sel.end);
            self.content.drain(start..end);
            self.cursor = start;
            true
        } else {
            false
        }
    }

    // ── UTF-8 ↔ UTF-16 helpers ──────────────────────────────────────────────

    fn utf8_to_utf16_offset(&self, utf8_offset: usize) -> usize {
        self.content[..utf8_offset.min(self.content.len())]
            .encode_utf16()
            .count()
    }

    fn utf16_to_utf8_offset(&self, utf16_offset: usize) -> usize {
        let mut utf16_count = 0usize;
        for (byte_idx, ch) in self.content.char_indices() {
            if utf16_count >= utf16_offset {
                return byte_idx;
            }
            utf16_count += ch.len_utf16();
        }
        self.content.len()
    }

    /// Move cursor one character to the left.
    fn move_left(&mut self) {
        if self.cursor > 0 {
            let prev = self.content[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.cursor = prev;
        }
    }

    /// Move cursor one character to the right.
    fn move_right(&mut self) {
        if self.cursor < self.content.len() {
            let next = self.content[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.content.len());
            self.cursor = next;
        }
    }
}

impl Focusable for TextFieldView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl gpui::EventEmitter<TextChanged> for TextFieldView {}
impl gpui::EventEmitter<TextSubmit> for TextFieldView {}

impl EntityInputHandler for TextFieldView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let start = self.utf16_to_utf8_offset(range.start);
        let end = self.utf16_to_utf8_offset(range.end);
        let clamped_start = start.min(self.content.len());
        let clamped_end = end.min(self.content.len());
        if clamped_start != start || clamped_end != end {
            *adjusted_range = Some(
                self.utf8_to_utf16_offset(clamped_start)
                    ..self.utf8_to_utf16_offset(clamped_end),
            );
        }
        Some(self.content[clamped_start..clamped_end].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let (start, end) = if let Some(ref sel) = self.selection {
            (sel.start, sel.end)
        } else {
            (self.cursor, self.cursor)
        };
        let start16 = self.utf8_to_utf16_offset(start);
        let end16 = self.utf8_to_utf16_offset(end);
        Some(UTF16Selection {
            range: start16..end16,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| {
            self.utf8_to_utf16_offset(r.start)..self.utf8_to_utf16_offset(r.end)
        })
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Clear any marked text first.
        self.marked_range = None;

        let (start, end) = if let Some(r) = range {
            (
                self.utf16_to_utf8_offset(r.start),
                self.utf16_to_utf8_offset(r.end),
            )
        } else if let Some(ref sel) = self.selection {
            (sel.start.min(sel.end), sel.start.max(sel.end))
        } else {
            (self.cursor, self.cursor)
        };

        let clamped_start = start.min(self.content.len());
        let clamped_end = end.min(self.content.len());
        self.content.replace_range(clamped_start..clamped_end, text);
        self.cursor = clamped_start + text.len();
        self.selection = None;
        self.scroll_to_cursor();
        self.reset_blink(cx);

        cx.emit(TextChanged(self.content.clone()));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (start, end) = if let Some(r) = range {
            (
                self.utf16_to_utf8_offset(r.start),
                self.utf16_to_utf8_offset(r.end),
            )
        } else if let Some(ref sel) = self.selection {
            (sel.start.min(sel.end), sel.start.max(sel.end))
        } else {
            (self.cursor, self.cursor)
        };

        let clamped_start = start.min(self.content.len());
        let clamped_end = end.min(self.content.len());
        self.content
            .replace_range(clamped_start..clamped_end, new_text);

        let mark_start = clamped_start;
        let mark_end = clamped_start + new_text.len();
        self.marked_range = Some(mark_start..mark_end);

        if let Some(sel_range) = new_selected_range {
            let sel_start = self.utf16_to_utf8_offset(sel_range.start) + clamped_start;
            let sel_end = self.utf16_to_utf8_offset(sel_range.end) + clamped_start;
            self.cursor = sel_end.min(self.content.len());
            self.selection = Some(
                sel_start.min(self.content.len())..sel_end.min(self.content.len()),
            );
        } else {
            self.cursor = mark_end;
            self.selection = None;
        }

        cx.emit(TextChanged(self.content.clone()));
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let start = self.utf16_to_utf8_offset(range_utf16.start);
        let clamped = start.min(self.content.len());
        let line_h = px(self.measured_line_h);
        let pad = point(px(6.0), px(4.0));
        let (cx_px, cy_px) = self.cursor_pos_from_layout(clamped);
        let x = element_bounds.origin.x + pad.x + cx_px;
        let y = element_bounds.origin.y + pad.y + cy_px;
        Some(Bounds::new(Point::new(x, y), size(px(8.0), line_h)))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        // Convert point (element-local) to text-area-local by subtracting padding,
        // adding scroll offset so IME popup tracks the actual glyph position.
        let local_x = f32::from(point.x) - 6.0
            + if self.multiline { 0.0 } else { self.h_scroll_offset };
        let local_y = f32::from(point.y) - 4.0
            + if self.multiline { self.scroll_offset } else { 0.0 };
        let utf8_offset = self.cursor_for_click(local_x, local_y);
        Some(self.utf8_to_utf16_offset(utf8_offset))
    }
}

impl Render for TextFieldView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let entity = cx.entity().clone();
        let paint_entity = cx.entity().clone();

        // Start or stop the blink cycle when focus changes.
        if focused != self.was_focused {
            self.was_focused = focused;
            if focused {
                self.reset_blink(cx);
            } else {
                self.stop_blinking();
            }
        }

        // Prepare data for the paint canvas closure (captures by value).
        let content = self.content.clone();
        let placeholder = self.placeholder.clone();
        let cursor_byte = self.cursor;
        let is_focused = focused;
        let cursor_visible = self.cursor_visible;
        let scroll_offset = self.scroll_offset;
        let h_scroll_offset = self.h_scroll_offset;
        let is_multiline = self.multiline;

        // The main canvas renders text and cursor via shape_line/paint, exactly
        // matching the glyph positions GPUI uses internally — like Zed does.
        // This guarantees the cursor aligns perfectly with the rendered text.
        let text_canvas = canvas(
            |_, _, _| {},
            move |bounds, (), window, cx| {
                let rem_size = window.rem_size();
                let font_size = rem_size * 0.75; // text_xs = 0.75rem
                let line_height = (font_size * 1.618_034).round();
                let mono_font = font(".SystemUIMonospacedFont");

                // Determine display text and color.
                let (display, text_hsla) = if content.is_empty() && !is_focused {
                    (placeholder.clone(), Hsla::from(rgba(0x6c7086ff)))
                } else {
                    (content.clone(), Hsla::from(rgba(0xcdd6f4ff)))
                };

                let pad_x = px(6.0);
                let pad_y = px(TEXT_FIELD_PAD_Y);
                let inner_w = bounds.size.width - pad_x * 2.0;

                // Single-line: horizontal scroll, no word wrap.
                // Multiline: vertical scroll, wrap at inner_w.
                let text_origin = if is_multiline {
                    let scroll_px = px(scroll_offset);
                    bounds.origin + point(pad_x, pad_y - scroll_px)
                } else {
                    let h_scroll_px = px(h_scroll_offset);
                    bounds.origin + point(pad_x - h_scroll_px, pad_y)
                };
                let wrap_width: Option<gpui::Pixels> = if is_multiline {
                    Some(inner_w)
                } else {
                    None
                };

                // Split content by explicit newlines.
                let raw_lines: Vec<&str> = if display.is_empty() {
                    vec![""]
                } else {
                    display.split('\n').collect()
                };

                let mut cursor_pos: Option<gpui::Point<Pixels>> = None;
                let mut byte_offset: usize = 0;
                let mut y_acc = px(0.0);

                let mut stored_lines: Vec<(usize, WrappedLine)> = Vec::new();

                for (line_idx, raw_line) in raw_lines.iter().enumerate() {
                    if !raw_line.is_empty() {
                        let line_text: SharedString = (*raw_line).to_string().into();
                        let run = TextRun {
                            len: raw_line.len(),
                            font: mono_font.clone(),
                            color: text_hsla,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        if let Ok(wrapped) = window.text_system().shape_text(
                            line_text,
                            font_size,
                            &[run],
                            wrap_width,
                            None,
                        ) {
                            for wl in wrapped {
                                let paint_origin = text_origin + point(px(0.0), y_acc);
                                let _ = wl.paint(
                                    paint_origin,
                                    line_height,
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );

                                // Cursor position within this line.
                                if cursor_pos.is_none()
                                    && cursor_byte >= byte_offset
                                    && cursor_byte <= byte_offset + raw_line.len()
                                {
                                    let local = cursor_byte - byte_offset;
                                    if let Some(p) = wl.position_for_index(local, line_height) {
                                        cursor_pos = Some(point(p.x, y_acc + p.y));
                                    }
                                }

                                let visual_rows = (wl.wrap_boundaries().len() + 1) as f32;
                                stored_lines.push((byte_offset, wl));
                                y_acc += line_height * visual_rows;
                            }
                        }
                    } else {
                        // Empty line — cursor may sit here.
                        if cursor_pos.is_none() && cursor_byte == byte_offset {
                            cursor_pos = Some(point(px(0.0), y_acc));
                        }
                        // Push an empty shaped line for click mapping.
                        let run = TextRun {
                            len: 0,
                            font: mono_font.clone(),
                            color: text_hsla,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        if let Ok(wrapped) = window.text_system().shape_text(
                            SharedString::from(""),
                            font_size,
                            &[run],
                            Some(inner_w),
                            None,
                        ) {
                            for wl in wrapped {
                                stored_lines.push((byte_offset, wl));
                            }
                        }
                        y_acc += line_height;
                    }

                    byte_offset += raw_line.len();
                    if line_idx < raw_lines.len() - 1 {
                        byte_offset += 1; // '\n'
                    }
                }

                // Store layout for click handling.
                paint_entity.update(cx, |this, _cx| {
                    this.shaped_layout = Some(TextFieldLayout(stored_lines));
                });

                // Paint the blinking cursor (adjusted for scroll offset).
                if is_focused && cursor_visible {
                    let cp = cursor_pos.unwrap_or(point(px(0.0), px(0.0)));
                    let cursor_origin = text_origin + cp;
                    window.paint_quad(fill(
                        gpui::Bounds::new(cursor_origin, size(px(1.5), line_height)),
                        rgba(0xcdd6f4ff),
                    ));
                }

                // Store measurements for click handling and dynamic sizing.
                // Round content_height to whole pixels to prevent sub-pixel oscillation.
                let lh_f = f32::from(line_height);
                let origin_x = f32::from(bounds.origin.x);
                let origin_y = f32::from(bounds.origin.y);
                let total_content_h =
                    (f32::from(y_acc) + TEXT_FIELD_PAD_Y * 2.0).round();
                let visible_h =
                    (f32::from(bounds.size.height) - TEXT_FIELD_PAD_Y * 2.0).max(0.0);
                let visible_w = f32::from(inner_w).max(0.0);

                paint_entity.update(cx, |this, _cx| {
                    this.field_origin_x = origin_x;
                    this.field_origin_y = origin_y;
                    this.measured_line_h = lh_f.max(1.0);
                    this.content_height = total_content_h;
                    this.visible_height = visible_h;
                    this.visible_width = visible_w;
                });

                // Install the input handler so IME / platform text input works.
                let focus2 = entity.read(cx).focus.clone();
                window.handle_input(
                    &focus2,
                    ElementInputHandler::new(bounds, entity.clone()),
                    cx,
                );
            },
        )
        .size_full();

        // Single-line: fixed height. Multiline: grow with content, capped at max.
        let dynamic_h = if self.multiline {
            self.content_height.max(TEXT_FIELD_MIN_H).min(TEXT_FIELD_MAX_H)
        } else {
            TEXT_FIELD_MIN_H
        };

        let field = div()
            .id(SharedString::from(format!("tf-{}", cx.entity_id().as_u64())))
            .relative()
            .w_full()
            .h(px(dynamic_h))
            .bg(gpui::rgb(0x313244))
            .rounded(px(4.0))
            .border_1()
            .border_color(if focused {
                gpui::rgb(0x89b4fa)
            } else {
                gpui::rgb(0x45475a)
            })
            .overflow_hidden()
            .track_focus(&self.focus);

        field
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    // Convert window coordinates to text-area-local coordinates.
                    // Subtract element origin and padding, add scroll offset.
                    let local_x = f32::from(event.position.x) - this.field_origin_x - 6.0
                        + if this.multiline { 0.0 } else { this.h_scroll_offset };
                    let local_y = f32::from(event.position.y) - this.field_origin_y
                        - TEXT_FIELD_PAD_Y
                        + if this.multiline { this.scroll_offset } else { 0.0 };
                    this.cursor = this.cursor_for_click(local_x, local_y);
                    this.selection = None;
                    this.scroll_to_cursor();
                    this.reset_blink(cx);
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                if !this.multiline {
                    return;
                }
                let delta_y = match event.delta {
                    ScrollDelta::Pixels(p) => -f32::from(p.y),
                    ScrollDelta::Lines(l) => -f32::from(l.y) * this.measured_line_h,
                };
                let max_scroll = (this.content_height - TEXT_FIELD_MAX_H).max(0.0);
                this.scroll_offset = (this.scroll_offset + delta_y).clamp(0.0, max_scroll);
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                match key {
                    "backspace" => {
                        if !this.delete_selection() && this.cursor > 0 {
                            this.move_left();
                            let remove_at = this.cursor;
                            let next_char_end = this.content[remove_at..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| remove_at + i)
                                .unwrap_or(this.content.len());
                            this.content.drain(remove_at..next_char_end);
                            cx.emit(TextChanged(this.content.clone()));
                        }
                        this.scroll_to_cursor();
                        this.reset_blink(cx);
                        cx.notify();
                    }
                    "delete" => {
                        if !this.delete_selection() && this.cursor < this.content.len() {
                            let next_char_end = this.content[this.cursor..]
                                .char_indices()
                                .nth(1)
                                .map(|(i, _)| this.cursor + i)
                                .unwrap_or(this.content.len());
                            this.content.drain(this.cursor..next_char_end);
                            cx.emit(TextChanged(this.content.clone()));
                        }
                        this.scroll_to_cursor();
                        this.reset_blink(cx);
                        cx.notify();
                    }
                    "left" => {
                        this.selection = None;
                        this.move_left();
                        this.scroll_to_cursor();
                        this.reset_blink(cx);
                        cx.notify();
                    }
                    "right" => {
                        this.selection = None;
                        this.move_right();
                        this.scroll_to_cursor();
                        this.reset_blink(cx);
                        cx.notify();
                    }
                    "home" => {
                        this.selection = None;
                        this.cursor = 0;
                        this.scroll_to_cursor();
                        this.reset_blink(cx);
                        cx.notify();
                    }
                    "end" => {
                        this.selection = None;
                        this.cursor = this.content.len();
                        this.scroll_to_cursor();
                        this.reset_blink(cx);
                        cx.notify();
                    }
                    "enter" => {
                        let shift = event.keystroke.modifiers.shift;
                        if this.multiline {
                            // submit_on_enter: Enter submits, Shift+Enter newline.
                            // !submit_on_enter: Enter newline, Shift+Enter submits.
                            let should_submit = this.submit_on_enter != shift;
                            if should_submit {
                                cx.emit(TextSubmit(this.content.clone()));
                            } else {
                                this.delete_selection();
                                this.content.insert(this.cursor, '\n');
                                this.cursor += 1;
                                cx.emit(TextChanged(this.content.clone()));
                                this.scroll_to_cursor();
                                this.reset_blink(cx);
                                cx.notify();
                            }
                        } else {
                            cx.emit(TextSubmit(this.content.clone()));
                        }
                    }
                    _ => {}
                }
            }))
            .child(text_canvas)
    }
}
