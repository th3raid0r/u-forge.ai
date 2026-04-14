use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use uuid::Uuid as ForgeUuid;

/// Unique identifier for graph objects (nodes).
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub ForgeUuid);

impl ObjectId {
    pub fn new_v4() -> Self {
        Self(ForgeUuid::new_v4())
    }

    pub fn parse_str(s: &str) -> Result<Self, uuid::Error> {
        ForgeUuid::parse_str(s).map(Self)
    }

    /// Return the inner UUID formatted with hyphens (e.g. for SQL params).
    pub fn hyphenated(&self) -> uuid::fmt::Hyphenated {
        self.0.hyphenated()
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Unique identifier for text chunks.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChunkId(pub ForgeUuid);

impl ChunkId {
    pub fn new_v4() -> Self {
        Self(ForgeUuid::new_v4())
    }

    pub fn parse_str(s: &str) -> Result<Self, uuid::Error> {
        ForgeUuid::parse_str(s).map(Self)
    }

    /// Return the inner UUID formatted with hyphens (e.g. for SQL params).
    pub fn hyphenated(&self) -> uuid::fmt::Hyphenated {
        self.0.hyphenated()
    }
}

impl std::fmt::Display for ChunkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Edge relationship type — a plain string label (e.g. `"led_by"`, `"located_in"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct EdgeType(pub String);

impl EdgeType {
    /// Construct an `EdgeType` from any string-like value.
    pub fn new(edge_type: impl Into<String>) -> Self {
        Self(edge_type.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EdgeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An edge connecting two objects in the knowledge graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub from: ObjectId,
    pub to: ObjectId,
    pub edge_type: EdgeType,
    pub weight: f32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub metadata: HashMap<String, String>,
}

impl Edge {
    pub fn new(from: ObjectId, to: ObjectId, edge_type: EdgeType) -> Self {
        Self {
            from,
            to,
            edge_type,
            weight: 1.0,
            created_at: chrono::Utc::now(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }
}

/// Core object metadata stored in the knowledge graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMetadata {
    pub id: ObjectId,
    pub object_type: String, // Changed from enum to string for dynamic types
    pub schema_name: Option<String>, // Optional schema reference
    pub name: String,
    pub description: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub tags: Vec<String>,
    pub properties: serde_json::Value, // Changed from HashMap<String, String> to JSON
}

impl ObjectMetadata {
    pub fn new(object_type: String, name: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: ObjectId::new_v4(),
            object_type,
            schema_name: None,
            name,
            description: None,
            created_at: now,
            updated_at: now,
            tags: Vec::new(),
            properties: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    pub fn with_description(mut self, description: String) -> Self {
        self.description = Some(description);
        self
    }

    pub fn with_property(mut self, key: String, value: String) -> Self {
        if let Some(obj) = self.properties.as_object_mut() {
            obj.insert(key, serde_json::Value::String(value));
        }
        self
    }

    /// Add a property with a JSON value
    pub fn with_json_property(mut self, key: String, value: serde_json::Value) -> Self {
        if let Some(obj) = self.properties.as_object_mut() {
            obj.insert(key, value);
        }
        self
    }

    /// Set the schema name for this object
    pub fn with_schema(mut self, schema_name: String) -> Self {
        self.schema_name = Some(schema_name);
        self
    }

    /// Get a property value as a string
    pub fn get_property(&self, key: &str) -> Option<String> {
        self.properties
            .as_object()
            .and_then(|obj| obj.get(key))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Get a property value as JSON
    pub fn get_json_property(&self, key: &str) -> Option<&serde_json::Value> {
        self.properties.as_object().and_then(|obj| obj.get(key))
    }

    /// Set a property value
    pub fn set_property(&mut self, key: String, value: String) {
        if let Some(obj) = self.properties.as_object_mut() {
            obj.insert(key, serde_json::Value::String(value));
        }
        self.touch();
    }

    /// Set a property with JSON value
    pub fn set_json_property(&mut self, key: String, value: serde_json::Value) {
        if let Some(obj) = self.properties.as_object_mut() {
            obj.insert(key, value);
        }
        self.touch();
    }

    pub fn add_tag(&mut self, tag: String) {
        if !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = chrono::Utc::now();
    }

    /// Flatten all node metadata into a single string for embedding and reranking.
    ///
    /// Produces a structured key-value representation that includes the name,
    /// type, description, all properties, tags, and any pre-formatted edge lines.
    /// Pass `edge_lines` as `&[]` when edges are not available.
    ///
    /// Properties whose keys duplicate the explicit fields (`name`, `type`,
    /// `description`) are skipped to avoid redundant text.
    pub fn flatten_for_embedding(&self, edge_lines: &[String]) -> String {
        let mut parts: Vec<String> = Vec::new();

        parts.push(format!("Name: {}", self.name));
        parts.push(format!("Type: {}", self.object_type));

        if let Some(desc) = &self.description {
            if !desc.is_empty() {
                parts.push(format!("Description: {}", desc));
            }
        }

        if let Some(props) = self.properties.as_object() {
            for (key, val) in props {
                let val_str = match val {
                    serde_json::Value::String(s) if !s.is_empty() => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Array(arr) => {
                        let items: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                        if items.is_empty() {
                            continue;
                        }
                        items.join(", ")
                    }
                    _ => continue,
                };
                parts.push(format!("{}: {}", key, val_str));
            }
        }

        if !self.tags.is_empty() {
            parts.push(format!("Tags: {}", self.tags.join(", ")));
        }

        if !edge_lines.is_empty() {
            parts.push(format!("Relationships:\n{}", edge_lines.join("\n")));
        }

        parts.join("\n")
    }
}

/// A text chunk associated with an object (for vector search and AI context)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextChunk {
    pub id: ChunkId,
    pub object_id: ObjectId,
    pub content: String,
    pub token_count: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub chunk_type: ChunkType,
}

/// Types of text chunks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChunkType {
    /// Main descriptive text for the object
    Description,
    /// Session notes or transcription
    SessionNote,
    /// AI-generated content
    AiGenerated,
    /// User notes
    UserNote,
    /// Imported content
    Imported,
}

impl TextChunk {
    pub fn new(object_id: ObjectId, content: String, chunk_type: ChunkType) -> Self {
        Self {
            id: ChunkId::new_v4(),
            object_id,
            token_count: estimate_token_count(&content),
            content,
            created_at: chrono::Utc::now(),
            chunk_type,
        }
    }
}

/// Conservative token count estimation.
///
/// Uses ~3 characters per token, which is closer to the actual ratio for
/// dense English prose (character backstories, location descriptions, lore
/// entries). This intentionally overestimates slightly to avoid exceeding LLM
/// context windows when assembling RAG context. The same ratio is used by
/// `split_text()` in `text.rs`.
fn estimate_token_count(text: &str) -> usize {
    text.len().div_ceil(3).max(1)
}

/// Query result for graph traversal and search
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub objects: Vec<ObjectMetadata>,
    pub edges: Vec<Edge>,
    pub chunks: Vec<TextChunk>,
    pub total_tokens: usize,
}

impl QueryResult {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            edges: Vec::new(),
            chunks: Vec::new(),
            total_tokens: 0,
        }
    }
}

impl Default for QueryResult {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryResult {
    pub fn add_object(&mut self, object: ObjectMetadata) {
        self.objects.push(object);
    }

    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    pub fn add_chunk(&mut self, chunk: TextChunk) {
        self.total_tokens += chunk.token_count;
        self.chunks.push(chunk);
    }

    /// Check if adding a chunk would exceed the token budget
    pub fn would_exceed_budget(&self, chunk: &TextChunk, budget: usize) -> bool {
        self.total_tokens + chunk.token_count > budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_metadata_creation() {
        let mut obj = ObjectMetadata::new("character".to_string(), "Gandalf".to_string())
            .with_description("A wise wizard".to_string())
            .with_property("race".to_string(), "Maiar".to_string());

        assert_eq!(obj.name, "Gandalf");
        assert_eq!(obj.object_type, "character");
        assert_eq!(obj.description, Some("A wise wizard".to_string()));
        assert_eq!(obj.get_property("race"), Some("Maiar".to_string()));

        obj.add_tag("wizard".to_string());
        obj.add_tag("wizard".to_string()); // Should not duplicate
        assert_eq!(obj.tags.len(), 1);
        assert_eq!(obj.tags[0], "wizard");
    }

    #[test]
    fn test_object_metadata_json_properties() {
        let obj = ObjectMetadata::new("spell".to_string(), "Fireball".to_string())
            .with_json_property(
                "level".to_string(),
                serde_json::Value::Number(serde_json::Number::from(3)),
            )
            .with_json_property(
                "damage".to_string(),
                serde_json::json!({"dice": "8d6", "type": "fire"}),
            );

        assert_eq!(obj.get_json_property("level").unwrap().as_u64(), Some(3));
        assert!(obj.get_json_property("damage").unwrap().is_object());
    }

    #[test]
    fn test_edge_creation() {
        let id1 = ObjectId::new_v4();
        let id2 = ObjectId::new_v4();

        let edge = Edge::new(id1, id2, EdgeType::new("knows"))
            .with_weight(0.8)
            .with_metadata("context".to_string(), "fellowship".to_string());

        assert_eq!(edge.from, id1);
        assert_eq!(edge.to, id2);
        assert_eq!(edge.edge_type, EdgeType::new("knows"));
        assert_eq!(edge.weight, 0.8);
        assert_eq!(
            edge.metadata.get("context"),
            Some(&"fellowship".to_string())
        );
    }

    #[test]
    fn test_edge_type_from_string() {
        let edge_type = EdgeType::new("led_by");
        assert_eq!(edge_type.as_str(), "led_by");

        let edge_type2 = EdgeType::new("governs");
        assert_eq!(edge_type2.as_str(), "governs");
    }

    #[test]
    fn test_text_chunk_creation() {
        let obj_id = ObjectId::new_v4();
        let content = "This is a test description with some content.".to_string();

        let chunk = TextChunk::new(obj_id, content.clone(), ChunkType::Description);

        assert_eq!(chunk.object_id, obj_id);
        assert_eq!(chunk.content, content);
        assert!(chunk.token_count > 0);
    }

    #[test]
    fn test_query_result_token_budget() {
        let mut result = QueryResult::new();
        let obj_id = ObjectId::new_v4();

        let chunk1 = TextChunk::new(obj_id, "Short text".to_string(), ChunkType::Description);
        let chunk2 = TextChunk::new(
            obj_id,
            "This is a much longer piece of text that should have more tokens".to_string(),
            ChunkType::UserNote,
        );

        result.add_chunk(chunk1);
        assert!(!result.would_exceed_budget(&chunk2, 100));

        result.add_chunk(chunk2);
        let large_chunk = TextChunk::new(obj_id, "x".repeat(1000), ChunkType::Description);
        assert!(result.would_exceed_budget(&large_chunk, 100));
    }
}
