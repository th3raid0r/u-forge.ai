//! Data ingestion module for u-forge.ai
//!
//! Imports the canonical JSONL export format produced by `convert_memorymesh`:
//!
//! ```json
//! {"entitytype":"node","id":"<uuid>","nodetype":"faction",
//!  "properties":{"name":"Galactic Empire","goals":["..."],...}}
//! {"entitytype":"edge","from":"Mayor Salvor Hardin","to":"Terminus","edgeType":"located_in"}
//! ```
//!
//! Field mapping:
//! - `entitytype`   — discriminant tag ("node" | "edge")
//! - `id`           — source UUID (stored as `_source_id` property; used for dedup)
//! - `nodetype`     — schema type name (e.g. "npc", "faction", "location")
//! - `properties`   — typed JSON object; arrays stay arrays, strings stay strings
//!
//! Dedup: nodes are matched first by `_source_id`, then by `(nodetype, name)`.

use crate::types::*;
use crate::KnowledgeGraph;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{error, info, warn};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "entitytype")]
pub enum JsonEntry {
    #[serde(rename = "node")]
    Node {
        id: String,
        #[serde(rename = "nodetype")]
        node_type: String,
        properties: Map<String, Value>,
    },
    #[serde(rename = "edge")]
    Edge {
        from: String,
        to: String,
        #[serde(rename = "edgeType")]
        edge_type: String,
    },
}

#[derive(Debug)]
pub struct IngestionStats {
    pub objects_created: usize,
    pub relationships_created: usize,
    pub parse_errors: usize,
}

pub struct DataIngestion<'a> {
    graph: &'a KnowledgeGraph,
    stats: IngestionStats,
}

impl<'a> DataIngestion<'a> {
    pub fn new(graph: &'a KnowledgeGraph) -> Self {
        Self {
            graph,
            stats: IngestionStats {
                objects_created: 0,
                relationships_created: 0,
                parse_errors: 0,
            },
        }
    }

    /// Import JSONL data from a file into the knowledge graph.
    pub async fn import_json_data<P: AsRef<Path>>(&mut self, data_file: P) -> Result<()> {
        let data_file = data_file.as_ref();
        info!("Loading JSON data from: {:?}", data_file);

        let file_content = fs::read_to_string(data_file)
            .with_context(|| format!("Failed to read file: {:?}", data_file))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for (line_num, line) in file_content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonEntry>(line) {
                Ok(entry) => match entry {
                    JsonEntry::Node { .. } => nodes.push(entry),
                    JsonEntry::Edge { .. } => edges.push(entry),
                },
                Err(e) => {
                    self.stats.parse_errors += 1;
                    error!("Line {}: Failed to parse JSON: {}", line_num + 1, e);
                    if line.len() > 100 {
                        error!("   Content preview: {}...", &line[..100]);
                    } else {
                        error!("   Content: {}", line);
                    }
                }
            }
        }

        if self.stats.parse_errors > 0 {
            warn!("Total parse errors: {}", self.stats.parse_errors);
        }

        info!(
            "Parsed {} nodes and {} edges from JSON",
            nodes.len(),
            edges.len()
        );

        let mut name_to_id = HashMap::new();
        self.create_objects(nodes, &mut name_to_id).await?;
        self.create_relationships(edges, &name_to_id).await?;

