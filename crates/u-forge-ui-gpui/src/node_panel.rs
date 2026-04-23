use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, relative, rgb, rgba, Context, Entity, MouseButton, MouseDownEvent, Window,
};
use parking_lot::RwLock;
use u_forge_core::ObjectId;
use u_forge_graph_view::GraphSnapshot;
use u_forge_ui_traits::node_color_for_type;

use crate::selection_model::SelectionModel;

// ── Events emitted by NodePanel ─────────────────────────────────────────────

/// Emitted when the user clicks the "+" button on a type group header.
/// Payload is the `object_type` string for the new node.
pub(crate) struct CreateNodeRequest(pub String);

/// Emitted when the user clicks the delete button next to a node entry.
/// Payload is the `ObjectId` of the node to delete.
pub(crate) struct DeleteNodeRequest(pub ObjectId);

// ── Node panel ──────────────────────────────────────────────────────────────

/// A group of nodes sharing the same object_type, for the node panel.
struct TypeGroup {
    type_name: String,
    /// (index into snapshot.nodes, display name, ObjectId)
    entries: Vec<(usize, String, ObjectId)>,
}

/// Sidebar node view listing all nodes grouped by type, alphabetically.
///
/// Emits [`CreateNodeRequest`] and [`DeleteNodeRequest`] events that the
/// parent `AppView` subscribes to in order to perform DB mutations and
/// refresh the snapshot.
pub(crate) struct NodePanel {
    selection: Entity<SelectionModel>,
    snapshot: Arc<RwLock<GraphSnapshot>>,
    /// Pre-sorted groups, rebuilt when the snapshot changes.
    groups: Vec<TypeGroup>,
    /// Which type groups are collapsed (by type_name).
    collapsed: std::collections::HashSet<String>,
}

impl gpui::EventEmitter<CreateNodeRequest> for NodePanel {}
impl gpui::EventEmitter<DeleteNodeRequest> for NodePanel {}

impl NodePanel {
    pub(crate) fn new(
        snapshot: Arc<RwLock<GraphSnapshot>>,
        selection: Entity<SelectionModel>,
    ) -> Self {
        let groups = Self::build_groups(&snapshot.read());
        // Start with all groups collapsed so the panel fits on screen.
        let collapsed: std::collections::HashSet<String> =
            groups.iter().map(|g| g.type_name.clone()).collect();
        Self {
            selection,
            snapshot,
            groups,
            collapsed,
        }
    }

    /// Rebuild groups from the current snapshot. Call this after `refresh_snapshot()`.
    pub(crate) fn refresh_groups(&mut self, cx: &mut gpui::Context<Self>) {
        self.groups = Self::build_groups(&self.snapshot.read());
        cx.notify();
    }

