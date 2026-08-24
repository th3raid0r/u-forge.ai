//! Existing `ChatPanel` element-tree construction, isolated from stream
//! execution and persistence state transitions.

use super::*;

impl ChatPanel {
    /// Build the virtualized history dropdown without allocating off-screen rows.
    fn build_history_list(&self, cx: &mut Context<Self>) -> impl IntoElement + Styled + use<> {
        let history_list_entity = cx.entity().clone();
        list(
            self.history_list_state.clone(),
            move |ix, _window, cx: &mut App| {
                let panel = history_list_entity.read(cx);
                let Some(session) = panel.session_list.get(ix).cloned() else {
                    return div().into_any_element();
                };
                let is_current = panel.current_session_id.as_deref() == Some(&session.id);
                let is_hovered = panel.hovered_history_ix == Some(ix);
                let entity_load = history_list_entity.clone();
                let sid_load = session.id.clone();
                let entity_del = history_list_entity.clone();
                let sid_del = session.id.clone();
                let entity_hover = history_list_entity.clone();
                let title = session.title.clone();

                // Both gradient stops share the same hue (only alpha differs)
                // so the gradient is invisible over empty space and only masks
                // text that actually overflows. Colours are pre-composited
                // equivalents of the semi-transparent row backgrounds:
                //   selected: rgba(0x45475a88) over #313244 ≈ #3C3D50
                //   hovered:  rgba(0x45475a66) over #313244 ≈ #393A4D
                let (gradient_start, gradient_end) = if is_current {
                    (rgba(0x3c3d5000), rgba(0x3c3d50ff))
                } else if is_hovered {
                    (rgba(0x393a4d00), rgba(0x393a4dff))
                } else {
                    (rgba(0x31324400), rgba(0x313244ff))
                };

                div()
                    .id(("hist", ix))
                    .relative()
                    .w_full()
                    .overflow_x_hidden()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(24.0))
                    .px_2()
                    .when(is_current, |el| el.bg(rgb(0x3c3d50)))
                    .hover(|s| s.bg(rgb(0x393a4d)))
                    .on_hover(move |is_hov, _window, cx| {
                        entity_hover.update(cx, |this, cx| {
                            this.hovered_history_ix = if *is_hov { Some(ix) } else { None };
                            cx.notify();
                        });
                    })
                    .child({
                        // No `relative()` here — keeping the title static avoids
                        // a separate stacking context that confuses the row's
                        // on_hover hit-testing when the cursor descends from above.
                        let mut title_el = div()
                            .id(("hist-title", ix))
                            .flex()
                            .items_center()
                            .min_w_0()
                            .text_sm()
                            .text_color(if is_current {
                                rgba(0xcdd6f4ff)
                            } else {
                                rgba(0xa6adc8ff)
                            })
                            .cursor_pointer()
                            .overflow_x_hidden()
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_: &MouseDownEvent, _window, cx: &mut App| {
                                    entity_load.update(cx, |this, cx| {
                                        this.load_session(&sid_load, cx);
                                    });
                                },
                            )
                            .child(title);
                        title_el.style().flex_grow = Some(1.0);
                        title_el.style().flex_shrink = Some(1.0);
                        title_el
                    })
                    // Gradient is a row-level absolute child so it doesn't
                    // interfere with the title's hit-box. right(26) aligns its
                    // right edge with the delete button's left edge
                    // (8 px padding + 18 px button = 26 px from outer right).
                    .child(
                        div()
                            .absolute()
                            .right(px(26.0))
                            .top_0()
                            .h_full()
                            .w(px(28.0))
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(gradient_start, 0.0),
                                linear_color_stop(gradient_end, 1.0),
                            )),
                    )
                    .child(
                        div()
                            .id(("hist-del", ix))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .w(px(18.0))
                            .h(px(18.0))
                            .rounded(px(2.0))
                            .text_sm()
                            .text_color(rgba(0xf38ba8aa))
                            .cursor_pointer()
                            .hover(|s| s.text_color(rgba(0xf38ba8ff)).bg(rgba(0x45475a66)))
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_: &MouseDownEvent, _window, cx: &mut App| {
                                    entity_del.update(cx, |this, cx| {
                                        this.delete_session(&sid_del, cx);
                                    });
                                },
                            )
                            .tooltip(Tooltip::text("Delete conversation"))
                            .child(Icon::new(
                                IconName::Trash,
                                IconSize::Small,
                                rgba(0xf38ba8ff),
                            )),
                    )
                    .into_any_element()
            },
        )
    }

    /// Build message rows at the parent render site so actions need no per-message subscriptions.
    fn build_message_list(
        &self,
        theme: UiTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + Styled + use<> {
        let list_entity = cx.entity().clone();
        list(self.list_state.clone(), move |ix, _window, cx: &mut App| {
            let _span = tracing::trace_span!("chat_panel::list_item", ix).entered();
            let panel = list_entity.read(cx);
            let Some(msg) = panel.messages.get(ix).cloned() else {
                return div().into_any_element();
            };
            let is_last = ix + 1 == panel.messages.len();
            let is_streaming = panel.streaming;
            let controls_locked = panel.controls_locked();
            let role = msg.read(cx).role;
            let can_retry = !controls_locked
                && matches!(role, ChatMessageRole::User | ChatMessageRole::Assistant);
            let show_delete = is_last && !is_streaming;

            let mut row = div()
                .id(("msg-row", ix))
                .flex()
                .flex_col()
                .w_full()
                .child(msg.clone());

            // Right-click anywhere on the row to open the context menu.
            // Resolve copy text at click time (not render time) so an active
            // drag-selection in the body is captured — selection changes in
            // the body's TextFieldView only notify that entity, not the panel,
            // so a render-time snapshot would be stale.
            let ctx_entity = list_entity.clone();
            let msg_for_ctx = msg.clone();
            row = row.on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, _window, cx: &mut App| {
                    let pos = event.position;
                    let text = msg_for_ctx.read(cx).copy_text_for_context(cx);
                    ctx_entity.update(cx, |panel, cx| {
                        panel.context_menu = Some(ContextMenuState {
                            position: pos,
                            text,
                        });
                        cx.notify();
                    });
                },
            );

            if can_retry || show_delete {
                let del_entity = list_entity.clone();
                let retry_entity = list_entity.clone();
                let copy_entity = list_entity.clone();
                let msg_entity_id = panel.messages[ix].entity_id();
                let msg_for_copy = msg.clone();

                let bubble_bg = match role {
                    ChatMessageRole::User => rgb(0x313244),
                    ChatMessageRole::Thinking => rgb(0x181825),
                    ChatMessageRole::Assistant | ChatMessageRole::ToolCall => rgb(0x1e1e2e),
                };

                let mut action_bar = div()
                    .id(("action-bar", ix))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_1()
                    .px_2()
                    .py(px(2.0))
                    .bg(bubble_bg);

                if can_retry {
                    action_bar = action_bar.child(
                        div()
                            .id(("retry", ix))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(theme.metrics.control_height_small)
                            .text_base()
                            .text_color(rgba(0x6c708688))
                            .cursor_pointer()
                            .hover(|s| s.text_color(rgba(0xcdd6f4ff)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                                retry_entity.update(cx, |this, cx| {
                                    this.retry_message(msg_entity_id, cx);
                                });
                            })
                            .tooltip(Tooltip::text("Try this response again"))
                            .child(Icon::new(
                                IconName::Refresh,
                                IconSize::Small,
                                rgba(0xa6adc8ff),
                            )),
                    );
                }

                // Copy button — always shown in the action bar.
                action_bar = action_bar.child(
                    div()
                        .id(("copy", ix))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(theme.metrics.control_height_small)
                        .text_base()
                        .text_color(rgba(0x6c708688))
                        .cursor_pointer()
                        .hover(|s| s.text_color(rgba(0xcdd6f4ff)))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                            let text = msg_for_copy.read(cx).copy_text_for_context(cx);
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            copy_entity.update(cx, |panel, cx| {
                                panel.context_menu = None;
                                cx.notify();
                            });
                        })
                        .tooltip(Tooltip::text("Copy message"))
                        .child(Icon::new(IconName::Copy, IconSize::Small, rgba(0xa6adc8ff))),
                );

                if show_delete {
                    action_bar = action_bar.child(
                        div()
                            .id(("del", ix))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(theme.metrics.control_height_small)
                            .rounded(px(2.0))
                            .text_base()
                            .text_color(rgba(0xf38ba8aa))
                            .cursor_pointer()
                            .hover(|s| s.text_color(rgba(0xf38ba8ff)).bg(rgba(0x45475a66)))
                            .on_mouse_down(MouseButton::Left, move |_, _, cx: &mut App| {
                                del_entity.update(cx, |this, cx| {
                                    let last = this.messages.len().saturating_sub(1);
                                    this.delete_message_at(last, cx);
                                });
                            })
                            .tooltip(Tooltip::text("Delete message"))
                            .child(Icon::new(
                                IconName::Trash,
                                IconSize::Small,
                                rgba(0xf38ba8ff),
                            )),
                    );
                }

                row = row.child(action_bar);
            }
            row.into_any_element()
        })
    }
}

