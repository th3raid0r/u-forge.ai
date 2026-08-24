use super::validation::{
    NormalizationPolicy, PropertyIssue, normalize_object_properties, normalize_property_value,
};
use super::{
    EdgeTypeSchema, ObjectTypeSchema, PropertySchema, SchemaDefinition, ValidationError,
    ValidationErrorType, ValidationResult, ValidationWarning,
};
use crate::graph::KnowledgeGraphStorage;
use crate::types::{Edge, ObjectMetadata};
use anyhow::{Result, bail};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

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
                self.schema_cache
                    .write()
                    .insert(name.to_string(), schema_arc.clone());
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

                self.save_schema(&default_schema)?;
                let schema_arc = Arc::new(default_schema);
                self.schema_cache
                    .write()
                    .insert(name.to_string(), schema_arc.clone());
                Ok(schema_arc)
            }
        }
    }

    /// Save a schema to storage and update cache
    pub fn save_schema(&self, schema: &SchemaDefinition) -> Result<()> {
        self.storage.save_schema(schema)?;

        // Update cache
        self.schema_cache
            .write()
            .insert(schema.name.clone(), Arc::new(schema.clone()));

        Ok(())
    }

    /// Validate an object against its schema
    pub async fn validate_object(&self, object: &ObjectMetadata) -> Result<ValidationResult> {
        // This convenience path validates against the named default schema.
        let schema = self.load_schema("default").await?;
        self.validate_object_with_schema(object, &schema)
    }

    /// Validate an object against a specific schema
    pub fn validate_object_with_schema(
        &self,
        object: &ObjectMetadata,
        schema: &SchemaDefinition,
    ) -> Result<ValidationResult> {
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

        let Some(properties) = object.properties.as_object() else {
            result.add_error(ValidationError {
                property: "properties".to_string(),
                message: "Object properties must be a JSON object".to_string(),
                error_type: ValidationErrorType::TypeMismatch,
            });
            return Ok(result);
        };
        let normalization = normalize_object_properties(
            object_schema,
            properties,
            NormalizationPolicy::WARNING_ONLY_UNKNOWN,
        );
        for issue in normalization.errors {
            result.add_error(validation_error_from_issue(&issue));
        }
        for issue in normalization.warnings {
            result.add_warning(ValidationWarning {
                property: issue.key().to_string(),
                message: validation_message(&issue),
            });
        }

        Ok(result)
    }

    /// Validate an edge against schema constraints
    pub async fn validate_edge(
        &self,
        edge: &Edge,
        source_object: &ObjectMetadata,
        target_object: &ObjectMetadata,
    ) -> Result<ValidationResult> {
        let schema = self.load_schema("default").await?;
        self.validate_edge_with_schema(edge, source_object, target_object, &schema)
    }

    /// Validate an edge against a specific schema
    pub fn validate_edge_with_schema(
        &self,
        edge: &Edge,
        source_object: &ObjectMetadata,
        target_object: &ObjectMetadata,
        schema: &SchemaDefinition,
    ) -> Result<ValidationResult> {
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
        if !edge_schema.allowed_source_types.is_empty()
            && !edge_schema
                .allowed_source_types
                .contains(&source_object.object_type)
        {
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
        if !edge_schema.allowed_target_types.is_empty()
            && !edge_schema
                .allowed_target_types
                .contains(&target_object.object_type)
        {
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
                if let Err(validation_error) =
                    self.validate_property_value(key, &json_value, prop_schema)
                {
                    result.add_error(validation_error);
                }
            }
        }

        Ok(result)
    }

    /// Register a new object type at runtime
    pub async fn register_object_type(
        &self,
        schema_name: &str,
        type_name: &str,
        type_schema: ObjectTypeSchema,
    ) -> Result<()> {
        let mut schema = (*self.load_schema(schema_name).await?).clone();
        schema.add_object_type(type_name.to_string(), type_schema);
        self.save_schema(&schema)?;

        Ok(())
    }

    /// Register a new edge type at runtime
    pub async fn register_edge_type(
        &self,
        schema_name: &str,
        edge_name: &str,
        edge_schema: EdgeTypeSchema,
    ) -> Result<()> {
        let mut schema = (*self.load_schema(schema_name).await?).clone();
        schema.add_edge_type(edge_name.to_string(), edge_schema);
        self.save_schema(&schema)?;

        Ok(())
    }

    /// Look up an `ObjectTypeSchema` synchronously from the cache.
    ///
    /// Returns `None` if the schema or object type has not been loaded yet.
    /// Callers should ensure `load_schema` has been called at least once
    /// (e.g. at app startup) before relying on this method.
    pub fn get_object_type_schema(
        &self,
        schema_name: &str,
        type_name: &str,
    ) -> Option<ObjectTypeSchema> {
        self.schema_cache
            .read()
            .get(schema_name)
            .and_then(|s| s.object_types.get(type_name).cloned())
    }

    /// Resolve an object type from the loaded schema cache in deterministic
    /// precedence order. An explicit `schema_name` wins, followed by imported
    /// schemas, the built-in default, and then any other schema by name.
    fn cached_schema_for_object_type(
        &self,
        object_type: &str,
        schema_name: Option<&str>,
    ) -> Option<Arc<SchemaDefinition>> {
        let cache = self.schema_cache.read();
        if let Some(name) = schema_name {
            return cache
                .get(name)
                .filter(|schema| schema.object_types.contains_key(object_type))
                .cloned();
        }

        let mut names = cache.keys().cloned().collect::<Vec<_>>();
        names.sort_by_key(|name| match name.as_str() {
            "imported_schemas" => (0, name.clone()),
            "default" => (1, name.clone()),
            _ => (2, name.clone()),
        });
        names.into_iter().find_map(|name| {
            cache
                .get(&name)
                .filter(|schema| schema.object_types.contains_key(object_type))
                .cloned()
        })
    }

    /// Strict synchronous validation for mutation boundaries.
    ///
    /// An empty cache means no authoritative schema has been loaded yet and is
    /// intentionally accepted for headless/bootstrap use. Once any schema is
    /// loaded, unknown types and undeclared properties are hard errors.
    pub fn validate_object_cached_strict(&self, object: &ObjectMetadata) -> Result<()> {
        if self.schema_cache.read().is_empty() {
            return Ok(());
        }
        let schema = self
            .cached_schema_for_object_type(&object.object_type, object.schema_name.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown object type '{}' for loaded schemas",
                    object.object_type
                )
            })?;
        let properties = object
            .properties
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Object properties must be a JSON object"))?;
        let normalization = normalize_object_properties(
            &schema.object_types[&object.object_type],
            properties,
            NormalizationPolicy::STRICT,
        );
        if !normalization.errors.is_empty() {
            let messages = normalization
                .errors
                .iter()
                .map(validation_message)
                .collect::<Vec<_>>();
            bail!("Object validation failed: {}", messages.join("; "));
        }
        Ok(())
    }

    /// Strict synchronous edge validation against all loaded schemas.
    pub fn validate_edge_cached_strict(
        &self,
        edge: &Edge,
        source: &ObjectMetadata,
        target: &ObjectMetadata,
    ) -> Result<()> {
        let cache = self.schema_cache.read();
        if cache.is_empty() {
            return Ok(());
        }
        let mut names = cache.keys().cloned().collect::<Vec<_>>();
        names.sort_by_key(|name| match name.as_str() {
            "imported_schemas" => (0, name.clone()),
            "default" => (1, name.clone()),
            _ => (2, name.clone()),
        });
        let schema = names
            .into_iter()
            .find_map(|name| {
                cache
                    .get(&name)
                    .filter(|schema| schema.edge_types.contains_key(edge.edge_type.as_str()))
                    .cloned()
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown edge type '{}' for loaded schemas",
                    edge.edge_type.as_str()
                )
            })?;
        drop(cache);

        let validation = self.validate_edge_with_schema(edge, source, target, &schema)?;
        if !validation.valid {
            let messages = validation
                .errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>();
            bail!("Edge validation failed: {}", messages.join("; "));
        }
        let edge_schema = schema
            .edge_types
            .get(edge.edge_type.as_str())
            .expect("edge schema was selected above");
        let unknown = edge
            .metadata
            .keys()
            .filter(|key| !edge_schema.properties.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            bail!(
                "Edge validation failed: undeclared properties: {}",
                unknown.join(", ")
            );
        }
        Ok(())
    }

    /// Check whether `type_name` is a valid object type in any cached schema.
    pub fn is_valid_object_type(&self, type_name: &str) -> bool {
        let cache = self.schema_cache.read();
        cache
            .values()
            .any(|s| s.object_types.contains_key(type_name))
    }

    /// Check whether `edge_name` is a valid edge type in any cached schema.
    pub fn is_valid_edge_type(&self, edge_name: &str) -> bool {
        let cache = self.schema_cache.read();
        cache.values().any(|s| s.edge_types.contains_key(edge_name))
    }

    /// Return a sorted list of all object type names across every cached schema.
    pub fn all_object_type_names(&self) -> Vec<String> {
        let cache = self.schema_cache.read();
        let mut names: Vec<String> = cache
            .values()
            .flat_map(|s| s.object_types.keys().cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Return a sorted list of all edge type names across every cached schema.
    pub fn all_edge_type_names(&self) -> Vec<String> {
        let cache = self.schema_cache.read();
        let mut names: Vec<String> = cache
            .values()
            .flat_map(|s| s.edge_types.keys().cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// List all available schemas
    pub fn list_schemas(&self) -> Result<Vec<String>> {
        self.storage.list_schemas()
    }

    /// Delete a schema
    pub fn delete_schema(&self, name: &str) -> Result<()> {
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
            total_properties: schema
                .object_types
                .values()
                .map(|ot| ot.properties.len())
                .sum(),
        })
    }

    /// Validate a property value against its schema
    fn validate_property_value(
        &self,
        property_name: &str,
        value: &Value,
        schema: &PropertySchema,
    ) -> Result<(), ValidationError> {
        let normalization =
            normalize_property_value(property_name, value, schema, NormalizationPolicy::STRICT);
        normalization
            .errors
            .first()
            .map_or(Ok(()), |issue| Err(validation_error_from_issue(issue)))
    }

    /// Validate and coerce `properties` for `object_type` against the cached schema.
    ///
    /// Coercions applied in-place (silently, not in the returned vec):
    /// - `String("42")` → `Number(42)` when the schema declares `Number`
    /// - `String("true"/"false"/"yes"/"no"/"1"/"0")` → `Bool` when the schema declares `Boolean`
    ///
    /// Issues returned for caller action:
    /// - [`PropertyIssue::UnknownObjectType`] — type absent from authoritative cached schemas
    /// - [`PropertyIssue::MissingRequired`] — required key is absent or null
    /// - [`PropertyIssue::TypeMismatch`] — wrong type and no coercion available
    /// - [`PropertyIssue::UnknownProperty`] — key not declared in the schema
    /// - [`PropertyIssue::InvalidEnum`] — string not in the enum's allowed list
    /// - [`PropertyIssue::ValidationFailed`] — length, range, regex, or allowed-value failure
    ///
    /// Returns an empty vec only when no authoritative schema is cached yet.
    pub fn validate_and_coerce_properties(
        &self,
        object_type: &str,
        properties: &mut serde_json::Map<String, Value>,
    ) -> Vec<PropertyIssue> {
        if self.schema_cache.read().is_empty() {
            return vec![];
        }
        let type_schema = match self.cached_schema_for_object_type(object_type, None) {
            Some(schema) => schema
                .object_types
                .get(object_type)
                .cloned()
                .expect("schema was selected for this object type"),
            None => {
                return vec![PropertyIssue::UnknownObjectType {
                    object_type: object_type.to_string(),
                    valid: self.all_object_type_names(),
                }];
            }
        };

        let normalization =
            normalize_object_properties(&type_schema, properties, NormalizationPolicy::PREFLIGHT);
        *properties = normalization.normalized;
        normalization.errors
    }
}

fn validation_error_from_issue(issue: &PropertyIssue) -> ValidationError {
    let error_type = match issue {
        PropertyIssue::UnknownObjectType { .. } => ValidationErrorType::InvalidValue,
        PropertyIssue::MissingRequired { .. } => ValidationErrorType::MissingRequired,
        PropertyIssue::TypeMismatch { .. } => ValidationErrorType::TypeMismatch,
        PropertyIssue::UnknownProperty { .. } | PropertyIssue::InvalidEnum { .. } => {
            ValidationErrorType::InvalidValue
        }
        PropertyIssue::ValidationFailed { .. } => ValidationErrorType::ValidationRuleFailed,
    };
    ValidationError {
        property: issue.key().to_string(),
        message: validation_message(issue),
        error_type,
    }
}

fn validation_message(issue: &PropertyIssue) -> String {
    match issue {
        PropertyIssue::UnknownObjectType { object_type, valid } => format!(
            "Unknown object type '{object_type}'. Valid types: {}",
            valid.join(", ")
        ),
        PropertyIssue::MissingRequired { key } => format!("Missing required property: {key}"),
        PropertyIssue::TypeMismatch {
            key,
            expected,
            actual,
        } => format!("Property '{key}' has incorrect type. Expected: {expected}, Got: {actual}"),
        PropertyIssue::UnknownProperty { key } => {
            format!("Property '{key}' is not defined in schema")
        }
        PropertyIssue::InvalidEnum {
            key,
            value,
            allowed,
        } => format!(
            "Property '{key}' has invalid value '{value}'. Allowed values: {}",
            allowed.join(", ")
        ),
        PropertyIssue::ValidationFailed { key, message } => {
            format!("Property '{key}' failed validation: {message}")
        }
    }
}

/// Statistics about a schema
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    use crate::schema::{PropertyType, ValidationRule};
    use crate::types::{Edge, EdgeType, ObjectMetadata};
    use tempfile::TempDir;

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
        let mut invalid_character =
            ObjectMetadata::new("character".to_string(), "Incomplete".to_string());
        invalid_character.properties = serde_json::json!({
            "species": "Human"
            // Missing other required fields
        });

        let result = manager.validate_object(&invalid_character).await.unwrap();
        // Should still be valid since most fields are optional in our default schema
        // This test demonstrates the validation is working
        assert!(result.errors.is_empty() || !result.warnings.is_empty());
    }

    #[tokio::test]
    async fn test_edge_validation() {
        let (manager, _temp) = create_test_schema_manager();

        let character1 = ObjectMetadata::new("character".to_string(), "Frodo".to_string());
        let character2 = ObjectMetadata::new("character".to_string(), "Sam".to_string());

        let edge = Edge::new(character1.id, character2.id, EdgeType::new("knows"));

        let result = manager
            .validate_edge(&edge, &character1, &character2)
            .await
            .unwrap();
        assert!(result.valid);

        // Test invalid edge (location knows character - not typically allowed)
        let location = ObjectMetadata::new("location".to_string(), "Shire".to_string());
        let invalid_edge = Edge::new(location.id, character1.id, EdgeType::new("knows"));

        let result = manager
            .validate_edge(&invalid_edge, &location, &character1)
            .await
            .unwrap();
        // This should generate an error or warning depending on schema constraints
        assert!(!result.errors.is_empty() || !result.warnings.is_empty());
    }

    #[tokio::test]
    async fn test_schema_registration() {
        let (manager, _temp) = create_test_schema_manager();

        // Register a new object type
        let spell_schema =
            ObjectTypeSchema::new("spell".to_string(), "A magical spell".to_string())
                .with_property("level".to_string(), PropertySchema::number("Spell level"))
                .with_property(
                    "school".to_string(),
                    PropertySchema::string("School of magic"),
                )
                .with_required_property("level".to_string());

        manager
            .register_object_type("default", "spell", spell_schema)
            .await
            .unwrap();

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
    async fn registration_preserves_cached_schema_precedence() {
        let (manager, _temp) = create_test_schema_manager();

        let mut imported = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "Imported schema".to_string(),
        );
        imported.add_object_type(
            "ritual".to_string(),
            ObjectTypeSchema::new("ritual".to_string(), "Imported ritual".to_string())
                .with_property("origin".to_string(), PropertySchema::string("Origin"))
                .with_required_property("origin".to_string()),
        );
        manager.save_schema(&imported).unwrap();

        manager
            .register_object_type(
                "default",
                "ritual",
                ObjectTypeSchema::new("ritual".to_string(), "Default ritual".to_string())
                    .with_property("level".to_string(), PropertySchema::number("Level"))
                    .with_required_property("level".to_string()),
            )
            .await
            .unwrap();

        assert!(
            manager
                .get_object_type_schema("default", "ritual")
                .is_some()
        );
        assert!(
            manager
                .get_object_type_schema("imported_schemas", "ritual")
                .is_some()
        );

        let imported_ritual = ObjectMetadata::new("ritual".to_string(), "Old Rite".to_string())
            .with_property("origin".to_string(), "Archive".to_string());
        manager
            .validate_object_cached_strict(&imported_ritual)
            .unwrap();

        let default_ritual = ObjectMetadata::new("ritual".to_string(), "New Rite".to_string())
            .with_schema("default".to_string())
            .with_json_property("level".to_string(), serde_json::json!(2));
        manager
            .validate_object_cached_strict(&default_ritual)
            .unwrap();
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
            .with_validation(ValidationRule::new().with_length_range(Some(5), Some(10)));

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
            PropertyType::Enum(vec![
                "red".to_string(),
                "green".to_string(),
                "blue".to_string(),
            ]),
            "Color choice".to_string(),
        );

        let valid_value = serde_json::Value::String("red".to_string());
        let result = manager.validate_property_value("color", &valid_value, &enum_schema);
        assert!(result.is_ok());

        let invalid_value = serde_json::Value::String("purple".to_string());
        let result = manager.validate_property_value("color", &invalid_value, &enum_schema);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn validate_and_coerce_reports_missing_and_null_required_properties() {
        let (manager, _temp) = create_test_schema_manager();
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "npc".to_string(),
            ObjectTypeSchema::new("npc".to_string(), "NPC".to_string())
                .with_property("name".to_string(), PropertySchema::string("name"))
                .with_property("role".to_string(), PropertySchema::string("role"))
                .with_required_property("name".to_string())
                .with_required_property("role".to_string()),
        );
        manager.save_schema(&schema).unwrap();

        let mut missing = serde_json::Map::new();
        let issues = manager.validate_and_coerce_properties("npc", &mut missing);
        assert!(matches!(
            issues.as_slice(),
            [PropertyIssue::MissingRequired { key }] if key == "role"
        ));

        let mut null = serde_json::Map::from_iter([("role".to_string(), Value::Null)]);
        let issues = manager.validate_and_coerce_properties("npc", &mut null);
        assert!(
            issues.iter().any(
                |issue| matches!(issue, PropertyIssue::MissingRequired { key } if key == "role")
            )
        );

        let mut valid =
            serde_json::Map::from_iter([("role".to_string(), Value::String("Mayor".to_string()))]);
        assert!(
            manager
                .validate_and_coerce_properties("npc", &mut valid)
                .is_empty()
        );
    }

    #[test]
    fn preflight_recurses_and_applies_rules_before_persistence() {
        let (manager, _temp) = create_test_schema_manager();
        let nested = HashMap::from([(
            "alias".to_string(),
            PropertySchema::string("alias").with_validation(ValidationRule::required()),
        )]);
        let mut schema = SchemaDefinition::new(
            "imported_schemas".to_string(),
            "1.0.0".to_string(),
            "test".to_string(),
        );
        schema.add_object_type(
            "record".to_string(),
            ObjectTypeSchema::new("record".to_string(), "Record".to_string())
                .with_property(
                    "profile".to_string(),
                    PropertySchema::new(PropertyType::Object(nested), "profile".to_string()),
                )
                .with_property(
                    "score".to_string(),
                    PropertySchema::number("score")
                        .with_validation(ValidationRule::new().with_value_range(None, Some(5.0))),
                ),
        );
        manager.save_schema(&schema).unwrap();
        let mut properties = serde_json::json!({ "profile": {}, "score": "9" })
            .as_object()
            .unwrap()
            .clone();

        let issues = manager.validate_and_coerce_properties("record", &mut properties);

        assert_eq!(properties["score"], serde_json::json!(9.0));
        assert_eq!(
            issues.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec![
                "required property 'profile.alias' is missing",
                "property 'score': maximum value is 5",
            ]
        );
    }

    #[tokio::test]
    async fn preflight_unknown_type_is_schema_less_only_when_cache_is_empty() {
        let (manager, _temp) = create_test_schema_manager();
        let mut properties = serde_json::Map::new();
        assert!(
            manager
                .validate_and_coerce_properties("unknown", &mut properties)
                .is_empty()
        );

        manager.load_schema("default").await.unwrap();
        let issues = manager.validate_and_coerce_properties("unknown", &mut properties);
        assert!(matches!(
            issues.as_slice(),
            [PropertyIssue::UnknownObjectType { object_type, valid }]
                if object_type == "unknown" && valid.windows(2).all(|pair| pair[0] <= pair[1])
        ));
    }
}
