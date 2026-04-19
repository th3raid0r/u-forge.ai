use gpui::{
    canvas, div, prelude::*, px, relative, rgb, rgba, Context, MouseButton, MouseDownEvent, Render,
    SharedString, Window,
};

use crate::text_field::{TextFieldView, TextSubmit};

use u_forge_core::PropertyType;

use super::field_spec::SubTab;
use super::{
    field_spec::{
        COLUMN_W, DETAIL_TAB_H, EDGE_ADD_BTN_H, EDGE_ROW_H, EDGE_SECTION_HEADER_H, PAGE_NAV_H,
        SUBTAB_BAR_H,
    },
    NodeEditorPanel,
};

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
                .bg(if is_active {
                    rgb(0x1e1e2e)
                } else {
                    rgb(0x181825)
                });

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
                        this.rebuild_field_subscriptions(cx);
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
                        this.close_tab(i, cx);
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
        let active_subtab = tab.active_subtab;

        // ── Sub-tab bar (Properties / Edges) ─────────────────────────────────
        let subtab_bar = {
            let props_active = active_subtab == SubTab::Properties;
            let edges_active = active_subtab == SubTab::Edges;

            let props_btn = div()
                .id("subtab-properties")
                .flex()
                .items_center()
                .px(px(10.0))
                .h_full()
                .text_xs()
                .cursor_pointer()
                .text_color(if props_active {
                    rgba(0xcdd6f4ff)
                } else {
                    rgba(0x6c7086ff)
                })
                .border_b_2()
                .border_color(if props_active {
                    rgb(0x89b4fa)
                } else {
                    rgb(0x00000000)
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        if let Some(idx) = this.active_tab {
                            if let Some(t) = this.tabs.get_mut(idx) {
                                t.active_subtab = SubTab::Properties;
                            }
                        }
                        this.edge_node_dropdown = None;
                        cx.notify();
                    }),
                )
                .child("Properties");

            let edges_btn = div()
                .id("subtab-edges")
                .flex()
                .items_center()
                .px(px(10.0))
                .h_full()
                .text_xs()
                .cursor_pointer()
                .text_color(if edges_active {
                    rgba(0xcdd6f4ff)
                } else {
                    rgba(0x6c7086ff)
                })
                .border_b_2()
                .border_color(if edges_active {
                    rgb(0x89b4fa)
                } else {
                    rgb(0x00000000)
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        if let Some(idx) = this.active_tab {
                            if let Some(t) = this.tabs.get_mut(idx) {
                                t.active_subtab = SubTab::Edges;
                            }
                        }
                        cx.notify();
                    }),
                )
                .child("Edges");

            div()
                .id("subtab-bar")
                .flex()
                .flex_row()
                .flex_none()
                .h(px(SUBTAB_BAR_H))
                .bg(rgb(0x181825))
                .border_b_1()
                .border_color(rgb(0x313244))
                .child(props_btn)
                .child(edges_btn)
        };

        // Compute column/page layout using measured panel dimensions.
        let panel_w = f32::from(self.panel_size.width);
        let panel_h = f32::from(self.panel_size.height);
        let available_h = (panel_h - DETAIL_TAB_H - SUBTAB_BAR_H - PAGE_NAV_H).max(100.0);
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
                    PropertyType::Boolean => {
                        let checked = value.and_then(|v| v.as_bool()).unwrap_or(false);
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
                                            let cur = t
                                                .edited_values
                                                .get(&key)
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            t.edited_values
                                                .insert(key.clone(), serde_json::Value::Bool(!cur));
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
                                    .bg(if checked {
                                        rgb(0x89b4fa)
                                    } else {
                                        rgb(0x313244)
                                    })
                                    .when(checked, |el| {
                                        el.child(
                                            div().text_xs().text_color(rgb(0x1e1e2e)).child("v"),
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
                    PropertyType::Enum(values) => {
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
                                        .hover(|style| style.bg(rgba(0x45475a88)))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(
                                                move |this, _: &MouseDownEvent, _window, cx| {
                                                    if let Some(tab_idx) = this.active_tab {
                                                        if let Some(t) = this.tabs.get_mut(tab_idx)
                                                        {
                                                            t.edited_values.insert(
                                                                key_inner.clone(),
                                                                serde_json::Value::String(
                                                                    val_str.clone(),
                                                                ),
                                                            );
                                                            // Also update the text field if it exists
                                                            if let Some(tf) =
                                                                t.field_entities.get(&key_inner)
                                                            {
                                                                tf.update(cx, |tf, cx| {
                                                                    tf.set_content(&val_str, cx);
                                                                });
                                                            }
                                                            t.recompute_dirty();
                                                        }
                                                    }
                                                    this.dropdown_open = None;
                                                    cx.notify();
                                                },
                                            ),
                                        )
                                        .child(label),
                                );
                            }

                            enum_div = enum_div.child(dropdown);
                        }

                        enum_div.into_any_element()
                    }
                    PropertyType::Array(_) => {
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
                        let is_adding = self
                            .array_add_field
                            .as_ref()
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
                                    .child(div().w(px(100.0)).child(add_entity.clone())),
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
                                                move |this: &mut Self,
                                                          _tf,
                                                          _event: &TextSubmit,
                                                          cx| {
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

        // ── Assemble content area based on active sub-tab ────────────────────

        let base = outer
            .child(measure_canvas)
            .child(tab_bar)
            .child(subtab_bar);

        if active_subtab == SubTab::Properties {
            // ── Properties sub-tab: scrollable form columns + page nav ────────
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

            base.child(form_area)
        } else {
            // ── Edges sub-tab: edge editor fills remaining height ─────────────
            let edges_section = self.render_edges_section(active_idx, true, cx);
            base.child(edges_section)
        }
    }
}