impl Render for ChatPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_start = Instant::now();
        let theme = *UiTheme::get(cx);
        let panel_focused = self.focus.contains_focused(window, cx);
        let enter_to_submit = self.enter_to_submit;
        let streaming = self.streaming;
        let connecting = self.connecting;
        let profile_reloading = self.profile_reload_state.is_reloading();
        let controls_locked = self.controls_locked();
        let profile_reload_error = self.profile_reload_state.error().map(ToString::to_string);
        let connect_error = self.connect_error.clone();
        let model_dropdown_open = self.model_dropdown_open;
        let toolbar_menu_open = self.toolbar_menu_open;
        let reasoning_capable = self
            .available_models
            .get(self.selected_model_idx)
            .is_some_and(|model| model.reasoning_capable);
        let reasoning_enabled = self.reasoning_enabled && reasoning_capable;
        let has_provider = self.chat_provider.is_some() || self.agent.is_some();
        let model_label = self.selected_model_label();
        let history_dropdown_open = self.history_dropdown_open;
        let history_label = self.session_title();

        // Virtualized history rows and message rows retain their existing
        // list-state and entity-cache boundaries in local builders.
        let history_list_el = self.build_history_list(cx);

        // Build the virtualized message list. Only visible items (+ overdraw
        // buffer) are rendered, so long chat histories don't slow down layout.
        // Each item is its own `Entity<ChatMessageView>` — streaming token
        // deltas only invalidate the target entity, not this panel.
        let list_el = self.build_message_list(theme, cx);

        // Model dropdown items.
        let model_items: Vec<_> = if model_dropdown_open {
            self.available_models
                .iter()
                .enumerate()
                .map(|(idx, m)| {
                    let is_selected = idx == self.selected_model_idx;
                    let label = m.picker_label();
                    div()
                        .id(("model", idx))
                        .flex()
                        .items_center()
                        .h(theme.metrics.control_height)
                        .px_2()
                        .text_sm()
                        .text_color(if is_selected {
                            rgba(0xcdd6f4ff)
                        } else {
                            rgba(0xa6adc8ff)
                        })
                        .when(is_selected, |el| el.bg(rgba(0x45475a88)))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(0x45475a66)))
                        .debug_selector(move || format!("model-option-{idx}"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                let changed =
                                    !this.controls_locked() && this.selected_model_idx != idx;
                                if changed {
                                    this.selected_model_idx = idx;
                                    this.apply_selected_chat_profile();
                                }
                                this.model_dropdown_open = false;
                                if changed {
                                    this.reload_selected_profile(cx);
                                } else {
                                    cx.notify();
                                }
                            }),
                        )
                        .child(label)
                })
                .collect()
        } else {
            Vec::new()
        };

        // ── Context menu state (captured before root building) ────────────────
        let ctx_pos = self.context_menu.as_ref().map(|m| m.position);
        let ctx_text = self
            .context_menu
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default();
        let input_field_paste = self.input_field.clone();
        let ctx_copy_listener = cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(ctx_text.clone()));
            this.context_menu = None;
            cx.notify();
        });
        let ctx_paste_listener = cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
            if let Some(clip) = cx.read_from_clipboard()
                && let Some(text) = clip.text()
            {
                input_field_paste.update(cx, |tf, cx| {
                    tf.insert_at_cursor(&text, cx);
                });
            }
            this.context_menu = None;
            cx.notify();
        });

        let root = div()
            .id("chat-panel")
            .key_context("AssistantPanel")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .bg(rgb(0x181825))
            .border_l_1()
            .border_color(rgb(0x313244))
            .when(panel_focused, |panel| {
                panel.border_1().border_color(rgba(0xb4befeff))
            })
            // ── Header: chat history selector ────────────────────────────────
            .child(
                div()
                    .id("chat-header")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .border_b_1()
                    .border_color(rgb(0x313244))
                    .child(
                        div()
                            .id("history-selector-row")
                            .flex()
                            .flex_row()
                            .flex_none()
                            .items_center()
                            .h(theme.metrics.panel_header_height)
                            .px_3()
                            .gap(px(theme.metrics.space_2))
                            .child(
                                div()
                                    .id("history-selector-btn")
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .h(theme.metrics.control_height_small)
                                    .bg(rgb(0x313244))
                                    .border_1()
                                    .border_color(rgb(0x45475a))
                                    .rounded(px(3.0))
                                    .text_sm()
                                    .text_color(if controls_locked {
                                        rgba(0x6c7086ff)
                                    } else {
                                        rgba(0xcdd6f4ff)
                                    })
                                    .overflow_x_hidden()
                                    .when(!controls_locked, |button| {
                                        button.cursor_pointer().on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.history_dropdown_open =
                                                    !this.history_dropdown_open;
                                                this.model_dropdown_open = false;
                                                this.toolbar_menu_open = false;
                                                cx.notify();
                                            }),
                                        )
                                    })
                                    .child(history_label),
                            )
                            .child(
                                div()
                                    .id("new-chat-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .px_2()
                                    .h(theme.metrics.control_height_small)
                                    .bg(rgb(0xa6e3a1))
                                    .rounded(px(3.0))
                                    .text_sm()
                                    .text_color(if controls_locked {
                                        rgba(0x6c7086ff)
                                    } else {
                                        rgba(0x1e1e2eff)
                                    })
                                    .tooltip(Tooltip::text("Start a new conversation"))
                                    .when(!controls_locked, |button| {
                                        button.cursor_pointer().on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                                this.new_session(cx);
                                            }),
                                        )
                                    })
                                    .child(Icon::new(
                                        IconName::Plus,
                                        IconSize::Medium,
                                        if controls_locked {
                                            rgba(0x6c7086ff)
                                        } else {
                                            rgba(0x1e1e2eff)
                                        },
                                    )),
                            )
                            .child({
                                let mut spacer = div();
                                spacer.style().flex_grow = Some(1.0);
                                spacer
                            })
                            .child(
                                div()
                                    .id("assistant-options-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .px_2()
                                    .h(theme.metrics.control_height_small)
                                    .rounded(px(3.0))
                                    .text_color(rgba(0xa6adc8ff))
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text("More Assistant actions"))
                                    .hover(|style| style.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            this.toolbar_menu_open = !this.toolbar_menu_open;
                                            this.history_dropdown_open = false;
                                            this.model_dropdown_open = false;
                                            cx.notify();
                                        }),
                                    )
                                    .child(Icon::new(
                                        IconName::MoreHorizontal,
                                        IconSize::Medium,
                                        rgba(0xa6adc8ff),
                                    )),
                            )
                            .child(
                                div()
                                    .id("assistant-zoom-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .px_2()
                                    .h(theme.metrics.control_height_small)
                                    .rounded(px(3.0))
                                    .text_sm()
                                    .text_color(rgba(0xa6adc8ff))
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text(if self.zoomed {
                                        "Restore Assistant panel"
                                    } else {
                                        "Maximize Assistant panel"
                                    }))
                                    .hover(|style| style.bg(rgba(0x45475a88)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|_this, _: &MouseDownEvent, _window, cx| {
                                            cx.emit(ToggleAssistantZoomRequested);
                                        }),
                                    )
                                    .child(Icon::new(
                                        IconName::Maximize,
                                        IconSize::Medium,
                                        rgba(0xa6adc8ff),
                                    )),
                            ),
                    )
                    .when(history_dropdown_open, |header| {
                        header.child(
                            div()
                                .id("history-dropdown")
                                .flex()
                                .flex_col()
                                .flex_none()
                                .w_full()
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                .rounded(px(3.0))
                                .mt_1()
                                .h(px(200.0))
                                .overflow_hidden()
                                .child({
                                    let mut el = history_list_el;
                                    el.style().flex_grow = Some(1.0);
                                    el.style().flex_shrink = Some(1.0);
                                    el.style().flex_basis = Some(relative(0.).into());
                                    el
                                }),
                        )
                    })
                    .when(toolbar_menu_open, |header| {
                        header.child(
                            div()
                                .id("assistant-options-menu")
                                .flex()
                                .flex_col()
                                .w_full()
                                .bg(rgb(0x313244))
                                .border_1()
                                .border_color(rgb(0x45475a))
                                .rounded(px(3.0))
                                .mt_1()
                                .child(
                                    div()
                                        .id("assistant-reload-model")
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .h(theme.metrics.control_height)
                                        .px_2()
                                        .text_sm()
                                        .text_color(if controls_locked || !has_provider {
                                            rgba(0x6c7086ff)
                                        } else {
                                            rgba(0xcdd6f4ff)
                                        })
                                        .when(!controls_locked && has_provider, |item| {
                                            item.cursor_pointer()
                                                .hover(|style| style.bg(rgba(0x45475a88)))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this, _: &MouseDownEvent, _window, cx| {
                                                            this.reload_selected_profile(cx);
                                                        },
                                                    ),
                                                )
                                        })
                                        .child(Icon::new(
                                            IconName::Refresh,
                                            IconSize::Medium,
                                            if controls_locked || !has_provider {
                                                rgba(0x6c7086ff)
                                            } else {
                                                rgba(0xa6adc8ff)
                                            },
                                        ))
                                        .child(if profile_reloading {
                                            "Reloading model…"
                                        } else {
                                            "Reload selected model"
                                        }),
                                ),
                        )
                    }),
            )
            // ── Message area (virtualized list) ──────────────────────────────
            .child({
                let mut msg_area = div()
                    .id("chat-messages")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            let mut changed = false;
                            if this.history_dropdown_open
                                || this.model_dropdown_open
                                || this.toolbar_menu_open
                            {
                                this.history_dropdown_open = false;
                                this.model_dropdown_open = false;
                                this.toolbar_menu_open = false;
                                changed = true;
                            }
                            if this.context_menu.is_some() {
                                this.context_menu = None;
                                changed = true;
                            }
                            if changed {
                                cx.notify();
                            }
                        }),
                    )
                    .child({
                        let mut el = list_el;
                        el.style().flex_grow = Some(1.0);
                        el.style().flex_shrink = Some(1.0);
                        el.style().flex_basis = Some(relative(0.).into());
                        el
                    });
                if streaming {
                    msg_area = msg_area.child(
                        div()
                            .id("streaming-indicator")
                            .flex_none()
                            .px_2()
                            .py_1()
                            .text_sm()
                            .text_color(rgba(0xf9e2afff))
                            .child("Generating…"),
                    );
                }
                msg_area.style().flex_grow = Some(1.0);
                msg_area.style().flex_shrink = Some(1.0);
                msg_area.style().flex_basis = Some(relative(0.).into());
                msg_area
            })
            // ── Input area ───────────────────────────────────────────────────
            .child(
                div()
                    .id("chat-input-area")
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w_full()
                    .border_t_1()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            let mut changed = false;
                            if this.history_dropdown_open
                                || this.model_dropdown_open
                                || this.toolbar_menu_open
                            {
                                this.history_dropdown_open = false;
                                this.model_dropdown_open = false;
                                this.toolbar_menu_open = false;
                                changed = true;
                            }
                            if this.context_menu.is_some() {
                                this.context_menu = None;
                                changed = true;
                            }
                            if changed {
                                cx.notify();
                            }
                        }),
                    )
                    // Right-click anywhere in the input area opens the context
                    // menu. Copy text = current selection in the input field (or
                    // empty, leaving only Paste useful). Message rows install
                    // their own Right handler that captures per-row text.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                            let pos = event.position;
                            let text = this
                                .input_field
                                .read(cx)
                                .selected_text()
                                .unwrap_or_default();
                            this.context_menu = Some(ContextMenuState {
                                position: pos,
                                text,
                            });
                            cx.notify();
                        }),
                    )
                    .border_color(rgb(0x313244))
                    .px_2()
                    .py_1()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id("submit-toggle-row")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .id("enter-toggle")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(14.0))
                                    .h(px(14.0))
                                    .border_1()
                                    .border_color(rgb(0x45475a))
                                    .rounded(px(2.0))
                                    .cursor_pointer()
                                    .when(enter_to_submit, |el| el.bg(rgba(0x89b4faff)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                            this.enter_to_submit = !this.enter_to_submit;
                                            this.input_field.update(cx, |field, _cx| {
                                                field.submit_on_enter = this.enter_to_submit;
                                            });
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgba(0x6c7086ff))
                                    .child("Enter to submit"),
                            ),
                    )
                    .child(
                        div()
                            .id("input-row")
                            .flex()
                            .flex_row()
                            .items_end()
                            .gap(px(4.0))
                            .child({
                                let mut field_container = div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .child(self.input_field.clone());
                                field_container.style().flex_grow = Some(1.0);
                                field_container.style().flex_shrink = Some(1.0);
                                field_container.style().flex_basis = Some(relative(0.).into());
                                field_container
                            })
                            .child({
                                #[derive(Clone, Copy)]
                                enum BtnState {
                                    Connect,
                                    Connecting,
                                    Reloading,
                                    Send,
                                    Stop,
                                }
                                let btn_state = if streaming {
                                    BtnState::Stop
                                } else if profile_reloading {
                                    BtnState::Reloading
                                } else if connecting {
                                    BtnState::Connecting
                                } else if !has_provider {
                                    BtnState::Connect
                                } else {
                                    BtnState::Send
                                };
                                let (label, bg, fg) = match btn_state {
                                    BtnState::Connect => {
                                        ("Connect", rgb(0xf9e2af_u32), rgba(0x1e1e2eff_u32))
                                    }
                                    BtnState::Connecting => {
                                        ("Connecting…", rgb(0x45475a_u32), rgba(0x6c7086ff_u32))
                                    }
                                    BtnState::Reloading => {
                                        ("Loading…", rgb(0x45475a_u32), rgba(0x6c7086ff_u32))
                                    }
                                    BtnState::Send => {
                                        ("Send", rgb(0x89b4fa_u32), rgba(0x1e1e2eff_u32))
                                    }
                                    BtnState::Stop => {
                                        ("Stop", rgb(0xf38ba8_u32), rgba(0x1e1e2eff_u32))
                                    }
                                };
                                let button_child = if matches!(btn_state, BtnState::Send) {
                                    Icon::new(IconName::Send, IconSize::Large, fg)
                                        .rotate_degrees(90.0)
                                        .into_any_element()
                                } else {
                                    div().child(label).into_any_element()
                                };
                                div()
                                    .id("send-btn")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .w(theme.metrics.control_height_small * 2.0)
                                    .h(theme.metrics.control_height)
                                    .bg(bg)
                                    .rounded(px(3.0))
                                    .text_base()
                                    .text_color(fg)
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text(match btn_state {
                                        BtnState::Connect => "Connect the Assistant",
                                        BtnState::Connecting => "Connecting the Assistant",
                                        BtnState::Reloading => "Loading the selected model",
                                        BtnState::Send => "Send message",
                                        BtnState::Stop => "Stop response",
                                    }))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |this, _: &MouseDownEvent, _window, cx| {
                                                match btn_state {
                                                    BtnState::Connect => cx.emit(ConnectRequested),
                                                    BtnState::Connecting | BtnState::Reloading => {}
                                                    BtnState::Send => this.do_send(cx),
                                                    BtnState::Stop => this.stop_stream(cx),
                                                }
                                            },
                                        ),
                                    )
                                    .child(button_child)
                            }),
                    )
                    .when_some(connect_error, |el, err| {
                        el.child(
                            div()
                                .id("connect-error")
                                .text_base()
                                .text_color(rgba(0xf38ba8ff))
                                .child(err),
                        )
                    })
                    .when(profile_reloading, |el| {
                        el.child(
                            div()
                                .id("profile-reloading")
                                .text_sm()
                                .text_color(rgba(0xf9e2afff))
                                .child("Loading the selected Assistant model…"),
                        )
                    })
                    .when_some(profile_reload_error, |el, err| {
                        el.child(
                            div()
                                .id("profile-reload-error")
                                .text_sm()
                                .text_color(rgba(0xf38ba8ff))
                                .child(err),
                        )
                    })
                    .child(
                        div()
                            .id("model-selector-row")
                            .flex()
                            .flex_col()
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgba(0x6c7086ff))
                                            .child("Model:"),
                                    )
                                    .child(
                                        div()
                                            .id("model-selector-btn")
                                            .debug_selector(|| "model-selector-btn".to_string())
                                            .flex()
                                            .items_center()
                                            .px_2()
                                            .h(theme.metrics.control_height)
                                            .bg(rgb(0x313244))
                                            .border_1()
                                            .border_color(rgb(0x45475a))
                                            .rounded(px(3.0))
                                            .text_sm()
                                            .text_color(if has_provider && !controls_locked {
                                                rgba(0xcdd6f4ff)
                                            } else {
                                                rgba(0x6c7086ff)
                                            })
                                            .when(!controls_locked && has_provider, |toggle| {
                                                toggle.cursor_pointer().on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this, _: &MouseDownEvent, _window, cx| {
                                                            // This control lives inside the input
                                                            // area's general menu-dismissal handler.
                                                            // Keep the opening press from bubbling
                                                            // up and immediately closing the menu.
                                                            cx.stop_propagation();
                                                            this.model_dropdown_open =
                                                                !this.model_dropdown_open;
                                                            this.history_dropdown_open = false;
                                                            this.toolbar_menu_open = false;
                                                            cx.notify();
                                                        },
                                                    ),
                                                )
                                            })
                                            .overflow_x_hidden()
                                            .child(model_label),
                                    )
                                    .child(
                                        div()
                                            .id("reasoning-toggle")
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .size(theme.metrics.control_height)
                                            .bg(if reasoning_enabled {
                                                rgb(0x45475a)
                                            } else {
                                                rgb(0x313244)
                                            })
                                            .border_1()
                                            .border_color(rgb(0x45475a))
                                            .rounded(px(3.0))
                                            .text_sm()
                                            .text_color(if reasoning_enabled {
                                                rgba(0xcdd6f4ff)
                                            } else {
                                                rgba(0x6c7086ff)
                                            })
                                            .tooltip(Tooltip::text(if !reasoning_capable {
                                                "Thinking follows this model's default"
                                            } else if reasoning_enabled {
                                                "Disable thinking"
                                            } else {
                                                "Enable thinking"
                                            }))
                                            .when(!controls_locked && reasoning_capable, |toggle| {
                                                toggle.cursor_pointer().on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(
                                                        |this, _: &MouseDownEvent, _window, cx| {
                                                            this.reasoning_enabled =
                                                                !this.reasoning_enabled;
                                                            this.profile_reload_state =
                                                                ProfileReloadState::Ready;
                                                            cx.notify();
                                                        },
                                                    ),
                                                )
                                            })
                                            .child(Icon::new(
                                                IconName::Thinking,
                                                IconSize::Medium,
                                                if !reasoning_capable {
                                                    rgba(0xa6adc8ff)
                                                } else if reasoning_enabled {
                                                    rgba(0xa6e3a1ff)
                                                } else {
                                                    rgba(0xf38ba8ff)
                                                },
                                            )),
                                    ),
                            )
                            .when(model_dropdown_open, |container| {
                                container.child(
                                    div()
                                        .id("model-dropdown")
                                        .debug_selector(|| "model-dropdown".to_string())
                                        .flex()
                                        .flex_col()
                                        .w_full()
                                        .bg(rgb(0x313244))
                                        .border_1()
                                        .border_color(rgb(0x45475a))
                                        .rounded(px(3.0))
                                        .mt_1()
                                        .max_h(px(200.0))
                                        .overflow_y_scroll()
                                        .children(model_items),
                                )
                            }),
                    ),
            );
        // ── Right-click context menu overlay ─────────────────────────────────
        let root = root.when_some(ctx_pos, |root, pos| {
            root.child(deferred(
                anchored().position(pos).anchor(Corner::TopLeft).child(
                    div()
                        .id("ctx-menu")
                        .w(px(140.0))
                        .bg(rgb(0x313244))
                        .border_1()
                        .border_color(rgb(0x45475a))
                        .rounded(px(3.0))
                        .child(
                            div()
                                .id("ctx-copy")
                                .flex()
                                .items_center()
                                .h(px(24.0))
                                .px_3()
                                .text_sm()
                                .text_color(rgba(0xcdd6f4ff))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgba(0x45475a88)))
                                .on_mouse_down(MouseButton::Left, ctx_copy_listener)
                                .child("Copy"),
                        )
                        .child(div().h(px(1.0)).w_full().bg(rgb(0x45475a)))
                        .child(
                            div()
                                .id("ctx-paste")
                                .flex()
                                .items_center()
                                .h(px(24.0))
                                .px_3()
                                .text_sm()
                                .text_color(rgba(0xcdd6f4ff))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgba(0x45475a88)))
                                .on_mouse_down(MouseButton::Left, ctx_paste_listener)
                                .child("Paste"),
                        ),
                ),
            ))
        });

        self.last_render_us = render_start.elapsed().as_micros() as u64;
        root
    }
}
