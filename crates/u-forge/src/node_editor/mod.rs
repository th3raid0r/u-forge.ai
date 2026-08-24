pub(crate) mod field_spec;
mod render;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    Context, Entity, FocusHandle, Focusable, Pixels, Point, ScrollHandle, Subscription, prelude::*,
};
use parking_lot::RwLock;
use u_forge_core::{
    EdgeType, KnowledgeGraph, ObjectId, ObjectMetadata, PropertyType, SchemaManager,
};
use u_forge_graph_view::GraphSnapshot;

use crate::selection_model::SelectionModel;
use crate::text_field::{TextArrowKey, TextChanged, TextFieldView, TextSubmit};

pub(crate) use field_spec::{EditableEdge, EditorTab};

pub(crate) struct CloseDirtyTabRequested(pub usize);
impl gpui::EventEmitter<CloseDirtyTabRequested> for NodeEditorPanel {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorSaveFailureStage {
    SchemaValidation,
    Persistence,
}

impl EditorSaveFailureStage {
    fn label(self) -> &'static str {
        match self {
            Self::SchemaValidation => "schema validation",
            Self::Persistence => "persistence",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSaveFailure {
    node_id: ObjectId,
    node_name: String,
    stage: EditorSaveFailureStage,
    message: String,
}

impl EditorSaveFailure {
    fn user_message(&self) -> String {
        let display_name = if self.node_name.trim().is_empty() {
            format!("unnamed node {}", self.node_id)
        } else {
            format!("\"{}\"", self.node_name)
        };
        format!(
            "Could not save {display_name} during {}: {}",
            self.stage.label(),
            self.message
        )
    }
}

#[derive(Debug, Default)]
pub(crate) struct EditorSaveResult {
    pub(crate) saved: usize,
    pub(crate) saved_ids: Vec<ObjectId>,
    pub(crate) discarded_ids: Vec<ObjectId>,
    pub(crate) skipped_edges: usize,
    failures: Vec<EditorSaveFailure>,
}

impl EditorSaveResult {
    pub(crate) fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    pub(crate) fn user_diagnostic(&self) -> Option<String> {
        let first = self.failures.first()?;
        if self.failures.len() == 1 {
            Some(first.user_message())
        } else {
            Some(format!(
                "Could not save {} nodes. First failure: {}",
                self.failures.len(),
                first.user_message()
            ))
        }
    }
}

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
    /// Subscription that selects the highlighted result when Enter is pressed.
    pub(crate) _submit_sub: Subscription,
    /// Subscription that moves the highlight on Up/Down arrow keys.
    pub(crate) _arrow_sub: Subscription,
    /// Index of the currently highlighted candidate (0 = first).
    pub(crate) highlighted_idx: usize,
}

#[derive(Clone)]
struct TabContextMenuState {
    position: Point<Pixels>,
    index: usize,
    focus: FocusHandle,
}

// ── Node editor panel ─────────────────────────────────────────────────────────

/// Editor panel with browser-style tabs for editing nodes.
///
/// Observes `SelectionModel` and opens tabs as nodes are selected.
pub(crate) struct NodeEditorPanel {
    pub(crate) focus: FocusHandle,
    pub(crate) tabs: Vec<EditorTab>,
    pub(crate) active_tab: Option<usize>,
    /// Keeps the active tab visible when tabs are opened, selected, or moved.
    tab_bar_scroll: ScrollHandle,
    /// Right-click actions for a Details tab.
    tab_context_menu: Option<TabContextMenuState>,
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
            focus: cx.focus_handle(),
            tabs: Vec::new(),
            active_tab: None,
            tab_bar_scroll: ScrollHandle::new(),
            tab_context_menu: None,
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
            self.activate_tab(idx, cx);
            return;
        }

        // Load the node from DB.
        let meta = match self.graph.get_object(node_id) {
            Ok(Some(m)) => m,
            _ => return,
        };

        self.open_tab_for_metadata(meta, false, cx);
    }

    pub(crate) fn open_new_draft(&mut self, meta: ObjectMetadata, cx: &mut Context<Self>) {
        self.open_tab_for_metadata(meta, true, cx);
    }

