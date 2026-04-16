pub(crate) mod field_spec;
mod render;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{prelude::*, Context, Entity, Pixels, Subscription};
use parking_lot::RwLock;
use u_forge_core::{EdgeType, KnowledgeGraph, ObjectId, ObjectMetadata, SchemaManager};
use u_forge_graph_view::GraphSnapshot;

use crate::selection_model::SelectionModel;
use crate::text_field::{TextChanged, TextFieldView};

pub(crate) use field_spec::{EditableEdge, EditorTab, FieldKind};

// ── Edge node-selector dropdown state ─────────────────────────────────────────

/// Tracks the state of a filterable node-selector dropdown used when editing
/// the `from` or `to` endpoint of an edge row.
pub(crate) struct EdgeNodeDropdown {
    /// Index into the active tab's `edited_edges` vec.
    pub(crate) edge_idx: usize,
    /// `false` = editing the "from" endpoint, `true` = editing the "to" endpoint.
    pub(crate) is_target: bool,
    /// The text field entity used to filter the node list.
    pub(crate) filter_entity: Entity<TextFieldView>,
    /// Current filter text (kept in sync via `TextChanged` subscription).
    pub(crate) filter_text: String,
    /// Subscription that keeps the `TextChanged` handler alive.
    pub(crate) _filter_sub: Subscription,
}

// ── Node editor panel ─────────────────────────────────────────────────────────

/// Editor panel with browser-style tabs for editing nodes.
///
/// Observes `SelectionModel` and opens tabs as nodes are selected.
pub(crate) struct NodeEditorPanel {
    pub(crate) tabs: Vec<EditorTab>,
    pub(crate) active_tab: Option<usize>,
    #[allow(dead_code)]
    selection: Entity<SelectionModel>,
    #[allow(dead_code)]
    pub(crate) snapshot: Arc<RwLock<GraphSnapshot>>,
    pub(crate) graph: Arc<KnowledgeGraph>,
    schema_mgr: Arc<SchemaManager>,
    /// Open dropdown field key (for enum fields).
    pub(crate) dropdown_open: Option<String>,
    /// Measured panel size in pixels, updated each frame via canvas measurement.
    pub(crate) panel_size: gpui::Size<Pixels>,
    /// Subscriptions to text field changes — kept alive so events fire.
    _field_subs: Vec<Subscription>,
    _selection_sub: Subscription,
    /// Active inline-add text field for array fields: (field_key, entity, subscription).
    pub(crate) array_add_field: Option<(String, Entity<TextFieldView>, Subscription)>,

    // ── Edge editing state ────────────────────────────────────────────────
    /// Active node-selector dropdown for edge from/to fields.
    pub(crate) edge_node_dropdown: Option<EdgeNodeDropdown>,
    /// Subscriptions to edge-type text field changes (per active tab).
    _edge_type_subs: Vec<Subscription>,
}

