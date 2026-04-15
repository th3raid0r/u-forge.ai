pub(crate) mod field_spec;
mod render;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{prelude::*, Context, Entity, Pixels, Subscription};
use parking_lot::RwLock;
use u_forge_core::{KnowledgeGraph, ObjectId, SchemaManager};
use u_forge_graph_view::GraphSnapshot;

use crate::selection_model::SelectionModel;
use crate::text_field::{TextChanged, TextFieldView};

pub(crate) use field_spec::{EditorTab, FieldKind};

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
    snapshot: Arc<RwLock<GraphSnapshot>>,
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
        }
    }

    /// Open a tab for the given node, or focus the existing one.
    pub(crate) fn open_or_focus_tab(&mut self, node_id: ObjectId, cx: &mut Context<Self>) {
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
                if k.eq_ignore_ascii_case("description") || k.eq_ignore_ascii_case("tags") {
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
            let key: String = key.clone();
            let sub =
                cx.subscribe(entity, move |this: &mut Self, _tf, event: &TextChanged, cx| {
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
    pub(crate) fn close_tab(&mut self, idx: usize) {
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
    pub(crate) fn save_dirty_tabs(&mut self) -> usize {
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
    pub(crate) fn has_dirty_tabs(&self) -> bool {
        self.tabs.iter().any(|t| t.dirty)
    }

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
}
