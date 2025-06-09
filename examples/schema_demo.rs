// examples/schema_demo.rs

//! Schema Demo - Demonstrates the configurable schema system in u-forge.ai
//! 
//! This example shows how to:
//! 1. Create custom object types and edge types
//! 2. Define validation rules and property schemas
//! 3. Validate objects against schemas
//! 4. Use the schema system for different TTRPG systems
//! 
//! Run with: cargo run --example schema_demo

use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;
use u_forge_ai::{
    KnowledgeGraph, ObjectBuilder, ObjectTypeSchema, PropertySchema,
    EdgeTypeSchema, SchemaDefinition
};
use u_forge_ai::schema::{PropertyType, ValidationRule};

#[tokio::main]
async fn main() -> Result<()> {
    println!("🧙‍♂️ u-forge.ai Schema System Demo");
    println!("=====================================\n");

    // Create a temporary database for this demo
    let temp_dir = TempDir::new()?;
    let graph = KnowledgeGraph::new(temp_dir.path(), None)?;
    let schema_manager = graph.get_schema_manager();

    // Demo 1: Create a D&D 5e Schema
    println!("📚 Demo 1: Creating a D&D 5e Schema");
    println!("-----------------------------------");
    
    let mut dnd5e_schema = SchemaDefinition::new(
        "dnd5e".to_string(),
        "1.0.0".to_string(),
        "Dungeons & Dragons 5th Edition schema".to_string(),
    );

    // Define a Spell object type
    let spell_schema = ObjectTypeSchema::new(
        "spell".to_string(),
        "A magical spell".to_string(),
    )
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
                "Divination".to_string(),
                "Enchantment".to_string(),
                "Evocation".to_string(),
                "Illusion".to_string(),
                "Necromancy".to_string(),
                "Transmutation".to_string(),
            ]),
            "School of magic".to_string(),
        ),
    )
    .with_property(
        "casting_time".to_string(),
        PropertySchema::string("Time required to cast the spell"),
    )
    .with_property(
        "range".to_string(),
        PropertySchema::string("Range of the spell"),
    )
    .with_property(
        "components".to_string(),
        PropertySchema::array(PropertyType::String),
    )
    .with_property(
        "duration".to_string(),
        PropertySchema::string("Duration of the spell effect"),
    )
    .with_property(
        "damage".to_string(),
        PropertySchema::string("Damage dealt by the spell (if any)"),
    )
    .with_required_property("level".to_string())
    .with_required_property("school".to_string())
    .with_allowed_edge("learned_by".to_string())
    .with_allowed_edge("cast_at".to_string());

    dnd5e_schema.add_object_type("spell".to_string(), spell_schema);

    // Define a Character class object type
    let class_schema = ObjectTypeSchema::new(
        "class".to_string(),
        "A character class".to_string(),
    )
    .with_property(
        "hit_die".to_string(),
        PropertySchema::string("Hit die type (e.g., d8, d10)"),
    )
    .with_property(
        "primary_ability".to_string(),
        PropertySchema::array(PropertyType::String),
    )
    .with_property(
        "saving_throws".to_string(),
        PropertySchema::array(PropertyType::String),
    )
    .with_property(
        "spellcasting".to_string(),
        PropertySchema::boolean("Whether the class can cast spells"),
    )
    .with_required_property("hit_die".to_string())
    .with_allowed_edge("available_to".to_string());

    dnd5e_schema.add_object_type("class".to_string(), class_schema);

    // Define custom edge types
    let learned_by_edge = EdgeTypeSchema::new(
        "learned_by".to_string(),
        "Indicates a character has learned a spell".to_string(),
    )
    .with_source_types(vec!["spell".to_string()])
    .with_target_types(vec!["character".to_string()])
    .with_property(
        "mastery_level".to_string(),
        PropertySchema::new(
            PropertyType::Enum(vec![
                "novice".to_string(),
                "adept".to_string(),
                "expert".to_string(),
                "master".to_string(),
            ]),
            "Level of mastery".to_string(),
        ),
    );

    dnd5e_schema.add_edge_type("learned_by".to_string(), learned_by_edge);

    // Save the schema
    schema_manager.save_schema(&dnd5e_schema).await?;
    println!("✅ Created D&D 5e schema with custom spell and class types");

    // Demo 2: Create objects using the new schema
    println!("\n🎲 Demo 2: Creating Objects with Custom Types");
    println!("----------------------------------------------");

    // Create a spell
    let fireball = ObjectBuilder::custom("spell".to_string(), "Fireball".to_string())
        .with_json_property("level".to_string(), json!(3))
        .with_json_property("school".to_string(), json!("Evocation"))
        .with_json_property("casting_time".to_string(), json!("1 action"))
        .with_json_property("range".to_string(), json!("150 feet"))
        .with_json_property("components".to_string(), json!(["V", "S", "M"]))
        .with_json_property("duration".to_string(), json!("Instantaneous"))
        .with_json_property("damage".to_string(), json!("8d6 fire"))
        .with_description("A bright streak flashes from your pointing finger to a point you choose within range and then blossoms with a low roar into an explosion of flame.".to_string())
        .build();

    // Validate the spell against the D&D 5e schema
    let dnd5e_schema = schema_manager.load_schema("dnd5e").await?;
    let validation_result = schema_manager.validate_object_with_schema(&fireball, &dnd5e_schema)?;
    if validation_result.valid {
        println!("✅ Fireball spell validation passed");
        let fireball_id = graph.add_object(fireball)?;
        println!("   Created Fireball (ID: {})", fireball_id);
    } else {
        println!("❌ Fireball validation failed: {:?}", validation_result.errors);
    }

    // Create a wizard class
    let wizard_class = ObjectBuilder::custom("class".to_string(), "Wizard".to_string())
        .with_json_property("hit_die".to_string(), json!("d6"))
        .with_json_property("primary_ability".to_string(), json!(["Intelligence"]))
        .with_json_property("saving_throws".to_string(), json!(["Intelligence", "Wisdom"]))
        .with_json_property("spellcasting".to_string(), json!(true))
        .with_description("Masters of arcane magic, wizards are supreme magic-users.".to_string())
        .build();

    let class_validation = schema_manager.validate_object_with_schema(&wizard_class, &dnd5e_schema)?;
    if class_validation.valid {
        println!("✅ Wizard class validation passed");
        let _wizard_id = graph.add_object(wizard_class)?;
        println!("   Created Wizard class");
    } else {
        println!("❌ Wizard validation failed: {:?}", class_validation.errors);
    }

    // Demo 3: Schema validation errors
    println!("\n🚫 Demo 3: Schema Validation Errors");
    println!("-----------------------------------");

    // Try to create an invalid spell (wrong school)
    let invalid_spell = ObjectBuilder::custom("spell".to_string(), "Invalid Spell".to_string())
        .with_json_property("level".to_string(), json!(3))
        .with_json_property("school".to_string(), json!("Pyromancy")) // Invalid school
        .build();

    let invalid_validation = schema_manager.validate_object_with_schema(&invalid_spell, &dnd5e_schema)?;
    if !invalid_validation.valid {
        println!("❌ Invalid spell correctly rejected:");
        for error in &invalid_validation.errors {
            println!("   • {}: {}", error.property, error.message);
        }
    }

    // Try a spell with level out of range
    let overpowered_spell = ObjectBuilder::custom("spell".to_string(), "Overpowered Spell".to_string())
        .with_json_property("level".to_string(), json!(15)) // Level too high
        .with_json_property("school".to_string(), json!("Evocation"))
        .build();

    let overpowered_validation = schema_manager.validate_object_with_schema(&overpowered_spell, &dnd5e_schema)?;
    if !overpowered_validation.valid {
        println!("❌ Overpowered spell correctly rejected:");
        for error in &overpowered_validation.errors {
            println!("   • {}: {}", error.property, error.message);
        }
    }

    // Demo 4: Create a different RPG schema (Cyberpunk)
    println!("\n🤖 Demo 4: Creating a Cyberpunk 2077 Schema");
    println!("---------------------------------------------");

    // Register new types for Cyberpunk
    let cyberware_schema = ObjectTypeSchema::new(
        "cyberware".to_string(),
        "Cybernetic enhancements".to_string(),
    )
    .with_property(
        "type".to_string(),
        PropertySchema::new(
            PropertyType::Enum(vec![
                "neural".to_string(),
                "ocular".to_string(),
                "limb".to_string(),
                "organ".to_string(),
                "skin".to_string(),
            ]),
            "Type of cybernetic enhancement".to_string(),
        ),
    )
    .with_property(
        "humanity_cost".to_string(),
        PropertySchema::number("Humanity cost of the implant")
            .with_validation(ValidationRule::new().with_value_range(Some(0.0), Some(10.0))),
    )
    .with_property(
        "manufacturer".to_string(),
        PropertySchema::string("Company that manufactured the cyberware"),
    )
    .with_property(
        "street_price".to_string(),
        PropertySchema::number("Street price in eurodollars"),
    )
    .with_required_property("type".to_string())
    .with_required_property("humanity_cost".to_string())
    .with_allowed_edge("installed_in".to_string());

    graph.register_object_type("cyberware", cyberware_schema).await?;
    println!("✅ Registered Cyberware object type");

    // Create some cyberware
    let neural_processor = ObjectBuilder::custom("cyberware".to_string(), "Zetatech Neural Processor".to_string())
        .with_json_property("type".to_string(), json!("neural"))
        .with_json_property("humanity_cost".to_string(), json!(2.5))
        .with_json_property("manufacturer".to_string(), json!("Zetatech"))
        .with_json_property("street_price".to_string(), json!(15000))
        .with_description("Advanced neural interface for direct data access".to_string())
        .build();

    let cyber_validation = graph.validate_object(&neural_processor).await?;
    if cyber_validation.valid {
        println!("✅ Neural processor validation passed");
        let _cyber_id = graph.add_object(neural_processor)?;
        println!("   Created Zetatech Neural Processor");
    }

    // Demo 5: Schema statistics and introspection
    println!("\n📊 Demo 5: Schema Statistics");
    println!("----------------------------");

    let stats = graph.get_schema_stats("default").await?;
    println!("Default schema statistics:");
    println!("  • Object types: {}", stats.object_type_count);
    println!("  • Edge types: {}", stats.edge_type_count);
    println!("  • Total properties: {}", stats.total_properties);

    let schemas = graph.list_schemas().await?;
    println!("\nAvailable schemas: {:?}", schemas);

    // Demo 6: Object relationships with custom edge types
    println!("\n🔗 Demo 6: Custom Relationships");
    println!("-------------------------------");

    // Create a character and establish learned spell relationship
    let gandalf = ObjectBuilder::character("Gandalf".to_string())
        .with_description("A wise wizard".to_string())
        .build();

    let gandalf_id = graph.add_object(gandalf)?;

    // Note: In a full implementation, we would create the "learned_by" relationship here
    // This would involve extending the Edge system to support the custom edge properties
    println!("✅ Created character Gandalf");
    println!("   (Custom edge relationships would be demonstrated here)");

    println!("\n🎉 Schema Demo Complete!");
    println!("========================");
    println!("The configurable schema system allows you to:");
    println!("• Define custom object types for any TTRPG system");
    println!("• Set validation rules and property constraints");
    println!("• Create type-safe relationships between objects");
    println!("• Evolve schemas over time without breaking existing data");
    println!("• Support multiple RPG systems in the same database");

    Ok(())
}