impl NodeEditorPanel {
    pub(crate) fn new(
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
                this.edge_node_dropdown = None;
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
                width: gpui::px(900.0),
                height: gpui::px(400.0),
            },
            _field_subs: Vec::new(),
            _selection_sub: sub,
            array_add_field: None,
            edge_node_dropdown: None,
            _edge_type_subs: Vec::new(),
        }
    }

    // ── Tab lifecycle ─────────────────────────────────────────────────────

    /// Open a tab for the given node, or focus the existing one.
    pub(crate) fn open_or_focus_tab(&mut self, node_id: ObjectId, cx: &mut Context<Self>) {
        // Already open?
        if let Some(idx) = self.tabs.iter().position(|t| t.node_id == node_id) {
            self.active_tab = Some(idx);
            self.rebuild_field_subscriptions(cx);
            return;
        }

        // Load the node from DB.
        let meta = match self.graph.get_object(node_id) {
            Ok(Some(m)) => m,
            _ => return,
        };

        self.open_tab_for_metadata(meta, false, cx);
    }

    /// Open a tab for a **newly created** node (from the node panel "+" button).
    ///
    /// The tab is marked `is_new = true` so that on save, if the name is still
    /// empty, the DB record is deleted and the tab discarded.
    pub(crate) fn open_new_node_tab(&mut self, node_id: ObjectId, cx: &mut Context<Self>) {
        // If already open, just focus.
        if let Some(idx) = self.tabs.iter().position(|t| t.node_id == node_id) {
            self.active_tab = Some(idx);
            self.rebuild_field_subscriptions(cx);
            return;
        }

        let meta = match self.graph.get_object(node_id) {
            Ok(Some(m)) => m,
            _ => return,
        };

        self.open_tab_for_metadata(meta, true, cx);
    }

    /// Shared logic for opening a tab from `ObjectMetadata`.
    fn open_tab_for_metadata(
        &mut self,
        meta: ObjectMetadata,
        is_new: bool,
        cx: &mut Context<Self>,
    ) {
        let node_id = meta.id;

        // Load schema for this object type. File-loaded schemas live under
        // "imported_schemas"; fall back to "default" for built-in types.
        let schema = self
            .schema_mgr
            .get_object_type_schema("imported_schemas", &meta.object_type)
            .or_else(|| {
                self.schema_mgr
                    .get_object_type_schema("default", &meta.object_type)
            });

        // Build edited_values from the metadata.
        let mut edited_values = HashMap::new();
        edited_values.insert(
            "name".to_string(),
            serde_json::Value::String(meta.name.clone()),
        );
        edited_values.insert(
            "description".to_string(),
            serde_json::Value::String(meta.description.clone().unwrap_or_default()),
        );
        if let Some(obj) = meta.properties.as_object() {
            for (k, v) in obj {
                if k.eq_ignore_ascii_case("name")
                    || k.eq_ignore_ascii_case("description")
                    || k.eq_ignore_ascii_case("tags")
                {
                    continue;
                }
                edited_values.insert(k.clone(), v.clone());
            }
        }
        edited_values.insert(
            "tags".to_string(),
            serde_json::Value::Array(
                meta.tags
                    .iter()
                    .map(|t| serde_json::Value::String(t.clone()))
                    .collect(),
            ),
        );

        // ── Load edges incident on this node ──────────────────────────────
        let db_edges = self.graph.get_relationships(node_id).unwrap_or_default();

        // Build a name-lookup map from all objects so we can resolve display
        // names for edge endpoints without per-edge DB queries.
        let name_map = self.build_node_name_map();

        let edited_edges: Vec<EditableEdge> = db_edges
            .iter()
            .map(|e| EditableEdge::from_edge(e, &name_map))
            .collect();

        // Create text field entities for edge type strings.
        let edge_type_entities: Vec<Entity<TextFieldView>> = edited_edges
            .iter()
            .map(|ee| {
                let et = ee.edge_type.clone();
                cx.new(|cx| {
                    let mut tf = TextFieldView::new(false, "edge type", cx);
                    tf.set_content(&et, cx);
                    tf
                })
            })
            .collect();

        // ── Create text field entities for property fields ────────────────

        let mut field_entities = HashMap::new();
        let tmp_tab = EditorTab {
            node_id,
            name: meta.name.clone(),
            object_type: meta.object_type.clone(),
            pinned: false,
            original: meta.clone(),
            edited_values: edited_values.clone(),
            schema: schema.clone(),
            dirty: is_new,
            current_page: 0,
            field_entities: HashMap::new(),
            edited_edges: Vec::new(),
            original_edges: Vec::new(),
            edge_type_entities: Vec::new(),
            is_new,
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
                        let val_str: String = edited_values
                            .get(&key)
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default();
                        tf.set_content(&val_str, cx);
                        tf
                    });
                    field_entities.insert(spec.key.clone(), entity);
                }
                FieldKind::Enum(_) => {
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

        let new_tab = EditorTab {
            node_id,
            name: meta.name.clone(),
            object_type: meta.object_type.clone(),
            pinned: false,
            original: meta,
            edited_values,
            schema,
            dirty: is_new,
            current_page: 0,
            field_entities,
            edited_edges,
            original_edges: db_edges,
            edge_type_entities,
            is_new,
        };

        // Replace the first unpinned non-dirty tab, or append.
        if let Some(idx) = self.tabs.iter().position(|t| !t.pinned && !t.dirty) {
            self.tabs[idx] = new_tab;
            self.active_tab = Some(idx);
        } else {
            self.tabs.push(new_tab);
            self.active_tab = Some(self.tabs.len() - 1);
        }

        self.rebuild_field_subscriptions(cx);
    }

    /// Close a tab by index.
    pub(crate) fn close_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        self.edge_node_dropdown = None;
        self.array_add_field = None;
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
        self.rebuild_field_subscriptions(cx);
    }

    /// Remove stale edge references to a deleted node from all open tabs.
    ///
    /// After a node is deleted, any edge rows in other tabs that reference the
    /// deleted node become invalid.  This method removes those edge rows,
    /// refreshes `original_edges` from the DB, and recomputes dirty state.
    pub(crate) fn remove_stale_edge_refs(&mut self, deleted_id: ObjectId) {
        self.edge_node_dropdown = None;

        for tab in &mut self.tabs {
            // Find indices of edited_edges that reference the deleted node.
            let stale: Vec<usize> = tab
                .edited_edges
                .iter()
                .enumerate()
                .filter(|(_, e)| e.from == Some(deleted_id) || e.to == Some(deleted_id))
                .map(|(i, _)| i)
                .collect();

            if stale.is_empty() {
                continue;
            }

            // Remove in reverse order so indices stay valid.
            for &i in stale.iter().rev() {
                tab.edited_edges.remove(i);
                if i < tab.edge_type_entities.len() {
                    tab.edge_type_entities.remove(i);
                }
            }

            // Refresh original_edges from DB.
            tab.original_edges = self
                .graph
                .get_relationships(tab.node_id)
                .unwrap_or_default();

            tab.recompute_dirty();
        }
    }

    // ── Save ──────────────────────────────────────────────────────────────

    /// Collect dirty tabs and save them to the DB.
    ///
    /// Returns `(count, saved_ids, discarded_ids)`:
    /// - `count`: how many nodes were actually persisted.
    /// - `saved_ids`: ObjectIds that need re-chunking/re-embedding.
    /// - `discarded_ids`: ObjectIds of "empty new" nodes that were deleted.
    pub(crate) fn save_dirty_tabs(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (usize, Vec<ObjectId>, Vec<ObjectId>) {
        let mut saved = 0usize;
        let mut saved_ids = Vec::new();
        let mut discarded_ids = Vec::new();

        // First pass: identify empty-new tabs to discard.
        let mut discard_indices = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            if tab.is_new && tab.name.trim().is_empty() {
                discard_indices.push(i);
                discarded_ids.push(tab.node_id);
            }
        }
        // Delete DB records for discarded nodes (reverse order to keep indices valid).
        for &idx in discard_indices.iter().rev() {
            let node_id = self.tabs[idx].node_id;
            let _ = self.graph.delete_object(node_id);
            self.tabs.remove(idx);
        }
        // Fix active_tab after removals.
        if self.tabs.is_empty() {
            self.active_tab = None;
        } else if let Some(active) = self.active_tab {
            if active >= self.tabs.len() {
                self.active_tab = Some(self.tabs.len() - 1);
            }
        }

        // Second pass: persist dirty tabs.
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
                // ── Save edge changes ─────────────────────────────────────
                Self::save_edges_for_tab(&self.graph, tab);

                saved_ids.push(tab.node_id);
                tab.original = meta;
                tab.dirty = false;
                tab.is_new = false;

                // Refresh original_edges so subsequent dirty checks are correct.
                tab.original_edges = self
                    .graph
                    .get_relationships(tab.node_id)
                    .unwrap_or_default();

                saved += 1;
            }
        }
        cx.notify();
        self.rebuild_field_subscriptions(cx);
        (saved, saved_ids, discarded_ids)
    }

    /// Persist edge changes for a single tab: delete removed edges and add new ones.
    ///
    /// Takes `graph` explicitly (rather than `&self`) so this can be called
    /// while iterating over `&mut self.tabs` without a borrow conflict.
    fn save_edges_for_tab(graph: &KnowledgeGraph, tab: &EditorTab) {
        // Build sets of (from, to, type) triples for comparison.
        let orig_set: Vec<(ObjectId, ObjectId, String)> = tab
            .original_edges
            .iter()
            .map(|e| (e.from, e.to, e.edge_type.as_str().to_string()))
            .collect();

        let edited_set: Vec<(ObjectId, ObjectId, String)> = tab
            .edited_edges
            .iter()
            .filter(|e| e.is_complete())
            .map(|e| {
                (
                    e.from.unwrap(),
                    e.to.unwrap(),
                    e.edge_type.trim().to_string(),
                )
            })
            .collect();

        // Delete edges that were in original but not in edited.
        for (from, to, et) in &orig_set {
            if !edited_set
                .iter()
                .any(|(f, t, e)| f == from && t == to && e == et)
            {
                let _ = graph.delete_edge(*from, *to, et);
            }
        }

        // Add edges that are in edited but not in original.
        for (from, to, et) in &edited_set {
            if !orig_set
                .iter()
                .any(|(f, t, e)| f == from && t == to && e == et)
            {
                let _ = graph.connect_objects(*from, *to, EdgeType::new(et.clone()));
            }
        }
    }

    /// Return true if any tab has unsaved changes.
    #[allow(dead_code)]
    pub(crate) fn has_dirty_tabs(&self) -> bool {
        self.tabs.iter().any(|t| t.dirty)
    }

    // ── Array inline add ──────────────────────────────────────────────────

    /// Commit the inline array-add text field: push its content into the array
    /// and close the inline editor.
    pub(crate) fn commit_array_add(&mut self, cx: &mut Context<Self>) {
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

    // ── Edge editing helpers ──────────────────────────────────────────────

    /// Add a new empty edge row to the active tab and create its type-field entity.
    pub(crate) fn add_edge_row(&mut self, cx: &mut Context<Self>) {
        if let Some(tab_idx) = self.active_tab {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.edited_edges.push(EditableEdge::empty());
                let entity = cx.new(|cx| TextFieldView::new(false, "edge type", cx));
                tab.edge_type_entities.push(entity);
                tab.recompute_dirty();
            }
        }
        self.rebuild_edge_type_subscriptions(cx);
        cx.notify();
    }

    /// Remove an edge row from the active tab by index.
    pub(crate) fn remove_edge_row(&mut self, edge_idx: usize, cx: &mut Context<Self>) {
        // Close any open dropdown that references this or later indices.
        self.edge_node_dropdown = None;
        if let Some(tab_idx) = self.active_tab {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                if edge_idx < tab.edited_edges.len() {
                    tab.edited_edges.remove(edge_idx);
                }
                if edge_idx < tab.edge_type_entities.len() {
                    tab.edge_type_entities.remove(edge_idx);
                }
                tab.recompute_dirty();
            }
        }
        self.rebuild_edge_type_subscriptions(cx);
        cx.notify();
    }

    /// Open the filterable node-selector dropdown for an edge endpoint.
    pub(crate) fn open_edge_dropdown(
        &mut self,
        edge_idx: usize,
        is_target: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        // Close any existing dropdown first.
        self.edge_node_dropdown = None;
        self.dropdown_open = None;

        let filter_entity = cx.new(|cx| TextFieldView::new(false, "search nodes\u{2026}", cx));
        window.focus(&filter_entity.read(cx).focus);

        let sub = cx.subscribe(&filter_entity, {
            move |this: &mut Self, _tf, event: &TextChanged, cx| {
                if let Some(dd) = &mut this.edge_node_dropdown {
                    dd.filter_text = event.0.clone();
                }
                cx.notify();
            }
        });

        self.edge_node_dropdown = Some(EdgeNodeDropdown {
            edge_idx,
            is_target,
            filter_entity,
            filter_text: String::new(),
            _filter_sub: sub,
        });
        cx.notify();
    }

    /// Select a node in the currently-open edge dropdown and close it.
    pub(crate) fn select_edge_node(
        &mut self,
        node_id: ObjectId,
        node_name: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(dd) = self.edge_node_dropdown.take() {
            if let Some(tab_idx) = self.active_tab {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    if let Some(edge) = tab.edited_edges.get_mut(dd.edge_idx) {
                        if dd.is_target {
                            edge.to = Some(node_id);
                            edge.to_name = node_name;
                        } else {
                            edge.from = Some(node_id);
                            edge.from_name = node_name;
                        }
                        tab.recompute_dirty();
                    }
                }
            }
        }
        cx.notify();
    }

    // ── Subscription management ───────────────────────────────────────────

    /// Rebuild property-field text change subscriptions for the active tab.
    fn rebuild_field_subscriptions(&mut self, cx: &mut Context<Self>) {
        self._field_subs.clear();
        if let Some(tab_idx) = self.active_tab {
            if let Some(tab) = self.tabs.get(tab_idx) {
                for (key, entity) in &tab.field_entities {
                    let key: String = key.clone();
                    let sub = cx.subscribe(
                        entity,
                        move |this: &mut Self, _tf, event: &TextChanged, cx| {
                            if let Some(tab_idx) = this.active_tab {
                                if let Some(tab) = this.tabs.get_mut(tab_idx) {
                                    tab.edited_values.insert(
                                        key.clone(),
                                        serde_json::Value::String(event.0.clone()),
                                    );
                                    if key == "name" {
                                        tab.name = event.0.clone();
                                    }
                                    tab.recompute_dirty();
                                    cx.notify();
                                }
                            }
                        },
                    );
                    self._field_subs.push(sub);
                }
            }
        }
        self.rebuild_edge_type_subscriptions(cx);
    }

    /// Rebuild edge-type text field subscriptions for the active tab.
    fn rebuild_edge_type_subscriptions(&mut self, cx: &mut Context<Self>) {
        self._edge_type_subs.clear();
        if let Some(tab_idx) = self.active_tab {
            if let Some(tab) = self.tabs.get(tab_idx) {
                for (i, entity) in tab.edge_type_entities.iter().enumerate() {
                    let sub = cx.subscribe(
                        entity,
                        move |this: &mut Self, _tf, event: &TextChanged, cx| {
                            if let Some(tab_idx) = this.active_tab {
                                if let Some(tab) = this.tabs.get_mut(tab_idx) {
                                    if let Some(edge) = tab.edited_edges.get_mut(i) {
                                        edge.edge_type = event.0.clone();
                                    }
                                    tab.recompute_dirty();
                                    cx.notify();
                                }
                            }
                        },
                    );
                    self._edge_type_subs.push(sub);
                }
            }
        }
    }

    // ── Utility ───────────────────────────────────────────────────────────

    /// Build a map of ObjectId → display name from the shared snapshot.
    ///
    /// Used when converting `Edge` records into `EditableEdge` with cached names.
    fn build_node_name_map(&self) -> HashMap<ObjectId, String> {
        let snap = self.snapshot.read();
        snap.nodes.iter().map(|n| (n.id, n.name.clone())).collect()
    }
}
