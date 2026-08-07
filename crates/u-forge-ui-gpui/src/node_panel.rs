use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use gpui::{
    App, Context, Corner, Entity, FocusHandle, Focusable, ListAlignment, ListState, MouseButton,
    MouseDownEvent, Pixels, Point, Window, anchored, deferred, div, list, point, prelude::*, px,
    relative, rgb, rgba,
};
use parking_lot::RwLock;
use u_forge_core::ObjectId;
use u_forge_graph_view::GraphSnapshot;
use u_forge_ui_traits::node_color_for_type;

use crate::selection_model::SelectionModel;
use crate::ui::components::{ContextMenu, Tooltip};
use crate::ui::icons::{Icon, IconName, IconSize};
use crate::{
    WorldActivateRow, WorldDeleteRow, WorldNextRow, WorldOpenContextMenu, WorldPreviousRow,
};

pub(crate) struct CreateNodeRequest(pub String);
pub(crate) struct DeleteNodeRequest(pub ObjectId);

#[derive(Clone)]
struct TypeGroup {
    type_name: String,
    entries: Vec<(String, ObjectId)>,
}

#[derive(Clone)]
enum WorldRow {
    Group {
        type_name: String,
        count: usize,
        collapsed: bool,
    },
    Item {
        name: String,
        node_id: ObjectId,
    },
}

#[derive(Clone)]
enum WorldContextTarget {
    Group(String),
    Item(ObjectId),
}

struct WorldContextMenuState {
    position: Point<Pixels>,
    target: WorldContextTarget,
}

pub(crate) struct NodePanel {
    focus: FocusHandle,
    selection: Entity<SelectionModel>,
    snapshot: Arc<RwLock<GraphSnapshot>>,
    groups: Vec<TypeGroup>,
    collapsed: HashSet<String>,
    visible_rows: Vec<WorldRow>,
    list_state: ListState,
    cursor_index: Option<usize>,
    context_menu: Option<WorldContextMenuState>,
}

impl gpui::EventEmitter<CreateNodeRequest> for NodePanel {}
impl gpui::EventEmitter<DeleteNodeRequest> for NodePanel {}

impl NodePanel {
    pub(crate) fn new(
        snapshot: Arc<RwLock<GraphSnapshot>>,
        selection: Entity<SelectionModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let groups = Self::build_groups(&snapshot.read());
        let collapsed = HashSet::new();
        let visible_rows = Self::flatten_rows(&groups, &collapsed);
        Self {
            focus: cx.focus_handle(),
            selection,
            snapshot,
            groups,
            collapsed,
            list_state: ListState::new(visible_rows.len(), ListAlignment::Top, px(200.0)),
            visible_rows,
            cursor_index: None,
            context_menu: None,
        }
    }

    pub(crate) fn refresh_groups(&mut self, cx: &mut Context<Self>) {
        self.groups = Self::build_groups(&self.snapshot.read());
        self.collapsed
            .retain(|name| self.groups.iter().any(|group| group.type_name == *name));
        self.rebuild_visible_rows();
        cx.notify();
    }

    fn build_groups(snapshot: &GraphSnapshot) -> Vec<TypeGroup> {
        let mut by_type: BTreeMap<String, Vec<(String, ObjectId)>> = BTreeMap::new();
        for node in &snapshot.nodes {
            by_type
                .entry(node.object_type.clone())
                .or_default()
                .push((node.name.clone(), node.id));
        }
        let mut groups = by_type
            .into_iter()
            .map(|(type_name, mut entries)| {
                entries.sort_by_key(|entry| entry.0.to_lowercase());
                TypeGroup { type_name, entries }
            })
            .collect::<Vec<_>>();
        groups.sort_by_key(|group| group.type_name.to_lowercase());
        groups
    }

