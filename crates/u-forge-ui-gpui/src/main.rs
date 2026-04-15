use std::ops::Range;
use std::sync::Arc;

use glam::Vec2;
use gpui::{
    anchored, canvas, deferred, div, fill, font, point, prelude::*, px, rgb, rgba, size, App,
    Application, Bounds, Context, Corner, ElementInputHandler, Entity, EntityInputHandler, Font,
    FocusHandle, Focusable, Hsla, KeyBinding, KeyDownEvent, Menu, MenuItem, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, ScrollDelta,
    ScrollWheelEvent, SharedString, Subscription, TextAlign, TextRun, UTF16Selection,
    Window, WindowBounds, WindowOptions, WrappedLine, actions, relative,
};
use parking_lot::RwLock;
use u_forge_core::{AppConfig, KnowledgeGraph, ObjectId};
use u_forge_graph_view::{build_snapshot, GraphSnapshot, LodLevel};
use u_forge_ui_traits::{generate_draw_commands, DrawCommand, Viewport, NODE_RADIUS};

actions!([SaveLayout, ToggleSidebar, ToggleRightPanel, ClearData, ImportData]);

/// Edges per batched PathBuilder.
const EDGE_BATCH_SIZE: usize = 500;

/// Legend entries matching the `type_color` palette in u-forge-ui-traits.
const LEGEND_ENTRIES: &[(&str, u32)] = &[
    ("Character / NPC", 0x89b4fa),
    ("Location", 0xa6e3a1),
    ("Faction", 0xf9e2af),
    ("Quest", 0xf38ba8),
    ("Item / Transport", 0xcba6f7),
    ("Currency", 0x94e2d5),
    ("Other", 0xcdd6f4),
];

/// Menu bar / status bar height in pixels.
const MENU_BAR_H: f32 = 28.0;

/// Status bar height in pixels.
const STATUS_BAR_H: f32 = 24.0;

/// Single-line text field height (px).
const TEXT_FIELD_MIN_H: f32 = 28.0;
/// Maximum text field height before scrolling kicks in (px).
const TEXT_FIELD_MAX_H: f32 = 120.0;
/// Vertical padding inside text fields (px).
const TEXT_FIELD_PAD_Y: f32 = 4.0;

// ── Text field widget ────────────────────────────────────────────────────────

/// Event emitted by `TextFieldView` when its content changes.
struct TextChanged(String);

/// Event emitted by `TextFieldView` when Enter is pressed in a single-line field.
#[allow(dead_code)]
struct TextSubmit(String);

/// Cached shaped text layout from the most recent paint, used for click-to-cursor mapping.
/// One WrappedLine per explicit '\n'-delimited line, paired with the byte offset where
/// that line starts in the content string. All fields use wrapped text for dynamic sizing.
struct TextFieldLayout(Vec<(usize, WrappedLine)>);

/// A minimal editable text field built on GPUI's `EntityInputHandler`.
///
/// Handles basic cursor movement, character insertion (via platform IME),
/// backspace, delete, home/end, and optional multiline editing.
struct TextFieldView {
    content: String,
    /// Cursor position as a UTF-8 byte offset into `content`.
    cursor: usize,
    /// Optional selection range (start..end) in UTF-8 byte offsets.
    selection: Option<Range<usize>>,
    /// IME marked (composing) text range.
    marked_range: Option<Range<usize>>,
    focus: FocusHandle,
    multiline: bool,
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
    /// Scroll offset (in pixels) for when content exceeds the visible area.
    scroll_offset: f32,
}