    /// Shared logic for opening a tab from `ObjectMetadata`.
    fn open_tab_for_metadata(
        &mut self,
        meta: ObjectMetadata,
        is_new: bool,
        cx: &mut Context<Self>,
    ) {
        self.tab_context_menu = None;
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
        if let Some(obj) = meta.properties.as_object() {
            for (k, v) in obj {
                if k.eq_ignore_ascii_case("name") {
                    continue;
                }
                edited_values.insert(k.clone(), v.clone());
            }
        }
        // Ensure description and tags always have entries in edited_values so
        // the UI fields render even when not yet set on the node.
        edited_values
            .entry("description".to_string())
            .or_insert(serde_json::Value::String(String::new()));
        edited_values
            .entry("tags".to_string())
            .or_insert(serde_json::Value::Array(vec![]));

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
                    let mut tf = TextFieldView::new(false, "relationship type", cx);
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
            pinned: is_new,
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
            active_subtab: field_spec::SubTab::default(),
        };
        let specs = tmp_tab.field_specs();
        for spec in &specs {
            match &spec.field_kind {
                PropertyType::Text
                | PropertyType::String
                | PropertyType::Number
                | PropertyType::Reference(_)
                | PropertyType::Object(_) => {
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
                PropertyType::Enum(_) => {
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
            pinned: is_new,
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
            active_subtab: field_spec::SubTab::default(),
        };

        // Replace the first unpinned non-dirty tab, or append.
        if let Some(idx) = self.tabs.iter().position(|t| !t.pinned && !t.dirty) {
            self.tabs[idx] = new_tab;
            self.active_tab = Some(idx);
        } else {
            self.tabs.push(new_tab);
            self.active_tab = Some(self.tabs.len() - 1);
        }

        self.reveal_active_tab();
        self.rebuild_field_subscriptions(cx);
    }

    pub(crate) fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active_tab = Some(index);
        self.tab_context_menu = None;
        self.edge_node_dropdown = None;
        self.array_add_field = None;
        self.reveal_active_tab();
        self.rebuild_field_subscriptions(cx);
        cx.notify();
    }

    /// Close a tab by index.
    pub(crate) fn close_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        self.edge_node_dropdown = None;
        self.array_add_field = None;
        self.tab_context_menu = None;
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
        self.reveal_active_tab();
        self.rebuild_field_subscriptions(cx);
    }

    pub(crate) fn activate_relative_tab(&mut self, previous: bool, cx: &mut Context<Self>) {
        let Some(active) = self.active_tab else {
            return;
        };
        let Some(next) = relative_tab_index(active, self.tabs.len(), previous) else {
            return;
        };
        self.activate_tab(next, cx);
    }