// ── Edge section rendering (split out for readability) ───────────────────────

impl NodeEditorPanel {
    /// Build the "EDGES" section.
    ///
    /// When `primary` is `true` (Edges sub-tab active), the section expands to
    /// fill the remaining panel height.
    fn render_edges_section(
        &mut self,
        active_idx: usize,
        primary: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tab = match self.tabs.get(active_idx) {
            Some(t) => t,
            None => return div().into_any_element(),
        };

        let edge_count = tab.edited_edges.len();
        let has_edges = edge_count > 0;

        // Gather the active dropdown state for rendering.
        let dd_edge_idx = self.edge_node_dropdown.as_ref().map(|dd| dd.edge_idx);

        // ── Section container ─────────────────────────────────────────────
        // `.relative()` makes this the positioned ancestor for the dropdown overlay.
        let mut section = div()
            .id("edges-section")
            .relative()
            .flex()
            .flex_col()
            .px_3()
            .pb_3()
            .gap(px(4.0));
        if primary {
            // Fill remaining panel height when shown as the primary content.
            section.style().flex_grow = Some(1.0);
            section.style().flex_shrink = Some(1.0);
            section.style().flex_basis = Some(relative(0.).into());
        }

        // Section header
        section = section.child(
            div()
                .id("edges-header")
                .flex()
                .items_center()
                .h(px(EDGE_SECTION_HEADER_H))
                .border_b_1()
                .border_color(rgb(0x313244))
                .text_xs()
                .text_color(rgba(0xa6adc8ff))
                .child(format!(
                    "EDGES{}",
                    if has_edges {
                        format!(" ({})", edge_count)
                    } else {
                        String::new()
                    }
                )),
        );

        // ── Edge rows ─────────────────────────────────────────────────────
        for ei in 0..edge_count {
            let tab = &self.tabs[active_idx];
            let edge = &tab.edited_edges[ei];

            // To button label
            let to_label: SharedString = if edge.to_name.is_empty() {
                "Select node\u{2026}".into()
            } else {
                let name = if edge.to_name.len() > 22 {
                    let mut s: String = edge.to_name.chars().take(21).collect();
                    s.push('\u{2026}');
                    s
                } else {
                    edge.to_name.clone()
                };
                name.into()
            };
            let to_has_value = edge.to.is_some();

            // Edge type text field
            let type_field = {
                let tab = &self.tabs[active_idx];
                if let Some(entity) = tab.edge_type_entities.get(ei) {
                    div()
                        .id(SharedString::from(format!("edge-type-{}", ei)))
                        .min_w(px(80.0))
                        .max_w(px(140.0))
                        .child(entity.clone())
                } else {
                    div()
                        .id(SharedString::from(format!("edge-type-{}", ei)))
                        .min_w(px(80.0))
                        .max_w(px(140.0))
                        .h(px(26.0))
                        .bg(rgb(0x313244))
                        .rounded(px(4.0))
                }
            };

            // Arrow to target
            let arrow = div()
                .flex()
                .items_center()
                .text_xs()
                .text_color(rgba(0x6c7086ff))
                .child("\u{2192}"); // →

            // "To" node selector button
            let to_btn = div()
                .id(SharedString::from(format!("edge-to-btn-{}", ei)))
                .flex()
                .items_center()
                .h(px(26.0))
                .px(px(6.0))
                .bg(rgb(0x313244))
                .rounded(px(4.0))
                .border_1()
                .border_color(if dd_edge_idx == Some(ei) {
                    rgb(0x89b4fa)
                } else {
                    rgb(0x45475a)
                })
                .text_xs()
                .text_color(if to_has_value {
                    rgba(0xcdd6f4ff)
                } else {
                    rgba(0x6c7086ff)
                })
                .cursor_pointer()
                .min_w(px(100.0))
                .max_w(px(160.0))
                .overflow_hidden()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        this.open_edge_dropdown(ei, true, window, cx);
                    }),
                )
                .child(to_label);

            // Delete button
            let delete_btn = div()
                .id(SharedString::from(format!("edge-del-{}", ei)))
                .flex()
                .items_center()
                .justify_center()
                .w(px(22.0))
                .h(px(22.0))
                .rounded(px(3.0))
                .cursor_pointer()
                .text_xs()
                .text_color(rgba(0xf38ba8aa))
                .hover(|style| style.bg(rgba(0xf38ba822)).text_color(rgba(0xf38ba8ff)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        this.remove_edge_row(ei, cx);
                    }),
                )
                .child("\u{2715}"); // ✕

            // Assemble the edge row — no dropdown inline; overlay is rendered
            // after the add button so it paints on top.
            let row = div()
                .id(SharedString::from(format!("edge-row-{}", ei)))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .h(px(EDGE_ROW_H))
                .child(type_field)
                .child(arrow)
                .child(to_btn)
                .child(delete_btn);

            section = section.child(row);
        }

        // ── "+ Add Edge" button ───────────────────────────────────────────
        let mut add_btn = div()
            .id("edge-add-btn")
            .flex()
            .items_center()
            .h(px(EDGE_ADD_BTN_H))
            .px(px(8.0))
            .bg(rgba(0x89b4fa1a))
            .rounded(px(4.0))
            .text_xs()
            .text_color(rgb(0x89b4fa))
            .cursor_pointer()
            .hover(|style: gpui::StyleRefinement| style.bg(rgba(0x89b4fa33)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.add_edge_row(cx);
                }),
            )
            .child("+ Add Edge");
        // Shrink to content width instead of stretching to fill the column.
        add_btn.style().align_self = Some(gpui::AlignItems::Start);
        section = section.child(add_btn);

        // ── Node-selector dropdown overlay ────────────────────────────────
        // Rendered last so it paints over all rows and the add button.
        // Positioned absolute within this section (which is `relative`).
        //
        // Y: bottom of row[ei] = header_h + (ei+1) * (row_h + gap)
        //    = EDGE_SECTION_HEADER_H + gap(4) + ei*(EDGE_ROW_H+4) + EDGE_ROW_H
        // X: align with section content edge (section has px_3 = 12px h-padding).
        if let Some(ref dd) = self.edge_node_dropdown {
            let ei = dd.edge_idx;
            let filter_lower = dd.filter_text.to_lowercase();
            let filter_entity = dd.filter_entity.clone();
            let highlighted = dd.highlighted_idx;

            let anchor_y = EDGE_SECTION_HEADER_H + 4.0 + ei as f32 * (EDGE_ROW_H + 4.0) + EDGE_ROW_H;

            // Build filtered node list from snapshot.
            let snap = self.snapshot.read();
            let mut candidates: Vec<(u_forge_core::ObjectId, String, String)> = snap
                .nodes
                .iter()
                .filter(|n| {
                    filter_lower.is_empty()
                        || n.name.to_lowercase().contains(&filter_lower)
                        || n.object_type.to_lowercase().contains(&filter_lower)
                })
                .map(|n| (n.id, n.name.clone(), n.object_type.clone()))
                .collect();
            drop(snap);
            candidates.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
            candidates.truncate(10);

            let mut dropdown_div = div()
                .id(SharedString::from(format!("edge-dd-overlay-{}", ei)))
                .absolute()
                .top(px(anchor_y))
                .left(px(12.0))
                .w(px(240.0))
                .max_h(px(220.0))
                .bg(rgb(0x1e1e2e))
                .border_1()
                .border_color(rgb(0x89b4fa))
                .rounded(px(4.0))
                .flex()
                .flex_col()
                .overflow_hidden();

            // Filter text field
            dropdown_div = dropdown_div.child(
                div()
                    .id(SharedString::from(format!("edge-dd-filter-overlay-{}", ei)))
                    .flex()
                    .flex_none()
                    .h(px(28.0))
                    .px(px(4.0))
                    .border_b_1()
                    .border_color(rgb(0x313244))
                    .child(filter_entity),
            );

            // Scrollable candidate list
            let mut list = div()
                .id(SharedString::from(format!("edge-dd-list-overlay-{}", ei)))
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .min_h_0();
            list.style().flex_grow = Some(1.0);
            list.style().flex_shrink = Some(1.0);
            list.style().flex_basis = Some(relative(0.).into());

            if candidates.is_empty() {
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(24.0))
                        .px(px(6.0))
                        .text_xs()
                        .text_color(rgba(0x6c7086ff))
                        .child("No matching nodes"),
                );
            } else {
                for (ci, (cand_id, cand_name, cand_type)) in candidates.iter().enumerate() {
                    let cand_id = *cand_id;
                    let cand_name_for_select = cand_name.clone();
                    let is_highlighted = ci == highlighted;

                    let display_name: SharedString = if cand_name.len() > 24 {
                        let mut s: String = cand_name.chars().take(23).collect();
                        s.push('\u{2026}');
                        s
                    } else {
                        cand_name.clone()
                    }
                    .into();

                    let type_badge: SharedString = cand_type.clone().into();

                    let mut row_div = div()
                        .id(SharedString::from(format!(
                            "edge-dd-opt-overlay-{}-{}",
                            ei, ci
                        )))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(26.0))
                        .px(px(6.0))
                        .text_xs()
                        .text_color(rgba(0xcdd6f4ff))
                        .cursor_pointer()
                        .hover(|style: gpui::StyleRefinement| style.bg(rgba(0x45475a88)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                this.select_edge_node(cand_id, cand_name_for_select.clone(), cx);
                            }),
                        )
                        .child(display_name)
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgba(0x6c7086aa))
                                .child(type_badge),
                        );

                    if is_highlighted {
                        row_div = row_div.bg(rgb(0x45475a));
                    }

                    list = list.child(row_div);
                }
            }

            dropdown_div = dropdown_div.child(list);
            section = section.child(dropdown_div);
        }

        section.into_any_element()
    }
}