    fn flatten_rows(groups: &[TypeGroup], collapsed: &HashSet<String>) -> Vec<WorldRow> {
        let mut rows = Vec::new();
        for group in groups {
            let is_collapsed = collapsed.contains(&group.type_name);
            rows.push(WorldRow::Group {
                type_name: group.type_name.clone(),
                count: group.entries.len(),
                collapsed: is_collapsed,
            });
            if !is_collapsed {
                rows.extend(group.entries.iter().map(|(name, node_id)| WorldRow::Item {
                    name: name.clone(),
                    node_id: *node_id,
                }));
            }
        }
        rows
    }

    fn rebuild_visible_rows(&mut self) {
        self.visible_rows = Self::flatten_rows(&self.groups, &self.collapsed);
        self.list_state.reset(self.visible_rows.len());
        self.cursor_index = self
            .cursor_index
            .map(|index| index.min(self.visible_rows.len().saturating_sub(1)))
            .filter(|_| !self.visible_rows.is_empty());
    }

    fn toggle_group(&mut self, type_name: &str) {
        if !self.collapsed.remove(type_name) {
            self.collapsed.insert(type_name.to_string());
        }
        self.rebuild_visible_rows();
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.visible_rows.is_empty() {
            self.cursor_index = None;
            return;
        }
        let current = self.cursor_index.unwrap_or(if delta < 0 {
            self.visible_rows.len()
        } else {
            usize::MAX
        });
        let next = if delta < 0 {
            current.wrapping_sub(1) % self.visible_rows.len()
        } else {
            current.wrapping_add(1) % self.visible_rows.len()
        };
        self.cursor_index = Some(next);
        self.list_state.scroll_to_reveal_item(next);
        if let WorldRow::Item { node_id, .. } = self.visible_rows[next] {
            self.selection.update(cx, |selection, cx| {
                selection.select_by_id(Some(node_id), cx)
            });
        }
        cx.notify();
    }

    fn activate_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.cursor_index else {
            return;
        };
        match self.visible_rows.get(index).cloned() {
            Some(WorldRow::Group { type_name, .. }) => self.toggle_group(&type_name),
            Some(WorldRow::Item { node_id, .. }) => self.selection.update(cx, |selection, cx| {
                selection.select_by_id(Some(node_id), cx)
            }),
            None => return,
        }
        cx.notify();
    }

    fn delete_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.cursor_index else {
            return;
        };
        if let Some(WorldRow::Item { node_id, .. }) = self.visible_rows.get(index) {
            cx.emit(DeleteNodeRequest(*node_id));
        }
    }

    fn open_cursor_context_menu(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.cursor_index else {
            return;
        };
        let target = match self.visible_rows.get(index) {
            Some(WorldRow::Group { type_name, .. }) => WorldContextTarget::Group(type_name.clone()),
            Some(WorldRow::Item { node_id, .. }) => WorldContextTarget::Item(*node_id),
            None => return,
        };
        self.context_menu = Some(WorldContextMenuState {
            position: point(px(24.0), px(52.0)),
            target,
        });
        cx.notify();
    }
}

impl Focusable for NodePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn node_type_color(object_type: &str) -> u32 {
    let [red, green, blue, _] = node_color_for_type(object_type);
    ((red as u32) << 16) | ((green as u32) << 8) | blue as u32
}

fn display_name(name: &str) -> String {
    if name.chars().count() <= 32 {
        return name.to_string();
    }
    let mut display = name.chars().take(31).collect::<String>();
    display.push('…');
    display
}