impl TextFieldView {
    fn new(multiline: bool, placeholder: &str, cx: &mut Context<Self>) -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            selection: None,
            marked_range: None,
            focus: cx.focus_handle(),
            multiline,
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
                    if px(local_y) < y_acc + line_visual_h || *byte_start == lines.last().map(|(b, _)| *b).unwrap_or(0) {
                        let rel_y = px(local_y) - y_acc;
                        let pos = point(px(local_x), rel_y);
                        let idx = wl.closest_index_for_position(pos, line_h)
                            .unwrap_or_else(|i| i);
                        return byte_start + idx;
                    }
                    y_acc += line_visual_h;
                }
                self.content.len()
            }
            None => {
                // No layout yet (first frame); clamp to content length.
                self.content.len().min(
                    (local_x / self.measured_line_h.max(1.0) * 2.0) as usize,
                )
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

    /// Adjust scroll_offset so the cursor at the given y-position (relative to
    /// content top) is visible within `visible_h` pixels.
    fn ensure_cursor_visible(&mut self, cursor_y: f32, visible_h: f32) {
        let line_h = self.measured_line_h;
        let cursor_bottom = cursor_y + line_h;
        if cursor_y < self.scroll_offset {
            self.scroll_offset = cursor_y;
        } else if cursor_bottom > self.scroll_offset + visible_h {
            self.scroll_offset = cursor_bottom - visible_h;
        }
        self.scroll_offset = self.scroll_offset.max(0.0);
    }

    /// Scroll to keep the current cursor visible, using cached layout from the
    /// previous paint frame. Called from key/mouse handlers — NOT from paint.
    fn scroll_to_cursor(&mut self) {
        let (_, cy) = self.cursor_pos_from_layout(self.cursor);
        self.ensure_cursor_visible(f32::from(cy), self.visible_height);
    }

    fn set_content(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.to_string();
        self.cursor = 0;
        self.selection = None;
        self.scroll_offset = 0.0;
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
        self.content
            .replace_range(clamped_start..clamped_end, text);
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
            self.selection = Some(sel_start.min(self.content.len())..sel_end.min(self.content.len()));
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
        Some(Bounds::new(
            Point::new(x, y),
            size(px(8.0), line_h),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        // Convert point (element-local) to text-area-local by subtracting padding.
        let local_x = f32::from(point.x) - 6.0;
        let local_y = f32::from(point.y) - 4.0;
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
                let scroll_px = px(scroll_offset);
                let text_origin = bounds.origin + point(pad_x, pad_y - scroll_px);
                let inner_w = bounds.size.width - pad_x * 2.0;

                // Split content by explicit newlines.
                let raw_lines: Vec<&str> = if display.is_empty() {
                    vec![""]
                } else {
                    display.split('\n').collect()
                };

                let mut cursor_pos: Option<gpui::Point<Pixels>> = None;
                let mut byte_offset: usize = 0;
                let mut y_acc = px(0.0);

                // Always use wrapping (shape_text) so all fields can grow dynamically.
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
                            Some(inner_w),
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
                        gpui::Bounds::new(
                            cursor_origin,
                            size(px(1.5), line_height),
                        ),
                        rgba(0xcdd6f4ff),
                    ));
                }

                // Store measurements for click handling and dynamic sizing.
                // Round content_height to whole pixels to prevent sub-pixel oscillation.
                let lh_f = f32::from(line_height);
                let origin_x = f32::from(bounds.origin.x);
                let origin_y = f32::from(bounds.origin.y);
                let total_content_h = (f32::from(y_acc) + TEXT_FIELD_PAD_Y * 2.0).round();
                let visible_h = (f32::from(bounds.size.height) - TEXT_FIELD_PAD_Y * 2.0).max(0.0);

                paint_entity.update(cx, |this, _cx| {
                    this.field_origin_x = origin_x;
                    this.field_origin_y = origin_y;
                    this.measured_line_h = lh_f.max(1.0);
                    this.content_height = total_content_h;
                    this.visible_height = visible_h;
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

        // Dynamic field height: start at single-line, grow with content, cap at max.
        let dynamic_h = self.content_height.max(TEXT_FIELD_MIN_H).min(TEXT_FIELD_MAX_H);

        let field = div()
            .id(SharedString::from(format!("tf-{}", cx.entity_id().as_u64())))
            .relative()
            .w_full()
            .h(px(dynamic_h))
            .bg(rgb(0x313244))
            .rounded(px(4.0))
            .border_1()
            .border_color(if focused { rgb(0x89b4fa) } else { rgb(0x45475a) })
            .overflow_hidden()
            .track_focus(&self.focus);

        field
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    // Convert window coordinates to text-area-local coordinates.
                    // Subtract element origin and padding, add scroll offset.
                    let local_x = f32::from(event.position.x) - this.field_origin_x - 6.0;
                    let local_y = f32::from(event.position.y) - this.field_origin_y
                        - TEXT_FIELD_PAD_Y + this.scroll_offset;
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
                        if this.multiline {
                            this.delete_selection();
                            this.content.insert(this.cursor, '\n');
                            this.cursor += 1;
                            cx.emit(TextChanged(this.content.clone()));
                            this.scroll_to_cursor();
                            this.reset_blink(cx);
                            cx.notify();
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

// ── Selection model ──────────────────────────────────────────────────────────

/// Shared selection state observed by both TreePanel and GraphCanvas.
/// When either side changes the selection, it calls `cx.notify()` so
/// observers re-render.
struct SelectionModel {
    /// Index into `GraphSnapshot::nodes` of the currently selected node.
    selected_node_idx: Option<usize>,
    /// ObjectId of the currently selected node (kept in sync with idx).
    selected_node_id: Option<ObjectId>,
    /// The shared snapshot — both panels read from this.
    snapshot: Arc<RwLock<GraphSnapshot>>,
}

impl SelectionModel {
    fn new(snapshot: Arc<RwLock<GraphSnapshot>>) -> Self {
        Self {
            selected_node_idx: None,
            selected_node_id: None,
            snapshot,
        }
    }

    /// Select a node by its snapshot index. Called from graph canvas clicks.
    fn select_by_idx(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        self.selected_node_idx = idx;
        self.selected_node_id = idx.map(|i| self.snapshot.read().nodes[i].id);
        cx.notify();
    }

    /// Select a node by ObjectId. Called from tree panel clicks.
    /// Returns the node index if found.
    fn select_by_id(&mut self, id: Option<ObjectId>, cx: &mut Context<Self>) -> Option<usize> {
        if let Some(id) = id {
            let snap = self.snapshot.read();
            let idx = snap.nodes.iter().position(|n| n.id == id);
            drop(snap);
            self.selected_node_idx = idx;
            self.selected_node_id = Some(id);
            cx.notify();
            idx
        } else {
            self.selected_node_idx = None;
            self.selected_node_id = None;
            cx.notify();
            None
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected_node_idx = None;
        self.selected_node_id = None;
        cx.notify();
    }
}

// ── Tree panel ──────────────────────────────────────────────────────────────

/// A group of nodes sharing the same object_type, for the tree panel.
struct TypeGroup {
    type_name: String,
    /// (index into snapshot.nodes, display name)
    entries: Vec<(usize, String, ObjectId)>,
}

/// Sidebar tree view listing all nodes grouped by type, alphabetically.
struct TreePanel {
    selection: Entity<SelectionModel>,
    /// Pre-sorted groups, rebuilt when the snapshot changes.
    groups: Vec<TypeGroup>,
    /// Which type groups are collapsed (by type_name).
    collapsed: std::collections::HashSet<String>,
}

impl TreePanel {
    fn new(
        snapshot: Arc<RwLock<GraphSnapshot>>,
        selection: Entity<SelectionModel>,
    ) -> Self {
        let groups = Self::build_groups(&snapshot.read());
        // Start with all groups collapsed so the tree fits on screen.
        let collapsed: std::collections::HashSet<String> =
            groups.iter().map(|g| g.type_name.clone()).collect();
        Self {
            selection,
            groups,
            collapsed,
        }
    }

    fn build_groups(snap: &GraphSnapshot) -> Vec<TypeGroup> {
        use std::collections::BTreeMap;
        let mut by_type: BTreeMap<String, Vec<(usize, String, ObjectId)>> = BTreeMap::new();
        for (idx, node) in snap.nodes.iter().enumerate() {
            by_type
                .entry(node.object_type.clone())
                .or_default()
                .push((idx, node.name.clone(), node.id));
        }
        let mut groups: Vec<TypeGroup> = by_type
            .into_iter()
            .map(|(type_name, mut entries)| {
                entries.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
                TypeGroup { type_name, entries }
            })
            .collect();
        // BTreeMap already sorts keys, but let's be explicit about case-insensitive sort.
        groups.sort_by(|a, b| a.type_name.to_lowercase().cmp(&b.type_name.to_lowercase()));
        groups
    }
}

/// Color for a type header in the tree panel, matching the node palette.
fn tree_type_color(object_type: &str) -> u32 {
    match object_type {
        "npc" | "character" => 0x89b4fa,
        "location" => 0xa6e3a1,
        "faction" => 0xf9e2af,
        "quest" => 0xf38ba8,
        "item" | "transportation" => 0xcba6f7,
        "currency" => 0x94e2d5,
        _ => 0xcdd6f4,
    }
}

/// Width of the sidebar in pixels.
const SIDEBAR_W: f32 = 220.0;

impl Render for TreePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id = self.selection.read(cx).selected_node_id;

        // Outer shell: fixed width, fills parent height, does not grow vertically.
        let mut panel = div()
            .id("tree-panel")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_W))
            .h_full()
            .min_h_0()
            .bg(rgb(0x181825))
            .border_r_1()
            .border_color(rgb(0x313244));

        // Fixed header
        panel = panel.child(
            div()
                .id("tree-header")
                .flex()
                .items_center()
                .h(px(28.0))
                .px_3()
                .flex_none()
                .border_b_1()
                .border_color(rgb(0x313244))
                .text_color(rgba(0xcdd6f4ff))
                .text_xs()
                .child("NODES"),
        );

        // Scrollable content area that fills remaining height.
        let mut scroll_area = div()
            .id("tree-scroll")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .min_h_0();
        scroll_area.style().flex_grow = Some(1.0);
        scroll_area.style().flex_shrink = Some(1.0);
        scroll_area.style().flex_basis = Some(relative(0.).into());

        // Groups
        for (group_idx, group) in self.groups.iter().enumerate() {
            let type_name = group.type_name.clone();
            let is_collapsed = self.collapsed.contains(&type_name);
            let type_color = tree_type_color(&type_name);
            let count = group.entries.len();
            let collapse_label = format!(
                "{} {} ({})",
                if is_collapsed { "▸" } else { "▾" },
                type_name,
                count
            );
            let type_name_for_click = type_name.clone();

            // Type group header
            scroll_area = scroll_area.child(
                div()
                    .id(("type-group", group_idx))
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .px_2()
                    .flex_none()
                    .text_color(rgb(type_color))
                    .text_xs()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                            if this.collapsed.contains(&type_name_for_click) {
                                this.collapsed.remove(&type_name_for_click);
                            } else {
                                this.collapsed.insert(type_name_for_click.clone());
                            }
                            cx.notify();
                        }),
                    )
                    .child(collapse_label),
            );

            // Node entries (if not collapsed)
            if !is_collapsed {
                for (entry_idx, (_node_idx, name, node_id)) in group.entries.iter().enumerate() {
                    let is_selected = selected_id == Some(*node_id);
                    let node_id = *node_id;
                    let display_name = if name.len() > 26 {
                        let mut s: String = name.chars().take(25).collect();
                        s.push('…');
                        s
                    } else {
                        name.clone()
                    };

                    scroll_area = scroll_area.child(
                        div()
                            .id(("node-entry", group_idx * 10000 + entry_idx))
                            .flex()
                            .items_center()
                            .h(px(22.0))
                            .pl(px(20.0))
                            .pr(px(4.0))
                            .flex_none()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(if is_selected {
                                rgba(0xffffffff)
                            } else {
                                rgba(0xa6adc8ff)
                            })
                            .when(is_selected, |el| el.bg(rgba(0x45475aaa)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                    this.selection.update(cx, |sel, cx| {
                                        sel.select_by_id(Some(node_id), cx);
                                    });
                                }),
                            )
                            .child(display_name),
                    );
                }
            }
        }

        panel.child(scroll_area)
    }
}

// ── Graph canvas ─────────────────────────────────────────────────────────────

struct GraphCanvas {
    snapshot: Arc<RwLock<GraphSnapshot>>,
    graph: Arc<KnowledgeGraph>,
    selection: Entity<SelectionModel>,
    /// Camera center in world space.
    camera: Vec2,
    zoom: f32,
    /// True when the user is panning the canvas (drag on empty space).
    panning: bool,
    /// Index into `snapshot.nodes` of the node being dragged, if any.
    dragging_node: Option<usize>,
    last_mouse: Point<Pixels>,
    /// Canvas bounds in window coordinates, updated each paint frame.
    /// Used to convert window-space mouse positions to canvas-local coordinates.
    canvas_bounds: Arc<RwLock<Bounds<Pixels>>>,
    /// When true, the next selection-model observe callback skips panning
    /// (because the selection originated from a canvas click, not the tree).
    suppress_pan: bool,
    /// Subscription to selection model changes (kept alive).
    _selection_sub: Subscription,
}

impl GraphCanvas {
    fn new(
        snapshot: Arc<RwLock<GraphSnapshot>>,
        graph: Arc<KnowledgeGraph>,
        selection: Entity<SelectionModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        // When the selection model changes, repaint and pan to the new selection
        // (unless the change originated from a canvas click — suppress_pan flag).
        let sel_sub = cx.observe(&selection, |this: &mut GraphCanvas, sel, cx| {
            if this.suppress_pan {
                this.suppress_pan = false;
            } else if let Some(idx) = sel.read(cx).selected_node_idx {
                let pos = this.snapshot.read().nodes[idx].position;
                this.camera = pos;
            }
            cx.notify();
        });
        Self {
            snapshot,
            graph,
            selection,
            camera: Vec2::ZERO,
            zoom: 1.0,
            panning: false,
            dragging_node: None,
            last_mouse: point(px(0.0), px(0.0)),
            canvas_bounds: Arc::new(RwLock::new(Bounds::default())),
            suppress_pan: false,
            _selection_sub: sel_sub,
        }
    }

    fn viewport(&self, canvas_size: Vec2) -> Viewport {
        Viewport {
            center: self.camera,
            size: canvas_size,
            zoom: self.zoom,
        }
    }

    /// Returns (canvas_size, canvas_origin) from the last paint frame.
    fn canvas_metrics(&self) -> (Vec2, Vec2) {
        let b = *self.canvas_bounds.read();
        (
            Vec2::new(f32::from(b.size.width), f32::from(b.size.height)),
            Vec2::new(f32::from(b.origin.x), f32::from(b.origin.y)),
        )
    }

    /// Persist all current node positions to the database.
    fn save_layout(&self) {
        let snap = self.snapshot.read();
        let positions: Vec<(ObjectId, f32, f32)> = snap
            .nodes
            .iter()
            .map(|n| (n.id, n.position.x, n.position.y))
            .collect();
        drop(snap);
        if let Err(e) = self.graph.save_layout(&positions) {
            eprintln!("Warning: failed to save layout: {e}");
        } else {
            eprintln!("Layout saved.");
        }
    }
}

