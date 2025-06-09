//! Simple HNSW integration test to understand the hnsw_rs v0.3 API
//! 
//! This example tests basic HNSW operations to identify API compatibility issues
//! mentioned in CLAUDE.MD and determine the correct integration approach.

use std::collections::HashMap;
use hnsw_rs::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Testing hnsw_rs v0.3 API compatibility...");
    
    // Test basic HNSW construction
    let nb_elem = 1000;
    let dimension = 384; // Same as our embedding dimension
    let max_nb_connection = 16; // Same as our config
    let ef_construction = 200; // Same as our config
    
    // Try to create an HNSW instance
    println!("Creating HNSW instance with {} dimensions", dimension);
    
    let mut hnsw = Hnsw::<f32, DistL2>::new(
        max_nb_connection,
        nb_elem,
        16, // layer factor
        ef_construction,
        DistL2 {},
    );
    
    println!("HNSW instance created successfully");
    
    // Create some test vectors
    let mut vectors = Vec::new();
    let mut metadata = HashMap::new();
    
    for i in 0..10 {
        let mut vector = vec![0.0f32; dimension];
        // Create a simple pattern - each vector has a different "dominant" dimension
        vector[i % dimension] = 1.0;
        vector[(i * 17) % dimension] = 0.5; // Add some variety
        
        vectors.push(vector.clone());
        metadata.insert(i, format!("test_chunk_{}", i));
        
        // Try to insert into HNSW
        println!("Inserting vector {} with {} dimensions", i, vector.len());
        hnsw.insert((&vector, i));
    }
    
    println!("Inserted {} vectors successfully", vectors.len());
    
    // Test search functionality
    let query_vector = &vectors[0]; // Use first vector as query
    let ef_search = 50;
    let nb_results = 5;
    
    println!("Searching for {} nearest neighbors...", nb_results);
    
    // The search method returns Vec<Neighbour> directly, not a Result
    let results = hnsw.search(&query_vector, nb_results, ef_search);
    println!("Search successful! Found {} results:", results.len());
    for (i, result) in results.iter().enumerate() {
        // Try to access result fields - this will help identify the correct struct layout
        println!("  Result {}: {:?}", i, result);
    }
    
    // Test getting number of elements
    println!("Testing element count methods...");
    
    // Try the new API method name mentioned in CLAUDE.MD
    let count = hnsw.get_nb_point();
    println!("get_nb_point() returned: {}", count);
    
    // Test serialization capabilities
    println!("Testing serialization...");
    
    let test_file = "/tmp/hnsw_test.bin";
    let test_path = std::path::Path::new("/tmp");
    let basename = "hnsw_test";
    match hnsw.file_dump(test_path, basename) {
        Ok(_) => {
            println!("Serialization to {}/{} successful", test_path.display(), basename);
            // Note: file_load method doesn't seem to exist in this API version
            println!("Deserialization method not available in this API version");
            // Clean up
            std::fs::remove_file(format!("{}/{}.hnsw", test_path.display(), basename)).ok();
        }
        Err(e) => {
            println!("Serialization failed: {}", e);
        }
    }
    
    println!("HNSW API test completed successfully!");
    Ok(())
}