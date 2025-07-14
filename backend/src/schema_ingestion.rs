use crate::schema::{SchemaDefinition, ObjectTypeSchema, PropertySchema, PropertyType, EdgeTypeSchema, ValidationRule, RelationshipDefinition, Cardinality};
use anyhow::{Context, Result};
use serde_json::{Value, Map};
use std::fs;
use std::path::{Path, PathBuf};

/// Schema ingestion system for loading JSON schema files
pub struct SchemaIngestion;

/// JSON schema structure as parsed from files
#[derive(Debug, Clone)]
struct JsonSchemaFile {
    name: String,
    description: String,
    properties: Map<String, Value>,
}

impl SchemaIngestion {
    /// Load schemas using environment variable or default path
    ///
    /// Uses UFORGE_SCHEMA_DIR environment variable if set, otherwise defaults to ./defaults/schemas
    pub fn load_default_schemas() -> Result<SchemaDefinition> {
        let schema_dir = std::env::var("UFORGE_SCHEMA_DIR")
            .unwrap_or_else(|_| "./defaults/schemas".to_string());

        println!("Attempting to load schemas from: {}", schema_dir);

        if !std::path::Path::new(&schema_dir).exists() {
            return Err(anyhow::anyhow!(
                "Schema directory not found: {}. Set UFORGE_SCHEMA_DIR environment variable or place schemas at ./defaults/schemas", 
                schema_dir
            ));
        }

        Self::load_schemas_from_directory(&schema_dir, "default", "1.0")
    }

    /// Load all JSON schema files from a directory and create a SchemaDefinition
    pub fn load_schemas_from_directory<P: AsRef<Path>>(
        directory: P,
        schema_name: &str,
        schema_version: &str,
    ) -> Result<SchemaDefinition> {
        let dir_path = directory.as_ref();
        if !dir_path.exists() {
            return Err(anyhow::anyhow!("Schema directory does not exist: {:?}", dir_path));
        }

        let mut schema_definition = SchemaDefinition::new(
            schema_name.to_string(),
            schema_version.to_string(),
            format!("Schema loaded from directory: {:?}", dir_path),
        );

        // Read all .json files in the directory
        let entries = fs::read_dir(dir_path)
            .context("Failed to read schema directory")?;

        let mut loaded_schemas = Vec::new();

        for entry in entries {
            let entry = entry.context("Failed to read directory entry")?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                match Self::load_json_schema_file(&path) {
                    Ok(json_schema) => {
                        loaded_schemas.push(json_schema);
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to load schema file {:?}: {}", path, e);
                    }
                }
            }
        }

        // Convert JSON schemas to ObjectTypeSchemas
        for json_schema in loaded_schemas {
            let object_type_schema = Self::convert_json_to_object_schema(json_schema)?;
            let object_type_name = Self::extract_object_type_name(&object_type_schema.name);
            schema_definition.add_object_type(object_type_name, object_type_schema);
        }

        // Add common edge types that appear in the schemas
        Self::add_common_edge_types(&mut schema_definition);

        println!("✅ Loaded {} object types from schema directory", schema_definition.object_types.len());

