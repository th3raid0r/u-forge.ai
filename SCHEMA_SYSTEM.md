# Configurable Schema System for u-forge.ai

## Overview

The u-forge.ai configurable schema system provides runtime-definable object types and validation rules for TTRPG worldbuilding. This system allows users to create custom object types, properties, and relationships without recompiling the application, making it adaptable to any TTRPG system.

**🎉 NEW: Dynamic Frontend Integration** - The frontend now dynamically adapts to backend schemas, displaying custom object types and properties defined in JSON schema files without requiring frontend code changes.

## Key Features

- **Dynamic Object Types**: Define new object types at runtime (spells, classes, vehicles, etc.)
- **Property Validation**: Type checking, value constraints, and custom validation rules
- **String-Based Edge Types**: Flexible relationship types using descriptive strings
- **Relationship Schemas**: Define valid edge types and their constraints
- **Schema Versioning**: Support for schema evolution and migration
- **Multi-System Support**: Multiple schemas can coexist in the same database
- **JSON Storage**: Flexible property storage using JSON for maximum adaptability

## Architecture

### Backend Components

1. **SchemaDefinition**: Top-level schema container
2. **ObjectTypeSchema**: Definition for a specific object type
3. **PropertySchema**: Individual property definitions with validation
4. **EdgeTypeSchema**: Relationship type definitions
5. **SchemaManager**: Runtime schema management and validation
6. **ValidationResult**: Validation outcome with errors and warnings

### Frontend Components (NEW)

1. **SchemaStore** (`schemaStore.ts`): Svelte store for dynamic schema management
2. **DynamicPropertyEditor** (`DynamicPropertyEditor.svelte`): Schema-driven form renderer
3. **Updated ContentEditor**: Now uses dynamic schemas instead of hardcoded types

### Storage Layer

- **Schemas**: Stored in dedicated RocksDB column family (`CF_SCHEMAS`)
- **Objects**: Use JSON serialization for flexible property storage
- **Caching**: In-memory schema cache for performance (backend + frontend)
- **Migration**: Support for schema evolution without data loss

## Basic Usage

### Creating a Schema (JSON File)

The easiest way to create new object types is by adding JSON schema files:

```json
// examples/schemas/spell.schema.json
{
  "name": "add_spell",
  "description": "Add a new Spell to the knowledge graph",
  "properties": {
    "name": {
      "type": "string",
      "description": "Spell name",
      "required": true
    },
    "level": {
      "type": "number",
      "description": "Spell level (0-9)",
      "required": true,
      "validation": {
        "min_value": 0,
        "max_value": 9
      }
    },
    "school": {
      "type": "string",
      "description": "School of magic",
      "required": true,
      "validation": {
        "allowed_values": [
          "Abjuration",
          "Conjuration", 
          "Evocation",
          "Illusion",
          "Necromancy",
          "Transmutation"
        ]
      }
    },
    "components": {
      "type": "array",
      "description": "Spell components (V, S, M)",
      "required": false
    }
  }
}
```

### Creating a Schema (Programmatically)

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

### Frontend Integration (NEW)

The frontend automatically adapts to schema changes:

```typescript
// Frontend automatically loads available schemas
import { schemaStore, availableObjectTypes } from '$lib/stores/schemaStore';

// Initialize schema system
await schemaStore.initialize();

// Get available object types (dynamically loaded)
const objectTypes = $availableObjectTypes; 
// Returns: [{ value: "spell", label: "Spell", icon: "✨" }, ...]

// Create object with schema validation
const defaultProperties = await schemaStore.createDefaultProperties("spell");
// Returns: { level: 0, school: "", components: [] }

// Validate properties against schema
const validation = await schemaStore.validateObjectProperties("spell", properties);
// Returns: { valid: boolean, errors: [...], warnings: [...] }
```

### Creating and Validating Objects

