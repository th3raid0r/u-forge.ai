// examples/swn_demo.rs

//! Stars Without Number Demo - Demonstrates schema ingestion and TTRPG worldbuilding
//! 
//! This example shows how to:
//! 1. Load JSON schema files from a directory
//! 2. Create a Stars Without Number campaign setting
//! 3. Populate it with characters, factions, locations, and quests
//! 4. Demonstrate relationships and validation
//! 
//! Run with: cargo run --example swn_demo

use anyhow::Result;
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;
use u_forge_ai::{
    KnowledgeGraph, ObjectBuilder, SchemaIngestion
};

#[tokio::main]
async fn main() -> Result<()> {
    println!("🚀 Stars Without Number - Campaign Demo");
    println!("=======================================\n");

    // Create a temporary database for this demo
    let temp_dir = TempDir::new()?;
    let graph = KnowledgeGraph::new(temp_dir.path(), None)?;
    let schema_manager = graph.get_schema_manager();

    // Load schemas from the examples/schemas directory
    println!("📚 Loading Stars Without Number Schemas");
    println!("---------------------------------------");
    
    let schema_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("schemas");
    
    if !schema_dir.exists() {
        println!("❌ Schema directory not found: {:?}", schema_dir);
        println!("Please ensure the schemas are in examples/schemas/");
        return Ok(());
    }

    let swn_schema = SchemaIngestion::load_schemas_from_directory(
        &schema_dir,
        "stars_without_number",
        "1.0.0",
    )?;

    schema_manager.save_schema(&swn_schema).await?;
    println!("✅ Loaded {} object types from SWN schemas\n", swn_schema.object_types.len());

    // Demo 1: Create Core Setting Elements
    println!("🌌 Demo 1: Creating the Outer Rim Sector");
    println!("----------------------------------------");

    // Create the main sector
    let outer_rim = ObjectBuilder::custom("location".to_string(), "Outer Rim Sector".to_string())
        .with_json_property("type".to_string(), json!("Sector"))
        .with_json_property("status".to_string(), json!("Active Colonial Region"))
        .with_json_property("atmosphere".to_string(), json!("Lawless frontier space"))
        .with_json_property("size".to_string(), json!("36 star systems"))
        .with_json_property("dangerLevel".to_string(), json!("High"))
        .with_json_property("notableFeatures".to_string(), json!([
            "Abandoned precursor ruins",
            "Rich asteroid mining fields", 
            "Unstable jump routes",
            "Pirate strongholds"
        ]))
        .with_json_property("tags".to_string(), json!(["frontier", "dangerous", "mining", "precursor"]))
        .with_description("A sparsely settled region on the galactic rim, known for its mineral wealth and lawless nature.".to_string())
        .build();

    let outer_rim_id = graph.add_object(outer_rim)?;
    println!("✅ Created Outer Rim Sector (ID: {})", outer_rim_id);

    // Create a key system within the sector
    let kepler_system = ObjectBuilder::custom("location".to_string(), "Kepler-442 System".to_string())
        .with_json_property("type".to_string(), json!("System"))
        .with_json_property("status".to_string(), json!("Contested Territory"))
        .with_json_property("parentLocation".to_string(), json!("Outer Rim Sector"))
        .with_json_property("atmosphere".to_string(), json!("Tense standoff between corporations"))
        .with_json_property("accessibility".to_string(), json!("Spike drive routes, some mined"))
        .with_json_property("notableFeatures".to_string(), json!([
            "Habitable garden world",
            "Massive asteroid belt", 
            "Ancient orbital station",
            "Corporate mining facilities"
        ]))
        .with_json_property("hazards".to_string(), json!([
            "Corporate security forces",
            "Asteroid field navigation",
            "Precursor automated defenses"
        ]))
        .with_json_property("tags".to_string(), json!(["corporate", "contested", "garden-world", "precursor"]))
        .with_description("A strategically important system containing a rare garden world and extensive mineral resources.".to_string())
        .build();

    let kepler_id = graph.add_object(kepler_system)?;
    println!("✅ Created Kepler-442 System (ID: {})", kepler_id);

    // Create the main planet
    let new_eden = ObjectBuilder::custom("location".to_string(), "New Eden".to_string())
        .with_json_property("type".to_string(), json!("Planet"))
        .with_json_property("status".to_string(), json!("Partially Terraformed"))
        .with_json_property("parentLocation".to_string(), json!("Kepler-442 System"))
        .with_json_property("atmosphere".to_string(), json!("Breathable with minor augmentation"))
        .with_json_property("accessibility".to_string(), json!("Orbital port, surface shuttles"))
        .with_json_property("size".to_string(), json!("Earth-sized"))
        .with_json_property("dangerLevel".to_string(), json!("Moderate"))
        .with_json_property("notableFeatures".to_string(), json!([
            "Expanding colonial settlements",
            "Untamed wilderness regions",
            "Precursor ruins in the southern continent",
            "Corporate agricultural domes"
        ]))
        .with_json_property("hazards".to_string(), json!([
            "Hostile native wildlife",
            "Unpredictable weather patterns",
            "Corporate territorial disputes"
        ]))
        .with_json_property("tags".to_string(), json!(["garden-world", "colonial", "terraforming", "wilderness"]))
        .with_description("A lush garden world in the early stages of colonial development, coveted by multiple factions.".to_string())
        .build();

    let new_eden_id = graph.add_object(new_eden)?;
    println!("✅ Created New Eden (ID: {})", new_eden_id);

    // Demo 2: Create Major Factions
    println!("\n🏛️ Demo 2: Creating Major Factions");
    println!("---------------------------------");

    // Stellar Dynamics Corporation
    let stellar_dynamics = ObjectBuilder::custom("faction".to_string(), "Stellar Dynamics Corporation".to_string())
        .with_json_property("type".to_string(), json!("Corporation"))
        .with_json_property("goals".to_string(), json!([
            "Monopolize Outer Rim mining operations",
            "Establish corporate governance on New Eden",
            "Exploit precursor technology discoveries"
        ]))
        .with_json_property("resources".to_string(), json!([
            "Advanced mining technology",
            "Corporate security fleet",
            "Significant capital reserves",
            "Political connections in Core Worlds"
        ]))
        .with_json_property("reputation".to_string(), json!("Ruthlessly efficient but exploitative"))
        .with_json_property("agendaPriority".to_string(), json!([
            "Secure New Eden mineral rights",
            "Eliminate Free Trader competition",
            "Reverse-engineer precursor artifacts"
        ]))
        .with_json_property("tags".to_string(), json!(["corporate", "mining", "ruthless", "powerful"]))
        .with_description("A massive interstellar corporation focused on resource extraction and territorial expansion.".to_string())
        .build();

    let stellar_id = graph.add_object(stellar_dynamics)?;
    println!("✅ Created Stellar Dynamics Corporation (ID: {})", stellar_id);

    // Free Traders Alliance
    let free_traders = ObjectBuilder::custom("faction".to_string(), "Free Traders Alliance".to_string())
        .with_json_property("type".to_string(), json!("Guild"))
        .with_json_property("goals".to_string(), json!([
            "Maintain free trade routes",
            "Resist corporate monopolization",
            "Protect independent colonies"
        ]))
        .with_json_property("resources".to_string(), json!([
            "Network of independent ships",
            "Established trade relationships",
            "Knowledge of hidden routes",
            "Colonial support"
        ]))
        .with_json_property("reputation".to_string(), json!("Trustworthy but sometimes chaotic"))
        .with_json_property("agendaPriority".to_string(), json!([
            "Break Stellar Dynamics blockade",
            "Establish New Eden trading post",
            "Unite independent spacers"
        ]))
        .with_json_property("tags".to_string(), json!(["traders", "independent", "freedom", "alliance"]))
        .with_description("A loose confederation of independent traders and spacers fighting corporate control.".to_string())
        .build();

    let traders_id = graph.add_object(free_traders)?;
    println!("✅ Created Free Traders Alliance (ID: {})", traders_id);

    // Set up faction rivalry
    graph.connect_objects_str(stellar_id, traders_id, "enemy_of")?;
    println!("🔗 Established rivalry between Stellar Dynamics and Free Traders");

    // Demo 3: Create Key NPCs
    println!("\n👥 Demo 3: Creating Key NPCs");
    println!("-----------------------------");

    // Corporate Executive
    let director_chen = ObjectBuilder::custom("npc".to_string(), "Director Liu Chen".to_string())
        .with_json_property("role".to_string(), json!("Regional Director"))
        .with_json_property("status".to_string(), json!("Active"))
        .with_json_property("currentLocation".to_string(), json!("New Eden"))
        .with_json_property("gender".to_string(), json!("Female"))
        .with_json_property("species".to_string(), json!("Human"))
        .with_json_property("background".to_string(), json!("Former military officer turned corporate executive"))
        .with_json_property("secret".to_string(), json!("Secretly funding research into precursor weapons"))
        .with_json_property("traits".to_string(), json!([
            "Ruthlessly efficient",
            "Military bearing",
            "Cybernetic eye implant"
        ]))
        .with_json_property("abilities".to_string(), json!([
            "Corporate Management",
            "Military Tactics",
            "Data Analysis",
            "Intimidation"
        ]))
        .with_json_property("importance".to_string(), json!("Major antagonist"))
        .with_json_property("reputation".to_string(), json!("Feared by subordinates, respected by competitors"))
        .with_json_property("alignment".to_string(), json!("Lawful Evil"))
        .with_json_property("motivation".to_string(), json!("Corporate advancement and personal power"))
        .with_json_property("affiliations".to_string(), json!(["Stellar Dynamics Corporation"]))
        .with_json_property("conditions".to_string(), json!(["Cybernetic enhancement", "High stress"]))
        .with_json_property("goals".to_string(), json!([
            "Secure complete control of New Eden",
            "Eliminate Free Trader interference",
            "Advance to corporate board of directors"
        ]))
        .with_json_property("tags".to_string(), json!(["corporate", "antagonist", "military", "cybernetic"]))
        .with_description("A cold, efficient corporate director with military background, overseeing Stellar Dynamics operations in the Outer Rim.".to_string())
        .build();

    let chen_id = graph.add_object(director_chen)?;
    println!("✅ Created Director Liu Chen (ID: {})", chen_id);

    // Free Trader Captain
    let captain_reeves = ObjectBuilder::custom("npc".to_string(), "Captain Sarah Reeves".to_string())
        .with_json_property("role".to_string(), json!("Ship Captain"))
        .with_json_property("status".to_string(), json!("Active"))
        .with_json_property("currentLocation".to_string(), json!("Kepler-442 System"))
        .with_json_property("gender".to_string(), json!("Female"))
        .with_json_property("species".to_string(), json!("Human"))
        .with_json_property("background".to_string(), json!("Third-generation spacer from the Frontier"))
        .with_json_property("secret".to_string(), json!("Has discovered coordinates to a precursor cache"))
        .with_json_property("traits".to_string(), json!([
            "Natural pilot",
            "Charismatic leader",
            "Protective of crew"
        ]))
        .with_json_property("abilities".to_string(), json!([
            "Starship Piloting",
            "Leadership",
            "Zero-G Combat",
            "Negotiation"
        ]))
        .with_json_property("importance".to_string(), json!("Major ally"))
        .with_json_property("reputation".to_string(), json!("Respected among independent traders"))
        .with_json_property("alignment".to_string(), json!("Chaotic Good"))
        .with_json_property("motivation".to_string(), json!("Freedom and prosperity for independent spacers"))
        .with_json_property("affiliations".to_string(), json!(["Free Traders Alliance"]))
        .with_json_property("conditions".to_string(), json!(["Ship captain", "Wanted by Stellar Dynamics"]))
        .with_json_property("goals".to_string(), json!([
            "Break the corporate blockade",
            "Establish safe trading routes",
            "Protect her crew and ship"
        ]))
        .with_json_property("tags".to_string(), json!(["trader", "pilot", "leader", "rebel"]))
        .with_description("A skilled starship captain leading the Free Traders resistance against corporate control.".to_string())
        .build();

    let reeves_id = graph.add_object(captain_reeves)?;
    println!("✅ Created Captain Sarah Reeves (ID: {})", reeves_id);

    // Demo 4: Create Artifacts and Technology
    println!("\n🔧 Demo 4: Creating Artifacts and Technology");
    println!("--------------------------------------------");

    // Precursor Navigation Computer
    let nav_computer = ObjectBuilder::custom("artifact".to_string(), "Precursor Navigation Matrix".to_string())
        .with_json_property("type".to_string(), json!("Precursor Technology"))
        .with_json_property("rarity".to_string(), json!("Legendary"))
        .with_json_property("effects".to_string(), json!([
            "Reveals hidden hyperspace routes",
            "Calculates impossible jump trajectories",
            "Immune to stellar interference"
        ]))
        .with_json_property("origin".to_string(), json!("Ancient precursor civilization"))
        .with_json_property("value".to_string(), json!("Incalculable - wars have been fought over less"))
        .with_json_property("usageRestrictions".to_string(), json!("Requires psychic interface, massive power source"))
        .with_json_property("lore".to_string(), json!("One of only three known navigation matrices, predates human spaceflight by millennia"))
        .with_json_property("chargePool".to_string(), json!("Unknown energy source, appears self-sustaining"))
        .with_json_property("relatedLocations".to_string(), json!(["New Eden"]))
        .with_json_property("tags".to_string(), json!(["precursor", "navigation", "psitech", "legendary"]))
        .with_description("An impossibly advanced navigation computer from the precursor civilization, capable of calculating hyperspace routes that should be impossible.".to_string())
        .build();

    let nav_computer_id = graph.add_object(nav_computer)?;
    println!("✅ Created Precursor Navigation Matrix (ID: {})", nav_computer_id);

    // Corporate Mining Suit
    let mining_suit = ObjectBuilder::custom("artifact".to_string(), "Stellar Dynamics Heavy Mining Suit".to_string())
        .with_json_property("type".to_string(), json!("Industrial Equipment"))
        .with_json_property("rarity".to_string(), json!("Uncommon"))
        .with_json_property("effects".to_string(), json!([
            "Environmental protection in vacuum",
            "Enhanced strength for heavy lifting",
            "Integrated mining laser array",
            "24-hour life support"
        ]))
        .with_json_property("origin".to_string(), json!("Stellar Dynamics manufacturing division"))
        .with_json_property("value".to_string(), json!("50,000 credits"))
        .with_json_property("usageRestrictions".to_string(), json!("Corporate authorization codes required"))
        .with_json_property("modificationLog".to_string(), json!([
            "Upgraded laser array (2387.3.15)",
            "Life support efficiency improvements (2387.8.22)"
        ]))
        .with_json_property("relatedCharacters".to_string(), json!(["Director Liu Chen"]))
        .with_json_property("tags".to_string(), json!(["corporate", "mining", "armor", "industrial"]))
        .with_description("A high-tech powered suit designed for dangerous mining operations in hostile environments.".to_string())
        .build();

    let mining_suit_id = graph.add_object(mining_suit)?;
    println!("✅ Created Mining Suit (ID: {})", mining_suit_id);

    // Demo 5: Create Active Quests
    println!("\n📜 Demo 5: Creating Active Quests");
    println!("---------------------------------");

    // Main quest line
    let main_quest = ObjectBuilder::custom("quest".to_string(), "The New Eden Crisis".to_string())
        .with_json_property("status".to_string(), json!("Active"))
        .with_json_property("type".to_string(), json!("Main"))
        .with_json_property("objectives".to_string(), json!([
            "Investigate corporate activities on New Eden",
            "Locate the missing precursor artifact",
            "Prevent corporate monopolization of the system",
            "Unite the Free Traders against Stellar Dynamics"
        ]))
        .with_json_property("rewards".to_string(), json!([
            "30,000 credits",
            "Free Trader Alliance membership",
            "Access to hidden trade routes",
            "Precursor technology insights"
        ]))
        .with_json_property("relatedCharacters".to_string(), json!(["Captain Sarah Reeves"]))
        .with_json_property("relatedNPCs".to_string(), json!(["Director Liu Chen", "Captain Sarah Reeves"]))
        .with_json_property("relatedLocations".to_string(), json!(["New Eden", "Kepler-442 System"]))
        .with_json_property("consequences".to_string(), json!([
            "Corporate control established or broken",
            "Free trade routes opened or closed",
            "Precursor technology distributed or monopolized",
            "Sector-wide conflict resolution"
        ]))
        .with_json_property("involvedFactions".to_string(), json!(["Stellar Dynamics Corporation", "Free Traders Alliance"]))
        .with_json_property("flags".to_string(), json!(["urgent", "sector-changing", "faction-war"]))
        .with_json_property("tags".to_string(), json!(["main-quest", "faction-conflict", "precursor", "economic"]))
        .with_description("A complex political and economic crisis threatening the balance of power in the Outer Rim sector.".to_string())
        .build();

    let main_quest_id = graph.add_object(main_quest)?;
    println!("✅ Created main quest: The New Eden Crisis (ID: {})", main_quest_id);

    // Side quest
    let side_quest = ObjectBuilder::custom("quest".to_string(), "Missing Cargo Shipment".to_string())
        .with_json_property("status".to_string(), json!("Active"))
        .with_json_property("type".to_string(), json!("Side"))
        .with_json_property("objectives".to_string(), json!([
            "Locate the missing cargo vessel 'Prosperity'",
            "Recover valuable medical supplies",
            "Determine cause of disappearance"
        ]))
        .with_json_property("rewards".to_string(), json!([
            "5,000 credits",
            "Medical supplies for crew",
            "Information about pirate activities"
        ]))
        .with_json_property("relatedCharacters".to_string(), json!(["Captain Sarah Reeves"]))
        .with_json_property("relatedLocations".to_string(), json!(["Kepler-442 System"]))
        .with_json_property("parentQuest".to_string(), json!("The New Eden Crisis"))
        .with_json_property("consequences".to_string(), json!([
            "Medical aid for colonial settlements",
            "Intel on pirate operations",
            "Improved Free Trader reputation"
        ]))
        .with_json_property("flags".to_string(), json!(["timed", "investigation"]))
        .with_json_property("tags".to_string(), json!(["side-quest", "rescue", "investigation", "pirates"]))
        .with_description("A cargo vessel carrying critical medical supplies has vanished in the asteroid belt.".to_string())
        .build();

    let side_quest_id = graph.add_object(side_quest)?;
    println!("✅ Created side quest: Missing Cargo Shipment (ID: {})", side_quest_id);

    // Demo 6: Create Player Characters
    println!("\n🎭 Demo 6: Creating Player Characters");
    println!("------------------------------------");

    // Example player character
    let player_character = ObjectBuilder::custom("player_character".to_string(), "Commander Alex Nova".to_string())
        .with_json_property("age".to_string(), json!("34"))
        .with_json_property("gender".to_string(), json!("Non-binary"))
        .with_json_property("occupation".to_string(), json!("Former Naval Officer"))
        .with_json_property("status".to_string(), json!("Active"))
        .with_json_property("species".to_string(), json!("Human"))
        .with_json_property("background".to_string(), json!("Resigned from the Terran Mandate Navy after witnessing corporate corruption"))
        .with_json_property("equipment".to_string(), json!([
            "Modified laser rifle",
            "Vacc suit with extra life support",
            "Personal communicator",
            "Naval service medals"
        ]))
        .with_json_property("secrets".to_string(), json!([
            "Has classified knowledge of naval ship movements",
            "Maintains contact with sympathetic naval officers"
        ]))
        .with_json_property("knownSecrets".to_string(), json!([
            "Corporate bribes to naval procurement",
            "Location of abandoned naval supply caches"
        ]))
        .with_json_property("publicInfo".to_string(), json!([
            "Honorably discharged naval commander",
            "Known for tactical brilliance",
            "Advocates for colonial independence"
        ]))
        .with_json_property("goals".to_string(), json!([
            "Expose corporate corruption",
            "Protect innocent colonists",
            "Find purpose beyond military service"
        ]))
        .with_json_property("affiliations".to_string(), json!(["Free Traders Alliance"]))
        .with_json_property("conditions".to_string(), json!(["Former military", "Idealistic"]))
        .with_json_property("tags".to_string(), json!(["player-character", "military", "leader", "idealist"]))
        .with_description("A former naval commander turned freedom fighter, seeking to protect colonial independence from corporate control.".to_string())
        .build();

    let player_id = graph.add_object(player_character)?;
    println!("✅ Created Player Character: Commander Alex Nova (ID: {})", player_id);

    // Demo 7: Display Relationships and Statistics
    println!("\n📊 Demo 7: Campaign Statistics");
    println!("------------------------------");

    let stats = graph.get_stats()?;
    println!("Total objects in campaign: {}", stats.node_count);
    println!("Total relationships: {}", stats.edge_count);
    println!("Total text chunks: {}", stats.chunk_count);

    let schema_stats = graph.get_schema_stats("stars_without_number").await?;
    println!("\nSchema Statistics:");
    println!("• Object types: {}", schema_stats.object_type_count);
    println!("• Edge types: {}", schema_stats.edge_type_count);
    println!("• Total properties: {}", schema_stats.total_properties);

    println!("\n🎯 Demo 8: Object Validation");
    println!("----------------------------");

    // Validate some objects against the schema
    let swn_schema = schema_manager.load_schema("stars_without_number").await?;
    
    // Test validation of the faction
    let faction_validation = schema_manager.validate_object_with_schema(&ObjectBuilder::custom("faction".to_string(), "Test Faction".to_string()).build(), &swn_schema)?;
    println!("Empty faction validation: {} (expected to fail required fields)", faction_validation.valid);
    if !faction_validation.valid {
        println!("  Validation errors: {}", faction_validation.errors.len());
    }

    // Test validation of a proper NPC
    let chen_meta = graph.get_object(chen_id)?.unwrap();
    let npc_validation = schema_manager.validate_object_with_schema(&chen_meta, &swn_schema)?;
    println!("Director Chen validation: {} (should pass)", npc_validation.valid);

    println!("\n🚀 Campaign Summary");
    println!("==================");
    println!("The Outer Rim sector is in crisis! Stellar Dynamics Corporation");
    println!("is attempting to monopolize the rich Kepler-442 system, while");
    println!("the Free Traders Alliance fights to maintain independence.");
    println!("");
    println!("Key locations:");
    println!("• {} - The contested sector", "Outer Rim Sector");
    println!("• {} - Strategic system with garden world", "Kepler-442 System");
    println!("• {} - The prize everyone wants", "New Eden");
    println!("");
    println!("Major players:");
    println!("• Director Liu Chen (Stellar Dynamics) - The corporate iron fist");
    println!("• Captain Sarah Reeves (Free Traders) - The resistance leader");
    println!("• Commander Alex Nova (Player) - The wild card");
    println!("");
    println!("The precursor navigation matrix could tip the balance of power...");
    println!("Who will control the future of the Outer Rim?");

    println!("\n✨ Demo Complete! Ready for adventure in the stars! ✨");

    Ok(())
}