        Ok(())
    }

    pub fn get_stats(&self) -> &IngestionStats {
        &self.stats
    }

    async fn create_objects(
        &mut self,
        nodes: Vec<JsonEntry>,
        name_to_id: &mut HashMap<String, ObjectId>,
    ) -> Result<()> {
        info!("Creating {} objects...", nodes.len());

        for entry in nodes {
            if let JsonEntry::Node {
                id: source_id,
                node_type,
                properties,
            } = entry
            {
                let name = match properties
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                {
                    Some(n) => n,
                    None => {
                        warn!(
                            "Node (id={}, type={}) has no 'name' in properties — skipping",
                            source_id, node_type
                        );
                        continue;
                    }
                };

                // Dedup: check by source_id first, then by (type, name).
                let existing_id = self.find_existing(&source_id, &node_type, &name);
                if let Some(existing) = existing_id {
                    warn!(
                        "Skipping duplicate '{}' (type: '{}'), reusing existing id {}",
                        name, node_type, existing
                    );
                    name_to_id.insert(name, existing);
                    continue;
                }

                let object_metadata = self
                    .create_object_by_type(&source_id, &node_type, &properties)
                    .await?;

                match self.graph.add_object(object_metadata) {
                    Ok(id) => {
                        name_to_id.insert(name, id);
                        self.stats.objects_created += 1;
                    }
                    Err(e) => {
                        error!("Failed to add object '{}': {}", name, e);
                    }
                }
            }
        }

        info!("Created {} objects total", self.stats.objects_created);
        Ok(())
    }

    /// Check for a pre-existing object by (type, name).
    ///
    /// The `source_id` parameter is accepted for forward-compatibility but is not yet
    /// queryable — a property-index lookup can be added once `find_by_property` exists
    /// on `KnowledgeGraph`.
    fn find_existing(
        &self,
        _source_id: &str,
        node_type: &str,
        name: &str,
    ) -> Option<ObjectId> {
        match self.graph.find_by_name(node_type, name) {
            Ok(results) if !results.is_empty() => Some(results[0].id),
            _ => None,
        }
    }

    async fn create_relationships(
        &mut self,
        edges: Vec<JsonEntry>,
        name_to_id: &HashMap<String, ObjectId>,
    ) -> Result<()> {
        info!("Creating {} relationships...", edges.len());

        for entry in edges {
            if let JsonEntry::Edge {
                from,
                to,
                edge_type,
            } = entry
            {
                let from_id = self.resolve_node_id(&from, name_to_id);
                let to_id = self.resolve_node_id(&to, name_to_id);

                match (from_id, to_id) {
                    (Some(fid), Some(tid)) => {
                        match self.graph.connect_objects_str(fid, tid, &edge_type) {
                            Ok(()) => self.stats.relationships_created += 1,
                            Err(e) => error!("Failed to create edge {} -> {}: {}", from, to, e),
                        }
                    }
                    _ => {
                        error!("Missing node reference for edge {} -> {}", from, to);
                    }
                }
            }
        }

        info!(
            "Created {} relationships total",
            self.stats.relationships_created
        );
        Ok(())
    }

    /// Resolve a node name to an ObjectId.
    ///
    /// Checks the in-session `name_to_id` map first (fast path), then falls back to a
    /// storage name-index scan for nodes created in previous import sessions (BUG-7 fix).
    fn resolve_node_id(
        &self,
        name: &str,
        name_to_id: &HashMap<String, ObjectId>,
    ) -> Option<ObjectId> {
        if let Some(&id) = name_to_id.get(name) {
            return Some(id);
        }

        match self.graph.find_by_name_only(name) {
            Ok(results) if !results.is_empty() => {
                if results.len() > 1 {
                    warn!(
                        "Ambiguous node name '{}' matched {} objects; using first match",
                        name,
                        results.len()
                    );
                }
                Some(results[0].id)
            }
            Ok(_) => None,
            Err(e) => {
                warn!("Storage lookup failed for node '{}': {}", name, e);
                None
            }
        }
    }

    async fn create_object_by_type(
        &self,
        source_id: &str,
        node_type: &str,
        properties: &Map<String, Value>,
    ) -> Result<ObjectMetadata> {
        use crate::ObjectBuilder;

        let name = properties
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Check whether the schema defines this type so we can use the exact type name.
        let schema_manager = self.graph.get_schema_manager();
        let use_original_type = if let Ok(schemas) = schema_manager.list_schemas() {
            let mut found = false;
            for schema_name in ["imported_schemas", "default"] {
                if schemas.contains(&schema_name.to_string()) {
                    if let Ok(schema) = schema_manager.load_schema(schema_name).await {
                        if schema.object_types.contains_key(node_type) {
                            found = true;
                            break;
                        }
                    }
                }
            }
            found
        } else {
            false
        };

        let mut builder = if use_original_type {
            ObjectBuilder::custom(node_type.to_string(), name.clone())
        } else {
            match node_type {
                "location" => ObjectBuilder::location(name.clone()),
                "npc" | "player_character" => ObjectBuilder::character(name.clone()),
                "faction" => ObjectBuilder::faction(name.clone()),
                "quest" | "setting_reference" | "system_reference" | "temporal" => {
                    ObjectBuilder::event(name.clone())
                }
                "artifact" | "currency" | "inventory" | "transportation" | "skills" => {
                    ObjectBuilder::item(name.clone())
                }
                _ => ObjectBuilder::custom(node_type.to_string(), name.clone()),
            }
        };

        // Store source id for future dedup without depending on name stability.
        builder = builder.with_property("_source_id".to_string(), source_id.to_string());

        builder = self.add_properties_to_builder(builder, properties);
        Ok(builder.build())
    }

    fn add_properties_to_builder(
        &self,
        mut builder: crate::ObjectBuilder,
        properties: &Map<String, Value>,
    ) -> crate::ObjectBuilder {
        for (key, value) in properties {
            // "name" is already set as the object's canonical name field.
            if key == "name" {
                continue;
            }

            match value {
                Value::String(s) => {
                    if key.eq_ignore_ascii_case("description") {
                        builder = builder.with_description(s.clone());
                    } else {
                        builder = builder.with_property(key.clone(), s.clone());
                    }
                }
                Value::Array(arr) if key.eq_ignore_ascii_case("tags") => {
                    for item in arr {
                        if let Value::String(tag) = item {
                            builder = builder.with_tag(tag.clone());
                        }
                    }
                }
                // Arrays and any other typed value (number, bool, nested object)
                other => {
                    builder = builder.with_json_property(key.clone(), other.clone());
                }
            }
        }
        builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn create_test_graph() -> (TempDir, KnowledgeGraph) {
        let temp_dir = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp_dir.path()).unwrap();
        (temp_dir, graph)
    }

    #[tokio::test]
    async fn test_json_parsing() {
        let json_data = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"location","properties":{"name":"Test Location","description":"A place"}}
{"entitytype":"edge","from":"Location A","to":"Location B","edgeType":"connects_to"}"#;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for line in json_data.lines() {
            match serde_json::from_str::<JsonEntry>(line) {
                Ok(entry) => match entry {
                    JsonEntry::Node { .. } => nodes.push(entry),
                    JsonEntry::Edge { .. } => edges.push(entry),
                },
                Err(_) => {}
            }
        }

        assert_eq!(nodes.len(), 1);
        assert_eq!(edges.len(), 1);
    }

    #[tokio::test]
    async fn test_properties_parsing() {
        let (_temp_dir, graph) = create_test_graph();
        let ingestion = DataIngestion::new(&graph);

        let mut props = Map::new();
        props.insert("name".to_string(), json!("Test"));
        props.insert("description".to_string(), json!("A test location"));
        props.insert("tags".to_string(), json!(["tag1", "tag2"]));
        props.insert("goals".to_string(), json!(["goal1", "goal2"]));
        props.insert("status".to_string(), json!("Active"));

        let builder = crate::ObjectBuilder::location("Test".to_string());
        let builder = ingestion.add_properties_to_builder(builder, &props);
        let object = builder.build();

        assert!(object.tags.contains(&"tag1".to_string()));
        assert!(object.tags.contains(&"tag2".to_string()));
        assert_eq!(object.description.as_deref(), Some("A test location"));
        assert_eq!(
            object.properties.get("status").and_then(|v| v.as_str()),
            Some("Active")
        );
        // Array property stored as JSON
        assert!(object.properties.get("goals").is_some());
    }

    #[tokio::test]
    async fn test_import_roundtrip() {
        let (_temp_dir, graph) = create_test_graph();
        let mut ingestion = DataIngestion::new(&graph);

        let jsonl = r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"location","properties":{"name":"Terminus","description":"A frontier world","tags":["planet","foundation"]}}
{"entitytype":"node","id":"00000000-0000-0000-0000-000000000002","nodetype":"npc","properties":{"name":"Hari Seldon","role":"Mathematician","currentLocation":"Terminus"}}
{"entitytype":"edge","from":"Hari Seldon","to":"Terminus","edgeType":"located_in"}"#;

        let temp = TempDir::new().unwrap();
        let file = temp.path().join("test.jsonl");
        std::fs::write(&file, jsonl).unwrap();

        ingestion.import_json_data(&file).await.unwrap();

        let stats = ingestion.get_stats();
        assert_eq!(stats.objects_created, 2);
        assert_eq!(stats.relationships_created, 1);
        assert_eq!(stats.parse_errors, 0);
    }
}