/// Convert DrawCommand color `[u8;4]` → gpui `rgb` u32 (ignores alpha).
fn color_to_rgb(c: [u8; 4]) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32)
}

/// Convert `[u8;4]` RGBA → `Hsla` for use with the text system.
fn color_to_hsla(c: [u8; 4]) -> Hsla {
    Hsla::from(rgba(
        ((c[0] as u32) << 24) | ((c[1] as u32) << 16) | ((c[2] as u32) << 8) | (c[3] as u32),
    ))
}

impl Render for GraphCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom = self.zoom;
        let camera = self.camera;
        let snapshot = self.snapshot.clone();
        let selected_node_idx = self.selection.read(cx).selected_node_idx;
        let canvas_bounds_arc = self.canvas_bounds.clone();

        div()
            .id("graph-root")
            .size_full()
            .overflow_hidden()
            .bg(rgb(0x1e1e2e))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.last_mouse = event.position;

                    let (canvas_size, origin) = this.canvas_metrics();
                    // Convert window-space position to canvas-local.
                    let screen_pos = Vec2::new(
                        f32::from(event.position.x) - origin.x,
                        f32::from(event.position.y) - origin.y,
                    );
                    let world_pos = this.viewport(canvas_size).screen_to_world(screen_pos);
                    let half_size = NODE_RADIUS * 1.5;

                    if let Some(idx) = this.snapshot.read().node_at_point_aabb(world_pos, half_size)
                    {
                        // Selection originated from the canvas — don't pan.
                        this.suppress_pan = true;
                        this.selection.update(cx, |sel, cx| {
                            sel.select_by_idx(Some(idx), cx);
                        });
                        this.dragging_node = Some(idx);
                    } else {
                        this.suppress_pan = true;
                        this.selection.update(cx, |sel, cx| sel.clear(cx));
                        this.panning = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    if this.dragging_node.is_some() {
                        this.snapshot.write().rebuild_spatial_index();
                        this.dragging_node = None;
                        cx.notify();
                    } else {
                        this.panning = false;
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                let delta = event.position - this.last_mouse;
                this.last_mouse = event.position;

                if let Some(node_idx) = this.dragging_node {
                    let world_delta = Vec2::new(
                        f32::from(delta.x) / this.zoom,
                        f32::from(delta.y) / this.zoom,
                    );
                    this.snapshot.write().nodes[node_idx].position += world_delta;
                    cx.notify();
                } else if this.panning {
                    this.camera.x -= f32::from(delta.x) / this.zoom;
                    this.camera.y -= f32::from(delta.y) / this.zoom;
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let factor = match event.delta {
                    ScrollDelta::Pixels(delta) => 1.0 + f32::from(delta.y) * 0.002,
                    ScrollDelta::Lines(delta) => 1.0 + delta.y * 0.1,
                };
                let (canvas_size, origin) = this.canvas_metrics();
                // Convert window-space position to canvas-local.
                let mouse_screen = Vec2::new(
                    f32::from(event.position.x) - origin.x,
                    f32::from(event.position.y) - origin.y,
                );
                let vp = this.viewport(canvas_size);
                let world_under_mouse = vp.screen_to_world(mouse_screen);

                this.zoom = (this.zoom * factor).clamp(0.05, 20.0);

                let new_vp = this.viewport(canvas_size);
                let new_screen = new_vp.world_to_screen(world_under_mouse);
                let screen_delta = mouse_screen - new_screen;
                this.camera.x -= screen_delta.x / this.zoom;
                this.camera.y -= screen_delta.y / this.zoom;

                cx.notify();
            }))
            .child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |bounds, (), window, cx| {
                        // Record bounds so event handlers can convert window → canvas coords.
                        *canvas_bounds_arc.write() = bounds;

                        window.paint_quad(fill(bounds, rgb(0x1e1e2e)));

                        let canvas_size =
                            Vec2::new(f32::from(bounds.size.width), f32::from(bounds.size.height));
                        // Offset added to every canvas-local position to get window coordinates.
                        let ox = f32::from(bounds.origin.x);
                        let oy = f32::from(bounds.origin.y);

                        let viewport = Viewport {
                            center: camera,
                            size: canvas_size,
                            zoom,
                        };

                        let snap = snapshot.read();
                        let commands = generate_draw_commands(&snap, &viewport, selected_node_idx);
                        let lod = viewport.lod_level();
                        drop(snap);

                        // ── Edges (batched paths) ────────────────────────────────────
                        let edge_commands: Vec<&DrawCommand> = commands
                            .iter()
                            .filter(|c| matches!(c, DrawCommand::Line { .. }))
                            .collect();

                        if !edge_commands.is_empty() {
                            let mut builder = PathBuilder::stroke(px(1.0));
                            let mut count = 0;
                            let edge_color = rgb(0x585b70);

                            for cmd in &edge_commands {
                                if let DrawCommand::Line { from, to, .. } = cmd {
                                    builder.move_to(point(px(from.x + ox), px(from.y + oy)));
                                    builder.line_to(point(px(to.x + ox), px(to.y + oy)));
                                    count += 1;

                                    if count >= EDGE_BATCH_SIZE {
                                        if let Ok(path) = builder.build() {
                                            window.paint_path(path, edge_color);
                                        }
                                        builder = PathBuilder::stroke(px(1.0));
                                        count = 0;
                                    }
                                }
                            }
                            if count > 0 {
                                if let Ok(path) = builder.build() {
                                    window.paint_path(path, edge_color);
                                }
                            }
                        }

                        // ── Nodes (squircles) ────────────────────────────────────────
                        let use_dots = lod == LodLevel::Dot;
                        for cmd in &commands {
                            if let DrawCommand::Circle {
                                center,
                                radius,
                                color,
                                selected,
                            } = cmd
                            {
                                let r = if use_dots {
                                    px(3.0)
                                } else {
                                    px(*radius * zoom)
                                };
                                let c = point(px(center.x + ox), px(center.y + oy));
                                let node_bounds =
                                    Bounds::new(point(c.x - r, c.y - r), size(r * 2.0, r * 2.0));
                                let col = color_to_rgb(*color);

                                if use_dots {
                                    window.paint_quad(fill(node_bounds, rgb(col)));
                                } else {
                                    let sq_radii = r * 0.6;

                                    if *selected {
                                        let hr = r * 1.45;
                                        let ring_bounds = Bounds::new(
                                            point(c.x - hr, c.y - hr),
                                            size(hr * 2.0, hr * 2.0),
                                        );
                                        window.paint_quad(
                                            fill(ring_bounds, rgb(0xffffff)).corner_radii(hr * 0.6),
                                        );
                                    }

                                    window.paint_quad(
                                        fill(node_bounds, rgb(col)).corner_radii(sq_radii),
                                    );
                                }
                            }
                        }

                        // ── Node labels (inside squircles) ───────────────────────────
                        let text_system = window.text_system().clone();
                        let sys_font: Font = font(".SystemUIFont");
                        for cmd in &commands {
                            if let DrawCommand::Text {
                                position,
                                content,
                                size,
                                color,
                            } = cmd
                            {
                                if content.is_empty() {
                                    continue;
                                }
                                let font_size = px(*size);
                                let text_color = color_to_hsla(*color);
                                let run = TextRun {
                                    len: content.len(),
                                    font: sys_font.clone(),
                                    color: text_color,
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                let shaped = text_system.shape_line(
                                    SharedString::from(content.clone()),
                                    font_size,
                                    &[run],
                                    None,
                                );
                                let line_height = font_size * 1.2;
                                let tx = position.x + ox - f32::from(shaped.width) / 2.0;
                                let ty = position.y + oy - f32::from(line_height) / 2.0;
                                let _ =
                                    shaped.paint(point(px(tx), px(ty)), line_height, window, cx);
                            }
                        }

                        // ── Color legend (bottom-right of canvas pane) ───────────────
                        {
                            let entry_h = 20.0_f32;
                            let swatch = 12.0_f32;
                            let pad = 8.0_f32;
                            let legend_w = 155.0_f32;
                            let legend_h = LEGEND_ENTRIES.len() as f32 * entry_h + pad * 2.0;
                            // Canvas-local bottom-right, then shifted to window coords.
                            let lx = canvas_size.x - legend_w - pad + ox;
                            let ly = canvas_size.y - legend_h - pad + oy;

                            window.paint_quad(fill(
                                Bounds::new(
                                    point(px(lx), px(ly)),
                                    size(px(legend_w), px(legend_h)),
                                ),
                                rgba(0x1e1e2ed8),
                            ));

                            let label_size = px(10.0);
                            let line_h = label_size * 1.3;

                            for (i, (label, color_hex)) in LEGEND_ENTRIES.iter().enumerate() {
                                let row_y = ly + pad + i as f32 * entry_h;
                                let center_y = row_y + entry_h / 2.0;

                                window.paint_quad(
                                    fill(
                                        Bounds::new(
                                            point(px(lx + pad), px(center_y - swatch / 2.0)),
                                            size(px(swatch), px(swatch)),
                                        ),
                                        rgb(*color_hex),
                                    )
                                    .corner_radii(px(3.0)),
                                );

                                let run = TextRun {
                                    len: label.len(),
                                    font: sys_font.clone(),
                                    color: Hsla::from(rgba(0xcdd6f4ff)),
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                let shaped = text_system.shape_line(
                                    SharedString::from(*label),
                                    label_size,
                                    &[run],
                                    None,
                                );
                                let text_x = lx + pad + swatch + 6.0;
                                let text_y = center_y - f32::from(line_h) / 2.0;
                                let _ =
                                    shaped.paint(point(px(text_x), px(text_y)), line_h, window, cx);
                            }
                        }
                    },
                )
                .size_full(),
            )
    }
}