impl Render for NodePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel_focused = self.focus.contains_focused(window, cx);
        let context_menu = self
            .context_menu
            .as_ref()
            .map(|menu| (menu.position, menu.target.clone()));
        let entity = cx.entity().clone();
        let list_entity = entity.clone();
        let mut rows = list(
            self.list_state.clone(),
            move |index, _window, cx: &mut App| {
                let panel = list_entity.read(cx);
                let Some(row) = panel.visible_rows.get(index).cloned() else {
                    return div().into_any_element();
                };
                let selected_id = panel.selection.read(cx).selected_node_id;
                let cursor_index = panel.cursor_index;

                match row {
                    WorldRow::Group {
                        type_name,
                        count,
                        collapsed,
                    } => {
                        let toggle_entity = list_entity.clone();
                        let toggle_name = type_name.clone();
                        let add_entity = list_entity.clone();
                        let add_name = type_name.clone();
                        let cursor_entity = list_entity.clone();
                        let context_entity = list_entity.clone();
                        let context_name = type_name.clone();
                        let toggle_tooltip = if collapsed {
                            format!("Expand {type_name}")
                        } else {
                            format!("Collapse {type_name}")
                        };
                        let add_tooltip = format!("Add {type_name}");
                        div()
                            .id(("world-group", index))
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(24.0))
                            .px_2()
                            .text_base()
                            .when(cursor_index == Some(index), |row| row.bg(rgba(0x45475a66)))
                            .child(
                                div()
                                    .id(("world-group-toggle", index))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .min_w_0()
                                    .text_color(rgb(node_type_color(&type_name)))
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text(toggle_tooltip))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_event: &MouseDownEvent, _window, cx: &mut App| {
                                            toggle_entity.update(cx, |panel, cx| {
                                                panel.cursor_index = Some(index);
                                                panel.toggle_group(&toggle_name);
                                                cx.notify();
                                            });
                                        },
                                    )
                                    .child(Icon::new(
                                        if collapsed {
                                            IconName::ChevronRight
                                        } else {
                                            IconName::ChevronDown
                                        },
                                        IconSize::Small,
                                        rgb(node_type_color(&type_name)),
                                    ))
                                    .child(format!("{type_name} ({count})")),
                            )
                            .child(
                                div()
                                    .id(("world-group-add", index))
                                    .flex()
                                    .items_center()
                                    .h(px(20.0))
                                    .px_2()
                                    .rounded(px(3.0))
                                    .text_xs()
                                    .text_color(rgba(0xa6e3a1ff))
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text(add_tooltip))
                                    .hover(|style| style.bg(rgba(0xa6e3a122)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_event: &MouseDownEvent, _window, cx: &mut App| {
                                            add_entity.update(cx, |panel, cx| {
                                                panel.collapsed.remove(&add_name);
                                                panel.rebuild_visible_rows();
                                                cx.emit(CreateNodeRequest(add_name.clone()));
                                                cx.notify();
                                            });
                                        },
                                    )
                                    .child(Icon::new(
                                        IconName::Plus,
                                        IconSize::Small,
                                        rgba(0xa6e3a1ff),
                                    )),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_event: &MouseDownEvent, _window, cx: &mut App| {
                                    cursor_entity.update(cx, |panel, _cx| {
                                        panel.cursor_index = Some(index);
                                    });
                                },
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                move |event: &MouseDownEvent, window, cx: &mut App| {
                                    context_entity.update(cx, |panel, cx| {
                                        panel.cursor_index = Some(index);
                                        panel.focus.focus(window);
                                        panel.context_menu = Some(WorldContextMenuState {
                                            position: event.position,
                                            target: WorldContextTarget::Group(context_name.clone()),
                                        });
                                        cx.notify();
                                    });
                                },
                            )
                            .into_any_element()
                    }
                    WorldRow::Item { name, node_id } => {
                        let is_selected = selected_id == Some(node_id);
                        let select_entity = list_entity.clone();
                        let delete_entity = list_entity.clone();
                        let context_entity = list_entity.clone();
                        let delete_tooltip = format!("Delete {name}");
                        div()
                            .id(("world-item", index))
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(24.0))
                            .pl(px(20.0))
                            .pr_1()
                            .text_base()
                            .cursor_pointer()
                            .text_color(if is_selected {
                                rgba(0xffffffff)
                            } else {
                                rgba(0xa6adc8ff)
                            })
                            .when(is_selected, |row| row.bg(rgba(0x45475aaa)))
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_event: &MouseDownEvent, window, cx: &mut App| {
                                    select_entity.update(cx, |panel, cx| {
                                        panel.cursor_index = Some(index);
                                        panel.focus.focus(window);
                                        panel.selection.update(cx, |selection, cx| {
                                            selection.select_by_id(Some(node_id), cx);
                                        });
                                    });
                                },
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                move |event: &MouseDownEvent, window, cx: &mut App| {
                                    context_entity.update(cx, |panel, cx| {
                                        panel.cursor_index = Some(index);
                                        panel.focus.focus(window);
                                        panel.selection.update(cx, |selection, cx| {
                                            selection.select_by_id(Some(node_id), cx);
                                        });
                                        panel.context_menu = Some(WorldContextMenuState {
                                            position: event.position,
                                            target: WorldContextTarget::Item(node_id),
                                        });
                                        cx.notify();
                                    });
                                },
                            )
                            .child(div().min_w_0().overflow_hidden().child(display_name(&name)))
                            .child(
                                div()
                                    .id(("world-item-delete", index))
                                    .flex()
                                    .items_center()
                                    .h(px(18.0))
                                    .px_1()
                                    .rounded(px(3.0))
                                    .text_xs()
                                    .text_color(rgba(0xf38ba8aa))
                                    .cursor_pointer()
                                    .tooltip(Tooltip::text(delete_tooltip))
                                    .hover(|style| {
                                        style.bg(rgba(0xf38ba822)).text_color(rgba(0xf38ba8ff))
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_event: &MouseDownEvent, _window, cx: &mut App| {
                                            cx.stop_propagation();
                                            delete_entity.update(cx, |_panel, cx| {
                                                cx.emit(DeleteNodeRequest(node_id));
                                            });
                                        },
                                    )
                                    .child(Icon::new(
                                        IconName::Trash,
                                        IconSize::Small,
                                        rgba(0xf38ba8ff),
                                    )),
                            )
                            .into_any_element()
                    }
                }
            },
        );
        rows.style().flex_grow = Some(1.0);
        rows.style().flex_shrink = Some(1.0);
        rows.style().flex_basis = Some(relative(0.0).into());

        let mut list_container = div()
            .id("world-list")
            .flex()
            .flex_col()
            .min_h_0()
            .overflow_hidden()
            .child(rows);
        list_container.style().flex_grow = Some(1.0);
        list_container.style().flex_shrink = Some(1.0);
        list_container.style().flex_basis = Some(relative(0.0).into());

        let root = div()
            .id("node-panel")
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .key_context("WorldPanel")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &WorldNextRow, _window, cx| {
                this.move_cursor(1, cx);
            }))
            .on_action(cx.listener(|this, _: &WorldPreviousRow, _window, cx| {
                this.move_cursor(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &WorldActivateRow, _window, cx| {
                this.activate_cursor(cx);
            }))
            .on_action(cx.listener(|this, _: &WorldDeleteRow, _window, cx| {
                this.delete_cursor(cx);
            }))
            .on_action(cx.listener(|this, _: &WorldOpenContextMenu, _window, cx| {
                this.open_cursor_context_menu(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    if this.context_menu.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .bg(rgb(0x181825))
            .border_r_1()
            .border_color(rgb(0x313244))
            .when(panel_focused, |panel| {
                panel.border_1().border_color(rgba(0xb4befeff))
            })
            .child(
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
                    .child("World"),
            )
            .child(list_container);

        root.when_some(context_menu, |root, (position, target)| {
            let menu_body = match target {
                WorldContextTarget::Group(type_name) => {
                    let toggle_entity = entity.clone();
                    let toggle_name = type_name.clone();
                    let create_entity = entity.clone();
                    div()
                        .w(px(180.0))
                        .child(context_menu_item(
                            "Show / Hide Group",
                            move |_event: &MouseDownEvent, _window, cx: &mut App| {
                                cx.stop_propagation();
                                toggle_entity.update(cx, |panel, cx| {
                                    panel.toggle_group(&toggle_name);
                                    panel.context_menu = None;
                                    cx.notify();
                                });
                            },
                        ))
                        .child(context_menu_item(
                            "New World Item",
                            move |_event: &MouseDownEvent, _window, cx: &mut App| {
                                cx.stop_propagation();
                                create_entity.update(cx, |panel, cx| {
                                    panel.context_menu = None;
                                    panel.collapsed.remove(&type_name);
                                    panel.rebuild_visible_rows();
                                    cx.emit(CreateNodeRequest(type_name.clone()));
                                    cx.notify();
                                });
                            },
                        ))
                        .into_any_element()
                }
                WorldContextTarget::Item(node_id) => {
                    let open_entity = entity.clone();
                    let delete_entity = entity.clone();
                    div()
                        .w(px(180.0))
                        .child(context_menu_item(
                            "Open Details",
                            move |_event: &MouseDownEvent, window, cx: &mut App| {
                                cx.stop_propagation();
                                open_entity.update(cx, |panel, cx| {
                                    panel.context_menu = None;
                                    panel.focus.focus(window);
                                    panel.selection.update(cx, |selection, cx| {
                                        selection.select_by_id(Some(node_id), cx);
                                    });
                                    cx.notify();
                                });
                            },
                        ))
                        .child(context_menu_item(
                            "Delete…",
                            move |_event: &MouseDownEvent, _window, cx: &mut App| {
                                cx.stop_propagation();
                                delete_entity.update(cx, |panel, cx| {
                                    panel.context_menu = None;
                                    cx.emit(DeleteNodeRequest(node_id));
                                    cx.notify();
                                });
                            },
                        ))
                        .into_any_element()
                }
            };
            root.child(deferred(
                anchored()
                    .position(position)
                    .anchor(Corner::TopLeft)
                    .child(ContextMenu::new("world-context-menu", menu_body)),
            ))
        })
    }
}

fn context_menu_item(
    label: &'static str,
    listener: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .flex()
        .items_center()
        .h(px(26.0))
        .px_3()
        .text_sm()
        .text_color(rgba(0xcdd6f4ff))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0x45475a88)))
        .on_mouse_down(MouseButton::Left, listener)
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattened_rows_preserve_group_and_item_order() {
        let groups = vec![
            TypeGroup {
                type_name: "Location".into(),
                entries: vec![("Alderaan".into(), ObjectId::new_v4())],
            },
            TypeGroup {
                type_name: "NPC".into(),
                entries: vec![("Bryn".into(), ObjectId::new_v4())],
            },
        ];
        let collapsed = HashSet::from(["NPC".to_string()]);
        let rows = NodePanel::flatten_rows(&groups, &collapsed);
        assert_eq!(rows.len(), 3);
        assert!(
            matches!(rows[0], WorldRow::Group { ref type_name, .. } if type_name == "Location")
        );
        assert!(matches!(rows[1], WorldRow::Item { ref name, .. } if name == "Alderaan"));
        assert!(
            matches!(rows[2], WorldRow::Group { ref type_name, collapsed: true, .. } if type_name == "NPC")
        );
    }

    #[test]
    fn initially_expanded_groups_include_every_item_row() {
        let groups = vec![
            TypeGroup {
                type_name: "Location".into(),
                entries: vec![
                    ("Alderaan".into(), ObjectId::new_v4()),
                    ("Bespin".into(), ObjectId::new_v4()),
                ],
            },
            TypeGroup {
                type_name: "NPC".into(),
                entries: vec![("Bryn".into(), ObjectId::new_v4())],
            },
        ];

        let rows = NodePanel::flatten_rows(&groups, &HashSet::new());

        assert_eq!(rows.len(), 5);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, WorldRow::Group { .. }))
                .count(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, WorldRow::Item { .. }))
                .count(),
            3
        );
    }
}
