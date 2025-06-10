use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
use anyhow::{Context, Result};

/// Schema definition for a complete TTRPG system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaDefinition {
    pub id: Uuid,
    pub name: String,
    pub version: String,
    pub description: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub object_types: HashMap<String, ObjectTypeSchema>,
    pub edge_types: HashMap<String, EdgeTypeSchema>,
    pub metadata: HashMap<String, String>,
}

impl SchemaDefinition {
    pub fn new(name: String, version: String, description: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4(),
            name,
            version,
            description,
            created_at: now,
            updated_at: now,
            object_types: HashMap::new(),
            edge_types: HashMap::new(),
            metadata: HashMap::new(),
        }
    }

    pub fn add_object_type(&mut self, name: String, schema: ObjectTypeSchema) {
        self.object_types.insert(name, schema);
        self.touch();
    }

    pub fn add_edge_type(&mut self, name: String, schema: EdgeTypeSchema) {
        self.edge_types.insert(name, schema);
        self.touch();
    }

    pub fn touch(&mut self) {
        self.updated_at = chrono::Utc::now();
    }

    /// Create a default D&D 5e-style schema based on current hardcoded types
    pub fn create_default() -> Self {
        let mut schema = Self::new(
            "default".to_string(),
            "1.0.0".to_string(),
            "Default TTRPG schema with basic object types".to_string(),
        );

        // Add default object types
        schema.add_object_type("character".to_string(), ObjectTypeSchema::default_character());
        schema.add_object_type("location".to_string(), ObjectTypeSchema::default_location());
        schema.add_object_type("faction".to_string(), ObjectTypeSchema::default_faction());
        schema.add_object_type("item".to_string(), ObjectTypeSchema::default_item());
        schema.add_object_type("event".to_string(), ObjectTypeSchema::default_event());
        schema.add_object_type("session".to_string(), ObjectTypeSchema::default_session());

        // Add default edge types
        schema.add_edge_type("related_to".to_string(), EdgeTypeSchema::default_related_to());
        schema.add_edge_type("contains".to_string(), EdgeTypeSchema::default_contains());
        schema.add_edge_type("member_of".to_string(), EdgeTypeSchema::default_member_of());
        schema.add_edge_type("knows".to_string(), EdgeTypeSchema::default_knows());
        schema.add_edge_type("enemy_of".to_string(), EdgeTypeSchema::default_enemy_of());
        schema.add_edge_type("ally_of".to_string(), EdgeTypeSchema::default_ally_of());

        schema
    }
}

/// Schema definition for a specific object type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectTypeSchema {
    pub name: String,
    pub description: String,
    pub properties: HashMap<String, PropertySchema>,
    pub required_properties: Vec<String>,
    pub allowed_edges: Vec<String>,
    pub inheritance: Option<String>, // Parent object type for inheritance
    pub metadata: HashMap<String, String>,
}

impl ObjectTypeSchema {
    pub fn new(name: String, description: String) -> Self {
        Self {
            name,
            description,
            properties: HashMap::new(),
            required_properties: Vec::new(),
            allowed_edges: Vec::new(),
            inheritance: None,
            metadata: HashMap::new(),
        }
    }

    pub fn with_property(mut self, name: String, schema: PropertySchema) -> Self {
        self.properties.insert(name, schema);
        self
    }

    pub fn with_required_property(mut self, name: String) -> Self {
        if !self.required_properties.contains(&name) {
            self.required_properties.push(name);
        }
        self
    }

    pub fn with_allowed_edge(mut self, edge_type: String) -> Self {
        if !self.allowed_edges.contains(&edge_type) {
            self.allowed_edges.push(edge_type);
        }
        self
    }

    // Default object type schemas based on current hardcoded types
    pub fn default_character() -> Self {
        Self::new("character".to_string(), "A character in the game world".to_string())
            .with_property("age".to_string(), PropertySchema::string("Character's age"))
            .with_property("gender".to_string(), PropertySchema::string("Character's gender"))
            .with_property("occupation".to_string(), PropertySchema::string("Character's occupation"))
            .with_property("status".to_string(), PropertySchema::string("Character's current status"))
            .with_property("species".to_string(), PropertySchema::string("Character's species"))
            .with_property("background".to_string(), PropertySchema::text("Character's background story"))
            .with_property("equipment".to_string(), PropertySchema::array(PropertyType::String))
            .with_property("secrets".to_string(), PropertySchema::array(PropertyType::String))
            .with_property("goals".to_string(), PropertySchema::array(PropertyType::String))
            .with_required_property("name".to_string())
            .with_allowed_edge("knows".to_string())
            .with_allowed_edge("enemy_of".to_string())
            .with_allowed_edge("ally_of".to_string())
            .with_allowed_edge("member_of".to_string())
    }

