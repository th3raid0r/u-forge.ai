//! Convert messy MemoryMesh JSONL exports to clean u-forge JSONL format.
//!
//! Usage:
//!   cargo run --example convert_memorymesh -- [input.jsonl] [output.jsonl]
//!
//! If input/output paths are omitted, stdin/stdout are used.
//!
//! Output format per node:
//!   { "entitytype": "node", "id": "<uuid>", "nodetype": "faction",
//!     "properties": { "name": "...", "goals": ["..."], ... } }
//!
//! What this does:
//!   - Parses "key: value" metadata strings from the messy export
//!   - Normalizes field names to match schema (case-insensitive lookup → canonical camelCase)
//!   - Discards fields not present in the schema for the node's type
//!   - Generates a fresh UUID for the top-level "id" field
//!   - Renames "metadata" → "properties" and promotes it to a typed JSON object
//!   - Renames top-level "type" → "entitytype" and "nodeType" → "nodetype"
//!   - Passes edges through with the same field renames
//!   - Emits a warning for each discarded field

use anyhow::Result;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use u_forge_core::schema::{PropertyType, SchemaIngestion};
use uuid::Uuid;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    // Open input: first arg or stdin
    let input: Box<dyn BufRead> = if args.len() > 1 {
        Box::new(BufReader::new(File::open(&args[1])?))
    } else {
        Box::new(BufReader::new(io::stdin()))
    };

    // Open output: second arg or stdout
    let output: Box<dyn Write> = if args.len() > 2 {
        Box::new(BufWriter::new(File::create(&args[2])?))
    } else {
        Box::new(BufWriter::new(io::stdout()))
    };
    let mut out = output;

    // Load schemas
    let schema = SchemaIngestion::load_default_schemas()?;

    // Build per-type case-insensitive lookup: lowercase_key → (canonical_key, is_array)
    // Also track all known type names for unknown-type warnings.
    let mut type_lookups: HashMap<String, HashMap<String, (String, bool)>> = HashMap::new();

    for (type_name, type_schema) in &schema.object_types {
        let mut lookup: HashMap<String, (String, bool)> = HashMap::new();
        for (prop_name, prop_schema) in &type_schema.properties {
            let is_array = matches!(prop_schema.property_type, PropertyType::Array(_));
            lookup.insert(prop_name.to_lowercase(), (prop_name.clone(), is_array));
        }
        type_lookups.insert(type_name.clone(), lookup);
    }

    let mut line_num: usize = 0;
    let mut nodes_converted: usize = 0;
    let mut edges_passed: usize = 0;
    let mut fields_discarded: usize = 0;

    for line in input.lines() {
        line_num += 1;
        let raw = line?;
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Line {}: JSON parse error — {}", line_num, e);
                continue;
            }
        };

        let obj = match value.as_object() {
            Some(o) => o,
            None => {
                eprintln!("Line {}: not a JSON object, skipping", line_num);
                continue;
            }
        };

        let entry_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if entry_type == "edge" {
            // Apply field renames to edges too; content is otherwise unchanged.
            let mut edge = obj.clone();
            edge.remove("type");
            edge.insert("entitytype".to_string(), Value::String("edge".to_string()));
            writeln!(out, "{}", serde_json::to_string(&Value::Object(edge))?)?;
            edges_passed += 1;
            continue;
        }

        if entry_type != "node" {
            eprintln!("Line {}: unknown entry type {:?}, skipping", line_num, entry_type);
            continue;
        }

        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let node_type = obj
            .get("nodeType")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let id = Uuid::new_v4().to_string();

        let raw_metadata = match obj.get("metadata").and_then(|v| v.as_array()) {
            Some(a) => a.clone(),
            None => vec![],
        };

        // Get schema lookup for this node type (if known)
        let type_lookup = type_lookups.get(&node_type);
        if type_lookup.is_none() {
            eprintln!(
                "Line {}: unknown nodetype {:?} for {:?} — passing through with field renames only",
                line_num, node_type, name
            );
            let output_entry = json!({
                "entitytype": "node",
                "id": id,
                "nodetype": node_type,
                "properties": obj.get("metadata").cloned().unwrap_or(Value::Object(Map::new()))
            });
            writeln!(out, "{}", serde_json::to_string(&output_entry)?)?;
            continue;
        }
        let type_lookup = type_lookup.unwrap();

        // metadata is output as a JSON object: { "fieldName": "value" } or
        // { "fieldName": ["val1", "val2"] } for array fields.
        // Bare tags (no colon) are collected into a synthetic "tags" array if no
        // explicit tags field is present.
        let mut meta_obj: Map<String, Value> = Map::new();
        let mut bare_tags: Vec<String> = Vec::new();

        for item in &raw_metadata {
            let s = match item.as_str() {
                Some(s) => s.trim(),
                None => continue,
            };

            if s.is_empty() {
                continue;
            }

            match s.find(':') {
                None => {
                    // Bare tag with no colon — accumulate for later
                    bare_tags.push(s.to_string());
                }
                Some(colon_pos) => {
                    let raw_key = s[..colon_pos].trim();
                    let value_str = s[colon_pos + 1..].trim();
                    let lower_key = raw_key.to_lowercase();

                    match type_lookup.get(&lower_key) {
                        None => {
                            eprintln!(
                                "  Discarding unknown field {:?} on {:?} ({:?})",
                                raw_key, name, node_type
                            );
                            fields_discarded += 1;
                        }
                        Some((canonical_key, is_array)) => {
                            let typed_value = if *is_array {
                                // Split comma-separated string into a proper JSON array
                                let elements: Vec<Value> = value_str
                                    .split(',')
                                    .map(|s| Value::String(s.trim().to_string()))
                                    .filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false))
                                    .collect();
                                Value::Array(elements)
                            } else {
                                Value::String(value_str.to_string())
                            };
                            meta_obj.insert(canonical_key.clone(), typed_value);
                        }
                    }
                }
            }
        }

        // Merge bare tags into the "tags" field if the schema supports it
        if !bare_tags.is_empty() {
            let tags_key = type_lookup
                .get("tags")
                .map(|(k, _)| k.as_str())
                .unwrap_or("tags");
            let entry = meta_obj
                .entry(tags_key.to_string())
                .or_insert_with(|| Value::Array(vec![]));
            if let Value::Array(arr) = entry {
                for tag in bare_tags {
                    arr.push(Value::String(tag));
                }
            }
        }

        let output_entry = json!({
            "entitytype": "node",
            "id": id,
            "nodetype": node_type,
            "properties": Value::Object(meta_obj)
        });

        writeln!(out, "{}", serde_json::to_string(&output_entry)?)?;
        nodes_converted += 1;
    }

    eprintln!(
        "\nDone. {} nodes converted, {} edges passed through, {} unknown fields discarded.",
        nodes_converted, edges_passed, fields_discarded
    );

    Ok(())
}
