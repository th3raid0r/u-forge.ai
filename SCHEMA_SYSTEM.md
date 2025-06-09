# Configurable Schema System for u-forge.ai

## Overview

The u-forge.ai configurable schema system provides runtime-definable object types and validation rules for TTRPG worldbuilding. This system allows users to create custom object types, properties, and relationships without recompiling the application, making it adaptable to any TTRPG system.

## Key Features

- **Dynamic Object Types**: Define new object types at runtime (spells, classes, vehicles, etc.)
- **Property Validation**: Type checking, value constraints, and custom validation rules
- **Relationship Schemas**: Define valid edge types and their constraints
- **Schema Versioning**: Support for schema evolution and migration
- **Multi-System Support**: Multiple schemas can coexist in the same database
- **JSON Storage**: Flexible property storage using JSON for maximum adaptability

## Architecture

### Core Components

1. **SchemaDefinition**: Top-level schema container
2. **ObjectTypeSchema**: Definition for a specific object type
3. **PropertySchema**: Individual property definitions with validation
4. **EdgeTypeSchema**: Relationship type definitions
5. **SchemaManager**: Runtime schema management and validation
6. **ValidationResult**: Validation outcome with errors and warnings

### Storage Layer

- **Schemas**: Stored in dedicated RocksDB column family (`CF_SCHEMAS`)
- **Objects**: Use JSON serialization for flexible property storage
- **Caching**: In-memory schema cache for performance
- **Migration**: Support for schema evolution without data loss

## Basic Usage

### Creating a Schema

```rust
use u_forge_ai::schema::{SchemaDefinition, ObjectTypeSchema, PropertySchema, PropertyType, ValidationRule};

// Create a new schema for D&D 5e
let mut dnd5e_schema = SchemaDefinition::new(
    "dnd5e".to_string(),
    "1.0.0".to_string(),
    "Dungeons & Dragons 5th Edition schema".to_string(),
);

// Define a spell object type
let spell_schema = ObjectTypeSchema::new("spell".to_string(), "A magical spell".to_string())
    .with_property(
        "level".to_string(),
        PropertySchema::number("Spell level (0-9)")
            .with_validation(ValidationRule::new().with_value_range(Some(0.0), Some(9.0))),
    )
    .with_property(
        "school".to_string(),
        PropertySchema::new(
            PropertyType::Enum(vec![
                "Abjuration".to_string(),
                "Conjuration".to_string(),
                "Evocation".to_string(),
                // ... other schools
            ]),
            "School of magic".to_string(),
        ),
    )
    .with_required_property("level".to_string())
    .with_required_property("school".to_string());

dnd5e_schema.add_object_type("spell".to_string(), spell_schema);
```

### Using the Schema Manager

```rust
// Get schema manager from knowledge graph
let schema_manager = graph.get_schema_manager();

// Save the schema
schema_manager.save_schema(&dnd5e_schema).await?;

// Register new types at runtime
schema_manager.register_object_type("dnd5e", "spell", spell_schema).await?;
```

### Creating and Validating Objects

```rust
// Create a spell object
let fireball = ObjectBuilder::custom("spell".to_string(), "Fireball".to_string())
    .with_json_property("level".to_string(), json!(3))
    .with_json_property("school".to_string(), json!("Evocation"))
    .build();

// Validate against schema
let dnd5e_schema = schema_manager.load_schema("dnd5e").await?;
let validation_result = schema_manager.validate_object_with_schema(&fireball, &dnd5e_schema)?;

if validation_result.valid {
    let object_id = graph.add_object(fireball)?;
    println!("Object created successfully: {}", object_id);
} else {
    for error in validation_result.errors {
        println!("Validation error: {}: {}", error.property, error.message);
    }
}
```

## Property Types

The schema system supports various property types:

### Basic Types

- **String**: Text values
- **Text**: Longer text content
- **Number**: Numeric values with optional range validation
- **Boolean**: True/false values

### Complex Types

- **Array**: Collections of other types
- **Object**: Nested objects with their own schemas
- **Reference**: References to other objects by ID
- **Enum**: Predefined list of allowed string values

### Example Property Definitions

```rust
// String with length constraints
PropertySchema::string("Character name")
    .with_validation(ValidationRule::new().with_length_range(Some(1), Some(50)))

// Number with value range
PropertySchema::number("Spell level")
    .with_validation(ValidationRule::new().with_value_range(Some(0.0), Some(9.0)))

// Enum with allowed values
PropertySchema::new(
    PropertyType::Enum(vec!["Fighter".to_string(), "Wizard".to_string(), "Rogue".to_string()]),
    "Character class".to_string()
)

// Array of strings
PropertySchema::array(PropertyType::String)

// Reference to another object
PropertySchema::reference("character")
```

## Validation Rules

### String Validation

- `min_length` / `max_length`: String length constraints
- `pattern`: Regex pattern matching
- `allowed_values`: Enum-like validation for strings

### Numeric Validation

- `min_value` / `max_value`: Numeric range constraints

### Example Validation Rules

```rust
ValidationRule::new()
    .with_length_range(Some(5), Some(100))
    .with_pattern(r"^[A-Za-z\s]+$".to_string())

ValidationRule::new()
    .with_value_range(Some(1.0), Some(20.0))
    .with_allowed_values(vec!["low".to_string(), "medium".to_string(), "high".to_string()])
```

