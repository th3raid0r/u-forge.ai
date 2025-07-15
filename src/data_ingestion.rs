//! Data ingestion module for u-forge.ai
//!
//! This module provides functionality for importing JSON data into the knowledge graph.
//! It handles line-delimited JSON parsing and object/relationship creation with proper
//! metadata handling.

use crate::types::*;
use crate::KnowledgeGraph;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{error, info, warn};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum JsonEntry {
    #[serde(rename = "node")]
    Node {
        name: String,
        #[serde(rename = "nodeType")]
        node_type: String,
        metadata: Vec<String>,
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

    /// Import JSON data from a file into the knowledge graph
    pub async fn import_json_data<P: AsRef<Path>>(&mut self, data_file: P) -> Result<()> {
        let data_file = data_file.as_ref();
        info!("Loading JSON data from: {:?}", data_file);

        let file_content = fs::read_to_string(data_file)
            .with_context(|| format!("Failed to read file: {:?}", data_file))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Parse line-delimited JSON
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

        // Create objects first
        let mut name_to_id = HashMap::new();
        self.create_objects(nodes, &mut name_to_id).await?;

        // Then create relationships
        self.create_relationships(edges, &name_to_id).await?;

        Ok(())
    }

    /// Import data using environment variable or default path
    ///
    /// Uses UFORGE_DATA_FILE environment variable if set, otherwise defaults to ./defaults/data/memory.json
    pub async fn import_default_data(&mut self) -> Result<()> {
        let data_file = std::env::var("UFORGE_DATA_FILE")
            .unwrap_or_else(|_| "./defaults/data/memory.json".to_string());

        info!("Attempting to load data from: {}", data_file);

        if std::path::Path::new(&data_file).exists() {
            self.import_json_data(&data_file).await?;
        } else {
            return Err(anyhow::anyhow!(
                "Data file not found: {}. Set UFORGE_DATA_FILE environment variable or place file at ./defaults/data/memory.json", 
                data_file
            ));
        }

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
                name,
                node_type,
                metadata,
            } = entry
            {
                let object_metadata = self
                    .create_object_by_type(&name, &node_type, &metadata)
                    .await?;

                match self.graph.add_object(object_metadata.clone()) {
                    Ok(id) => {
                        name_to_id.insert(name, id);
                        self.stats.objects_created += 1;
                    }
                    Err(e) => {
                        error!("Failed to add object {}: {}", name, e);
                    }
                }
            }
        }

        info!("Created {} objects total", self.stats.objects_created);
        Ok(())
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
                if let (Some(&from_id), Some(&to_id)) = (name_to_id.get(&from), name_to_id.get(&to))
                {
                    match self.graph.connect_objects_str(from_id, to_id, &edge_type) {
                        Ok(()) => self.stats.relationships_created += 1,
                        Err(e) => error!("Failed to create edge {} -> {}: {}", from, to, e),
                    }
                } else {
                    error!("Missing node reference for edge {} -> {}", from, to);
                }
            }
        }

        info!(
            "Created {} relationships total",
            self.stats.relationships_created
        );
        Ok(())
    }

    async fn create_object_by_type(
        &self,
        name: &str,
        node_type: &str,
        metadata: &[String],
    ) -> Result<ObjectMetadata> {
        use crate::ObjectBuilder;

        // First, check if this type exists in any available schema
        let schema_manager = self.graph.get_schema_manager();
        let use_original_type = if let Ok(schemas) = schema_manager.list_schemas().await {
            // Try imported_schemas first, then default
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

        let object_metadata = if use_original_type {
            // Use the original type from JSON if it exists in schema
            let mut builder = ObjectBuilder::custom(node_type.to_string(), name.to_string());
            builder = self.add_metadata_to_builder(builder, metadata);
            builder.build()
        } else {
            // Fall back to the original mapping logic
            match node_type {
                "location" => {
                    let mut builder = ObjectBuilder::location(name.to_string());
                    builder = self.add_metadata_to_builder(builder, metadata);
                    builder.build()
                }
                "npc" | "player_character" => {
                    let mut builder = ObjectBuilder::character(name.to_string());
                    builder = self.add_metadata_to_builder(builder, metadata);
                    builder.build()
                }
                "faction" => {
                    let mut builder = ObjectBuilder::faction(name.to_string());
                    builder = self.add_metadata_to_builder(builder, metadata);
                    builder.build()
                }
                "quest" | "setting_reference" | "system_reference" | "temporal" => {
                    let mut builder = ObjectBuilder::event(name.to_string());
                    builder = self.add_metadata_to_builder(builder, metadata);
                    builder.build()
                }
                "artifact" | "currency" | "inventory" | "transportation" | "skills" => {
                    let mut builder = ObjectBuilder::item(name.to_string());
                    builder = self.add_metadata_to_builder(builder, metadata);
                    builder.build()
                }
                _ => {
                    let mut builder =
                        ObjectBuilder::custom(node_type.to_string(), name.to_string());
                    builder = self.add_metadata_to_builder(builder, metadata);
                    builder.build()
                }
            }
        };

        Ok(object_metadata)
    }

    fn add_metadata_to_builder(
        &self,
        mut builder: crate::ObjectBuilder,
        metadata: &[String],
    ) -> crate::ObjectBuilder {
        for meta_item in metadata {
            // Handle different metadata formats
            if meta_item.contains(':') {
                // Key-value pair
                let parts: Vec<&str> = meta_item.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let key = parts[0].trim();
                    let value = parts[1].trim();
                    builder = builder.with_property(key.to_string(), value.to_string());
                }
            } else {
                // Tag
                builder = builder.with_tag(meta_item.clone());
            }
        }
        builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_graph() -> (TempDir, KnowledgeGraph) {
        let temp_dir = TempDir::new().unwrap();
        let graph = KnowledgeGraph::new(temp_dir.path(), None).unwrap();
        (temp_dir, graph)
    }

    #[tokio::test]
    async fn test_json_parsing() {
        let json_data = r#"{"type": "node", "name": "Test Location", "nodeType": "location", "metadata": ["tag1", "property:value"]}
{"type": "edge", "from": "Location A", "to": "Location B", "edgeType": "connects_to"}"#;

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
    async fn test_metadata_parsing() {
        let (_temp_dir, graph) = create_test_graph();
        let ingestion = DataIngestion::new(&graph);

        let metadata = vec![
            "simple_tag".to_string(),
            "key:value".to_string(),
            "another:complex value".to_string(),
        ];

        let builder = crate::ObjectBuilder::location("Test".to_string());
        let builder = ingestion.add_metadata_to_builder(builder, &metadata);
        let object = builder.build();

        assert!(object.tags.contains(&"simple_tag".to_string()));
        assert_eq!(
            object.properties.get("key").and_then(|v| v.as_str()),
            Some("value")
        );
        assert_eq!(
            object.properties.get("another").and_then(|v| v.as_str()),
            Some("complex value")
        );
    }
}
