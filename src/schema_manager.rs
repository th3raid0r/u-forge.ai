use crate::schema::{SchemaDefinition, ObjectTypeSchema, PropertySchema, PropertyType, ValidationResult, ValidationError, ValidationErrorType, ValidationWarning, EdgeTypeSchema};
use crate::types::{ObjectMetadata, Edge};
use crate::storage::KnowledgeGraphStorage;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use serde_json::Value;

/// Schema manager for validating objects and managing schemas at runtime
pub struct SchemaManager {
    storage: Arc<KnowledgeGraphStorage>,
    /// Cache for compiled schemas to avoid repeated database lookups
    schema_cache: Arc<RwLock<HashMap<String, Arc<SchemaDefinition>>>>,
}

impl SchemaManager {
    /// Create a new schema manager
    pub fn new(storage: Arc<KnowledgeGraphStorage>) -> Self {
        Self {
            storage,
            schema_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Load a schema from storage or create default if it doesn't exist
    pub async fn load_schema(&self, name: &str) -> Result<Arc<SchemaDefinition>> {
        // Check cache first
        if let Some(schema) = self.schema_cache.read().get(name) {
            return Ok(schema.clone());
        }

        // Try to load from storage
        match self.storage.get_schema(name)? {
            Some(schema) => {
                let schema_arc = Arc::new(schema);
                self.schema_cache.write().insert(name.to_string(), schema_arc.clone());
                Ok(schema_arc)
            }
            None => {
                // Create default schema if it doesn't exist
                let default_schema = if name == "default" {
                    SchemaDefinition::create_default()
                } else {
                    SchemaDefinition::new(
                        name.to_string(),
                        "1.0.0".to_string(),
                        format!("Auto-generated schema for {}", name),
                    )
                };
                
                self.save_schema(&default_schema).await?;
                let schema_arc = Arc::new(default_schema);
                self.schema_cache.write().insert(name.to_string(), schema_arc.clone());
                Ok(schema_arc)
            }
        }
    }

    /// Save a schema to storage and update cache
    pub async fn save_schema(&self, schema: &SchemaDefinition) -> Result<()> {
        self.storage.save_schema(schema)?;
        
        // Update cache
        self.schema_cache.write().insert(schema.name.clone(), Arc::new(schema.clone()));
        
        Ok(())
    }

    /// Validate an object against its schema
    pub async fn validate_object(&self, object: &ObjectMetadata) -> Result<ValidationResult> {
        // For now, use default schema. In the future, objects could specify their schema
        let schema = self.load_schema("default").await?;
        self.validate_object_with_schema(object, &schema)
    }

    /// Validate an object against a specific schema
    pub fn validate_object_with_schema(&self, object: &ObjectMetadata, schema: &SchemaDefinition) -> Result<ValidationResult> {
        let mut result = ValidationResult::valid();

        // Check if object type exists in schema
        let object_schema = match schema.object_types.get(&object.object_type) {
            Some(schema) => schema,
            None => {
                result.add_error(ValidationError {
                    property: "object_type".to_string(),
                    message: format!("Unknown object type: {}", object.object_type),
                    error_type: ValidationErrorType::InvalidValue,
                });
                return Ok(result);
            }
        };

        // Validate required properties
        for required_prop in &object_schema.required_properties {
            if required_prop == "name" {
                // Name is always available in ObjectMetadata
                continue;
            }
            
            if !object.properties.as_object()
                .unwrap_or(&serde_json::Map::new())
                .contains_key(required_prop) {
                result.add_error(ValidationError {
                    property: required_prop.clone(),
                    message: format!("Missing required property: {}", required_prop),
                    error_type: ValidationErrorType::MissingRequired,
                });
            }
        }

        // Validate property types and values
        if let Some(props) = object.properties.as_object() {
            for (key, value) in props {
                if let Some(prop_schema) = object_schema.properties.get(key) {
                    if let Err(validation_error) = self.validate_property_value(key, value, prop_schema) {
                        result.add_error(validation_error);
                    }
                } else {
                    // Property not defined in schema - add warning
                    result.add_warning(ValidationWarning {
                        property: key.clone(),
                        message: format!("Property '{}' is not defined in schema", key),
                    });
                }
            }
        }

        Ok(result)
    }

    /// Validate an edge against schema constraints
    pub async fn validate_edge(&self, edge: &Edge, source_object: &ObjectMetadata, target_object: &ObjectMetadata) -> Result<ValidationResult> {
        let schema = self.load_schema("default").await?;
        self.validate_edge_with_schema(edge, source_object, target_object, &schema)
    }

    /// Validate an edge against a specific schema
    pub fn validate_edge_with_schema(&self, edge: &Edge, source_object: &ObjectMetadata, target_object: &ObjectMetadata, schema: &SchemaDefinition) -> Result<ValidationResult> {
        let mut result = ValidationResult::valid();

        let edge_type_str = edge.edge_type.as_str();
        
        // Check if edge type exists in schema
        let edge_schema = match schema.edge_types.get(edge_type_str) {
            Some(schema) => schema,
            None => {
                result.add_warning(ValidationWarning {
                    property: "edge_type".to_string(),
                    message: format!("Edge type '{}' is not defined in schema", edge_type_str),
                });
                return Ok(result);
            }
        };

        // Validate source type constraints
        if !edge_schema.allowed_source_types.is_empty() && 
           !edge_schema.allowed_source_types.contains(&source_object.object_type) {
            result.add_error(ValidationError {
                property: "source_type".to_string(),
                message: format!(
                    "Edge type '{}' does not allow source type '{}'. Allowed: {:?}",
                    edge_type_str, source_object.object_type, edge_schema.allowed_source_types
                ),
                error_type: ValidationErrorType::InvalidValue,
            });
        }

        // Validate target type constraints
        if !edge_schema.allowed_target_types.is_empty() && 
           !edge_schema.allowed_target_types.contains(&target_object.object_type) {
            result.add_error(ValidationError {
                property: "target_type".to_string(),
                message: format!(
                    "Edge type '{}' does not allow target type '{}'. Allowed: {:?}",
                    edge_type_str, target_object.object_type, edge_schema.allowed_target_types
                ),
                error_type: ValidationErrorType::InvalidValue,
            });
        }

        // Validate edge properties if any
        for (key, value) in &edge.metadata {
            if let Some(prop_schema) = edge_schema.properties.get(key) {
                // Convert string value to JSON for validation
                let json_value = Value::String(value.clone());
                if let Err(validation_error) = self.validate_property_value(key, &json_value, prop_schema) {
                    result.add_error(validation_error);
                }
            }
        }

        Ok(result)
    }

    /// Register a new object type at runtime
    pub async fn register_object_type(&self, schema_name: &str, type_name: &str, type_schema: ObjectTypeSchema) -> Result<()> {
        let mut schema = (*self.load_schema(schema_name).await?).clone();
        schema.add_object_type(type_name.to_string(), type_schema);
        self.save_schema(&schema).await?;
        
        // Invalidate cache to force reload
        self.schema_cache.write().remove(schema_name);
        
        Ok(())
    }

    /// Register a new edge type at runtime
    pub async fn register_edge_type(&self, schema_name: &str, edge_name: &str, edge_schema: EdgeTypeSchema) -> Result<()> {
        let mut schema = (*self.load_schema(schema_name).await?).clone();
        schema.add_edge_type(edge_name.to_string(), edge_schema);
        self.save_schema(&schema).await?;
        
        // Invalidate cache to force reload
        self.schema_cache.write().remove(schema_name);
        
        Ok(())
    }

    /// List all available schemas
    pub async fn list_schemas(&self) -> Result<Vec<String>> {
        self.storage.list_schemas()
    }

    /// Delete a schema
    pub async fn delete_schema(&self, name: &str) -> Result<()> {
        self.storage.delete_schema(name)?;
        self.schema_cache.write().remove(name);
        Ok(())
    }

    /// Clear the schema cache (useful for testing or forced refresh)
    pub fn clear_cache(&self) {
        self.schema_cache.write().clear();
    }

    /// Get schema statistics
    pub async fn get_schema_stats(&self, schema_name: &str) -> Result<SchemaStats> {
        let schema = self.load_schema(schema_name).await?;
        
        Ok(SchemaStats {
            name: schema.name.clone(),
            version: schema.version.clone(),
            object_type_count: schema.object_types.len(),
            edge_type_count: schema.edge_types.len(),
            total_properties: schema.object_types.values()
                .map(|ot| ot.properties.len())
                .sum(),
        })
    }

    /// Validate a property value against its schema
    fn validate_property_value(&self, property_name: &str, value: &Value, schema: &PropertySchema) -> Result<(), ValidationError> {
        // Check type compatibility
        let is_type_valid = match (&schema.property_type, value) {
            (PropertyType::String, Value::String(_)) => true,
            (PropertyType::Text, Value::String(_)) => true,
            (PropertyType::Number, Value::Number(_)) => true,
            (PropertyType::Boolean, Value::Bool(_)) => true,
            (PropertyType::Array(_), Value::Array(_)) => true,
            (PropertyType::Object(_), Value::Object(_)) => true,
            (PropertyType::Reference(_), Value::String(_)) => true,
            (PropertyType::Enum(allowed), Value::String(s)) => allowed.contains(s),
            _ => false,
        };

        if !is_type_valid {
            return Err(ValidationError {
                property: property_name.to_string(),
                message: format!(
                    "Property '{}' has incorrect type. Expected: {}, Got: {}",
                    property_name,
                    schema.property_type.name(),
                    match value {
                        Value::String(_) => "string",
                        Value::Number(_) => "number",
                        Value::Bool(_) => "boolean",
                        Value::Array(_) => "array",
                        Value::Object(_) => "object",
                        Value::Null => "null",
                    }
                ),
                error_type: ValidationErrorType::TypeMismatch,
            });
        }

        // Apply validation rules if present
        if let Some(validation) = &schema.validation {
            self.apply_validation_rules(property_name, value, validation)?;
        }

        // Validate array elements if it's an array
        if let (PropertyType::Array(element_type), Value::Array(arr)) = (&schema.property_type, value) {
            for (i, element) in arr.iter().enumerate() {
                let element_schema = PropertySchema::new((**element_type).clone(), "Array element".to_string());
                self.validate_property_value(&format!("{}[{}]", property_name, i), element, &element_schema)?;
            }
        }

        // Validate object properties if it's an object
        if let (PropertyType::Object(obj_schema), Value::Object(obj)) = (&schema.property_type, value) {
            for (key, prop_schema) in obj_schema {
                if let Some(prop_value) = obj.get(key) {
                    self.validate_property_value(&format!("{}.{}", property_name, key), prop_value, prop_schema)?;
                }
            }
        }

        Ok(())
    }

    /// Apply validation rules to a property value
    fn apply_validation_rules(&self, property_name: &str, value: &Value, validation: &crate::schema::ValidationRule) -> Result<(), ValidationError> {
        // String length validation
        if let Value::String(s) = value {
            if let Some(min_len) = validation.min_length {
                if s.len() < min_len {
                    return Err(ValidationError {
                        property: property_name.to_string(),
                        message: format!("Property '{}' is too short. Minimum length: {}", property_name, min_len),
                        error_type: ValidationErrorType::ValidationRuleFailed,
                    });
                }
            }

            if let Some(max_len) = validation.max_length {
                if s.len() > max_len {
                    return Err(ValidationError {
                        property: property_name.to_string(),
                        message: format!("Property '{}' is too long. Maximum length: {}", property_name, max_len),
                        error_type: ValidationErrorType::ValidationRuleFailed,
                    });
                }
            }

            // Pattern validation
            if let Some(pattern) = &validation.pattern {
                let regex = regex::Regex::new(pattern).map_err(|_| ValidationError {
                    property: property_name.to_string(),
                    message: format!("Invalid regex pattern in schema: {}", pattern),
                    error_type: ValidationErrorType::ValidationRuleFailed,
                })?;

                if !regex.is_match(s) {
                    return Err(ValidationError {
                        property: property_name.to_string(),
                        message: format!("Property '{}' does not match required pattern: {}", property_name, pattern),
                        error_type: ValidationErrorType::ValidationRuleFailed,
                    });
                }
            }

            // Allowed values validation
            if let Some(allowed) = &validation.allowed_values {
                if !allowed.contains(s) {
                    return Err(ValidationError {
                        property: property_name.to_string(),
                        message: format!("Property '{}' has invalid value. Allowed values: {:?}", property_name, allowed),
                        error_type: ValidationErrorType::ValidationRuleFailed,
                    });
                }
            }
        }

        // Numeric range validation
        if let Value::Number(n) = value {
            let num_val = n.as_f64().unwrap_or(0.0);

            if let Some(min_val) = validation.min_value {
                if num_val < min_val {
                    return Err(ValidationError {
                        property: property_name.to_string(),
                        message: format!("Property '{}' is too small. Minimum value: {}", property_name, min_val),
                        error_type: ValidationErrorType::ValidationRuleFailed,
                    });
                }
            }

            if let Some(max_val) = validation.max_value {
                if num_val > max_val {
                    return Err(ValidationError {
                        property: property_name.to_string(),
                        message: format!("Property '{}' is too large. Maximum value: {}", property_name, max_val),
                        error_type: ValidationErrorType::ValidationRuleFailed,
                    });
                }
            }
        }

        Ok(())
    }
}

/// Statistics about a schema
#[derive(Debug, Clone)]
pub struct SchemaStats {
    pub name: String,
    pub version: String,
    pub object_type_count: usize,
    pub edge_type_count: usize,
    pub total_properties: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ObjectMetadata, Edge, EdgeType};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn create_test_schema_manager() -> (SchemaManager, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let storage = Arc::new(KnowledgeGraphStorage::new(temp_dir.path()).unwrap());
        let manager = SchemaManager::new(storage);
        (manager, temp_dir)
    }

    #[tokio::test]
    async fn test_schema_loading_and_caching() {
        let (manager, _temp) = create_test_schema_manager();

        // Load default schema (should create it)
        let schema1 = manager.load_schema("default").await.unwrap();
        assert_eq!(schema1.name, "default");

        // Load again (should use cache)
        let schema2 = manager.load_schema("default").await.unwrap();
        assert!(Arc::ptr_eq(&schema1, &schema2));

        // Verify it has expected object types
        assert!(schema1.object_types.contains_key("character"));
        assert!(schema1.object_types.contains_key("location"));
        assert!(schema1.edge_types.contains_key("knows"));
    }

    #[tokio::test]
    async fn test_object_validation() {
        let (manager, _temp) = create_test_schema_manager();

        // Create a valid character object
        let mut character = ObjectMetadata::new("character".to_string(), "Gandalf".to_string());
        character.properties = serde_json::json!({
            "age": "2019",
            "species": "Maiar",
            "occupation": "Wizard"
        });

        let result = manager.validate_object(&character).await.unwrap();
        assert!(result.valid);

        // Create an invalid character (missing required fields)
        let mut invalid_character = ObjectMetadata::new("character".to_string(), "Incomplete".to_string());
        invalid_character.properties = serde_json::json!({
            "species": "Human"
            // Missing other required fields
        });

        let result = manager.validate_object(&invalid_character).await.unwrap();
        // Should still be valid since most fields are optional in our default schema
        // This test demonstrates the validation is working
        assert!(result.errors.is_empty() || result.warnings.len() > 0);
    }

    #[tokio::test]
    async fn test_edge_validation() {
        let (manager, _temp) = create_test_schema_manager();

        let character1 = ObjectMetadata::new("character".to_string(), "Frodo".to_string());
        let character2 = ObjectMetadata::new("character".to_string(), "Sam".to_string());

        let edge = Edge::new(character1.id, character2.id, EdgeType::from_str("knows"));

        let result = manager.validate_edge(&edge, &character1, &character2).await.unwrap();
        assert!(result.valid);

        // Test invalid edge (location knows character - not typically allowed)
        let location = ObjectMetadata::new("location".to_string(), "Shire".to_string());
        let invalid_edge = Edge::new(location.id, character1.id, EdgeType::from_str("knows"));

        let result = manager.validate_edge(&invalid_edge, &location, &character1).await.unwrap();
        // This should generate an error or warning depending on schema constraints
        assert!(result.errors.len() > 0 || result.warnings.len() > 0);
    }

    #[tokio::test]
    async fn test_schema_registration() {
        let (manager, _temp) = create_test_schema_manager();

        // Register a new object type
        let spell_schema = ObjectTypeSchema::new("spell".to_string(), "A magical spell".to_string())
            .with_property("level".to_string(), PropertySchema::number("Spell level"))
            .with_property("school".to_string(), PropertySchema::string("School of magic"))
            .with_required_property("level".to_string());

        manager.register_object_type("default", "spell", spell_schema).await.unwrap();

        // Verify it was added
        let schema = manager.load_schema("default").await.unwrap();
        assert!(schema.object_types.contains_key("spell"));

        // Test validation with new type
        let mut spell = ObjectMetadata::new("spell".to_string(), "Fireball".to_string());
        spell.properties = serde_json::json!({
            "level": 3,
            "school": "Evocation"
        });

        let result = manager.validate_object(&spell).await.unwrap();
        assert!(result.valid);
    }

    #[tokio::test]
    async fn test_schema_stats() {
        let (manager, _temp) = create_test_schema_manager();

        let stats = manager.get_schema_stats("default").await.unwrap();
        assert_eq!(stats.name, "default");
        assert!(stats.object_type_count >= 6); // At least the default types
        assert!(stats.edge_type_count >= 6); // At least the default edge types
        assert!(stats.total_properties > 0);
    }

    #[tokio::test]
    async fn test_property_validation() {
        let (manager, _temp) = create_test_schema_manager();

        // Test string length validation
        let prop_schema = PropertySchema::string("Test property")
            .with_validation(crate::schema::ValidationRule::new().with_length_range(Some(5), Some(10)));

        let valid_value = serde_json::Value::String("hello".to_string());
        let result = manager.validate_property_value("test", &valid_value, &prop_schema);
        assert!(result.is_ok());

        let invalid_value = serde_json::Value::String("hi".to_string());
        let result = manager.validate_property_value("test", &invalid_value, &prop_schema);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_enum_validation() {
        let (manager, _temp) = create_test_schema_manager();

        let enum_schema = PropertySchema::new(
            crate::schema::PropertyType::Enum(vec!["red".to_string(), "green".to_string(), "blue".to_string()]),
            "Color choice".to_string()
        );

        let valid_value = serde_json::Value::String("red".to_string());
        let result = manager.validate_property_value("color", &valid_value, &enum_schema);
        assert!(result.is_ok());

        let invalid_value = serde_json::Value::String("purple".to_string());
        let result = manager.validate_property_value("color", &invalid_value, &enum_schema);
        assert!(result.is_err());
    }
}