    fn build_groups(snap: &GraphSnapshot) -> Vec<TypeGroup> {
        let mut by_type: BTreeMap<String, Vec<(usize, String, ObjectId)>> = BTreeMap::new();
        for (idx, node) in snap.nodes.iter().enumerate() {
            by_type.entry(node.object_type.clone()).or_default().push((
                idx,
                node.name.clone(),
                node.id,
            ));
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

/// Color for a type header in the node panel — delegates to the shared palette.
fn node_type_color(object_type: &str) -> u32 {
    let [r, g, b, _] = node_color_for_type(object_type);
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

impl Render for NodePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id = self.selection.read(cx).selected_node_id;

        // Outer shell: fixed width, fills parent height, does not grow vertically.
        let mut panel = div()
            .id("node-panel")
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .h_full()
            .min_h_0()
            .bg(rgb(0x181825))
            .border_r_1()
            .border_color(rgb(0x313244));

        // Fixed header
        panel = panel.child(
            div()
                .id("node-header")
                .flex()
                .items_center()
                .h(px(28.0))
                .px_3()
                .flex_none()
                .border_b_1()
                .border_color(rgb(0x313244))
                .text_color(rgba(0xcdd6f4ff))
                .text_base()
                .child("Nodes"),
        );

        // Scrollable content area that fills remaining height.
        let mut scroll_area = div()
            .id("node-scroll")
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
            let type_color = node_type_color(&type_name);
            let count = group.entries.len();
            let collapse_label = format!(
                "{} {} ({})",
                if is_collapsed { "\u{25B8}" } else { "\u{25BE}" },
                type_name,
                count
            );
            let type_name_for_click = type_name.clone();
            let type_name_for_add = type_name.clone();

            // Type group header row: collapse toggle on the left, "+" button on the right.
            scroll_area = scroll_area.child(
                div()
                    .id(("type-group", group_idx))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(24.0))
                    .px_2()
                    .flex_none()
                    .text_base()
                    // Collapse/expand label (left side) — clicking toggles collapse.
                    .child(
                        div()
                            .id(("type-collapse", group_idx))
                            .flex()
                            .items_center()
                            .text_color(rgb(type_color))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let tn = type_name_for_click.clone();
                                    move |this, _: &MouseDownEvent, _window, cx| {
                                        if this.collapsed.contains(&tn) {
                                            this.collapsed.remove(&tn);
                                        } else {
                                            this.collapsed.insert(tn.clone());
                                        }
                                        cx.notify();
                                    }
                                }),
                            )
                            .child(collapse_label),
                    )
                    // "+" add-node button (right side).
                    .child(
                        div()
                            .id(("type-add", group_idx))
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(20.0))
                            .h(px(20.0))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .text_color(rgba(0xa6e3a1ff)) // Catppuccin green
                            .hover(|style| style.bg(rgba(0xa6e3a122)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                    cx.emit(CreateNodeRequest(type_name_for_add.clone()));
                                    // Auto-expand the group so the new node is visible.
                                    this.collapsed.remove(&type_name_for_click);
                                    cx.notify();
                                }),
                            )
                            .child("+"),
                    ),
            );

            // Node entries (if not collapsed)
            if !is_collapsed {
                for (entry_idx, (_node_idx, name, node_id)) in group.entries.iter().enumerate() {
                    let is_selected = selected_id == Some(*node_id);
                    let node_id = *node_id;
                    let node_id_for_delete = node_id;
                    let display_name = if name.len() > 24 {
                        let mut s: String = name.chars().take(23).collect();
                        s.push('\u{2026}');
                        s
                    } else {
                        name.clone()
                    };

                    scroll_area = scroll_area.child(
                        div()
                            .id(("node-entry", group_idx * 10000 + entry_idx))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .h(px(22.0))
                            .pl(px(20.0))
                            .pr(px(4.0))
                            .flex_none()
                            .text_base()
                            .cursor_pointer()
                            .text_color(if is_selected {
                                rgba(0xffffffff)
                            } else {
                                rgba(0xa6adc8ff)
                            })
                            .when(is_selected, |el| el.bg(rgba(0x45475aaa)))
                            // Click on the node name area to select it.
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                    this.selection.update(cx, |sel, cx| {
                                        sel.select_by_id(Some(node_id), cx);
                                    });
                                }),
                            )
                            // Node name on the left.
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .child(display_name),
                            )
                            // Delete button on the right.
                            .child(
                                div()
                                    .id(("node-delete", group_idx * 10000 + entry_idx))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(18.0))
                                    .h(px(18.0))
                                    .rounded(px(3.0))
                                    .flex_none()
                                    .cursor_pointer()
                                    .text_color(rgba(0xf38ba8aa)) // Catppuccin red (muted)
                                    .hover(|style| {
                                        style.bg(rgba(0xf38ba822)).text_color(rgba(0xf38ba8ff))
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |_this, event: &MouseDownEvent, _window, cx| {
                                                // Stop propagation: prevent the parent row's
                                                // on_mouse_down from selecting the node.
                                                let _ = event;
                                                cx.emit(DeleteNodeRequest(node_id_for_delete));
                                            },
                                        ),
                                    )
                                    .child("\u{2715}"), // ✕ multiplication sign
                            ),
                    );
                }
            }
        }

        panel.child(scroll_area)
    }
}
