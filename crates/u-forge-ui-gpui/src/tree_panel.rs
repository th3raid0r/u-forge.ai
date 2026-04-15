use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{div, prelude::*, px, rgb, rgba, relative, Context, Entity, MouseButton, MouseDownEvent, Window};
use parking_lot::RwLock;
use u_forge_core::ObjectId;
use u_forge_graph_view::GraphSnapshot;
use u_forge_ui_traits::node_color_for_type;

use crate::selection_model::SelectionModel;

// ── Tree panel ──────────────────────────────────────────────────────────────

/// A group of nodes sharing the same object_type, for the tree panel.
struct TypeGroup {
    type_name: String,
    /// (index into snapshot.nodes, display name)
    entries: Vec<(usize, String, ObjectId)>,
}

/// Sidebar tree view listing all nodes grouped by type, alphabetically.
pub(crate) struct TreePanel {
    selection: Entity<SelectionModel>,
    /// Pre-sorted groups, rebuilt when the snapshot changes.
    groups: Vec<TypeGroup>,
    /// Which type groups are collapsed (by type_name).
    collapsed: std::collections::HashSet<String>,
}

impl TreePanel {
    pub(crate) fn new(
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

/// Color for a type header in the tree panel — delegates to the shared palette.
fn tree_type_color(object_type: &str) -> u32 {
    let [r, g, b, _] = node_color_for_type(object_type);
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Width of the sidebar in pixels.
pub(crate) const SIDEBAR_W: f32 = 220.0;

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