    pub fn default_location() -> Self {
        Self::new("location".to_string(), "A location in the game world".to_string())
            .with_property("type".to_string(), PropertySchema::string("Type of location"))
            .with_property("status".to_string(), PropertySchema::string("Current state of location"))
            .with_property("atmosphere".to_string(), PropertySchema::string("General feel/mood"))
            .with_property("size".to_string(), PropertySchema::string("Size or scale"))
            .with_property("danger_level".to_string(), PropertySchema::string("Level of danger"))
            .with_property("notable_features".to_string(), PropertySchema::array(PropertyType::String))
            .with_required_property("name".to_string())
            .with_required_property("type".to_string())
            .with_allowed_edge("contains".to_string())
            .with_allowed_edge("connected_to".to_string())
    }

    pub fn default_faction() -> Self {
        Self::new("faction".to_string(), "An organization or group".to_string())
            .with_property("type".to_string(), PropertySchema::string("Type of faction"))
            .with_property("goals".to_string(), PropertySchema::array(PropertyType::String))
            .with_property("resources".to_string(), PropertySchema::array(PropertyType::String))
            .with_property("reputation".to_string(), PropertySchema::string("Public reputation"))
            .with_required_property("name".to_string())
            .with_required_property("type".to_string())
            .with_allowed_edge("allied_with".to_string())
            .with_allowed_edge("enemy_of".to_string())
            .with_allowed_edge("led_by".to_string())
    }

    pub fn default_item() -> Self {
        Self::new("item".to_string(), "An item, artifact, or object".to_string())
            .with_property("type".to_string(), PropertySchema::string("Type of item"))
            .with_property("rarity".to_string(), PropertySchema::string("Item rarity"))
            .with_property("value".to_string(), PropertySchema::string("Item value"))
            .with_property("properties".to_string(), PropertySchema::array(PropertyType::String))
            .with_required_property("name".to_string())
            .with_allowed_edge("owned_by".to_string())
            .with_allowed_edge("located_in".to_string())
    }

    pub fn default_event() -> Self {
        Self::new("event".to_string(), "An event or happening".to_string())
            .with_property("date".to_string(), PropertySchema::string("When the event occurred"))
            .with_property("location".to_string(), PropertySchema::reference("location"))
            .with_property("participants".to_string(), PropertySchema::array(PropertyType::Reference("character".to_string())))
            .with_property("outcome".to_string(), PropertySchema::string("Result of the event"))
            .with_required_property("name".to_string())
            .with_allowed_edge("happened_at".to_string())
            .with_allowed_edge("caused_by".to_string())
            .with_allowed_edge("leads_to".to_string())
    }

    pub fn default_session() -> Self {
        Self::new("session".to_string(), "A game session".to_string())
            .with_property("date".to_string(), PropertySchema::string("Session date"))
            .with_property("participants".to_string(), PropertySchema::array(PropertyType::Reference("character".to_string())))
            .with_property("summary".to_string(), PropertySchema::text("Session summary"))
            .with_property("notes".to_string(), PropertySchema::text("Session notes"))
            .with_required_property("name".to_string())
            .with_allowed_edge("includes".to_string())
    }
}

/// Schema definition for a property within an object type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertySchema {
    pub property_type: PropertyType,
    pub description: String,
    pub validation: Option<ValidationRule>,
    pub relationship: Option<RelationshipDefinition>,
    pub default_value: Option<serde_json::Value>,
    pub metadata: HashMap<String, String>,
}

impl PropertySchema {
    pub fn new(property_type: PropertyType, description: String) -> Self {
        Self {
            property_type,
            description,
            validation: None,
            relationship: None,
            default_value: None,
            metadata: HashMap::new(),
        }
    }

    pub fn string(description: &str) -> Self {
        Self::new(PropertyType::String, description.to_string())
    }

    pub fn text(description: &str) -> Self {
        Self::new(PropertyType::Text, description.to_string())
    }

    pub fn number(description: &str) -> Self {
        Self::new(PropertyType::Number, description.to_string())
    }

    pub fn boolean(description: &str) -> Self {
        Self::new(PropertyType::Boolean, description.to_string())
    }