        Ok(schema_definition)
    }

    /// Load a single JSON schema file
    fn load_json_schema_file<P: AsRef<Path>>(file_path: P) -> Result<JsonSchemaFile> {
        let content = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {:?}", file_path.as_ref()))?;

        let json: Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON in file: {:?}", file_path.as_ref()))?;

        let obj = json.as_object()
            .ok_or_else(|| anyhow::anyhow!("JSON file must contain an object"))?;

        let name = obj.get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' field"))?
            .to_string();

        let description = obj.get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("No description")
            .to_string();

        let properties = obj.get("properties")
            .and_then(|v| v.as_object())
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'properties' field"))?
            .clone();

        Ok(JsonSchemaFile {
            name,
            description,
            properties,
        })
    }

    /// Convert a JSON schema to an ObjectTypeSchema
    fn convert_json_to_object_schema(json_schema: JsonSchemaFile) -> Result<ObjectTypeSchema> {
        let object_type_name = Self::extract_object_type_name(&json_schema.name);
        let mut object_schema = ObjectTypeSchema::new(object_type_name, json_schema.description);

        for (prop_name, prop_value) in json_schema.properties {
            let prop_obj = prop_value.as_object()
                .ok_or_else(|| anyhow::anyhow!("Property '{}' must be an object", prop_name))?;

            let property_schema = Self::convert_json_property_to_schema(prop_name.clone(), prop_obj)?;
            
            // Check if this property is required
            if prop_obj.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
                object_schema = object_schema.with_required_property(prop_name.clone());
            }

            // Add any relationship edge types as allowed edges
            if let Some(relationship) = prop_obj.get("relationship") {
                if let Some(edge_type) = relationship.get("edgeType").and_then(|v| v.as_str()) {
                    object_schema = object_schema.with_allowed_edge(edge_type.to_string());
                }
            }

            object_schema = object_schema.with_property(prop_name, property_schema);
        }

        Ok(object_schema)
    }

    /// Convert a JSON property definition to a PropertySchema
    fn convert_json_property_to_schema(prop_name: String, prop_obj: &Map<String, Value>) -> Result<PropertySchema> {
        let prop_type = prop_obj.get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Property '{}' missing type", prop_name))?;

        let description = prop_obj.get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("No description")
            .to_string();

        let property_type = match prop_type {
            "string" => PropertyType::String,
            "number" => PropertyType::Number,
            "boolean" => PropertyType::Boolean,
            "array" => {
                // For arrays, try to determine the element type
                if let Some(items) = prop_obj.get("items") {
                    if let Some(items_obj) = items.as_object() {
                        if let Some(item_type) = items_obj.get("type").and_then(|v| v.as_str()) {
                            match item_type {
                                "string" => PropertyType::Array(Box::new(PropertyType::String)),
                                "number" => PropertyType::Array(Box::new(PropertyType::Number)),
                                "boolean" => PropertyType::Array(Box::new(PropertyType::Boolean)),
                                _ => PropertyType::Array(Box::new(PropertyType::String)), // Default to string
                            }
                        } else {
                            PropertyType::Array(Box::new(PropertyType::String))
                        }
                    } else {
                        PropertyType::Array(Box::new(PropertyType::String))
                    }
                } else {
                    PropertyType::Array(Box::new(PropertyType::String))
                }
            },
            _ => PropertyType::String, // Default to string for unknown types
        };

        let mut property_schema = PropertySchema::new(property_type, description);

        // Add validation rules
        let mut validation_rule = ValidationRule::new();
        let mut has_validation = false;

        // Handle enum values
        if let Some(enum_values) = prop_obj.get("enum") {
            if let Some(enum_array) = enum_values.as_array() {
                let enum_strings: Vec<String> = enum_array
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect();
                
                if !enum_strings.is_empty() {
                    // Update property type to enum
                    property_schema.property_type = PropertyType::Enum(enum_strings.clone());
                    validation_rule = validation_rule.with_allowed_values(enum_strings);
                    has_validation = true;
                }
            }
        }

        // Mark as required if specified
        if prop_obj.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
            validation_rule.required = true;
            has_validation = true;
        }

        if has_validation {
            property_schema = property_schema.with_validation(validation_rule);
        }

        // Add relationship information if present
        if let Some(relationship) = prop_obj.get("relationship") {
            if let Some(relationship_obj) = relationship.as_object() {
                let edge_type = relationship_obj.get("edgeType")
                    .and_then(|v| v.as_str())
                    .unwrap_or("related_to")
                    .to_string();
                
                let rel_description = relationship_obj.get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Related entity")
                    .to_string();

                let relationship_def = RelationshipDefinition::new(edge_type, rel_description)
                    .with_cardinality(Cardinality::ManyToMany);

                property_schema = property_schema.with_relationship(relationship_def);
            }
        }

        Ok(property_schema)
    }

    /// Extract object type name from schema name (e.g., "add_npc" -> "npc")
    fn extract_object_type_name(schema_name: &str) -> String {
        if schema_name.starts_with("add_") {
            schema_name.strip_prefix("add_").unwrap_or(schema_name).to_string()
        } else {
            schema_name.to_string()
        }
    }

    /// Add common edge types found in the schemas
    fn add_common_edge_types(schema_definition: &mut SchemaDefinition) {
        let edge_types = vec![
            ("owned_by", "Ownership relationship", vec!["artifact", "currency", "inventory", "transportation"], vec!["player_character", "npc", "faction"]),
            ("led_by", "Leadership relationship", vec!["faction"], vec!["player_character", "npc"]),
            ("allied_with", "Alliance relationship", vec!["faction"], vec!["faction"]),
            ("rival_of", "Rivalry relationship", vec!["faction"], vec!["faction"]),
            ("subfaction_of", "Sub-organization relationship", vec!["faction"], vec!["faction"]),
            ("a_part_of", "Containment relationship", vec!["location"], vec!["location"]),
            ("contains", "Contains relationship", vec!["location"], vec!["location", "artifact"]),
            ("present_in", "Presence relationship", vec!["player_character", "npc"], vec!["location"]),
            ("takes_place_in", "Event location relationship", vec!["quest"], vec!["location"]),
            ("located_at", "Item location relationship", vec!["artifact"], vec!["location"]),
            ("controlled_by", "Control relationship", vec!["location"], vec!["faction", "player_character", "npc"]),
            ("occurred_at", "Event occurrence relationship", vec!["temporal"], vec!["location"]),
            ("located_in", "Current location relationship", vec!["npc", "player_character"], vec!["location"]),
            ("originates_from", "Origin relationship", vec!["npc", "player_character"], vec!["location"]),
            ("member_of", "Membership relationship", vec!["player_character", "npc"], vec!["faction"]),
            ("player_can", "Player ability relationship", vec!["player_character"], vec!["skills"]),
            ("npc_can", "NPC ability relationship", vec!["npc"], vec!["skills"]),
            ("sourced_from", "Source reference relationship", vec!["skills"], vec!["system_reference"]),
            ("applies_to", "Application relationship", vec!["system_reference", "setting_reference"], vec!["player_character", "npc", "location", "faction"]),
            ("modifies_source", "Modification relationship", vec!["setting_reference"], vec!["system_reference"]),
            ("associated_with", "General association", vec!["quest"], vec!["artifact"]),
            ("found_at", "Discovery location", vec!["artifact"], vec!["location"]),
            ("subquest_of", "Sub-quest relationship", vec!["quest"], vec!["quest"]),
            ("affects_faction", "Faction impact relationship", vec!["quest"], vec!["faction"]),
        ];

        for (edge_name, description, source_types, target_types) in edge_types {
            let edge_schema = EdgeTypeSchema::new(edge_name.to_string(), description.to_string())
                .with_source_types(source_types.into_iter().map(|s| s.to_string()).collect())
                .with_target_types(target_types.into_iter().map(|s| s.to_string()).collect());

            schema_definition.add_edge_type(edge_name.to_string(), edge_schema);
        }
    }

    /// Get a list of available schema files in a directory
    pub fn list_schema_files<P: AsRef<Path>>(directory: P) -> Result<Vec<PathBuf>> {
        let dir_path = directory.as_ref();
        if !dir_path.exists() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(dir_path)
            .context("Failed to read schema directory")?;

        let mut schema_files = Vec::new();

        for entry in entries {
            let entry = entry.context("Failed to read directory entry")?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                schema_files.push(path);
            }
        }

        schema_files.sort();
        Ok(schema_files)
    }

    /// Validate that a directory contains valid schema files
    pub fn validate_schema_directory<P: AsRef<Path>>(directory: P) -> Result<Vec<String>> {
        let schema_files = Self::list_schema_files(&directory)?;
        let mut errors = Vec::new();

        for file_path in schema_files {
            if let Err(e) = Self::load_json_schema_file(&file_path) {
                errors.push(format!("{:?}: {}", file_path, e));
            }
        }

        Ok(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs::File;
    use std::io::Write;

    fn create_test_schema_file(dir: &Path, name: &str, content: &str) -> Result<()> {
        let file_path = dir.join(format!("{}.json", name));
        let mut file = File::create(file_path)?;
        file.write_all(content.as_bytes())?;
        Ok(())
    }

    #[test]
    fn test_load_simple_schema() {
        let temp_dir = TempDir::new().unwrap();
        let schema_content = r#"{
            "name": "add_test_object",
            "description": "A test object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Object name",
                    "required": true
                },
                "value": {
                    "type": "number",
                    "description": "Object value",
                    "required": false
                }
            }
        }"#;

        create_test_schema_file(temp_dir.path(), "test_object", schema_content).unwrap();

        let schema = SchemaIngestion::load_schemas_from_directory(
            temp_dir.path(),
            "test_schema",
            "1.0.0"
        ).unwrap();

        assert_eq!(schema.name, "test_schema");
        assert!(schema.object_types.contains_key("test_object"));
        
        let object_type = &schema.object_types["test_object"];
        assert_eq!(object_type.name, "test_object");
        assert!(object_type.properties.contains_key("name"));
        assert!(object_type.properties.contains_key("value"));
        assert!(object_type.required_properties.contains(&"name".to_string()));
    }

    #[test]
    fn test_enum_property_conversion() {
        let temp_dir = TempDir::new().unwrap();
        let schema_content = r#"{
            "name": "add_quest",
            "description": "A quest object",
            "properties": {
                "status": {
                    "type": "string",
                    "description": "Quest status",
                    "enum": ["Active", "Completed", "Failed"],
                    "required": true
                }
            }
        }"#;

        create_test_schema_file(temp_dir.path(), "quest", schema_content).unwrap();

        let schema = SchemaIngestion::load_schemas_from_directory(
            temp_dir.path(),
            "test_schema",
            "1.0.0"
        ).unwrap();

        let quest_type = &schema.object_types["quest"];
        let status_prop = &quest_type.properties["status"];
        
        match &status_prop.property_type {
            PropertyType::Enum(values) => {
                assert_eq!(values.len(), 3);
                assert!(values.contains(&"Active".to_string()));
                assert!(values.contains(&"Completed".to_string()));
                assert!(values.contains(&"Failed".to_string()));
            }
            _ => panic!("Expected enum property type"),
        }
    }

    #[test]
    fn test_relationship_property_conversion() {
        let temp_dir = TempDir::new().unwrap();
        let schema_content = r#"{
            "name": "add_character",
            "description": "A character object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "Character location",
                    "relationship": {
                        "edgeType": "located_in",
                        "description": "Current location"
                    }
                }
            }
        }"#;

        create_test_schema_file(temp_dir.path(), "character", schema_content).unwrap();

        let schema = SchemaIngestion::load_schemas_from_directory(
            temp_dir.path(),
            "test_schema",
            "1.0.0"
        ).unwrap();

        let character_type = &schema.object_types["character"];
        let location_prop = &character_type.properties["location"];
        
        assert!(location_prop.relationship.is_some());
        let relationship = location_prop.relationship.as_ref().unwrap();
        assert_eq!(relationship.edge_type, "located_in");
        assert!(character_type.allowed_edges.contains(&"located_in".to_string()));
    }

    #[test]
    fn test_array_property_conversion() {
        let temp_dir = TempDir::new().unwrap();
        let schema_content = r#"{
            "name": "add_inventory",
            "description": "An inventory object",
            "properties": {
                "items": {
                    "type": "array",
                    "description": "List of items",
                    "items": {
                        "type": "string"
                    }
                }
            }
        }"#;

        create_test_schema_file(temp_dir.path(), "inventory", schema_content).unwrap();

        let schema = SchemaIngestion::load_schemas_from_directory(
            temp_dir.path(),
            "test_schema",
            "1.0.0"
        ).unwrap();

        let inventory_type = &schema.object_types["inventory"];
        let items_prop = &inventory_type.properties["items"];
        
        match &items_prop.property_type {
            PropertyType::Array(element_type) => {
                match element_type.as_ref() {
                    PropertyType::String => {}, // Expected
                    _ => panic!("Expected string array element type"),
                }
            }
            _ => panic!("Expected array property type"),
        }
    }

    #[test]
    fn test_schema_validation() {
        let temp_dir = TempDir::new().unwrap();
        
        // Valid schema
        let valid_content = r#"{
            "name": "add_test",
            "description": "Valid schema",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name field"
                }
            }
        }"#;
        create_test_schema_file(temp_dir.path(), "valid", valid_content).unwrap();

        // Invalid schema (missing required fields)
        let invalid_content = r#"{
            "invalid": "schema"
        }"#;
        create_test_schema_file(temp_dir.path(), "invalid", invalid_content).unwrap();

        let errors = SchemaIngestion::validate_schema_directory(temp_dir.path()).unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("invalid.json"));
    }
}