// ── Node editor panel ─────────────────────────────────────────────────────────

/// Height of the tab bar inside the editor panel.
const DETAIL_TAB_H: f32 = 28.0;

/// Target column width for the multi-column form layout.
const COLUMN_W: f32 = 300.0;

/// Height of a single-line form field (label + input + gap).
const FIELD_H_SINGLE: f32 = 52.0;

/// Height of a multiline form field (label + textarea + gap).
const FIELD_H_MULTI: f32 = 104.0;

/// Space reserved for page navigation buttons.
const PAGE_NAV_H: f32 = 32.0;

use std::collections::HashMap;
use u_forge_core::{ObjectTypeSchema, PropertyType, SchemaManager};

/// Describes a single form field for rendering.
struct FieldSpec {
    key: String,
    label: String,
    required: bool,
    multiline: bool,
    field_kind: FieldKind,
}

enum FieldKind {
    Text,
    Number,
    Boolean,
    Enum(Vec<String>),
    Array,
}

impl FieldSpec {
    fn height(&self) -> f32 {
        match self.field_kind {
            FieldKind::Boolean => FIELD_H_SINGLE,
            FieldKind::Array => FIELD_H_MULTI,
            // Text fields use FIELD_H_SINGLE for layout estimation — actual height
            // is dynamic (the TextFieldView grows with content up to TEXT_FIELD_MAX_H).
            _ => FIELD_H_SINGLE,
        }
    }
}

/// Compare two JSON values for equality, treating string representations of
/// numbers/booleans as equal to their typed counterparts. This is needed because
/// the TextChanged handler always produces `Value::String`, but the original
/// properties may store `Value::Number` or `Value::Bool`.
fn values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    if a == b {
        return true;
    }
    // Compare by string representation: if both render to the same text, treat as equal.
    let a_str = match a {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let b_str = match b {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    a_str == b_str
}

/// A single editor tab representing one node being edited.
struct EditorTab {
    node_id: ObjectId,
    name: String,
    #[allow(dead_code)]
    object_type: String,
    pinned: bool,
    original: u_forge_core::ObjectMetadata,
    edited_values: HashMap<String, serde_json::Value>,
    schema: Option<ObjectTypeSchema>,
    dirty: bool,
    current_page: usize,
    /// Text field entities for the form — keyed by field name.
    field_entities: HashMap<String, Entity<TextFieldView>>,
}

impl EditorTab {
    /// Build the ordered list of field specs from the schema + edited values.
    fn field_specs(&self) -> Vec<FieldSpec> {
        let mut specs = Vec::new();

        // 1. name — always first
        specs.push(FieldSpec {
            key: "name".into(),
            label: "Name".into(),
            required: true,
            multiline: false,
            field_kind: FieldKind::Text,
        });

        // 2. description — always second
        specs.push(FieldSpec {
            key: "description".into(),
            label: "Description".into(),
            required: false,
            multiline: true,
            field_kind: FieldKind::Text,
        });

        if let Some(schema) = &self.schema {
            // Collect required keys (excluding name/description/tags handled separately)
            let skip = ["name", "description", "tags"];
            let mut required_keys: Vec<&String> = schema
                .required_properties
                .iter()
                .filter(|k| !skip.contains(&k.as_str()))
                .collect();
            required_keys.sort();

            // Collect optional keys
            let mut optional_keys: Vec<&String> = schema
                .properties
                .keys()
                .filter(|k| {
                    !skip.contains(&k.as_str())
                        && !schema.required_properties.contains(k)
                })
                .collect();
            optional_keys.sort();

            for key in required_keys.iter().chain(optional_keys.iter()) {
                if let Some(prop) = schema.properties.get(*key) {
                    let (kind, multiline) = match &prop.property_type {
                        PropertyType::Text => (FieldKind::Text, true),
                        PropertyType::String | PropertyType::Reference(_) => {
                            (FieldKind::Text, false)
                        }
                        PropertyType::Number => (FieldKind::Number, false),
                        PropertyType::Boolean => (FieldKind::Boolean, false),
                        PropertyType::Enum(vals) => {
                            (FieldKind::Enum(vals.clone()), false)
                        }
                        PropertyType::Array(_) => (FieldKind::Array, false),
                        PropertyType::Object(_) => (FieldKind::Text, true),
                    };
                    specs.push(FieldSpec {
                        key: (*key).clone(),
                        label: key.replace('_', " "),
                        required: schema.required_properties.contains(key),
                        multiline,
                        field_kind: kind,
                    });
                }
            }

            // Extra properties not in schema
            for key in self.edited_values.keys() {
                if skip.contains(&key.as_str()) {
                    continue;
                }
                if schema.properties.contains_key(key) {
                    continue;
                }
                specs.push(FieldSpec {
                    key: key.clone(),
                    label: key.replace('_', " "),
                    required: false,
                    multiline: false,
                    field_kind: FieldKind::Text,
                });
            }
        } else {
            // No schema — render all edited_values as text fields
            let mut keys: Vec<&String> = self
                .edited_values
                .keys()
                .filter(|k| !["name", "description", "tags"].contains(&k.as_str()))
                .collect();
            keys.sort();
            for key in keys {
                specs.push(FieldSpec {
                    key: key.clone(),
                    label: key.replace('_', " "),
                    required: false,
                    multiline: false,
                    field_kind: FieldKind::Text,
                });
            }
        }

        // tags — always last
        specs.push(FieldSpec {
            key: "tags".into(),
            label: "Tags".into(),
            required: false,
            multiline: false,
            field_kind: FieldKind::Array,
        });

        specs
    }

    /// Recompute the dirty flag by comparing edited_values against original.
    fn recompute_dirty(&mut self) {
        let orig = &self.original;
        let vals = &self.edited_values;

        let name_changed =
            vals.get("name").and_then(|v| v.as_str()) != Some(orig.name.as_str());

        // Treat empty string the same as None for description comparison.
        let edited_desc = vals
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let orig_desc = orig.description.as_deref().filter(|s| !s.is_empty());
        let desc_changed = edited_desc != orig_desc;

        let mut props_changed = false;
        if let Some(orig_obj) = orig.properties.as_object() {
            for (k, v) in vals.iter() {
                if k == "name" || k == "description" || k == "tags" {
                    continue;
                }
                // Treat empty string as equivalent to missing/null.
                let edited_empty = v.as_str().is_some_and(|s| s.is_empty())
                    || v.is_null();
                match orig_obj.get(k) {
                    Some(orig_v) if values_equal(orig_v, v) => {}
                    None | Some(&serde_json::Value::Null) if edited_empty => {}
                    _ => {
                        props_changed = true;
                        break;
                    }
                }
            }
            // Check if original has keys not present in edited_values
            // (skip keys whose original value is null/empty — they match absence).
            if !props_changed {
                for (k, v) in orig_obj.iter() {
                    if vals.contains_key(k) {
                        continue;
                    }
                    let orig_empty = v.as_str().is_some_and(|s| s.is_empty())
                        || v.is_null();
                    if !orig_empty {
                        props_changed = true;
                        break;
                    }
                }
            }
        }

        let tags_changed = {
            let edited_tags: Vec<String> = vals
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            edited_tags != orig.tags
        };

        self.dirty = name_changed || desc_changed || props_changed || tags_changed;
    }
}

/// Editor panel with browser-style tabs for editing nodes.
///
/// Observes `SelectionModel` and opens tabs as nodes are selected.
struct NodeEditorPanel {
    tabs: Vec<EditorTab>,
    active_tab: Option<usize>,
    #[allow(dead_code)]
    selection: Entity<SelectionModel>,
    #[allow(dead_code)]
    snapshot: Arc<RwLock<GraphSnapshot>>,
    graph: Arc<KnowledgeGraph>,
    schema_mgr: Arc<SchemaManager>,
    /// Open dropdown field key (for enum fields).
    dropdown_open: Option<String>,
    /// Measured panel size in pixels, updated each frame via canvas measurement.
    panel_size: gpui::Size<Pixels>,
    /// Subscriptions to text field changes — kept alive so events fire.
    _field_subs: Vec<Subscription>,
    _selection_sub: Subscription,
    /// Active inline-add text field for array fields: (field_key, entity, subscription).
    array_add_field: Option<(String, Entity<TextFieldView>, Subscription)>,
}

impl NodeEditorPanel {
    fn new(
        snapshot: Arc<RwLock<GraphSnapshot>>,
        selection: Entity<SelectionModel>,
        graph: Arc<KnowledgeGraph>,
        schema_mgr: Arc<SchemaManager>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&selection, |this: &mut Self, sel, cx| {
            let selected_id = sel.read(cx).selected_node_id;
            if let Some(node_id) = selected_id {
                this.array_add_field = None;
                this.open_or_focus_tab(node_id, cx);
            }
            cx.notify();
        });
        Self {
            tabs: Vec::new(),
            active_tab: None,
            selection,
            snapshot,
            graph,
            schema_mgr,
            dropdown_open: None,
            panel_size: gpui::Size {
                width: px(900.0),
                height: px(400.0),
            },
            _field_subs: Vec::new(),
            _selection_sub: sub,
            array_add_field: None,
        }
    }

    /// Open a tab for the given node, or focus the existing one.
    fn open_or_focus_tab(&mut self, node_id: ObjectId, cx: &mut Context<Self>) {
        // Already open?
        if let Some(idx) = self.tabs.iter().position(|t| t.node_id == node_id) {
            self.active_tab = Some(idx);
            return;
        }

        // Load the node from DB.
        let meta = match self.graph.get_object(node_id) {
            Ok(Some(m)) => m,
            _ => return,
        };

        // Load schema for this object type.
        let schema = self
            .schema_mgr
            .get_object_type_schema("default", &meta.object_type);

        // Build edited_values from the metadata.
        let mut edited_values = HashMap::new();
        edited_values.insert(
            "name".to_string(),
            serde_json::Value::String(meta.name.clone()),
        );
        edited_values.insert(
            "description".to_string(),
            serde_json::Value::String(
                meta.description.clone().unwrap_or_default(),
            ),
        );
        if let Some(obj) = meta.properties.as_object() {
            for (k, v) in obj {
                if k.eq_ignore_ascii_case("description") || k.eq_ignore_ascii_case("tags") {
                    continue;
                }
                edited_values.insert(k.clone(), v.clone());
            }
        }
        edited_values.insert(
            "tags".to_string(),
            serde_json::Value::Array(
                meta.tags.iter().map(|t| serde_json::Value::String(t.clone())).collect(),
            ),
        );

        // Create text field entities for this tab.
        let mut field_entities = HashMap::new();
        let tmp_tab = EditorTab {
            node_id,
            name: meta.name.clone(),
            object_type: meta.object_type.clone(),
            pinned: false,
            original: meta.clone(),
            edited_values: edited_values.clone(),
            schema: schema.clone(),
            dirty: false,
            current_page: 0,
            field_entities: HashMap::new(),
        };
        let specs = tmp_tab.field_specs();
        for spec in &specs {
            match spec.field_kind {
                FieldKind::Text | FieldKind::Number => {
                    let multiline = spec.multiline;
                    let placeholder = spec.label.clone();
                    let key = spec.key.clone();
                    let entity = cx.new(|cx| {
                        let mut tf = TextFieldView::new(multiline, &placeholder, cx);
                        let val = edited_values
                            .get(&key)
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        tf.set_content(val, cx);
                        tf
                    });
                    field_entities.insert(spec.key.clone(), entity);
                }
                FieldKind::Enum(_) => {
                    // Enum uses a text display + dropdown, so we need a text field
                    // to show the current value (read-only style, but clickable).
                    let key = spec.key.clone();
                    let entity = cx.new(|cx| {
                        let mut tf = TextFieldView::new(false, &spec.label, cx);
                        let val = edited_values
                            .get(&key)
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        tf.set_content(val, cx);
                        tf
                    });
                    field_entities.insert(spec.key.clone(), entity);
                }
                _ => {}
            }
        }

        // Subscribe to text changes from each field. Keep subscriptions alive.
        self._field_subs.clear();
        for (key, entity) in &field_entities {
            let key = key.clone();
            let sub = cx.subscribe(entity, move |this: &mut Self, _tf, event: &TextChanged, cx| {
                if let Some(tab_idx) = this.active_tab {
                    if let Some(tab) = this.tabs.get_mut(tab_idx) {
                        tab.edited_values.insert(key.clone(), serde_json::Value::String(event.0.clone()));
                        if key == "name" {
                            tab.name = event.0.clone();
                        }
                        tab.recompute_dirty();
                        cx.notify();
                    }
                }
            });
            self._field_subs.push(sub);
        }

        let new_tab = EditorTab {
            node_id,
            name: meta.name.clone(),
            object_type: meta.object_type.clone(),
            pinned: false,
            original: meta,
            edited_values,
            schema,
            dirty: false,
            current_page: 0,
            field_entities,
        };

        // Replace the first unpinned tab, or append.
        if let Some(idx) = self.tabs.iter().position(|t| !t.pinned) {
            self.tabs[idx] = new_tab;
            self.active_tab = Some(idx);
        } else {
            self.tabs.push(new_tab);
            self.active_tab = Some(self.tabs.len() - 1);
        }
    }

    /// Close a tab by index.
    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.active_tab = None;
        } else if let Some(active) = self.active_tab {
            if active >= self.tabs.len() {
                self.active_tab = Some(self.tabs.len() - 1);
            } else if active > idx {
                self.active_tab = Some(active - 1);
            }
        }
    }

    /// Collect dirty tabs and save them to the DB. Returns count of saved nodes.
    fn save_dirty_tabs(&mut self) -> usize {
        let mut saved = 0;
        for tab in &mut self.tabs {
            if !tab.dirty {
                continue;
            }
            let mut meta = tab.original.clone();
            meta.name = tab
                .edited_values
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&meta.name)
                .to_string();
            meta.description = tab
                .edited_values
                .get("description")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);

            // Rebuild properties JSON.
            let mut props = serde_json::Map::new();
            for (k, v) in &tab.edited_values {
                if ["name", "description", "tags"].contains(&k.as_str()) {
                    continue;
                }
                props.insert(k.clone(), v.clone());
            }
            meta.properties = serde_json::Value::Object(props);

            // Tags.
            if let Some(tags_val) = tab.edited_values.get("tags") {
                if let Some(arr) = tags_val.as_array() {
                    meta.tags = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                }
            }

            if self.graph.update_object(meta.clone()).is_ok() {
                tab.original = meta;
                tab.dirty = false;
                saved += 1;
            }
        }
        saved
    }

    /// Return true if any tab has unsaved changes.
    #[allow(dead_code)]
    fn has_dirty_tabs(&self) -> bool {
        self.tabs.iter().any(|t| t.dirty)
    }

    /// Commit the inline array-add text field: push its content into the array
    /// and close the inline editor.
    fn commit_array_add(&mut self, cx: &mut Context<Self>) {
        if let Some((key, entity, _sub)) = self.array_add_field.take() {
            let text = entity.read(cx).content.trim().to_string();
            if !text.is_empty() {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let arr = tab
                            .edited_values
                            .entry(key)
                            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                        if let Some(a) = arr.as_array_mut() {
                            a.push(serde_json::Value::String(text));
                        }
                        tab.recompute_dirty();
                    }
                }
            }
            cx.notify();
        }
    }
}

