//! Schema system: definition types, runtime manager, JSON ingestion.
mod definition;
mod ingestion;
mod manager;

pub use definition::{
    Cardinality, EdgeTypeSchema, ObjectTypeSchema, PropertySchema, PropertyType,
    RelationshipDefinition, SchemaDefinition, ValidationError, ValidationErrorType,
    ValidationResult, ValidationRule, ValidationWarning,
};
pub use ingestion::SchemaIngestion;
pub use manager::{PropertyIssue, SchemaManager, SchemaStats};