## Edge Type Schemas

Define relationships between object types:

```rust
let learned_by_edge = EdgeTypeSchema::new(
    "learned_by".to_string(),
    "Indicates a character has learned a spell".to_string(),
)
.with_source_types(vec!["spell".to_string()])
.with_target_types(vec!["character".to_string()])
.with_property(
    "mastery_level".to_string(),
    PropertySchema::new(
        PropertyType::Enum(vec!["novice".to_string(), "expert".to_string()]),
        "Level of mastery".to_string(),
    ),
);
```

## Default Schema

The system includes a default schema with standard TTRPG object types:

- **character**: Game characters (PCs and NPCs)
- **location**: Places in the game world
- **faction**: Organizations and groups
- **item**: Equipment, artifacts, and objects
- **event**: Occurrences and plot points
- **session**: Game sessions and notes

Each comes with appropriate properties and allowed relationships.

## Schema Evolution

### Versioning

Schemas include version information for migration support:

```rust
let schema = SchemaDefinition::new(
    "my_system".to_string(),
    "2.0.0".to_string(),  // Version updated
    "Updated schema with new features".to_string(),
);
```

### Adding New Types

New object types can be added to existing schemas without breaking existing data:

```rust
// Add a new vehicle type to an existing schema
schema_manager.register_object_type("my_system", "vehicle", vehicle_schema).await?;
```

### Property Migration

When modifying existing properties, consider:

1. **Additive changes** (new optional properties): Safe
2. **Constraint changes** (tighter validation): May require data migration
3. **Type changes**: Require careful migration planning

## Performance Considerations

### Caching

- Schemas are cached in memory after first load
- Cache invalidation on schema updates
- Batch validation for better performance

### Storage

- JSON serialization for flexibility vs. performance trade-off
- Separate column family for schema storage
- Efficient schema lookups by name

### Validation

- Schema validation on object creation/update
- Optional validation for read-heavy workloads
- Bulk validation utilities for data migration

## Error Handling

### Validation Errors

The system provides detailed validation errors:

```rust
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub property: String,
    pub message: String,
    pub error_type: ValidationErrorType,
}

pub enum ValidationErrorType {
    MissingRequired,
    TypeMismatch,
    InvalidValue,
    InvalidReference,
    ValidationRuleFailed,
}
```

### Error Recovery

- Graceful handling of schema mismatches
- Warnings for unknown properties
- Fallback to permissive validation when needed

## Best Practices

### Schema Design

1. **Start Simple**: Begin with basic properties and add complexity over time
2. **Use Enums**: Prefer enums over free-text for constrained values
3. **Required vs Optional**: Carefully consider which properties are required
4. **Naming Conventions**: Use consistent, descriptive property names

### Validation Strategy

1. **Validate Early**: Check objects at creation time
2. **Provide Feedback**: Give clear, actionable error messages
3. **Allow Warnings**: Distinguish between errors and warnings
4. **Batch Operations**: Use bulk validation for large datasets

### Performance

1. **Cache Schemas**: Load once, use many times
2. **Selective Validation**: Skip validation for trusted data sources
3. **Index Properties**: Consider indexing frequently queried properties
4. **Lazy Loading**: Load schemas on demand

## Integration with Vector Search

The schema system integrates with the vector search engine:

- Objects maintain their vector embeddings regardless of schema
- Schema validation occurs before embedding generation
- Search results include schema-aware object metadata
- Type-aware filtering in search queries

## Future Enhancements

### Planned Features

1. **Computed Properties**: Properties derived from other properties
2. **Property Dependencies**: Conditional required properties
3. **Schema Inheritance**: Object type inheritance hierarchies
4. **Migration Scripts**: Automated data migration on schema changes
5. **Schema Import/Export**: Share schemas between installations
6. **Visual Schema Editor**: GUI for schema design

### Advanced Validation

1. **Cross-Object Validation**: Validation rules spanning multiple objects
2. **Custom Validators**: User-defined validation functions
3. **Async Validation**: External validation services
4. **Conditional Logic**: Property validation based on other properties

## Examples

See `examples/schema_demo.rs` for a comprehensive demonstration of:

- Creating custom D&D 5e and Cyberpunk schemas
- Defining object types with complex properties
- Validation success and failure scenarios
- Runtime schema registration
- Multi-schema environments

## API Reference

### Core Types

- `SchemaDefinition`: Top-level schema container
- `ObjectTypeSchema`: Object type definition
- `PropertySchema`: Property definition with validation
- `EdgeTypeSchema`: Relationship type definition
- `ValidationResult`: Validation outcome

### Manager Interface

- `SchemaManager::new()`: Create manager instance
- `load_schema(name)`: Load schema by name
- `save_schema(schema)`: Persist schema
- `validate_object(object)`: Validate against default schema
- `validate_object_with_schema(object, schema)`: Validate against specific schema
- `register_object_type()`: Add new object type
- `register_edge_type()`: Add new edge type

### Builder Patterns

- `ObjectTypeSchema::new()`: Create object type
- `PropertySchema::string()`: String property
- `PropertySchema::number()`: Numeric property
- `ValidationRule::new()`: Create validation rule

The configurable schema system provides the foundation for flexible, type-safe TTRPG worldbuilding while maintaining excellent performance and user experience.