use super::{
    Cardinality, EdgeTypeSchema, ObjectTypeSchema, PropertySchema, PropertyType,
    RelationshipDefinition, SchemaDefinition, ValidationRule,
};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::collections::HashMap;
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
        let schema_dir =
            std::env::var("UFORGE_SCHEMA_DIR").unwrap_or_else(|_| "./defaults/schemas".to_string());

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
            return Err(anyhow::anyhow!(
                "Schema directory does not exist: {:?}",
                dir_path
            ));
        }

        let mut schema_definition = SchemaDefinition::new(
            schema_name.to_string(),
            schema_version.to_string(),
            format!("Schema loaded from directory: {:?}", dir_path),
        );

        // Parse files in a stable order and fail the whole authoritative schema
        // load if any definition is malformed. Silently skipping one type can
        // weaken later object and edge validation.
        for path in Self::list_schema_files(dir_path)? {
            let json_schema = Self::load_json_schema_file(&path)
                .with_context(|| format!("Invalid schema file '{}'", path.display()))?;
            let object_type_schema = Self::convert_json_to_object_schema(json_schema)
                .with_context(|| format!("Invalid schema file '{}'", path.display()))?;
            let object_type_name = Self::extract_object_type_name(&object_type_schema.name);
            schema_definition.add_object_type(object_type_name, object_type_schema);
        }

        // Derive edge type constraints from relationship properties declared
        // in the schema files. The owning object type is the edge source; an
        // optional relationship.targetType narrows the target endpoint.
        Self::add_relationship_edge_types(&mut schema_definition);

        println!(
            "✅ Loaded {} object types from schema directory",
            schema_definition.object_types.len()
        );

        Ok(schema_definition)
    }

    /// Load a single JSON schema file
    fn load_json_schema_file<P: AsRef<Path>>(file_path: P) -> Result<JsonSchemaFile> {
        let content = fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read file: {:?}", file_path.as_ref()))?;

        let json: Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON in file: {:?}", file_path.as_ref()))?;

        let obj = json
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("JSON file must contain an object"))?;

        Self::reject_unknown_keys(
            obj,
            &["name", "description", "properties", "additionalProperties"],
            "schema",
        )?;

        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Field 'name' must be a non-empty string"))?
            .to_string();

        let description = match obj.get("description") {
            Some(value) => value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Field 'description' must be a string"))?
                .to_string(),
            None => "No description".to_string(),
        };

        if let Some(additional_properties) = obj.get("additionalProperties") {
            additional_properties
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("Field 'additionalProperties' must be a boolean"))?;
        }

        let properties = obj
            .get("properties")
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
            let prop_obj = prop_value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Property '{}' must be an object", prop_name))?;

            let property_schema = Self::convert_json_property_to_schema(
                &format!("property '{prop_name}'"),
                prop_obj,
            )?;

            // Check if this property is required
            if property_schema
                .validation
                .as_ref()
                .is_some_and(|validation| validation.required)
            {
                object_schema = object_schema.with_required_property(prop_name.clone());
            }

            // Add any relationship edge types as allowed edges
            if let Some(relationship) = &property_schema.relationship {
                object_schema = object_schema.with_allowed_edge(relationship.edge_type.clone());
            }

            object_schema = object_schema.with_property(prop_name, property_schema);
        }

        Ok(object_schema)
    }

    /// Convert a JSON property definition to a PropertySchema
    fn convert_json_property_to_schema(
        property_path: &str,
        prop_obj: &Map<String, Value>,
    ) -> Result<PropertySchema> {
        Self::reject_unknown_keys(
            prop_obj,
            &[
                "type",
                "description",
                "required",
                "items",
                "properties",
                "targetType",
                "enum",
                "validation",
                "relationship",
            ],
            property_path,
        )?;

        let prop_type = prop_obj
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("{property_path} is missing string field 'type'"))?;

        let description = match prop_obj.get("description") {
            Some(value) => value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("{property_path}.description must be a string"))?
                .to_string(),
            None => "No description".to_string(),
        };

        let required = match prop_obj.get("required") {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("{property_path}.required must be a boolean"))?,
            None => false,
        };

        if prop_type != "array" && prop_obj.contains_key("items") {
            bail!("{property_path}.items is only valid for type 'array'");
        }
        if prop_type != "object" && prop_obj.contains_key("properties") {
            bail!("{property_path}.properties is only valid for type 'object'");
        }
        if prop_type != "reference" && prop_obj.contains_key("targetType") {
            bail!("{property_path}.targetType is only valid for type 'reference'");
        }
        if !matches!(prop_type, "string" | "enum") && prop_obj.contains_key("enum") {
            bail!("{property_path}.enum is only valid for type 'string' or 'enum'");
        }

        let mut validation_rule = match prop_obj.get("validation") {
            Some(value) => Self::parse_validation_rule(property_path, value)?,
            None => ValidationRule::new(),
        };

        let top_level_enum = match prop_obj.get("enum") {
            Some(value) => Some(Self::parse_string_list(
                &format!("{property_path}.enum"),
                value,
            )?),
            None => None,
        };

        if let (Some(compatibility_values), Some(canonical_values)) =
            (&top_level_enum, &validation_rule.allowed_values)
            && compatibility_values != canonical_values
        {
            bail!("{property_path} declares conflicting enum and validation.allowed_values lists");
        }

        let enum_values = validation_rule
            .allowed_values
            .clone()
            .or_else(|| top_level_enum.clone());

        let property_type = match prop_type {
            "string" => enum_values
                .clone()
                .map_or(PropertyType::String, PropertyType::Enum),
            "text" => PropertyType::Text,
            "number" => PropertyType::Number,
            "boolean" => PropertyType::Boolean,
            "array" => {
                let element_type = match prop_obj.get("items") {
                    Some(value) => Self::parse_array_item_type(property_path, value)?,
                    // Compatibility with the shipped pre-v0.1.1 schemas. This
                    // shorthand is explicit in the file contract; supplied
                    // item schemas are never weakened or defaulted.
                    None => PropertyType::String,
                };
                PropertyType::Array(Box::new(element_type))
            }
            "object" => {
                let nested = prop_obj
                    .get("properties")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{property_path}.properties must be an object for type 'object'"
                        )
                    })?;
                let mut properties = HashMap::new();
                for (name, value) in nested {
                    let nested_obj = value.as_object().ok_or_else(|| {
                        anyhow::anyhow!("{property_path}.{name} must be an object")
                    })?;
                    properties.insert(
                        name.clone(),
                        Self::convert_json_property_to_schema(
                            &format!("{property_path}.{name}"),
                            nested_obj,
                        )?,
                    );
                }
                PropertyType::Object(properties)
            }
            "reference" => {
                let target_type = prop_obj
                    .get("targetType")
                    .and_then(Value::as_str)
                    .filter(|target| !target.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{property_path}.targetType must be a non-empty string for type 'reference'"
                        )
                    })?;
                PropertyType::Reference(target_type.to_string())
            }
            "enum" => PropertyType::Enum(enum_values.clone().ok_or_else(|| {
                anyhow::anyhow!("{property_path} of type 'enum' requires validation.allowed_values")
            })?),
            unknown => bail!("{property_path} has unsupported type '{unknown}'"),
        };

        if top_level_enum.is_some() {
            validation_rule.allowed_values = enum_values;
        }
        validation_rule.required = required;
        Self::validate_rule_compatibility(property_path, &property_type, &validation_rule)?;

        let mut property_schema = PropertySchema::new(property_type, description);
        if required || Self::has_validation_constraints(&validation_rule) {
            property_schema = property_schema.with_validation(validation_rule);
        }

        // Add relationship information if present
        if let Some(relationship) = prop_obj.get("relationship") {
            let relationship_obj = relationship
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("{property_path}.relationship must be an object"))?;
            Self::reject_unknown_keys(
                relationship_obj,
                &["edgeType", "targetType", "description"],
                &format!("{property_path}.relationship"),
            )?;
            let edge_type = relationship_obj
                .get("edgeType")
                .and_then(|v| v.as_str())
                .filter(|edge_type| !edge_type.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{property_path}.relationship.edgeType must be a non-empty string"
                    )
                })?
                .to_string();

            let rel_description = match relationship_obj.get("description") {
                Some(value) => value
                    .as_str()
                    .ok_or_else(|| {
                        anyhow::anyhow!("{property_path}.relationship.description must be a string")
                    })?
                    .to_string(),
                None => "Related entity".to_string(),
            };

            let mut relationship_def = RelationshipDefinition::new(edge_type, rel_description)
                .with_cardinality(Cardinality::ManyToMany);

            if let Some(target_type) = relationship_obj.get("targetType") {
                let target_type = target_type
                    .as_str()
                    .filter(|target| !target.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{property_path}.relationship.targetType must be a non-empty string"
                        )
                    })?;
                relationship_def = relationship_def.with_target_type(target_type.to_string());
            }

            property_schema = property_schema.with_relationship(relationship_def);
        }

        Ok(property_schema)
    }

    fn parse_array_item_type(property_path: &str, value: &Value) -> Result<PropertyType> {
        let item_path = format!("{property_path}.items");
        let item_obj = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{item_path} must be an object"))?;
        let item_schema = Self::convert_json_property_to_schema(&item_path, item_obj)?;
        if item_schema.validation.as_ref().is_some_and(|validation| {
            validation.required
                || validation.min_length.is_some()
                || validation.max_length.is_some()
                || validation.min_value.is_some()
                || validation.max_value.is_some()
                || validation.pattern.is_some()
                || (validation.allowed_values.is_some()
                    && !matches!(item_schema.property_type, PropertyType::Enum(_)))
        }) {
            bail!("{item_path} validation and requiredness are not supported");
        }
        if item_schema.relationship.is_some() {
            bail!("{item_path} relationship metadata is not supported");
        }
        Ok(item_schema.property_type)
    }

    fn parse_validation_rule(property_path: &str, value: &Value) -> Result<ValidationRule> {
        let validation_path = format!("{property_path}.validation");
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{validation_path} must be an object"))?;
        Self::reject_unknown_keys(
            object,
            &[
                "min_length",
                "max_length",
                "min_value",
                "max_value",
                "pattern",
                "allowed_values",
            ],
            &validation_path,
        )?;

        let min_length = Self::optional_usize(object, "min_length", &validation_path)?;
        let max_length = Self::optional_usize(object, "max_length", &validation_path)?;
        if let (Some(min), Some(max)) = (min_length, max_length)
            && min > max
        {
            bail!("{validation_path}.min_length must not exceed max_length");
        }

        let min_value = Self::optional_f64(object, "min_value", &validation_path)?;
        let max_value = Self::optional_f64(object, "max_value", &validation_path)?;
        if let (Some(min), Some(max)) = (min_value, max_value)
            && min > max
        {
            bail!("{validation_path}.min_value must not exceed max_value");
        }

        let pattern = match object.get("pattern") {
            Some(value) => {
                let pattern = value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("{validation_path}.pattern must be a string"))?;
                regex::Regex::new(pattern).with_context(|| {
                    format!("{validation_path}.pattern is not a valid regular expression")
                })?;
                Some(pattern.to_string())
            }
            None => None,
        };

        let allowed_values = object
            .get("allowed_values")
            .map(|value| {
                Self::parse_string_list(&format!("{validation_path}.allowed_values"), value)
            })
            .transpose()?;

        Ok(ValidationRule {
            min_length,
            max_length,
            min_value,
            max_value,
            pattern,
            allowed_values,
            required: false,
        })
    }

    fn optional_usize(
        object: &Map<String, Value>,
        key: &str,
        context: &str,
    ) -> Result<Option<usize>> {
        object
            .get(key)
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        anyhow::anyhow!("{context}.{key} must be a non-negative integer")
                    })
            })
            .transpose()
    }

    fn optional_f64(object: &Map<String, Value>, key: &str, context: &str) -> Result<Option<f64>> {
        object
            .get(key)
            .map(|value| {
                value
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| anyhow::anyhow!("{context}.{key} must be a finite number"))
            })
            .transpose()
    }

    fn parse_string_list(context: &str, value: &Value) -> Result<Vec<String>> {
        let values = value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("{context} must be an array of strings"))?;
        if values.is_empty() {
            bail!("{context} must not be empty");
        }
        let mut parsed = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let item = value
                .as_str()
                .filter(|item| !item.is_empty())
                .ok_or_else(|| anyhow::anyhow!("{context}[{index}] must be a non-empty string"))?
                .to_string();
            if parsed.contains(&item) {
                bail!("{context} contains duplicate value '{item}'");
            }
            parsed.push(item);
        }
        Ok(parsed)
    }

    fn validate_rule_compatibility(
        property_path: &str,
        property_type: &PropertyType,
        rule: &ValidationRule,
    ) -> Result<()> {
        let is_string_like = matches!(
            property_type,
            PropertyType::String
                | PropertyType::Text
                | PropertyType::Reference(_)
                | PropertyType::Enum(_)
        );
        if (rule.min_length.is_some()
            || rule.max_length.is_some()
            || rule.pattern.is_some()
            || rule.allowed_values.is_some())
            && !is_string_like
        {
            bail!("{property_path} has string validation rules for a non-string property");
        }
        if (rule.min_value.is_some() || rule.max_value.is_some())
            && !matches!(property_type, PropertyType::Number)
        {
            bail!("{property_path} has numeric validation rules for a non-number property");
        }
        Ok(())
    }

    fn has_validation_constraints(rule: &ValidationRule) -> bool {
        rule.min_length.is_some()
            || rule.max_length.is_some()
            || rule.min_value.is_some()
            || rule.max_value.is_some()
            || rule.pattern.is_some()
            || rule.allowed_values.is_some()
    }

    fn reject_unknown_keys(
        object: &Map<String, Value>,
        allowed: &[&str],
        context: &str,
    ) -> Result<()> {
        let mut unknown = object
            .keys()
            .filter(|key| !allowed.contains(&key.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        unknown.sort();
        if !unknown.is_empty() {
            bail!(
                "{context} contains unsupported field(s): {}",
                unknown.join(", ")
            );
        }
        Ok(())
    }

    /// Extract object type name from schema name (e.g., "add_npc" -> "npc")
    fn extract_object_type_name(schema_name: &str) -> String {
        if schema_name.starts_with("add_") {
            schema_name
                .strip_prefix("add_")
                .unwrap_or(schema_name)
                .to_string()
        } else {
            schema_name.to_string()
        }
    }

    /// Add edge types declared by relationship properties in the object schemas.
    fn add_relationship_edge_types(schema_definition: &mut SchemaDefinition) {
        let mut relationship_sources: Vec<_> = schema_definition
            .object_types
            .iter()
            .flat_map(|(object_type, object_schema)| {
                object_schema
                    .properties
                    .values()
                    .filter_map(|property_schema| property_schema.relationship.clone())
                    .map(|relationship| (object_type.clone(), relationship))
            })
            .collect();
        relationship_sources.sort_by(|(left_type, left_rel), (right_type, right_rel)| {
            left_rel
                .edge_type
                .cmp(&right_rel.edge_type)
                .then_with(|| left_type.cmp(right_type))
        });

        for (object_type, relationship) in relationship_sources {
            let edge_schema = schema_definition
                .edge_types
                .entry(relationship.edge_type.clone())
                .or_insert_with(|| {
                    EdgeTypeSchema::new(
                        relationship.edge_type.clone(),
                        relationship.description.clone(),
                    )
                });

            if !edge_schema.allowed_source_types.contains(&object_type) {
                edge_schema.allowed_source_types.push(object_type);
            }

            if let Some(target_type) = relationship.target_type
                && !edge_schema.allowed_target_types.contains(&target_type)
            {
                edge_schema.allowed_target_types.push(target_type);
            }

            edge_schema.allowed_source_types.sort();
            edge_schema.allowed_target_types.sort();
        }
    }

    /// Get a list of available schema files in a directory
    pub fn list_schema_files<P: AsRef<Path>>(directory: P) -> Result<Vec<PathBuf>> {
        let dir_path = directory.as_ref();
        if !dir_path.exists() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(dir_path).context("Failed to read schema directory")?;

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
            let result = Self::load_json_schema_file(&file_path)
                .and_then(Self::convert_json_to_object_schema);
            if let Err(error) = result {
                errors.push(format!("{}: {error:#}", file_path.display()));
            }
        }

        Ok(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

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

        let schema =
            SchemaIngestion::load_schemas_from_directory(temp_dir.path(), "test_schema", "1.0.0")
                .unwrap();

        assert_eq!(schema.name, "test_schema");
        assert!(schema.object_types.contains_key("test_object"));

        let object_type = &schema.object_types["test_object"];
        assert_eq!(object_type.name, "test_object");
        assert!(object_type.properties.contains_key("name"));
        assert!(object_type.properties.contains_key("value"));
        assert!(
            object_type
                .required_properties
                .contains(&"name".to_string())
        );
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

        let schema =
            SchemaIngestion::load_schemas_from_directory(temp_dir.path(), "test_schema", "1.0.0")
                .unwrap();

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
                        "targetType": "location",
                        "description": "Current location"
                    }
                }
            }
        }"#;

        create_test_schema_file(temp_dir.path(), "character", schema_content).unwrap();

        let schema =
            SchemaIngestion::load_schemas_from_directory(temp_dir.path(), "test_schema", "1.0.0")
                .unwrap();

        let character_type = &schema.object_types["character"];
        let location_prop = &character_type.properties["location"];

        assert!(location_prop.relationship.is_some());
        let relationship = location_prop.relationship.as_ref().unwrap();
        assert_eq!(relationship.edge_type, "located_in");
        assert_eq!(relationship.target_type.as_deref(), Some("location"));
        assert!(
            character_type
                .allowed_edges
                .contains(&"located_in".to_string())
        );
    }

    #[test]
    fn test_located_in_allows_location_sources() {
        let temp_dir = TempDir::new().unwrap();
        let schema_content = r#"{
            "name": "add_location",
            "description": "A location object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Location name",
                    "required": true
                },
                "parentLocation": {
                    "type": "string",
                    "description": "Parent location",
                    "relationship": {
                        "edgeType": "located_in",
                        "targetType": "location",
                        "description": "Parent location"
                    }
                }
            }
        }"#;

        create_test_schema_file(temp_dir.path(), "location", schema_content).unwrap();

        let schema =
            SchemaIngestion::load_schemas_from_directory(temp_dir.path(), "test_schema", "1.0.0")
                .unwrap();

        let located_in = &schema.edge_types["located_in"];
        assert!(
            located_in
                .allowed_source_types
                .contains(&"location".to_string())
        );
        assert!(
            located_in
                .allowed_target_types
                .contains(&"location".to_string())
        );
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

        let schema =
            SchemaIngestion::load_schemas_from_directory(temp_dir.path(), "test_schema", "1.0.0")
                .unwrap();

        let inventory_type = &schema.object_types["inventory"];
        let items_prop = &inventory_type.properties["items"];

        match &items_prop.property_type {
            PropertyType::Array(element_type) => {
                match element_type.as_ref() {
                    PropertyType::String => {} // Expected
                    _ => panic!("Expected string array element type"),
                }
            }
            _ => panic!("Expected array property type"),
        }
    }

    #[test]
    fn test_recursive_property_and_validation_conversion() {
        let temp_dir = TempDir::new().unwrap();
        let schema_content = r#"{
            "name": "add_record",
            "description": "A recursive record",
            "properties": {
                "summary": {
                    "type": "text",
                    "validation": {
                        "min_length": 3,
                        "max_length": 40,
                        "pattern": "^[A-Z]",
                        "allowed_values": ["Alpha", "Beta"]
                    }
                },
                "rating": {
                    "type": "number",
                    "validation": { "min_value": 0.5, "max_value": 5.0 }
                },
                "active": { "type": "boolean" },
                "references": {
                    "type": "array",
                    "items": { "type": "reference", "targetType": "record" }
                },
                "profile": {
                    "type": "object",
                    "properties": {
                        "alias": { "type": "string", "required": true },
                        "flags": {
                            "type": "array",
                            "items": { "type": "boolean" }
                        }
                    }
                },
                "owner": { "type": "reference", "targetType": "npc" },
                "state": {
                    "type": "enum",
                    "validation": { "allowed_values": ["draft", "final"] }
                },
                "legacyState": {
                    "type": "string",
                    "enum": ["open", "closed"]
                }
            }
        }"#;
        create_test_schema_file(temp_dir.path(), "record", schema_content).unwrap();

        let schema =
            SchemaIngestion::load_schemas_from_directory(temp_dir.path(), "test_schema", "1.0.0")
                .unwrap();
        let properties = &schema.object_types["record"].properties;

        assert!(matches!(
            properties["summary"].property_type,
            PropertyType::Text
        ));
        let summary_validation = properties["summary"].validation.as_ref().unwrap();
        assert_eq!(summary_validation.min_length, Some(3));
        assert_eq!(summary_validation.max_length, Some(40));
        assert_eq!(summary_validation.pattern.as_deref(), Some("^[A-Z]"));
        assert_eq!(
            summary_validation.allowed_values.as_deref(),
            Some(["Alpha".to_string(), "Beta".to_string()].as_slice())
        );
        let rating_validation = properties["rating"].validation.as_ref().unwrap();
        assert_eq!(rating_validation.min_value, Some(0.5));
        assert_eq!(rating_validation.max_value, Some(5.0));
        assert!(matches!(
            properties["active"].property_type,
            PropertyType::Boolean
        ));
        assert!(matches!(
            properties["references"].property_type,
            PropertyType::Array(ref item)
                if matches!(item.as_ref(), PropertyType::Reference(target) if target == "record")
        ));
        assert!(matches!(
            properties["owner"].property_type,
            PropertyType::Reference(ref target) if target == "npc"
        ));
        assert!(matches!(
            properties["state"].property_type,
            PropertyType::Enum(ref values) if values == &["draft", "final"]
        ));
        assert!(matches!(
            properties["legacyState"].property_type,
            PropertyType::Enum(ref values) if values == &["open", "closed"]
        ));

        let PropertyType::Object(profile) = &properties["profile"].property_type else {
            panic!("expected nested object property");
        };
        assert!(
            profile["alias"]
                .validation
                .as_ref()
                .is_some_and(|validation| validation.required)
        );
        assert!(matches!(
            profile["flags"].property_type,
            PropertyType::Array(ref item) if matches!(item.as_ref(), PropertyType::Boolean)
        ));
    }

    #[test]
    fn malformed_property_contracts_fail_closed_with_context() {
        let cases = [
            (
                "unknown_type",
                r#"{ "type": "mystery" }"#,
                "unsupported type 'mystery'",
            ),
            (
                "non_object_items",
                r#"{ "type": "array", "items": "string" }"#,
                "property 'value'.items must be an object",
            ),
            (
                "unknown_item_type",
                r#"{ "type": "array", "items": { "type": "mystery" } }"#,
                "property 'value'.items has unsupported type 'mystery'",
            ),
            (
                "object_without_properties",
                r#"{ "type": "object" }"#,
                "properties must be an object for type 'object'",
            ),
            (
                "reference_without_target",
                r#"{ "type": "reference" }"#,
                "targetType must be a non-empty string",
            ),
            (
                "unsupported_validation",
                r#"{ "type": "string", "validation": { "format": "uuid" } }"#,
                "contains unsupported field(s): format",
            ),
            (
                "invalid_pattern",
                r#"{ "type": "string", "validation": { "pattern": "[" } }"#,
                "pattern is not a valid regular expression",
            ),
            (
                "unsupported_property_field",
                r#"{ "type": "string", "default": "x" }"#,
                "contains unsupported field(s): default",
            ),
            (
                "conflicting_enum_forms",
                r#"{
                    "type": "enum",
                    "enum": ["a"],
                    "validation": { "allowed_values": ["b"] }
                }"#,
                "conflicting enum and validation.allowed_values",
            ),
        ];

        for (name, property, expected) in cases {
            let temp_dir = TempDir::new().unwrap();
            let schema = format!(
                r#"{{
                    "name": "add_test",
                    "properties": {{ "value": {property} }}
                }}"#
            );
            create_test_schema_file(temp_dir.path(), name, &schema).unwrap();
            let error = SchemaIngestion::load_schemas_from_directory(
                temp_dir.path(),
                "test_schema",
                "1.0.0",
            )
            .unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains(&format!("{name}.json")), "{message}");
            assert!(message.contains("property 'value'"), "{message}");
            assert!(message.contains(expected), "{message}");
        }
    }

    #[test]
    fn shipped_schemas_conform_to_external_contract() {
        let schema_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../defaults/schemas/Sine Nomine");
        let schema =
            SchemaIngestion::load_schemas_from_directory(schema_dir, "imported_schemas", "1.0.0")
                .unwrap();
        assert_eq!(schema.object_types.len(), 13);
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