    pub(crate) fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active_tab = self
            .active_tab
            .map(|active| remap_index_after_move(active, from, to));
        self.tab_context_menu = None;
        self.edge_node_dropdown = None;
        self.array_add_field = None;
        self.reveal_active_tab();
        self.rebuild_field_subscriptions(cx);
        cx.notify();
    }

    fn reveal_active_tab(&self) {
        if let Some(active) = self.active_tab {
            self.tab_bar_scroll.scroll_to_item(active);
        }
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
    /// The result includes persisted/discarded IDs, incomplete edges, and
    /// actionable validation or persistence failures for AppView presentation.
    pub(crate) fn save_dirty_tabs(&mut self, cx: &mut Context<Self>) -> EditorSaveResult {
        self.save_tabs(None, cx)
    }

    pub(crate) fn save_active_tab(&mut self, cx: &mut Context<Self>) -> EditorSaveResult {
        let active = self.active_tab;
        self.save_tabs(active, cx)
    }

    pub(crate) fn save_tab(&mut self, index: usize, cx: &mut Context<Self>) -> EditorSaveResult {
        self.save_tabs(Some(index), cx)
    }

    fn save_tabs(&mut self, target: Option<usize>, cx: &mut Context<Self>) -> EditorSaveResult {
        let mut result = EditorSaveResult::default();

        // First pass: identify empty-new tabs to discard.
        let mut discard_indices = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            if target.is_none_or(|target| target == i) && tab.is_new && tab.name.trim().is_empty() {
                discard_indices.push(i);
                result.discarded_ids.push(tab.node_id);
            }
        }
        // Empty drafts have never reached storage; removing their tabs is enough.
        for &idx in discard_indices.iter().rev() {
            self.tab_context_menu = None;
            self.tabs.remove(idx);
            self.active_tab = match self.active_tab {
                _ if self.tabs.is_empty() => None,
                Some(active) if active == idx => Some(idx.min(self.tabs.len() - 1)),
                Some(active) if active > idx => Some(active - 1),
                active => active,
            };
        }
        let target = if target.is_some() && !discard_indices.is_empty() {
            Some(usize::MAX)
        } else {
            target
        };
        self.reveal_active_tab();

        // Second pass: persist dirty tabs.
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            if !tab.dirty || target.is_some_and(|target| target != index) {
                continue;
            }
            let meta = match persist_editor_values(
                &self.graph,
                &tab.original,
                &tab.edited_values,
                tab.is_new,
            ) {
                Ok(meta) => meta,
                Err(failure) => {
                    tracing::warn!(
                        node_id = %failure.node_id,
                        node_name = failure.node_name,
                        stage = failure.stage.label(),
                        error = failure.message,
                        "Node save failed"
                    );
                    result.failures.push(failure);
                    continue;
                }
            };

            // ── Save edge changes ─────────────────────────────────────────
            result.skipped_edges += Self::save_edges_for_tab(&self.graph, tab);

            result.saved_ids.push(tab.node_id);
            tab.original = meta;
            tab.dirty = false;
            tab.is_new = false;

            // Refresh original_edges so subsequent dirty checks are correct.
            tab.original_edges = self
                .graph
                .get_relationships(tab.node_id)
                .unwrap_or_default();

            result.saved += 1;
        }
        cx.notify();
        self.rebuild_field_subscriptions(cx);
        result
    }

    /// Persist edge changes for a single tab: delete removed edges and add new ones.
    ///
    /// Returns the number of edges that were skipped because one or both endpoints
    /// were still `None` (incomplete edges left by the user before saving).
    ///
    /// Takes `graph` explicitly (rather than `&self`) so this can be called
    /// while iterating over `&mut self.tabs` without a borrow conflict.
    fn save_edges_for_tab(graph: &KnowledgeGraph, tab: &EditorTab) -> usize {
        // Build sets of (from, to, type) triples for comparison.
        let orig_set: Vec<(ObjectId, ObjectId, String)> = tab
            .original_edges
            .iter()
            .map(|e| (e.from, e.to, e.edge_type.as_str().to_string()))
            .collect();

        let incomplete_count = tab.edited_edges.iter().filter(|e| !e.is_complete()).count();

        let edited_set: Vec<(ObjectId, ObjectId, String)> = tab
            .edited_edges
            .iter()
            .filter(|e| e.is_complete())
            .map(|e| {
                let (Some(from), Some(to)) = (e.from, e.to) else {
                    unreachable!("is_complete() guarantees both endpoints are Some")
                };
                (from, to, e.edge_type.trim().to_string())
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

        incomplete_count
    }

    /// Return true if any tab has unsaved changes.
    pub(crate) fn has_dirty_tabs(&self) -> bool {
        self.tabs.iter().any(|t| t.dirty)
    }

    pub(crate) fn incomplete_relationship_count(&self) -> usize {
        self.incomplete_relationship_count_for(None)
    }

    pub(crate) fn active_incomplete_relationship_count(&self) -> usize {
        self.active_tab.map_or(0, |active| {
            self.incomplete_relationship_count_for(Some(active))
        })
    }

    pub(crate) fn incomplete_relationship_count_at(&self, index: usize) -> usize {
        self.incomplete_relationship_count_for(Some(index))
    }

    fn incomplete_relationship_count_for(&self, target: Option<usize>) -> usize {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(index, _tab)| target.is_none_or(|target| target == *index))
            .filter(|(_index, tab)| tab.dirty)
            .flat_map(|(_index, tab)| &tab.edited_edges)
            .filter(|edge| {
                edge.from.is_none() || edge.to.is_none() || edge.edge_type.trim().is_empty()
            })
            .count()
    }

    // ── Array inline add ──────────────────────────────────────────────────

    /// Commit the inline array-add text field: push its content into the array
    /// and close the inline editor.
    pub(crate) fn commit_array_add(&mut self, cx: &mut Context<Self>) {
        if let Some((key, entity, _sub)) = self.array_add_field.take() {
            let text = entity.read(cx).content.trim().to_string();
            if !text.is_empty()
                && let Some(tab_idx) = self.active_tab
                && let Some(tab) = self.tabs.get_mut(tab_idx)
            {
                let arr = tab
                    .edited_values
                    .entry(key)
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                if let Some(a) = arr.as_array_mut() {
                    a.push(serde_json::Value::String(text));
                }
                tab.recompute_dirty();
            }
            cx.notify();
        }
    }

    // ── Edge editing helpers ──────────────────────────────────────────────

    /// Add a new edge row to the active tab, pre-populated with the current node as source.
    pub(crate) fn add_edge_row(&mut self, cx: &mut Context<Self>) {
        if let Some(tab_idx) = self.active_tab
            && let Some(tab) = self.tabs.get_mut(tab_idx)
        {
            let node_id = tab.node_id;
            let node_name = tab.name.clone();
            let mut edge = EditableEdge::empty();
            edge.from = Some(node_id);
            edge.from_name = node_name;
            tab.edited_edges.push(edge);
            let entity = cx.new(|cx| TextFieldView::new(false, "relationship type", cx));
            tab.edge_type_entities.push(entity);
            tab.recompute_dirty();
        }
        self.rebuild_edge_type_subscriptions(cx);
        cx.notify();
    }

    /// Remove an edge row from the active tab by index.
    pub(crate) fn remove_edge_row(&mut self, edge_idx: usize, cx: &mut Context<Self>) {
        // Close any open dropdown that references this or later indices.
        self.edge_node_dropdown = None;
        if let Some(tab_idx) = self.active_tab
            && let Some(tab) = self.tabs.get_mut(tab_idx)
        {
            if edge_idx < tab.edited_edges.len() {
                tab.edited_edges.remove(edge_idx);
            }
            if edge_idx < tab.edge_type_entities.len() {
                tab.edge_type_entities.remove(edge_idx);
            }
            tab.recompute_dirty();
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
                    dd.highlighted_idx = 0; // Reset to first result when filter changes.
                }
                cx.notify();
            }
        });

        let submit_sub = cx.subscribe(&filter_entity, {
            move |this: &mut Self, _tf, _event: &TextSubmit, cx| {
                let (filter_lower, highlighted) = match &this.edge_node_dropdown {
                    Some(dd) => (dd.filter_text.to_lowercase(), dd.highlighted_idx),
                    None => return,
                };
                let snap = this.snapshot.read();
                let mut candidates: Vec<(u_forge_core::ObjectId, String)> = snap
                    .nodes
                    .iter()
                    .filter(|n| {
                        filter_lower.is_empty()
                            || n.name.to_lowercase().contains(&filter_lower)
                            || n.object_type.to_lowercase().contains(&filter_lower)
                    })
                    .map(|n| (n.id, n.name.clone()))
                    .collect();
                drop(snap);
                candidates.sort_by_key(|a| a.1.to_lowercase());
                let target = highlighted.min(candidates.len().saturating_sub(1));
                if let Some((id, name)) = candidates.into_iter().nth(target) {
                    this.select_edge_node(id, name, cx);
                }
            }
        });

        let arrow_sub = cx.subscribe(&filter_entity, {
            move |this: &mut Self, _tf, event: &TextArrowKey, cx| {
                let filter_lower = match &this.edge_node_dropdown {
                    Some(dd) => dd.filter_text.to_lowercase(),
                    None => return,
                };
                let snap = this.snapshot.read();
                let count = snap
                    .nodes
                    .iter()
                    .filter(|n| {
                        filter_lower.is_empty()
                            || n.name.to_lowercase().contains(&filter_lower)
                            || n.object_type.to_lowercase().contains(&filter_lower)
                    })
                    .count()
                    .min(10);
                drop(snap);
                if let Some(dd) = &mut this.edge_node_dropdown {
                    if event.0 {
                        // Down
                        dd.highlighted_idx = (dd.highlighted_idx + 1).min(count.saturating_sub(1));
                    } else {
                        // Up
                        dd.highlighted_idx = dd.highlighted_idx.saturating_sub(1);
                    }
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
            _submit_sub: submit_sub,
            _arrow_sub: arrow_sub,
            highlighted_idx: 0,
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
        if let Some(dd) = self.edge_node_dropdown.take()
            && let Some(tab_idx) = self.active_tab
            && let Some(tab) = self.tabs.get_mut(tab_idx)
            && let Some(edge) = tab.edited_edges.get_mut(dd.edge_idx)
        {
            if dd.is_target {
                edge.to = Some(node_id);
                edge.to_name = node_name;
            } else {
                edge.from = Some(node_id);
                edge.from_name = node_name;
            }
            tab.recompute_dirty();
        }
        cx.notify();
    }

    // ── Subscription management ───────────────────────────────────────────

    /// Rebuild property-field text change subscriptions for the active tab.
    fn rebuild_field_subscriptions(&mut self, cx: &mut Context<Self>) {
        self._field_subs.clear();
        if let Some(tab_idx) = self.active_tab
            && let Some(tab) = self.tabs.get(tab_idx)
        {
            for (key, entity) in &tab.field_entities {
                let key: String = key.clone();
                let sub = cx.subscribe(
                    entity,
                    move |this: &mut Self, _tf, event: &TextChanged, cx| {
                        if let Some(tab_idx) = this.active_tab
                            && let Some(tab) = this.tabs.get_mut(tab_idx)
                        {
                            tab.edited_values
                                .insert(key.clone(), serde_json::Value::String(event.0.clone()));
                            if key == "name" {
                                tab.name = event.0.clone();
                            }
                            tab.recompute_dirty();
                            cx.notify();
                        }
                    },
                );
                self._field_subs.push(sub);
            }
        }
        self.rebuild_edge_type_subscriptions(cx);
    }

    /// Rebuild edge-type text field subscriptions for the active tab.
    fn rebuild_edge_type_subscriptions(&mut self, cx: &mut Context<Self>) {
        self._edge_type_subs.clear();
        if let Some(tab_idx) = self.active_tab
            && let Some(tab) = self.tabs.get(tab_idx)
        {
            for (i, entity) in tab.edge_type_entities.iter().enumerate() {
                let sub = cx.subscribe(
                    entity,
                    move |this: &mut Self, _tf, event: &TextChanged, cx| {
                        if let Some(tab_idx) = this.active_tab
                            && let Some(tab) = this.tabs.get_mut(tab_idx)
                        {
                            if let Some(edge) = tab.edited_edges.get_mut(i) {
                                edge.edge_type = event.0.clone();
                            }
                            tab.recompute_dirty();
                            cx.notify();
                        }
                    },
                );
                self._edge_type_subs.push(sub);
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

fn persist_editor_values(
    graph: &KnowledgeGraph,
    original: &ObjectMetadata,
    edited_values: &HashMap<String, serde_json::Value>,
    is_new: bool,
) -> Result<ObjectMetadata, EditorSaveFailure> {
    let mut metadata = original.clone();
    metadata.name = edited_values
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or(&metadata.name)
        .to_string();

    // Rebuild properties from edited values. `name` remains top-level, while
    // every schema property (including description/tags) stays in the JSON map.
    let mut properties = serde_json::Map::new();
    for (key, value) in edited_values {
        if key == "name" {
            continue;
        }
        if key == "description" && value.as_str().is_some_and(str::is_empty) {
            continue;
        }
        properties.insert(key.clone(), value.clone());
    }

    let issues = graph.validate_and_coerce_properties(&metadata.object_type, &mut properties);
    if !issues.is_empty() {
        return Err(EditorSaveFailure {
            node_id: metadata.id,
            node_name: metadata.name,
            stage: EditorSaveFailureStage::SchemaValidation,
            message: issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    metadata.properties = serde_json::Value::Object(properties);

    let persist_result = if is_new {
        graph.add_object(metadata.clone()).map(|_| ())
    } else {
        graph.update_object(metadata.clone())
    };
    persist_result.map_err(|error| EditorSaveFailure {
        node_id: metadata.id,
        node_name: metadata.name.clone(),
        stage: EditorSaveFailureStage::Persistence,
        message: format!("{error:#}"),
    })?;

    Ok(metadata)
}

impl Focusable for NodeEditorPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

fn relative_tab_index(active: usize, len: usize, previous: bool) -> Option<usize> {
    if len == 0 || active >= len {
        return None;
    }
    Some(if previous {
        active.checked_sub(1).unwrap_or(len - 1)
    } else {
        (active + 1) % len
    })
}

fn remap_index_after_move(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < index && index <= to {
        index - 1
    } else if to <= index && index < from {
        index + 1
    } else {
        index
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EditorSaveFailure, EditorSaveFailureStage, EditorSaveResult, persist_editor_values,
        relative_tab_index, remap_index_after_move,
    };
    use std::collections::HashMap;
    use tempfile::TempDir;
    use u_forge_core::{
        KnowledgeGraph, ObjectMetadata, ObjectTypeSchema, PropertySchema, SchemaDefinition,
    };

    fn graph_with_spell_schema() -> (KnowledgeGraph, TempDir) {
        let temp = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp.path()).unwrap();
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "spell".to_string(),
            ObjectTypeSchema::new("spell".to_string(), "Spell".to_string())
                .with_property("level".to_string(), PropertySchema::number("level"))
                .with_required_property("level".to_string()),
        );
        graph.get_schema_manager().save_schema(&schema).unwrap();
        (graph, temp)
    }

    #[test]
    fn relative_tab_navigation_wraps_and_rejects_stale_state() {
        assert_eq!(relative_tab_index(0, 3, false), Some(1));
        assert_eq!(relative_tab_index(2, 3, false), Some(0));
        assert_eq!(relative_tab_index(0, 3, true), Some(2));
        assert_eq!(relative_tab_index(2, 3, true), Some(1));
        assert_eq!(relative_tab_index(0, 0, false), None);
        assert_eq!(relative_tab_index(3, 3, false), None);
    }

    #[test]
    fn tab_reordering_preserves_the_active_item() {
        assert_eq!(remap_index_after_move(1, 1, 3), 3);
        assert_eq!(remap_index_after_move(2, 1, 3), 1);
        assert_eq!(remap_index_after_move(3, 1, 3), 2);
        assert_eq!(remap_index_after_move(3, 3, 0), 0);
        assert_eq!(remap_index_after_move(0, 3, 0), 1);
        assert_eq!(remap_index_after_move(1, 3, 0), 2);
        assert_eq!(remap_index_after_move(4, 1, 3), 4);
    }

    #[test]
    fn editor_save_normalizes_before_the_final_write() {
        let (graph, _temp) = graph_with_spell_schema();
        let original = ObjectMetadata::new("spell".to_string(), "Shield".to_string());
        let edited = HashMap::from([
            (
                "name".to_string(),
                serde_json::Value::String("Shield".to_string()),
            ),
            (
                "level".to_string(),
                serde_json::Value::String("3".to_string()),
            ),
        ]);

        let persisted = persist_editor_values(&graph, &original, &edited, true).unwrap();

        assert_eq!(
            persisted.get_json_property("level"),
            Some(&serde_json::json!(3.0))
        );
        assert!(graph.get_object(original.id).unwrap().is_some());
    }

    #[test]
    fn editor_save_returns_actionable_preflight_failure_without_writing() {
        let (graph, _temp) = graph_with_spell_schema();
        let original = ObjectMetadata::new("spell".to_string(), "Incomplete".to_string());
        let edited = HashMap::from([(
            "name".to_string(),
            serde_json::Value::String("Incomplete".to_string()),
        )]);

        let failure = persist_editor_values(&graph, &original, &edited, true).unwrap_err();

        assert_eq!(failure.stage, EditorSaveFailureStage::SchemaValidation);
        assert!(
            failure
                .message
                .contains("required property 'level' is missing")
        );
        assert!(graph.get_object(original.id).unwrap().is_none());
    }

    #[test]
    fn editor_save_returns_final_persistence_failure_without_writing() {
        let (graph, _temp) = graph_with_spell_schema();
        graph
            .get_schema_manager()
            .save_schema(&SchemaDefinition::create_default())
            .unwrap();
        let original = ObjectMetadata::new("spell".to_string(), "Misrouted".to_string())
            .with_schema("default".to_string());
        let edited = HashMap::from([
            (
                "name".to_string(),
                serde_json::Value::String("Misrouted".to_string()),
            ),
            ("level".to_string(), serde_json::json!(3)),
        ]);

        let failure = persist_editor_values(&graph, &original, &edited, true).unwrap_err();

        assert_eq!(failure.stage, EditorSaveFailureStage::Persistence);
        assert!(failure.message.contains("Unknown object type 'spell'"));
        assert!(graph.get_object(original.id).unwrap().is_none());
    }

    #[test]
    fn editor_save_failure_summary_is_stable() {
        let first_id = u_forge_core::ObjectId::new_v4();
        let second_id = u_forge_core::ObjectId::new_v4();
        let result = EditorSaveResult {
            failures: vec![
                EditorSaveFailure {
                    node_id: first_id,
                    node_name: "First".to_string(),
                    stage: EditorSaveFailureStage::SchemaValidation,
                    message: "required property 'role' is missing".to_string(),
                },
                EditorSaveFailure {
                    node_id: second_id,
                    node_name: "Second".to_string(),
                    stage: EditorSaveFailureStage::Persistence,
                    message: "database is read-only".to_string(),
                },
            ],
            ..EditorSaveResult::default()
        };

        assert_eq!(
            result.user_diagnostic().as_deref(),
            Some(
                "Could not save 2 nodes. First failure: Could not save \"First\" during schema validation: required property 'role' is missing"
            )
        );
    }
}
