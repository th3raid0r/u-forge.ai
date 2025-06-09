use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// Re-export Uuid for easier access throughout the crate
pub use uuid::Uuid as ForgeUuid;

/// Core object types in the TTRPG knowledge graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ObjectType {
    Character,
    Location,
    Faction,
    Item,
    Event,
    Session,
    CustomType(String),
}

impl ObjectType {
    pub fn as_str(&self) -> &str {
        match self {
            ObjectType::Character => "character",
            ObjectType::Location => "location",
            ObjectType::Faction => "faction",
            ObjectType::Item => "item",
            ObjectType::Event => "event",
            ObjectType::Session => "session",
            ObjectType::CustomType(name) => name,
        }
    }
}

/// Unique identifier for graph objects
pub type ObjectId = ForgeUuid;

/// Unique identifier for text chunks
pub type ChunkId = ForgeUuid;

/// Edge relationship types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EdgeType {
    /// Generic relationships
    RelatedTo,
    Contains,
    OwnedBy,
    LocatedIn,
    MemberOf,
    
    /// Character-specific relationships
    Knows,
    EnemyOf,
    AllyOf,
    FamilyOf,
    
    /// Event relationships
    CausedBy,
    LeadsTo,
    HappenedAt,
    ParticipatedIn,
    
    /// Custom relationship type
    Custom(String),
}

impl EdgeType {
    pub fn as_str(&self) -> &str {
        match self {
            EdgeType::RelatedTo => "related_to",
            EdgeType::Contains => "contains",
            EdgeType::OwnedBy => "owned_by",
            EdgeType::LocatedIn => "located_in",
            EdgeType::MemberOf => "member_of",
            EdgeType::Knows => "knows",
            EdgeType::EnemyOf => "enemy_of",
            EdgeType::AllyOf => "ally_of",
            EdgeType::FamilyOf => "family_of",
            EdgeType::CausedBy => "caused_by",
            EdgeType::LeadsTo => "leads_to",
            EdgeType::HappenedAt => "happened_at",
            EdgeType::ParticipatedIn => "participated_in",
            EdgeType::Custom(name) => name,
        }
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
            id: ForgeUuid::new_v4(),
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

    /// Create a new object with legacy ObjectType enum (for backward compatibility)
    pub fn new_with_type(object_type: ObjectType, name: String) -> Self {
        Self::new(object_type.as_str().to_string(), name)
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
        self.properties.as_object()
            .and_then(|obj| obj.get(key))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Get a property value as JSON
    pub fn get_json_property(&self, key: &str) -> Option<&serde_json::Value> {
        self.properties.as_object()
            .and_then(|obj| obj.get(key))
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
            id: ForgeUuid::new_v4(),
            object_id,
            token_count: estimate_token_count(&content),
            content,
            created_at: chrono::Utc::now(),
            chunk_type,
        }
    }
}

/// Simple token count estimation (rough approximation)
fn estimate_token_count(text: &str) -> usize {
    // Very rough estimation: ~4 characters per token on average
    (text.len() / 4).max(1)
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
            .with_json_property("level".to_string(), serde_json::Value::Number(serde_json::Number::from(3)))
            .with_json_property("damage".to_string(), serde_json::json!({"dice": "8d6", "type": "fire"}));
        
        assert_eq!(obj.get_json_property("level").unwrap().as_u64(), Some(3));
        assert!(obj.get_json_property("damage").unwrap().is_object());
    }

    #[test]
    fn test_backward_compatibility() {
        let obj = ObjectMetadata::new_with_type(ObjectType::Character, "Frodo".to_string());
        assert_eq!(obj.object_type, "character");
    }

    #[test]
    fn test_edge_creation() {
        let id1 = ForgeUuid::new_v4();
        let id2 = ForgeUuid::new_v4();
        
        let edge = Edge::new(id1, id2, EdgeType::Knows)
            .with_weight(0.8)
            .with_metadata("context".to_string(), "fellowship".to_string());
        
        assert_eq!(edge.from, id1);
        assert_eq!(edge.to, id2);
        assert_eq!(edge.edge_type, EdgeType::Knows);
        assert_eq!(edge.weight, 0.8);
        assert_eq!(edge.metadata.get("context"), Some(&"fellowship".to_string()));
    }

    #[test]
    fn test_text_chunk_creation() {
        let obj_id = ForgeUuid::new_v4();
        let content = "This is a test description with some content.".to_string();
        
        let chunk = TextChunk::new(obj_id, content.clone(), ChunkType::Description);
        
        assert_eq!(chunk.object_id, obj_id);
        assert_eq!(chunk.content, content);
        assert!(chunk.token_count > 0);
    }

    #[test]
    fn test_query_result_token_budget() {
        let mut result = QueryResult::new();
        let obj_id = ForgeUuid::new_v4();
        
        let chunk1 = TextChunk::new(obj_id, "Short text".to_string(), ChunkType::Description);
        let chunk2 = TextChunk::new(obj_id, "This is a much longer piece of text that should have more tokens".to_string(), ChunkType::UserNote);
        
        result.add_chunk(chunk1);
        assert!(!result.would_exceed_budget(&chunk2, 100));
        
        result.add_chunk(chunk2);
        let large_chunk = TextChunk::new(obj_id, "x".repeat(1000), ChunkType::Description);
        assert!(result.would_exceed_budget(&large_chunk, 100));
    }
}