impl Render for NodeEditorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Measure panel size each frame so column layout adapts to window resizes.
        let entity_for_measure = cx.entity().clone();
        let measure_canvas = canvas(
            |_, _, _| {},
            move |bounds, (), _window, cx| {
                entity_for_measure.update(cx, |this, _cx| {
                    this.panel_size = bounds.size;
                });
            },
        )
        .w_full()
        .h_full()
        .absolute();

        let outer = div()
            .id("node-editor-panel")
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(0x1e1e2e))
            .border_b_1()
            .border_color(rgb(0x313244));

        if self.tabs.is_empty() {
            return outer
                .child(measure_canvas)
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgba(0x6c7086ff))
                        .child("Select a node to view details"),
                );
        }

        let active_idx = self.active_tab.unwrap_or(0);

        // ── Tab bar ──────────────────────────────────────────────────────────
        let mut tab_bar = div()
            .id("editor-tab-bar")
            .flex()
            .flex_row()
            .flex_none()
            .h(px(DETAIL_TAB_H))
            .overflow_x_scroll()
            .bg(rgb(0x181825))
            .border_b_1()
            .border_color(rgb(0x313244));

        for (i, tab) in self.tabs.iter().enumerate() {
            let is_active = i == active_idx;
            let is_dirty = tab.dirty;
            let is_pinned = tab.pinned;
            let tab_name: SharedString = tab.name.clone().into();

            let accent_color = if is_dirty {
                rgb(0xfab387) // Catppuccin peach (orange) for dirty
            } else {
                rgb(0x89b4fa) // Catppuccin blue for clean
            };

            let mut tab_el = div()
                .id(("editor-tab", i))
                .flex()
                .flex_row()
                .items_center()
                .flex_none()
                .h_full()
                .px(px(8.0))
                .gap(px(4.0))
                .text_xs()
                .cursor_pointer()
                .text_color(if is_active {
                    rgba(0xcdd6f4ff)
                } else {
                    rgba(0xa6adc8ff)
                })
                .bg(if is_active { rgb(0x1e1e2e) } else { rgb(0x181825) });

            if is_active {
                tab_el = tab_el.border_b_2().border_color(accent_color);
            }

            // Pin indicator
            let pin_label: SharedString = if is_pinned { "P".into() } else { "o".into() };
            let pin_btn = div()
                .id(("tab-pin", i))
                .text_xs()
                .text_color(if is_pinned {
                    rgba(0xf9e2afff)
                } else {
                    rgba(0x6c7086ff)
                })
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        if let Some(tab) = this.tabs.get_mut(i) {
                            tab.pinned = !tab.pinned;
                        }
                        cx.notify();
                    }),
                )
                .child(pin_label);

            // Tab name — click to activate
            let name_el = div()
                .id(("tab-name", i))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        this.active_tab = Some(i);
                        cx.notify();
                    }),
                )
                .child(tab_name);

            // Close button
            let close_btn = div()
                .id(("tab-close", i))
                .text_xs()
                .text_color(rgba(0x6c7086ff))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        this.close_tab(i);
                        cx.notify();
                    }),
                )
                .child("x");

            tab_el = tab_el.child(pin_btn).child(name_el).child(close_btn);

            // Dirty indicator dot
            if is_dirty {
                tab_el = tab_el.child(
                    div()
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded(px(3.0))
                        .bg(rgb(0xfab387)),
                );
            }

            tab_bar = tab_bar.child(tab_el);
        }

        // ── Form content for active tab ──────────────────────────────────────
        if active_idx >= self.tabs.len() {
            return outer.child(measure_canvas).child(tab_bar);
        }

        let tab = &self.tabs[active_idx];
        let specs = tab.field_specs();

        // Compute column/page layout using measured panel dimensions.
        let panel_w = f32::from(self.panel_size.width);
        let panel_h = f32::from(self.panel_size.height);
        let available_h = (panel_h - DETAIL_TAB_H - PAGE_NAV_H).max(100.0);
        let max_cols = ((panel_w / COLUMN_W) as usize).max(1);

        // Greedy column fill to determine how many fields fit per page.
        let mut pages: Vec<Vec<Vec<usize>>> = Vec::new(); // pages[page][col][field_idx]
        let mut current_page: Vec<Vec<usize>> = vec![Vec::new()];
        let mut col_h = 0.0_f32;

        for (fi, spec) in specs.iter().enumerate() {
            let fh = spec.height();
            if col_h + fh > available_h && !current_page.last().unwrap().is_empty() {
                // Start a new column.
                if current_page.len() < max_cols {
                    current_page.push(Vec::new());
                    col_h = 0.0;
                } else {
                    // Start a new page.
                    pages.push(current_page);
                    current_page = vec![Vec::new()];
                    col_h = 0.0;
                }
            }
            current_page.last_mut().unwrap().push(fi);
            col_h += fh;
        }
        if !current_page.iter().all(|c| c.is_empty()) {
            pages.push(current_page);
        }

        let total_pages = pages.len();
        let current_page_idx = tab.current_page.min(total_pages.saturating_sub(1));
        let page_cols = pages.get(current_page_idx).cloned().unwrap_or_default();

        // Build the form columns.
        let mut columns_div = div()
            .id("form-columns")
            .flex()
            .flex_row()
            .gap(px(12.0))
            .p_3();
        columns_div.style().flex_grow = Some(1.0);
        columns_div.style().flex_shrink = Some(1.0);
        columns_div.style().flex_basis = Some(relative(0.).into());

        let dropdown_open = self.dropdown_open.clone();

        for (ci, col_fields) in page_cols.iter().enumerate() {
            let mut col = div()
                .id(("form-col", ci))
                .flex()
                .flex_col()
                .min_w(px(200.0))
                .gap(px(8.0));
            col.style().flex_grow = Some(1.0);
            col.style().flex_shrink = Some(1.0);
            col.style().flex_basis = Some(relative(0.).into());

            for &fi in col_fields {
                let spec = &specs[fi];
                let value = tab.edited_values.get(&spec.key);

                // Label
                let label_text = if spec.required {
                    format!("{} *", spec.label)
                } else {
                    spec.label.clone()
                };

                let label = div()
                    .text_xs()
                    .text_color(rgba(0xa6adc8ff))
                    .child(label_text);

                // Widget
                let widget: gpui::AnyElement = match &spec.field_kind {
                    FieldKind::Boolean => {
                        let checked = value
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let key = spec.key.clone();
                        div()
                            .id(SharedString::from(format!("bool-{}", spec.key)))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .h(px(28.0))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                    if let Some(tab_idx) = this.active_tab {
                                        if let Some(t) = this.tabs.get_mut(tab_idx) {
                                            let cur = t.edited_values
                                                .get(&key)
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            t.edited_values.insert(
                                                key.clone(),
                                                serde_json::Value::Bool(!cur),
                                            );
                                            t.recompute_dirty();
                                        }
                                    }
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .rounded(px(3.0))
                                    .border_1()
                                    .border_color(rgb(0x45475a))
                                    .bg(if checked { rgb(0x89b4fa) } else { rgb(0x313244) })
                                    .when(checked, |el| {
                                        el.child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(0x1e1e2e))
                                                .child("v"),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgba(0xcdd6f4ff))
                                    .child(if checked { "true" } else { "false" }),
                            )
                            .into_any_element()
                    }
                    FieldKind::Enum(values) => {
                        let current_val: SharedString = value
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                            .into();
                        let key = spec.key.clone();
                        let is_open = dropdown_open.as_deref() == Some(&spec.key);

                        let mut enum_div = div()
                            .id(SharedString::from(format!("enum-{}", spec.key)))
                            .flex()
                            .flex_col()
                            .w_full()
                            .relative();

                        // Current value button
                        let select_btn = div()
                            .id(SharedString::from(format!("enum-btn-{}", spec.key)))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .h(px(28.0))
                            .px(px(6.0))
                            .bg(rgb(0x313244))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(rgb(0x45475a))
                            .text_xs()
                            .text_color(rgba(0xcdd6f4ff))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let key = key.clone();
                                    move |this, _: &MouseDownEvent, _window, cx| {
                                        if this.dropdown_open.as_deref() == Some(&key) {
                                            this.dropdown_open = None;
                                        } else {
                                            this.dropdown_open = Some(key.clone());
                                        }
                                        cx.notify();
                                    }
                                }),
                            )
                            .child(current_val)
                            .child(div().text_xs().text_color(rgba(0x6c7086ff)).child("v"));

                        enum_div = enum_div.child(select_btn);

                        // Dropdown overlay
                        if is_open {
                            let mut dropdown = div()
                                .id(SharedString::from(format!("enum-drop-{}", spec.key)))
                                .absolute()
                                .top(px(30.0))
                                .left_0()
                                .w_full()
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                .rounded(px(4.0))
                                .overflow_y_scroll()
                                .max_h(px(150.0));

                            for val in values {
                                let val_str = val.clone();
                                let key_inner = key.clone();
                                let label: SharedString = val.clone().into();
                                dropdown = dropdown.child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "enum-opt-{}-{}",
                                            spec.key, val
                                        )))
                                        .flex()
                                        .items_center()
                                        .h(px(24.0))
                                        .px(px(6.0))
                                        .text_xs()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .cursor_pointer()
                                        .hover(|style| {
                                            style.bg(rgba(0x45475a88))
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                                if let Some(tab_idx) = this.active_tab {
                                                    if let Some(t) = this.tabs.get_mut(tab_idx) {
                                                        t.edited_values.insert(
                                                            key_inner.clone(),
                                                            serde_json::Value::String(val_str.clone()),
                                                        );
                                                        // Also update the text field if it exists
                                                        if let Some(tf) = t.field_entities.get(&key_inner) {
                                                            tf.update(cx, |tf, cx| {
                                                                tf.set_content(&val_str, cx);
                                                            });
                                                        }
                                                        t.recompute_dirty();
                                                    }
                                                }
                                                this.dropdown_open = None;
                                                cx.notify();
                                            }),
                                        )
                                        .child(label),
                                );
                            }

                            enum_div = enum_div.child(dropdown);
                        }

                        enum_div.into_any_element()
                    }
                    FieldKind::Array => {
                        // Render array items as comma-separated tags with an add field.
                        let items: Vec<String> = value
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let key = spec.key.clone();

                        let mut array_div = div()
                            .id(SharedString::from(format!("arr-{}", spec.key)))
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(4.0))
                            .min_h(px(28.0));

                        for (item_idx, item) in items.iter().enumerate() {
                            let item_label: SharedString = item.clone().into();
                            let key_rm = key.clone();
                            array_div = array_div.child(
                                div()
                                    .id(SharedString::from(format!(
                                        "arr-item-{}-{}",
                                        spec.key, item_idx
                                    )))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(2.0))
                                    .px(px(6.0))
                                    .h(px(22.0))
                                    .bg(rgb(0x45475a))
                                    .rounded(px(3.0))
                                    .text_xs()
                                    .text_color(rgba(0xcdd6f4ff))
                                    .child(item_label)
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "arr-rm-{}-{}",
                                                spec.key, item_idx
                                            )))
                                            .text_xs()
                                            .text_color(rgba(0xf38ba8ff))
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    move |this, _: &MouseDownEvent, _window, cx| {
                                                        if let Some(tab_idx) = this.active_tab {
                                                            if let Some(t) =
                                                                this.tabs.get_mut(tab_idx)
                                                            {
                                                                if let Some(arr) = t
                                                                    .edited_values
                                                                    .get_mut(&key_rm)
                                                                    .and_then(|v| v.as_array_mut())
                                                                {
                                                                    if item_idx < arr.len() {
                                                                        arr.remove(item_idx);
                                                                    }
                                                                }
                                                                t.recompute_dirty();
                                                            }
                                                        }
                                                        cx.notify();
                                                    },
                                                ),
                                            )
                                            .child("x"),
                                    ),
                            );
                        }

                        // Inline add: show text field if active for this key,
                        // otherwise show the "+" button.
                        let is_adding = self.array_add_field.as_ref()
                            .is_some_and(|(k, _, _)| k == &key);

                        if is_adding {
                            let (_, ref add_entity, _) = self.array_add_field.as_ref().unwrap();
                            array_div = array_div.child(
                                div()
                                    .id(SharedString::from(format!("arr-adding-{}", spec.key)))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .w(px(100.0))
                                            .child(add_entity.clone()),
                                    ),
                            );
                        } else {
                            let key_add = key.clone();
                            array_div = array_div.child(
                                div()
                                    .id(SharedString::from(format!("arr-add-{}", spec.key)))
                                    .flex()
                                    .items_center()
                                    .px(px(6.0))
                                    .h(px(22.0))
                                    .bg(rgba(0x89b4fa33))
                                    .rounded(px(3.0))
                                    .text_xs()
                                    .text_color(rgb(0x89b4fa))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                            // Create an inline text field for adding a new item.
                                            let k = key_add.clone();
                                            let entity = cx.new(|cx| {
                                                TextFieldView::new(false, "new item", cx)
                                            });
                                            window.focus(&entity.read(cx).focus);
                                            // Subscribe: on Enter, commit the value.
                                            let sub = cx.subscribe(&entity, {
                                                move |this: &mut Self, _tf, _event: &TextSubmit, cx| {
                                                    this.commit_array_add(cx);
                                                }
                                            });
                                            this.array_add_field = Some((k, entity, sub));
                                            cx.notify();
                                        }),
                                    )
                                    .child("+"),
                            );
                        }

                        array_div.into_any_element()
                    }
                    _ => {
                        // Text / Number — use the TextFieldView entity.
                        if let Some(entity) = tab.field_entities.get(&spec.key) {
                            div().child(entity.clone()).into_any_element()
                        } else {
                            // Fallback: display as static text.
                            let display: SharedString = value
                                .map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .unwrap_or_default()
                                .into();
                            div()
                                .h(px(28.0))
                                .px(px(6.0))
                                .bg(rgb(0x313244))
                                .rounded(px(4.0))
                                .text_xs()
                                .text_color(rgba(0xcdd6f4ff))
                                .child(display)
                                .into_any_element()
                        }
                    }
                };

                let field_div = div()
                    .id(SharedString::from(format!("field-{}", spec.key)))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(label)
                    .child(widget);

                col = col.child(field_div);
            }

            columns_div = columns_div.child(col);
        }

        // ── Page navigation overlay ──────────────────────────────────────────
        let has_prev = current_page_idx > 0;
        let has_next = current_page_idx + 1 < total_pages;

        let mut form_area = div()
            .id("form-area")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .min_h_0()
            .relative();
        form_area.style().flex_grow = Some(1.0);
        form_area.style().flex_shrink = Some(1.0);
        form_area.style().flex_basis = Some(relative(0.).into());

        form_area = form_area.child(columns_div);

        if has_prev {
            form_area = form_area.child(
                div()
                    .id("page-prev")
                    .absolute()
                    .top(px(4.0))
                    .right(px(8.0))
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .h(px(24.0))
                    .bg(rgba(0x313244dd))
                    .rounded(px(4.0))
                    .text_xs()
                    .text_color(rgba(0xcdd6f4ff))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                            if let Some(tab_idx) = this.active_tab {
                                if let Some(t) = this.tabs.get_mut(tab_idx) {
                                    t.current_page = t.current_page.saturating_sub(1);
                                }
                            }
                            cx.notify();
                        }),
                    )
                    .child("< Prev"),
            );
        }

        if has_next {
            form_area = form_area.child(
                div()
                    .id("page-next")
                    .absolute()
                    .bottom(px(4.0))
                    .right(px(8.0))
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .h(px(24.0))
                    .bg(rgba(0x313244dd))
                    .rounded(px(4.0))
                    .text_xs()
                    .text_color(rgba(0xcdd6f4ff))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                            if let Some(tab_idx) = this.active_tab {
                                if let Some(t) = this.tabs.get_mut(tab_idx) {
                                    t.current_page += 1;
                                }
                            }
                            cx.notify();
                        }),
                    )
                    .child("Next >"),
            );
        }

        outer.child(measure_canvas).child(tab_bar).child(form_area)
    }
}