    pub fn array(element_type: PropertyType) -> Self {
        Self::new(PropertyType::Array(Box::new(element_type)), "Array of items".to_string())
    }

    pub fn reference(target_type: &str) -> Self {
        Self::new(PropertyType::Reference(target_type.to_string()), format!("Reference to {}", target_type))
    }

    pub fn with_validation(mut self, validation: ValidationRule) -> Self {
        self.validation = Some(validation);
        self
    }

    pub fn with_relationship(mut self, relationship: RelationshipDefinition) -> Self {
        self.relationship = Some(relationship);
        self
    }

    pub fn with_default(mut self, default: serde_json::Value) -> Self {
        self.default_value = Some(default);
        self
    }
}

/// Types of properties that can be stored
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PropertyType {
    String,
    Text, // Longer text content
    Number,
    Boolean,
    Array(Box<PropertyType>),
    Object(HashMap<String, PropertySchema>), // Nested object
    Reference(String), // Reference to another object type
    Enum(Vec<String>), // Enumerated values
}

impl PropertyType {
    pub fn name(&self) -> &'static str {
        match self {
            PropertyType::String => "string",
            PropertyType::Text => "text",
            PropertyType::Number => "number",
            PropertyType::Boolean => "boolean",
            PropertyType::Array(_) => "array",
            PropertyType::Object(_) => "object",
            PropertyType::Reference(_) => "reference",
            PropertyType::Enum(_) => "enum",
        }
    }
}

/// Validation rules for property values
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationRule {
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pattern: Option<String>, // Regex pattern
    pub allowed_values: Option<Vec<String>>,
    pub required: bool,
}

impl ValidationRule {
    pub fn new() -> Self {
        Self {
            min_length: None,
            max_length: None,
            min_value: None,
            max_value: None,
            pattern: None,
            allowed_values: None,
            required: false,
        }
    }

    pub fn required() -> Self {
        Self {
            required: true,
            ..Self::new()
        }
    }

    pub fn with_length_range(mut self, min: Option<usize>, max: Option<usize>) -> Self {
        self.min_length = min;
        self.max_length = max;
        self
    }

    pub fn with_value_range(mut self, min: Option<f64>, max: Option<f64>) -> Self {
        self.min_value = min;
        self.max_value = max;
        self
    }

    pub fn with_allowed_values(mut self, values: Vec<String>) -> Self {
        self.allowed_values = Some(values);
        self
    }

    pub fn with_pattern(mut self, pattern: String) -> Self {
        self.pattern = Some(pattern);
        self
    }
}

/// Relationship definition for properties that create edges
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipDefinition {
    pub edge_type: String,
    pub target_type: Option<String>,
    pub description: String,
    pub cardinality: Cardinality,
}

impl RelationshipDefinition {
    pub fn new(edge_type: String, description: String) -> Self {
        Self {
            edge_type,
            target_type: None,
            description,
            cardinality: Cardinality::ManyToMany,
        }
    }

    pub fn with_target_type(mut self, target_type: String) -> Self {
        self.target_type = Some(target_type);
        self
    }

    pub fn with_cardinality(mut self, cardinality: Cardinality) -> Self {
        self.cardinality = cardinality;
        self
    }
}

/// Cardinality constraints for relationships
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Cardinality {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
}

/// Schema definition for edge types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeTypeSchema {
    pub name: String,
    pub description: String,
    pub allowed_source_types: Vec<String>,
    pub allowed_target_types: Vec<String>,
    pub properties: HashMap<String, PropertySchema>,
    pub bidirectional: bool,
    pub metadata: HashMap<String, String>,
}

impl EdgeTypeSchema {
    pub fn new(name: String, description: String) -> Self {
        Self {
            name,
            description,
            allowed_source_types: Vec::new(),
            allowed_target_types: Vec::new(),
            properties: HashMap::new(),
            bidirectional: false,
            metadata: HashMap::new(),
        }
    }

    pub fn with_source_types(mut self, types: Vec<String>) -> Self {
        self.allowed_source_types = types;
        self
    }

    pub fn with_target_types(mut self, types: Vec<String>) -> Self {
        self.allowed_target_types = types;
        self
    }

    pub fn bidirectional(mut self) -> Self {
        self.bidirectional = true;
        self
    }

    pub fn with_property(mut self, name: String, schema: PropertySchema) -> Self {
        self.properties.insert(name, schema);
        self
    }

