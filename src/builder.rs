//! Fluent builder for [`ObjectMetadata`].

use anyhow::Result;

use crate::types::{ObjectId, ObjectMetadata};
use crate::KnowledgeGraph;

/// Fluent builder for constructing [`ObjectMetadata`] with TTRPG-friendly
/// convenience constructors.
///
/// # Example
/// ```no_run
/// use u_forge_ai::ObjectBuilder;
/// let obj = ObjectBuilder::character("Gandalf".to_string())
///     .with_description("A wizard".to_string())
///     .with_property("race".to_string(), "Maiar".to_string())
///     .with_tag("wizard".to_string())
///     .build();
/// ```
pub struct ObjectBuilder {
    metadata: ObjectMetadata,
}

impl ObjectBuilder {
    pub fn character(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("character".to_string(), name),
        }
    }

    pub fn location(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("location".to_string(), name),
        }
    }

    pub fn faction(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("faction".to_string(), name),
        }
    }

    pub fn item(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("item".to_string(), name),
        }
    }

    pub fn event(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("event".to_string(), name),
        }
    }

    pub fn session(name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new("session".to_string(), name),
        }
    }

    pub fn custom(object_type: String, name: String) -> Self {
        Self {
            metadata: ObjectMetadata::new(object_type, name),
        }
    }

    pub fn with_description(mut self, description: String) -> Self {
        self.metadata.description = Some(description);
        self
    }

    pub fn with_property(mut self, key: String, value: String) -> Self {
        self.metadata = self.metadata.with_property(key, value);
        self
    }

    pub fn with_json_property(mut self, key: String, value: serde_json::Value) -> Self {
        self.metadata = self.metadata.with_json_property(key, value);
        self
    }

    pub fn with_tag(mut self, tag: String) -> Self {
        self.metadata.add_tag(tag);
        self
    }

    /// Consume the builder and return the finished [`ObjectMetadata`].
    pub fn build(self) -> ObjectMetadata {
        self.metadata
    }

    /// Build and immediately insert into `graph`.  Returns the new [`ObjectId`].
    pub fn add_to_graph(self, graph: &KnowledgeGraph) -> Result<ObjectId> {
        graph.add_object(self.build())
    }
}