// ── Root app view ─────────────────────────────────────────────────────────────

struct AppView {
    graph_canvas: Entity<GraphCanvas>,
    tree_panel: Entity<TreePanel>,
    node_editor: Entity<NodeEditorPanel>,
    #[allow(dead_code)]
    selection: Entity<SelectionModel>,
    snapshot: Arc<RwLock<GraphSnapshot>>,
    graph: Arc<KnowledgeGraph>,
    data_file: std::path::PathBuf,
    schema_dir: std::path::PathBuf,
    file_menu_open: bool,
    view_menu_open: bool,
    sidebar_open: bool,
    right_panel_open: bool,
    /// Status message displayed in the status bar during/after data operations.
    data_status: Option<String>,
}

impl AppView {
    fn new(
        snapshot: GraphSnapshot,
        graph: Arc<KnowledgeGraph>,
        schema_mgr: Arc<SchemaManager>,
        data_file: std::path::PathBuf,
        schema_dir: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot_arc = Arc::new(RwLock::new(snapshot));
        let selection = cx.new(|_cx| SelectionModel::new(snapshot_arc.clone()));
        let graph_canvas = cx.new(|cx| {
            GraphCanvas::new(snapshot_arc.clone(), graph.clone(), selection.clone(), cx)
        });
        let tree_panel = cx.new(|_cx| TreePanel::new(snapshot_arc.clone(), selection.clone()));
        let node_editor = cx.new(|cx| {
            NodeEditorPanel::new(
                snapshot_arc.clone(),
                selection.clone(),
                graph.clone(),
                schema_mgr,
                cx,
            )
        });
        Self {
            graph_canvas,
            tree_panel,
            node_editor,
            selection,
            snapshot: snapshot_arc,
            graph,
            data_file,
            schema_dir,
            file_menu_open: false,
            view_menu_open: false,
            sidebar_open: false,
            right_panel_open: false,
            data_status: None,
        }
    }