    // Default edge type schemas
    pub fn default_related_to() -> Self {
        Self::new("related_to".to_string(), "Generic relationship".to_string())
            .with_property("context".to_string(), PropertySchema::string("Context of the relationship"))
            .bidirectional()
    }

    pub fn default_contains() -> Self {
        Self::new("contains".to_string(), "Containment relationship".to_string())
            .with_source_types(vec!["location".to_string(), "faction".to_string()])
            .with_target_types(vec!["location".to_string(), "character".to_string(), "item".to_string()])
    }

    pub fn default_member_of() -> Self {
        Self::new("member_of".to_string(), "Membership relationship".to_string())
            .with_source_types(vec!["character".to_string()])
            .with_target_types(vec!["faction".to_string()])
            .with_property("role".to_string(), PropertySchema::string("Role within the organization"))
            .with_property("rank".to_string(), PropertySchema::string("Rank or level"))
    }

    pub fn default_knows() -> Self {
        Self::new("knows".to_string(), "Knowledge relationship between characters".to_string())
            .with_source_types(vec!["character".to_string()])
            .with_target_types(vec!["character".to_string()])
            .with_property("relationship".to_string(), PropertySchema::string("Nature of the relationship"))
            .bidirectional()
    }

    pub fn default_enemy_of() -> Self {
        Self::new("enemy_of".to_string(), "Hostile relationship".to_string())
            .with_source_types(vec!["character".to_string(), "faction".to_string()])
            .with_target_types(vec!["character".to_string(), "faction".to_string()])
            .with_property("reason".to_string(), PropertySchema::string("Reason for hostility"))
            .bidirectional()
    }

    pub fn default_ally_of() -> Self {
        Self::new("ally_of".to_string(), "Allied relationship".to_string())
            .with_source_types(vec!["character".to_string(), "faction".to_string()])
            .with_target_types(vec!["character".to_string(), "faction".to_string()])
            .with_property("alliance_type".to_string(), PropertySchema::string("Type of alliance"))
            .bidirectional()
    }
}

/// Result of schema validation
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
}

impl ValidationResult {
    pub fn valid() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn invalid(errors: Vec<ValidationError>) -> Self {
        Self {
            valid: false,
            errors,
            warnings: Vec::new(),
        }
    }

    pub fn add_error(&mut self, error: ValidationError) {
        self.errors.push(error);
        self.valid = false;
    }

    pub fn add_warning(&mut self, warning: ValidationWarning) {
        self.warnings.push(warning);
    }
}

/// Validation error details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    pub property: String,
    pub message: String,
    pub error_type: ValidationErrorType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValidationErrorType {
    MissingRequired,
    TypeMismatch,
    InvalidValue,
    InvalidReference,
    ValidationRuleFailed,
}

/// Validation warning details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationWarning {
    pub property: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_creation() {
        let schema = SchemaDefinition::create_default();
        assert_eq!(schema.name, "default");
        assert!(schema.object_types.contains_key("character"));
        assert!(schema.object_types.contains_key("location"));
        assert!(schema.edge_types.contains_key("knows"));
    }

    #[test]
    fn test_object_type_schema() {
        let character_schema = ObjectTypeSchema::default_character();
        assert_eq!(character_schema.name, "character");
        assert!(character_schema.properties.contains_key("age"));
        assert!(character_schema.required_properties.contains(&"name".to_string()));
        assert!(character_schema.allowed_edges.contains(&"knows".to_string()));
    }

    #[test]
    fn test_property_schema() {
        let prop = PropertySchema::string("Test description")
            .with_validation(ValidationRule::required().with_length_range(Some(1), Some(100)));
        
        assert_eq!(prop.property_type.name(), "string");
        assert!(prop.validation.is_some());
        assert!(prop.validation.unwrap().required);
    }

    #[test]
    fn test_edge_type_schema() {
        let edge_schema = EdgeTypeSchema::default_knows();
        assert_eq!(edge_schema.name, "knows");
        assert!(edge_schema.bidirectional);
        assert!(edge_schema.allowed_source_types.contains(&"character".to_string()));
    }

    #[test]
    fn test_validation_result() {
        let mut result = ValidationResult::valid();
        assert!(result.valid);

        result.add_error(ValidationError {
            property: "test".to_string(),
            message: "Test error".to_string(),
            error_type: ValidationErrorType::MissingRequired,
        });

        assert!(!result.valid);
        assert_eq!(result.errors.len(), 1);
    }
}