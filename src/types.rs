use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

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
pub type ObjectId = Uuid;

/// Unique identifier for text chunks
pub type ChunkId = Uuid;

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
    pub object_type: ObjectType,
    pub name: String,
    pub description: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub tags: Vec<String>,
    pub properties: HashMap<String, String>,
}

impl ObjectMetadata {
    pub fn new(object_type: ObjectType, name: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4(),
            object_type,
            name,
            description: None,
            created_at: now,
            updated_at: now,
            tags: Vec::new(),
            properties: HashMap::new(),
        }
    }
    
    pub fn with_description(mut self, description: String) -> Self {
        self.description = Some(description);
        self
    }
    
    pub fn with_property(mut self, key: String, value: String) -> Self {
        self.properties.insert(key, value);
        self
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
            id: Uuid::new_v4(),
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
        let mut obj = ObjectMetadata::new(ObjectType::Character, "Gandalf".to_string())
            .with_description("A wise wizard".to_string())
            .with_property("race".to_string(), "Maiar".to_string());
        
        assert_eq!(obj.name, "Gandalf");
        assert_eq!(obj.object_type, ObjectType::Character);
        assert_eq!(obj.description, Some("A wise wizard".to_string()));
        assert_eq!(obj.properties.get("race"), Some(&"Maiar".to_string()));
        
        obj.add_tag("wizard".to_string());
        obj.add_tag("wizard".to_string()); // Should not duplicate
        assert_eq!(obj.tags.len(), 1);
        assert_eq!(obj.tags[0], "wizard");
    }

    #[test]
    fn test_edge_creation() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        
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
        let obj_id = Uuid::new_v4();
        let content = "This is a test description with some content.".to_string();
        
        let chunk = TextChunk::new(obj_id, content.clone(), ChunkType::Description);
        
        assert_eq!(chunk.object_id, obj_id);
        assert_eq!(chunk.content, content);
        assert!(chunk.token_count > 0);
    }

    #[test]
    fn test_query_result_token_budget() {
        let mut result = QueryResult::new();
        let obj_id = Uuid::new_v4();
        
        let chunk1 = TextChunk::new(obj_id, "Short text".to_string(), ChunkType::Description);
        let chunk2 = TextChunk::new(obj_id, "This is a much longer piece of text that should have more tokens".to_string(), ChunkType::UserNote);
        
        result.add_chunk(chunk1);
        assert!(!result.would_exceed_budget(&chunk2, 100));
        
        result.add_chunk(chunk2);
        let large_chunk = TextChunk::new(obj_id, "x".repeat(1000), ChunkType::Description);
        assert!(result.would_exceed_budget(&large_chunk, 100));
    }
}