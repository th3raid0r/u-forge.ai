use std::collections::HashMap;

use gpui::Entity;
use u_forge_core::{ObjectId, ObjectMetadata, ObjectTypeSchema, PropertyType};

use crate::text_field::TextFieldView;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Height of the tab bar inside the editor panel.
pub(crate) const DETAIL_TAB_H: f32 = 28.0;

/// Target column width for the multi-column form layout.
pub(crate) const COLUMN_W: f32 = 300.0;

/// Height of a single-line form field (label + input + gap).
pub(crate) const FIELD_H_SINGLE: f32 = 52.0;

/// Height of a multiline form field (label + textarea + gap).
pub(crate) const FIELD_H_MULTI: f32 = 104.0;

/// Space reserved for page navigation buttons.
pub(crate) const PAGE_NAV_H: f32 = 32.0;

// ── Field types ───────────────────────────────────────────────────────────────

/// Infer a FieldKind from a JSON value's runtime type, used as a fallback when
/// no schema is available or when a property is not declared in the schema.
pub(crate) fn field_kind_from_value(v: &serde_json::Value) -> FieldKind {
    match v {
        serde_json::Value::Bool(_) => FieldKind::Boolean,
        serde_json::Value::Number(_) => FieldKind::Number,
        serde_json::Value::Array(_) => FieldKind::Array,
        _ => FieldKind::Text,
    }
}

/// Describes a single form field for rendering.
pub(crate) struct FieldSpec {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) required: bool,
    pub(crate) multiline: bool,
    pub(crate) field_kind: FieldKind,
}

pub(crate) enum FieldKind {
    Text,
    Number,
    Boolean,
    Enum(Vec<String>),
    Array,
}

impl FieldSpec {
    pub(crate) fn height(&self) -> f32 {
        match self.field_kind {
            FieldKind::Boolean => FIELD_H_SINGLE,
            FieldKind::Array => FIELD_H_MULTI,
            _ => if self.multiline { FIELD_H_MULTI } else { FIELD_H_SINGLE },
        }
    }
}

/// Compare two JSON values for equality, treating string representations of
/// numbers/booleans as equal to their typed counterparts. This is needed because
/// the TextChanged handler always produces `Value::String`, but the original
/// properties may store `Value::Number` or `Value::Bool`.
pub(crate) fn values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
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
pub(crate) struct EditorTab {
    pub(crate) node_id: ObjectId,
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) object_type: String,
    pub(crate) pinned: bool,
    pub(crate) original: ObjectMetadata,
    pub(crate) edited_values: HashMap<String, serde_json::Value>,
    pub(crate) schema: Option<ObjectTypeSchema>,
    pub(crate) dirty: bool,
    pub(crate) current_page: usize,
    /// Text field entities for the form — keyed by field name.
    pub(crate) field_entities: HashMap<String, Entity<TextFieldView>>,
}

impl EditorTab {
    /// Build the ordered list of field specs from the schema + edited values.
    pub(crate) fn field_specs(&self) -> Vec<FieldSpec> {
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
                    !skip.contains(&k.as_str()) && !schema.required_properties.contains(k)
                })
                .collect();
            optional_keys.sort();

            for key in required_keys.iter().chain(optional_keys.iter()) {
                if let Some(prop) = schema.properties.get(*key) {
                    let (kind, multiline) = match &prop.property_type {
                        PropertyType::Text | PropertyType::String | PropertyType::Reference(_) => {
                            (FieldKind::Text, true)
                        }
                        PropertyType::Number => (FieldKind::Number, false),
                        PropertyType::Boolean => (FieldKind::Boolean, false),
                        PropertyType::Enum(vals) => (FieldKind::Enum(vals.clone()), false),
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

            // Extra properties not in schema — infer kind from JSON value type.
            for key in self.edited_values.keys() {
                if skip.contains(&key.as_str()) {
                    continue;
                }
                if schema.properties.contains_key(key) {
                    continue;
                }
                let kind = self
                    .edited_values
                    .get(key)
                    .map(field_kind_from_value)
                    .unwrap_or(FieldKind::Text);
                let multiline = matches!(kind, FieldKind::Text);
                specs.push(FieldSpec {
                    key: key.clone(),
                    label: key.replace('_', " "),
                    required: false,
                    multiline,
                    field_kind: kind,
                });
            }
        } else {
            // No schema — infer field kind from JSON value type.
            let mut keys: Vec<&String> = self
                .edited_values
                .keys()
                .filter(|k| !["name", "description", "tags"].contains(&k.as_str()))
                .collect();
            keys.sort();
            for key in keys {
                let kind = self
                    .edited_values
                    .get(key)
                    .map(field_kind_from_value)
                    .unwrap_or(FieldKind::Text);
                let multiline = matches!(kind, FieldKind::Text);
                specs.push(FieldSpec {
                    key: key.clone(),
                    label: key.replace('_', " "),
                    required: false,
                    multiline,
                    field_kind: kind,
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
    pub(crate) fn recompute_dirty(&mut self) {
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
                let edited_empty =
                    v.as_str().is_some_and(|s| s.is_empty()) || v.is_null();
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
                    let orig_empty =
                        v.as_str().is_some_and(|s| s.is_empty()) || v.is_null();
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