**Backend (Rust)**:
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
    
    // Create relationships using string-based edge types
    graph.connect_objects_str(character_id, object_id, "learned_spell")?;
    graph.connect_objects_str(object_id, evocation_school_id, "belongs_to_school")?;
} else {
    for error in validation_result.errors {
        println!("Validation error: {}: {}", error.property, error.message);
    }
}
```

**Frontend (Automatic UI Generation)**:
The frontend automatically generates forms based on schema definitions. Users simply:
1. Select "Spell" from the object type dropdown (dynamically populated)
2. Fill in form fields that are automatically generated from the schema
3. Get real-time validation feedback
4. Save with automatic schema validation

No frontend code changes needed when adding new object types!

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

### String-Based Relationship Types

**All edge types are now string-based for maximum flexibility**. This eliminates the need to map semantic relationships to predefined enum variants:

```rust
// Create relationships directly with descriptive strings
graph.connect_objects_str(character_id, spell_id, "learned_spell")?;
graph.connect_objects_str(faction_id, territory_id, "controls_territory")?;
graph.connect_objects_str(npc_id, quest_id, "offers_quest")?;
graph.connect_objects_str(item_id, location_id, "found_at")?;
```

### Schema-Defined Edge Types

Define valid relationship types and their constraints in schemas:

```rust
let learned_spell_edge = EdgeTypeSchema::new(
    "learned_spell".to_string(),
    "Indicates a character has learned a spell".to_string(),
)
.with_source_types(vec!["character".to_string()])
.with_target_types(vec!["spell".to_string()])
.with_property(
    "mastery_level".to_string(),
    PropertySchema::new(
        PropertyType::Enum(vec!["novice".to_string(), "expert".to_string()]),
        "Level of mastery".to_string(),
    ),
)
.with_property(
    "date_learned".to_string(),
    PropertySchema::string("When the spell was learned"),
);

// Add to schema
dnd5e_schema.add_edge_type("learned_spell".to_string(), learned_spell_edge);
```

### Benefits of String-Based Edge Types

1. **Semantic Clarity**: Relationship names like `"governs"`, `"led_by"`, `"trades_with"` preserve exact meaning
2. **Domain Flexibility**: Different TTRPG systems can define custom relationship vocabularies
3. **No Enum Constraints**: Add new relationship types without code changes
4. **Schema Validation**: Edge type definitions provide validation and constraints

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

## Dynamic Frontend Implementation

### How It Works

1. **Schema Loading**: Frontend fetches available schemas from backend via Tauri commands
2. **Dynamic UI Generation**: Form fields are generated based on schema property definitions
3. **Real-time Validation**: Properties are validated against schema rules as users type
4. **Automatic Adaptation**: Adding new schema files automatically adds new object types to the UI

### Supported Property Types

The dynamic frontend supports all schema property types:

| Property Type | UI Component | Features |
|---------------|--------------|----------|
| `string` | Text input | Length validation, pattern matching |
| `text` | Textarea | Multi-line text with length limits |
| `number` | Number input | Min/max validation, step increments |
| `boolean` | Checkbox | True/false values |
| `array` | Dynamic list | Add/remove items, validation per item |
| `enum` | Select dropdown | Predefined options from schema |
| `object` | JSON editor | Nested object editing (planned) |
| `reference` | Object picker | References to other objects (planned) |

### Schema Store Features

- **Lazy Loading**: Object type schemas loaded on-demand for performance
- **Caching**: Schemas cached in memory after first load
- **Error Handling**: Graceful fallbacks when schemas can't be loaded
- **Validation**: Real-time property validation with user-friendly error messages
- **Default Values**: Automatic generation of default property values

## Examples

### JSON Schema Files

The project includes comprehensive example schemas in `src-tauri/examples/schemas/`:

- `player_character.schema.json` - Player characters with skills, affiliations
- `npc.schema.json` - Non-player characters
- `location.schema.json` - Places with climate, government details
- `faction.schema.json` - Organizations with goals, resources
- `quest.schema.json` - Missions with objectives, rewards
- `artifact.schema.json` - Magical items with powers
- `temporal.schema.json` - Time-based events
- `transportation.schema.json` - Vehicles and mounts
- `currency.schema.json` - Economic systems
- `inventory.schema.json` - Item collections
- `skills.schema.json` - Character abilities
- `setting_reference.schema.json` - Campaign settings
- `system_reference.schema.json` - Game system rules

### Adding a New Object Type

1. **Create Schema File** (`examples/schemas/vehicle.schema.json`):
```json
{
  "name": "add_vehicle",
  "description": "Add a new Vehicle to the knowledge graph",
  "properties": {
    "name": {
      "type": "string",
      "description": "Vehicle name",
      "required": true
    },
    "type": {
      "type": "string",
      "description": "Type of vehicle",
      "required": true,
      "validation": {
        "allowed_values": ["car", "ship", "aircraft", "mount"]
      }
    },
    "speed": {
      "type": "number",
      "description": "Maximum speed",
      "required": false,
      "validation": {
        "min_value": 0,
        "max_value": 1000
      }
    }
  }
}
```

2. **Restart Application**: The schema will be automatically loaded
3. **Use in Frontend**: "Vehicle" appears in object type dropdown automatically
4. **Dynamic Properties**: Form fields generated based on schema

### Edge Type Examples

```rust
// Domain-specific relationship types
graph.connect_objects_str(character_id, spell_id, "knows_spell")?;
graph.connect_objects_str(character_id, faction_id, "sworn_enemy_of")?;
graph.connect_objects_str(location_id, faction_id, "controlled_by")?;
graph.connect_objects_str(quest_id, npc_id, "given_by")?;
graph.connect_objects_str(item_id, character_id, "forged_by")?;