    /// Rebuild the in-memory snapshot from the graph and push it to all child views.
    fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
        match u_forge_graph_view::build_snapshot(&self.graph) {
            Ok(snap) => {
                *self.snapshot.write() = snap;
                cx.notify();
            }
            Err(e) => {
                eprintln!("Failed to rebuild snapshot: {e}");
            }
        }
    }

    fn do_clear_data(&mut self, cx: &mut Context<Self>) {
        match self.graph.clear_all() {
            Ok(()) => {
                self.data_status = Some("Data cleared.".to_string());
                self.refresh_snapshot(cx);
            }
            Err(e) => {
                self.data_status = Some(format!("Clear failed: {e}"));
                cx.notify();
            }
        }
    }

    fn do_import_data(&mut self, cx: &mut Context<Self>) {
        let graph = self.graph.clone();
        let data_file = self.data_file.clone();
        let schema_dir = self.schema_dir.to_string_lossy().into_owned();

        self.data_status = Some("Importing…".to_string());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = u_forge_core::ingest::setup_and_index(
                &graph,
                &schema_dir,
                data_file.to_str().unwrap_or(""),
            )
            .await;

            this.update(cx, |view: &mut AppView, cx| {
                match result {
                    Ok(stats) => {
                        view.data_status = Some(format!(
                            "Import done — {} nodes, {} edges",
                            stats.objects_created, stats.relationships_created
                        ));
                        view.refresh_snapshot(cx);
                    }
                    Err(e) => {
                        view.data_status = Some(format!("Import failed: {e}"));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn do_save(&mut self, cx: &mut Context<Self>) {
        // 1. Save layout positions.
        self.graph_canvas.read(cx).save_layout();

        // 2. Save all dirty editor tabs.
        let saved = self.node_editor.update(cx, |editor, _cx| {
            editor.save_dirty_tabs()
        });

        if saved > 0 {
            // Update the snapshot so tree panel and canvas reflect edits.
            let editor = self.node_editor.read(cx);
            let mut snap = self.snapshot.write();
            for tab in &editor.tabs {
                if let Some(node) = snap.nodes.iter_mut().find(|n| n.id == tab.node_id) {
                    node.name = tab.name.clone();
                    if let Some(desc) = tab.edited_values.get("description").and_then(|v| v.as_str()) {
                        node.description = if desc.is_empty() { None } else { Some(desc.to_string()) };
                    }
                }
            }
            drop(snap);
            eprintln!("Saved {} node(s).", saved);
        }

        cx.notify();
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let file_menu_open = self.file_menu_open;
        let view_menu_open = self.view_menu_open;
        let sidebar_open = self.sidebar_open;
        let right_panel_open = self.right_panel_open;

        // Read graph stats for the status bar.
        let snap = self.snapshot.read();
        let node_count = snap.nodes.len();
        let edge_count = snap.edges.len();
        drop(snap);

        div()
            .id("app-root")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e2e))
            // Handle actions dispatched from native menu or keybindings.
            .on_action(cx.listener(|this, _: &SaveLayout, _window, cx| {
                this.do_save(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _window, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, _window, cx| {
                this.right_panel_open = !this.right_panel_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearData, _window, cx| {
                this.do_clear_data(cx);
            }))
            .on_action(cx.listener(|this, _: &ImportData, _window, cx| {
                this.do_import_data(cx);
            }))
            // ── Menu bar ──────────────────────────────────────────────────────
            .child(
                div()
                    .id("menu-bar")
                    .flex()
                    .flex_none()
                    .h(px(MENU_BAR_H))
                    .w_full()
                    .bg(rgb(0x181825))
                    .border_b_1()
                    .border_color(rgb(0x313244))
                    .items_center()
                    .child(
                        // "File" menu button
                        div()
                            .id("file-btn")
                            .flex()
                            .items_center()
                            .h_full()
                            .px_3()
                            .text_color(rgba(0xcdd6f4ff))
                            .text_sm()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                    this.file_menu_open = !this.file_menu_open;
                                    this.view_menu_open = false;
                                    cx.notify();
                                }),
                            )
                            .child("File"),
                    )
                    .child(
                        // "View" menu button
                        div()
                            .id("view-btn")
                            .flex()
                            .items_center()
                            .h_full()
                            .px_3()
                            .text_color(rgba(0xcdd6f4ff))
                            .text_sm()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                    this.view_menu_open = !this.view_menu_open;
                                    this.file_menu_open = false;
                                    cx.notify();
                                }),
                            )
                            .child("View"),
                    ),
            )
            // ── Body: optional sidebar + main content ─────────────────────────
            .child({
                let mut body = div()
                    .id("body")
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .overflow_hidden();
                body.style().flex_grow = Some(1.0);
                body.style().flex_shrink = Some(1.0);
                body.style().flex_basis = Some(relative(0.).into());

                // Left sidebar (tree panel)
                if sidebar_open {
                    body = body.child(self.tree_panel.clone());
                }

                // Main workspace: editor 30% + graph 70% (vertical split)
                let mut workspace = div()
                    .id("workspace")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden();
                workspace.style().flex_grow = Some(1.0);
                workspace.style().flex_shrink = Some(1.0);
                workspace.style().flex_basis = Some(relative(0.).into());

                // Node detail pane — 3 parts out of 10 (30%).
                let mut editor = div()
                    .id("editor-pane")
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_h_0()
                    .child(self.node_editor.clone());
                editor.style().flex_grow = Some(3.0);
                editor.style().flex_shrink = Some(1.0);
                editor.style().flex_basis = Some(relative(0.).into());

                // Graph canvas pane — 7 parts out of 10 (70%).
                let mut graph_pane = div()
                    .w_full()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.graph_canvas.clone());
                graph_pane.style().flex_grow = Some(7.0);
                graph_pane.style().flex_shrink = Some(1.0);
                graph_pane.style().flex_basis = Some(relative(0.).into());

                body = body.child(workspace.child(editor).child(graph_pane));

                // Right panel placeholder (chat)
                if right_panel_open {
                    body = body.child(
                        div()
                            .id("right-panel")
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w(px(280.0))
                            .h_full()
                            .min_h_0()
                            .bg(rgb(0x181825))
                            .border_l_1()
                            .border_color(rgb(0x313244))
                            .items_center()
                            .justify_center()
                            .text_color(rgba(0x6c7086ff))
                            .text_sm()
                            .child("Chat — coming soon"),
                    );
                }

                body
            })
            // ── Status bar ────────────────────────────────────────────────────
            .child(
                div()
                    .id("status-bar")
                    .flex()
                    .flex_none()
                    .flex_row()
                    .h(px(STATUS_BAR_H))
                    .w_full()
                    .bg(rgb(0x181825))
                    .border_t_1()
                    .border_color(rgb(0x313244))
                    .items_center()
                    .text_xs()
                    // ── Left: panel toggle buttons ────────────────────────────
                    .child(
                        div()
                            .id("status-left")
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_none()
                            .gap(px(2.0))
                            .px_1()
                            .child(
                                div()
                                    .id("status-tree-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(px(STATUS_BAR_H - 4.0))
                                    .cursor_pointer()
                                    .text_color(if sidebar_open {
                                        rgba(0xcdd6f4ff)
                                    } else {
                                        rgba(0x6c7086ff)
                                    })
                                    .when(sidebar_open, |el| el.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            this.sidebar_open = !this.sidebar_open;
                                            cx.notify();
                                        }),
                                    )
                                    .child("Tree"),
                            ),
                    )
                    // ── Center: graph stats + operation status ────────────────
                    .child({
                        let data_status = self.data_status.clone();
                        let mut center = div()
                            .id("status-center")
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .gap(px(12.0))
                            .text_color(rgba(0xa6adc8ff));
                        center.style().flex_grow = Some(1.0);
                        center = center.child(format!("{} nodes  ·  {} edges", node_count, edge_count));
                        if let Some(msg) = data_status {
                            center = center.child(
                                div()
                                    .text_color(rgba(0xa6e3a1ff))
                                    .child(msg),
                            );
                        }
                        center
                    })
                    // ── Right: chat toggle button ─────────────────────────────
                    .child(
                        div()
                            .id("status-right")
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_none()
                            .gap(px(2.0))
                            .px_1()
                            .child(
                                div()
                                    .id("status-chat-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(px(STATUS_BAR_H - 4.0))
                                    .cursor_pointer()
                                    .text_color(if right_panel_open {
                                        rgba(0xcdd6f4ff)
                                    } else {
                                        rgba(0x6c7086ff)
                                    })
                                    .when(right_panel_open, |el| el.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            this.right_panel_open = !this.right_panel_open;
                                            cx.notify();
                                        }),
                                    )
                                    .child("Chat"),
                            ),
                    ),
            )
            // ── File dropdown overlay ─────────────────────────────────────────
            .when(file_menu_open, |root| {
                root.child(deferred(
                    anchored()
                        .position(point(px(0.0), px(MENU_BAR_H)))
                        .anchor(Corner::TopLeft)
                        .child(
                            div()
                                .id("file-dropdown")
                                .w(px(200.0))
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                .child(
                                    div()
                                        .id("save-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.do_save(cx);
                                                this.file_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child("Save                Ctrl+S"),
                                )
                                // ── separator ──
                                .child(
                                    div()
                                        .h(px(1.0))
                                        .w_full()
                                        .bg(rgb(0x45475a)),
                                )
                                .child(
                                    div()
                                        .id("import-data-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.file_menu_open = false;
                                                this.do_import_data(cx);
                                            }),
                                        )
                                        .child("Import Data"),
                                )
                                .child(
                                    div()
                                        .id("clear-data-item")
                                        .flex()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xf38ba8ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.file_menu_open = false;
                                                this.do_clear_data(cx);
                                            }),
                                        )
                                        .child("Clear Data"),
                                ),
                        ),
                ))
            })
            // ── View dropdown overlay ─────────────────────────────────────────
            .when(view_menu_open, |root| {
                // Position horizontally after the "File" button (~30px wide + padding).
                root.child(deferred(
                    anchored()
                        .position(point(px(32.0), px(MENU_BAR_H)))
                        .anchor(Corner::TopLeft)
                        .child(
                            div()
                                .id("view-dropdown")
                                .w(px(200.0))
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                // Left Panel toggle
                                .child(
                                    div()
                                        .id("toggle-left-item")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.sidebar_open = !this.sidebar_open;
                                                this.view_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child(if sidebar_open {
                                            "  Left Panel       Ctrl+B"
                                        } else {
                                            "    Left Panel       Ctrl+B"
                                        }),
                                )
                                // Right Panel toggle
                                .child(
                                    div()
                                        .id("toggle-right-item")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .h(px(28.0))
                                        .px_3()
                                        .text_color(rgba(0xcdd6f4ff))
                                        .text_sm()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.right_panel_open = !this.right_panel_open;
                                                this.view_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                        .child(if right_panel_open {
                                            "  Right Panel      Ctrl+J"
                                        } else {
                                            "    Right Panel      Ctrl+J"
                                        }),
                                ),
                        ),
                ))
            })
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cfg = AppConfig::load_default();
    let data_dir = cfg.storage.db_path.clone();
    let data_file = cfg.data.import_file.clone();
    let schema_dir = cfg.data.schema_dir.clone();

    let (snapshot, graph, schema_mgr) = {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        rt.block_on(async {
            let graph =
                Arc::new(KnowledgeGraph::new(&data_dir).expect("failed to open knowledge graph"));

            let stats = graph.get_stats().expect("failed to get stats");
            if stats.node_count == 0 {
                if data_file.exists() {
                    let mut ingestion = u_forge_core::DataIngestion::new(&graph);
                    ingestion
                        .import_json_data(&data_file)
                        .await
                        .expect("failed to import data");
                    let stats = graph.get_stats().expect("failed to get stats");
                    eprintln!(
                        "Imported {} nodes, {} edges from {}",
                        stats.node_count,
                        stats.edge_count,
                        data_file.display()
                    );
                } else {
                    eprintln!(
                        "Warning: import file '{}' not found, using empty graph",
                        data_file.display()
                    );
                }
            } else {
                eprintln!(
                    "Loaded existing graph: {} nodes, {} edges",
                    stats.node_count, stats.edge_count
                );
            }

            // Pre-load schemas so they're available synchronously in the UI.
            let schema_mgr = graph.get_schema_manager();
            if let Err(e) = schema_mgr.load_schema("default").await {
                eprintln!("Warning: could not load default schema: {e}");
            }

            let snapshot = build_snapshot(&graph).expect("failed to build snapshot");
            (snapshot, graph, schema_mgr)
        })
    };

    Application::new().run(move |cx: &mut App| {
        // Register keybindings.
        cx.bind_keys([
            KeyBinding::new("ctrl-s", SaveLayout, None),
            KeyBinding::new("ctrl-b", ToggleSidebar, None),
            KeyBinding::new("ctrl-j", ToggleRightPanel, None),
        ]);

        // Register native application menu (macOS menu bar; no-op on Linux).
        cx.set_menus(vec![
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Save", SaveLayout),
                    MenuItem::separator(),
                    MenuItem::action("Import Data", ImportData),
                    MenuItem::action("Clear Data", ClearData),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Left Panel", ToggleSidebar),
                    MenuItem::action("Toggle Right Panel", ToggleRightPanel),
                ],
            },
        ]);

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    AppView::new(snapshot, graph, schema_mgr, data_file, schema_dir, cx)
                })
            },
        )
        .unwrap();
    });
}