// No need for enum mapping - schemas define validity
schema.add_edge_type("knows_spell".to_string(), 
    EdgeTypeSchema::new("knows_spell".to_string(), "Character knowledge of spells")
        .with_source_types(vec!["character".to_string()])
        .with_target_types(vec!["spell".to_string()]));
```

### API Reference

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

### Relationship API

- `graph.connect_objects_str(from, to, "edge_type")`: Create string-based relationship
- `graph.connect_objects_weighted_str(from, to, "edge_type", weight)`: Weighted relationship
- `EdgeType::from_str("edge_type")`: Convert string to EdgeType (for compatibility)

### Builder Patterns

- `ObjectTypeSchema::new()`: Create object type
- `PropertySchema::string()`: String property
- `PropertySchema::number()`: Numeric property
- `ValidationRule::new()`: Create validation rule
- `EdgeTypeSchema::new()`: Create edge type definition

## Testing the System

The schema system can be tested with the CLI demo:

```bash
# Test with example schemas
cd backend
cargo run --example cli_demo ../src-tauri/examples/data/memory.json ../src-tauri/examples/schemas

# Expected output:
# ✅ Loaded 13 object types from schema directory
# ✅ Objects created: 220
# ✅ Relationships created: 312
```

Or run the full Tauri application:

```bash
# Start development environment
./dev.sh

# The frontend will automatically:
# - Load available schemas from backend
# - Display custom object types in dropdowns
# - Generate appropriate form fields
# - Provide real-time validation
```

## Benefits Achieved

### Complete Flexibility
- Any TTRPG system can define custom object types without frontend code changes
- Properties are completely configurable through JSON schema files
- No compilation required - just add schema files and restart

### Type Safety
- Real-time validation against schema rules
- Prevents invalid data entry with clear error messages
- Frontend and backend validation consistency

### Better User Experience
- Appropriate UI components for each property type (dropdowns for enums, number inputs for numbers, etc.)
- Auto-completion and validation feedback
- Context-aware form fields with helpful descriptions

### Maintainability
- Single source of truth for object definitions
- Schemas can be version controlled and shared
- Easy to update and distribute to team members

### Performance
- Lazy loading of schemas for fast startup
- Efficient caching system minimizes API calls
- Responsive UI that doesn't block on schema loading

The configurable schema system provides the foundation for flexible, type-safe TTRPG worldbuilding while maintaining excellent performance and user experience. With the new dynamic frontend integration, users can now define completely custom object types and see them immediately reflected in the user interface without any code changes.