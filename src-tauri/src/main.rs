#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

use u_forge_ai::{types::*, KnowledgeGraph, ObjectBuilder, SchemaIngestion, SchemaStats};

// Application state
#[derive(Debug, Clone)]
struct AppConfig {
    db_path: PathBuf,
    cache_dir: PathBuf,
    current_project: Option<String>,
}

// Shared application state
struct AppState {
    knowledge_graph: Arc<Mutex<Option<KnowledgeGraph>>>,
    config: Arc<Mutex<AppConfig>>,
    session_id: String,
}

// API Response types
#[derive(Debug, Serialize, Deserialize)]
struct ApiResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

impl<T> ApiResponse<T> {
    fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    fn error(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

// Frontend data structures
#[derive(Debug, Serialize, Deserialize)]
struct ProjectInfo {
    name: String,
    path: String,
    created_at: chrono::DateTime<chrono::Utc>,
    last_modified: chrono::DateTime<chrono::Utc>,
    object_count: usize,
    relationship_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct ObjectSummary {
    id: String,
    name: String,
    object_type: String,
    description: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    tags: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RelationshipSummary {
    from_id: String,
    from_name: String,
    to_id: String,
    to_name: String,
    relationship_type: String,
    weight: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SearchRequest {
    query: String,
    object_types: Option<Vec<String>>,
    limit: Option<usize>,
    use_semantic: bool,
    use_exact: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct SearchResult {
    objects: Vec<ObjectSummary>,
    relationships: Vec<RelationshipSummary>,
    query_time_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateObjectRequest {
    name: String,
    object_type: String,
    description: Option<String>,
    properties: HashMap<String, serde_json::Value>,
    tags: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateRelationshipRequest {
    from_id: String,
    to_id: String,
    relationship_type: String,
    weight: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PathConfiguration {
    schema_dir: Option<String>,
    data_file: Option<String>,
    current_schema_dir: String,
    current_data_file: String,
}

// JsonEntry is now provided by the ingestion module

#[derive(Debug, Serialize, Deserialize)]
struct GraphData {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GraphNode {
    id: String,
    name: String,
    node_type: String,
    size: usize,
    color: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GraphEdge {
    id: String,
    source: String,
    target: String,
    edge_type: String,
    weight: Option<f32>,
}

#[derive(Serialize)]
struct DatabaseStatus {
    has_data: bool,
    has_schemas: bool,
    object_count: usize,
    relationship_count: usize,
    schema_count: usize,
}

#[derive(Serialize)]
struct SchemaListResponse {
    schemas: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SchemaResponse {
    name: String,
    version: String,
    description: String,
    object_types: Vec<String>,
    edge_types: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ValidationResponse {
    valid: bool,
    errors: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ObjectTypeSchemaResponse {
    name: String,
    description: String,
    properties: Vec<PropertyInfo>,
    required_properties: Vec<String>,
    allowed_edges: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PropertyInfo {
    name: String,
    property_type: String,
    description: String,
    required: bool,
    validation_rules: Option<PropertyValidation>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PropertyValidation {
    min_length: Option<usize>,
    max_length: Option<usize>,
    min_value: Option<f64>,
    max_value: Option<f64>,
    pattern: Option<String>,
    allowed_values: Option<Vec<String>>,
}

// Tauri commands
#[tauri::command]
async fn restore_project_connection(
    state: State<'_, AppState>,
) -> Result<ApiResponse<String>, String> {
    info!("🔄 [restore_project_connection] Starting restoration attempt");

    let config_guard = state.config.lock().await;
    let kg_guard = state.knowledge_graph.lock().await;

    // If knowledge graph is already initialized, we're good
    if kg_guard.is_some() {
        drop(kg_guard);
        drop(config_guard);
        info!("✅ [restore_project_connection] Knowledge graph already active");
        return Ok(ApiResponse::success(
            "Project connection already active".to_string(),
        ));
    }

    // Check if we have a configured project path
    let db_path = config_guard.db_path.clone();
    let cache_dir = config_guard.cache_dir.clone();
    let project_name = config_guard.current_project.clone();

    info!("🔍 [restore_project_connection] Config - db_path: {:?}, cache_dir: {:?}, project_name: {:?}",
          db_path, cache_dir, project_name);

    drop(config_guard);
    drop(kg_guard);

    // Only try to restore if we have a project name AND the database actually exists
    // Don't try to restore default paths like "./db"
    if project_name.is_none() {
        info!("ℹ️ [restore_project_connection] No project name configured");
        return Ok(ApiResponse::error(
            "No project configured to restore".to_string(),
        ));
    }

    // Check if this looks like a real project path (not the default "./db")
    if db_path == PathBuf::from("./db") || db_path == PathBuf::from("db") {
        info!("ℹ️ [restore_project_connection] Using default path, not restoring");
        return Ok(ApiResponse::error(
            "No custom project path configured".to_string(),
        ));
    }

    // Check if the database directory exists and has RocksDB files
    let db_exists = db_path.exists();
    let current_exists = db_path.join("CURRENT").exists();
    info!(
        "🔍 [restore_project_connection] Database checks - db_exists: {}, current_exists: {}",
        db_exists, current_exists
    );

    if !db_exists || !current_exists {
        info!("ℹ️ [restore_project_connection] Database not found or incomplete");
        return Ok(ApiResponse::error(
            "No existing database found to restore".to_string(),
        ));
    }

    info!(
        "🔗 [restore_project_connection] Attempting to restore project connection to: {:?}",
        db_path
    );

    // Try to reconnect to the existing database
    match KnowledgeGraph::new(&db_path, Some(&cache_dir)) {
        Ok(graph) => {
            let mut kg_guard = state.knowledge_graph.lock().await;
            *kg_guard = Some(graph);

            let message = format!(
                "Successfully restored connection to project: {}",
                project_name.unwrap()
            );
            info!("✅ [restore_project_connection] {}", message);
            Ok(ApiResponse::success(message))
        }
        Err(e) => {
            let error_msg = format!("Failed to restore project connection: {}", e);
            error!("❌ [restore_project_connection] {}", error_msg);

            // Ensure we don't leave corrupted state - reset config to defaults
            let mut config_guard = state.config.lock().await;
            config_guard.db_path = PathBuf::from("./db");
            config_guard.cache_dir = PathBuf::from("./cache");
            config_guard.current_project = None;
            drop(config_guard);

            // Ensure knowledge graph is cleared
            let mut kg_guard = state.knowledge_graph.lock().await;
            *kg_guard = None;
            drop(kg_guard);

            Ok(ApiResponse::error(error_msg))
        }
    }
}

#[tauri::command]
async fn check_database_status(
    state: State<'_, AppState>,
) -> Result<ApiResponse<DatabaseStatus>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        // Get storage stats (includes object and edge counts)
        let storage_stats = match kg.get_stats() {
            Ok(stats) => stats,
            Err(e) => {
                warn!("Failed to get storage stats: {}", e);
                return Ok(ApiResponse::error(format!(
                    "Failed to get database stats: {}",
                    e
                )));
            }
        };

        // Get schema count
        let schema_manager = kg.get_schema_manager();
        let schema_count = match schema_manager.list_schemas().await {
            Ok(schemas) => schemas.len(),
            Err(_) => 0,
        };

        let status = DatabaseStatus {
            has_data: storage_stats.node_count > 0 || storage_stats.edge_count > 0,
            has_schemas: schema_count > 0,
            object_count: storage_stats.node_count,
            relationship_count: storage_stats.edge_count,
            schema_count,
        };

        info!(
            "Database status - Objects: {}, Relationships: {}, Schemas: {}",
            storage_stats.node_count, storage_stats.edge_count, schema_count
        );

        Ok(ApiResponse::success(status))
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn initialize_project(
    state: State<'_, AppState>,
    project_name: String,
    project_path: String,
) -> Result<ApiResponse<ProjectInfo>, String> {
    info!("Initializing project: {} at {}", project_name, project_path);

    let mut config = state.config.lock().await;
    config.db_path = PathBuf::from(&project_path).join("db");
    config.cache_dir = PathBuf::from(&project_path).join("cache");
    config.current_project = Some(project_name.clone());

    // Check if project already exists
    let project_exists = config.db_path.exists() && config.db_path.join("CURRENT").exists(); // RocksDB marker file

    if project_exists {
        info!("Existing project detected at {}", config.db_path.display());
    }

    // Create directories if they don't exist
    if let Err(e) = std::fs::create_dir_all(&config.db_path) {
        error!("Failed to create db directory: {}", e);
        return Ok(ApiResponse::error(format!(
            "Failed to create database directory: {}",
            e
        )));
    }

    if let Err(e) = std::fs::create_dir_all(&config.cache_dir) {
        error!("Failed to create cache directory: {}", e);
        return Ok(ApiResponse::error(format!(
            "Failed to create cache directory: {}",
            e
        )));
    }

    // Initialize knowledge graph
    match KnowledgeGraph::new(&config.db_path, Some(&config.cache_dir)) {
        Ok(graph) => {
            // Get current stats directly from the newly created graph
            let (object_count, relationship_count) = if project_exists {
                match check_database_status(state.clone()).await {
                    Ok(response) if response.success => {
                        let status = response.data.unwrap();
                        (status.object_count, status.relationship_count)
                    }
                    _ => (0, 0),
                }
            } else {
                (0, 0)
            };

            // Store the graph in state after getting stats
            let mut kg = state.knowledge_graph.lock().await;
            *kg = Some(graph);
            drop(kg); // Release lock

            let project_info = ProjectInfo {
                name: project_name,
                path: project_path,
                created_at: chrono::Utc::now(),
                last_modified: chrono::Utc::now(),
                object_count,
                relationship_count,
            };

            if project_exists {
                info!(
                    "Existing project loaded successfully with {} objects and {} relationships",
                    object_count, relationship_count
                );
            } else {
                info!("New project initialized successfully");
            }

            Ok(ApiResponse::success(project_info))
        }
        Err(e) => {
            error!("Failed to initialize knowledge graph: {}", e);
            Ok(ApiResponse::error(format!(
                "Failed to initialize knowledge graph: {}",
                e
            )))
        }
    }
}

#[tauri::command]
async fn get_project_stats(state: State<'_, AppState>) -> Result<ApiResponse<ProjectInfo>, String> {
    let kg_guard = state.knowledge_graph.lock().await;
    let config_guard = state.config.lock().await;

    if let (Some(kg), Some(project_name)) = (kg_guard.as_ref(), &config_guard.current_project) {
        match kg.get_stats() {
            Ok(stats) => {
                let project_info = ProjectInfo {
                    name: project_name.clone(),
                    path: config_guard
                        .db_path
                        .parent()
                        .unwrap_or(&config_guard.db_path)
                        .to_string_lossy()
                        .to_string(),
                    created_at: chrono::Utc::now(), // TODO: Store actual creation time
                    last_modified: chrono::Utc::now(),
                    object_count: stats.node_count,
                    relationship_count: stats.edge_count,
                };
                Ok(ApiResponse::success(project_info))
            }
            Err(e) => Ok(ApiResponse::error(format!(
                "Failed to get project stats: {}",
                e
            ))),
        }
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn search_knowledge_graph(
    state: State<'_, AppState>,
    request: SearchRequest,
) -> Result<ApiResponse<SearchResult>, String> {
    let start_time = std::time::Instant::now();
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        // TODO: Implement proper search based on request parameters
        // For now, return all objects as a placeholder
        match kg.get_all_objects() {
            Ok(objects) => {
                let object_summaries: Vec<ObjectSummary> = objects
                    .into_iter()
                    .filter(|obj| {
                        if request
                            .object_types
                            .as_ref()
                            .map_or(false, |types| !types.is_empty())
                        {
                            request
                                .object_types
                                .as_ref()
                                .unwrap()
                                .contains(&obj.object_type.to_string())
                        } else {
                            true
                        }
                    })
                    .filter(|obj| {
                        if !request.query.is_empty() {
                            obj.name
                                .to_lowercase()
                                .contains(&request.query.to_lowercase())
                                || obj
                                    .description
                                    .as_deref()
                                    .unwrap_or("")
                                    .to_lowercase()
                                    .contains(&request.query.to_lowercase())
                        } else {
                            true
                        }
                    })
                    .take(request.limit.unwrap_or(50))
                    .map(|obj| ObjectSummary {
                        id: obj.id.to_string(),
                        name: obj.name,
                        object_type: obj.object_type.to_string(),
                        description: obj.description,
                        created_at: obj.created_at,
                        tags: obj.tags,
                    })
                    .collect();

                let search_result = SearchResult {
                    objects: object_summaries,
                    relationships: vec![], // TODO: Include relationships in search
                    query_time_ms: start_time.elapsed().as_millis() as u64,
                };

                Ok(ApiResponse::success(search_result))
            }
            Err(e) => Ok(ApiResponse::error(format!("Search failed: {}", e))),
        }
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn create_object(
    state: State<'_, AppState>,
    request: CreateObjectRequest,
) -> Result<ApiResponse<String>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        let mut builder = ObjectBuilder::custom(request.object_type.clone(), request.name.clone());

        if let Some(desc) = request.description.clone() {
            builder = builder.with_description(desc);
        }

        for tag in request.tags.clone() {
            builder = builder.with_tag(tag);
        }

        for (key, value) in request.properties.clone() {
            builder = builder.with_json_property(key, value);
        }

        let object = builder.build();

        // Validate object against schema before creation
        let schema_manager = kg.get_schema_manager();
        match schema_manager.validate_object(&object).await {
            Ok(validation_result) => {
                if !validation_result.valid {
                    let errors: Vec<String> = validation_result
                        .errors
                        .into_iter()
                        .map(|e| format!("{}: {}", e.property, e.message))
                        .collect();
                    return Ok(ApiResponse::error(format!(
                        "Schema validation failed: {}",
                        errors.join("; ")
                    )));
                }

                // Log warnings if any
                for warning in validation_result.warnings {
                    warn!(
                        "Schema validation warning for {}: {}: {}",
                        object.name, warning.property, warning.message
                    );
                }
            }
            Err(e) => {
                warn!("Failed to validate object against schema: {}", e);
                // Continue with creation - schema validation is optional
            }
        }

        match kg.add_object(object) {
            Ok(id) => {
                info!("Created object '{}' with ID: {}", request.name, id);
                Ok(ApiResponse::success(id.to_string()))
            }
            Err(e) => {
                error!("Failed to create object: {}", e);
                Ok(ApiResponse::error(format!(
                    "Failed to create object: {}",
                    e
                )))
            }
        }
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn create_relationship(
    state: State<'_, AppState>,
    request: CreateRelationshipRequest,
) -> Result<ApiResponse<String>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        let from_id = match Uuid::parse_str(&request.from_id) {
            Ok(id) => id,
            Err(_) => return Ok(ApiResponse::error("Invalid from_id format".to_string())),
        };

        let to_id = match Uuid::parse_str(&request.to_id) {
            Ok(id) => id,
            Err(_) => return Ok(ApiResponse::error("Invalid to_id format".to_string())),
        };

        if let Some(weight) = request.weight {
            match kg.connect_objects_weighted_str(
                from_id,
                to_id,
                &request.relationship_type,
                weight,
            ) {
                Ok(()) => {
                    info!(
                        "Created weighted relationship: {} -[{}:{}]-> {}",
                        from_id, request.relationship_type, weight, to_id
                    );
                    Ok(ApiResponse::success(
                        "Relationship created successfully".to_string(),
                    ))
                }
                Err(e) => {
                    error!("Failed to create weighted relationship: {}", e);
                    Ok(ApiResponse::error(format!(
                        "Failed to create relationship: {}",
                        e
                    )))
                }
            }
        } else {
            match kg.connect_objects_str(from_id, to_id, &request.relationship_type) {
                Ok(()) => {
                    info!(
                        "Created relationship: {} -[{}]-> {}",
                        from_id, request.relationship_type, to_id
                    );
                    Ok(ApiResponse::success(
                        "Relationship created successfully".to_string(),
                    ))
                }
                Err(e) => {
                    error!("Failed to create relationship: {}", e);
                    Ok(ApiResponse::error(format!(
                        "Failed to create relationship: {}",
                        e
                    )))
                }
            }
        }
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn get_graph_data(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<ApiResponse<GraphData>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        match kg.get_all_objects() {
            Ok(objects) => {
                let limited_objects: Vec<_> =
                    objects.into_iter().take(limit.unwrap_or(100)).collect();

                let mut nodes = Vec::new();
                let mut edges = Vec::new();

                // Create nodes
                for obj in &limited_objects {
                    let color = match obj.object_type.as_str() {
                        "character" => "#4CAF50",
                        "location" => "#2196F3",
                        "faction" => "#FF9800",
                        "item" => "#9C27B0",
                        "event" => "#F44336",
                        "session" => "#607D8B",
                        _ => "#795548",
                    };

                    nodes.push(GraphNode {
                        id: obj.id.to_string(),
                        name: obj.name.clone(),
                        node_type: obj.object_type.to_string(),
                        size: 10, // Default size since relationships are handled separately
                        color: color.to_string(),
                    });
                }

                // Create edges
                for obj in &limited_objects {
                    let relationships = kg.get_relationships(obj.id.clone()).unwrap_or_default();
                    for relationship in &relationships {
                        edges.push(GraphEdge {
                            id: format!("{}_{}", obj.id, relationship.to),
                            source: obj.id.to_string(),
                            target: relationship.to.to_string(),
                            edge_type: relationship.edge_type.as_str().to_string(),
                            weight: Some(relationship.weight),
                        });
                    }
                }

                let graph_data = GraphData { nodes, edges };
                Ok(ApiResponse::success(graph_data))
            }
            Err(e) => Ok(ApiResponse::error(format!(
                "Failed to get graph data: {}",
                e
            ))),
        }
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn get_object_details(
    state: State<'_, AppState>,
    object_id: String,
) -> Result<ApiResponse<ObjectMetadata>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        let id = match Uuid::parse_str(&object_id) {
            Ok(id) => id,
            Err(_) => return Ok(ApiResponse::error("Invalid object ID format".to_string())),
        };

        match kg.get_object(id) {
            Ok(Some(obj)) => Ok(ApiResponse::success(obj)),
            Ok(None) => Ok(ApiResponse::error("Object not found".to_string())),
            Err(e) => Ok(ApiResponse::error(format!("Failed to get object: {}", e))),
        }
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn import_custom_data(
    state: State<'_, AppState>,
    data_file_path: String,
    schema_dir_path: Option<String>,
) -> Result<ApiResponse<String>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        // Load schemas if directory is provided
        if let Some(schema_dir) = schema_dir_path {
            match u_forge_ai::schema_ingestion::SchemaIngestion::load_schemas_from_directory(
                &schema_dir,
                "imported_schemas",
                "1.0.0",
            ) {
                Ok(schema_definition) => {
                    let schema_manager = kg.get_schema_manager();
                    if let Err(e) = schema_manager.save_schema(&schema_definition).await {
                        warn!("Failed to save schema: {}", e);
                    } else {
                        info!(
                            "✅ Successfully loaded {} object types from {}",
                            schema_definition.object_types.len(),
                            schema_dir
                        );
                    }
                }
                Err(e) => {
                    let error_msg = format!("Failed to load schemas from {}: {}", schema_dir, e);
                    error!("{}", error_msg);
                    return Ok(ApiResponse::error(error_msg));
                }
            }
        }

        // Import data from specified file
        let mut ingestion = u_forge_ai::data_ingestion::DataIngestion::new(kg);
        match ingestion.import_json_data(&data_file_path).await {
            Ok(()) => {
                let stats = ingestion.get_stats();
                let message = format!(
                    "Successfully imported {} objects and {} relationships from {}",
                    stats.objects_created, stats.relationships_created, data_file_path
                );
                info!("{}", message);
                Ok(ApiResponse::success(message))
            }
            Err(e) => {
                let error_msg = format!("Failed to import data from {}: {}", data_file_path, e);
                error!("{}", error_msg);
                Ok(ApiResponse::error(error_msg))
            }
        }
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn import_default_data(state: State<'_, AppState>) -> Result<ApiResponse<String>, String> {
    // Check if database already has data
    match check_database_status(state.clone()).await {
        Ok(status_response) if status_response.success => {
            let status = status_response.data.unwrap();
            if status.has_data && status.has_schemas {
                let message = format!(
                    "Database already contains {} objects and {} relationships with {} schemas. Skipping default data import.",
                    status.object_count, status.relationship_count, status.schema_count
                );
                info!("{}", message);
                return Ok(ApiResponse::success(message));
            } else if status.has_data {
                info!("Database has data but missing schemas, loading default schemas only...");
            } else if status.has_schemas {
                info!("Database has schemas but no data, importing data only...");
            } else {
                info!("Empty database detected, loading both schemas and data...");
            }
        }
        _ => {
            info!("Could not determine database status, proceeding with import...");
        }
    }

    let kg_guard = state.knowledge_graph.lock().await;
    if let Some(kg) = kg_guard.as_ref() {
        // Load default schemas if needed
        match u_forge_ai::schema_ingestion::SchemaIngestion::load_default_schemas() {
            Ok(schema_definition) => {
                let schema_manager = kg.get_schema_manager();
                if let Err(e) = schema_manager.save_schema(&schema_definition).await {
                    warn!("Failed to save default schema: {}", e);
                } else {
                    info!(
                        "✅ Successfully loaded {} object types from default schemas",
                        schema_definition.object_types.len()
                    );
                }
            }
            Err(e) => {
                warn!("Failed to load default schemas: {}", e);
            }
        }

        // Import default data
        let mut ingestion = u_forge_ai::data_ingestion::DataIngestion::new(kg);
        match ingestion.import_default_data().await {
            Ok(()) => {
                let stats = ingestion.get_stats();
                let message = format!(
                    "Successfully imported {} objects and {} relationships using default data",
                    stats.objects_created, stats.relationships_created
                );
                info!("{}", message);
                Ok(ApiResponse::success(message))
            }
            Err(e) => {
                let error_msg = format!("Failed to import default data: {}", e);
                error!("{}", error_msg);
                Ok(ApiResponse::error(error_msg))
            }
        }
    } else {
        Ok(ApiResponse::error("No project initialized".to_string()))
    }
}

#[tauri::command]
async fn get_path_configuration() -> Result<ApiResponse<PathConfiguration>, String> {
    let schema_dir = std::env::var("UFORGE_SCHEMA_DIR").ok();
    let data_file = std::env::var("UFORGE_DATA_FILE").ok();

    // Determine current paths using the same logic as the ingestion module
    let current_schema_dir = schema_dir.clone().unwrap_or_else(|| {
        let env_schema = std::env::var("UFORGE_SCHEMA_DIR").unwrap_or_default();
        let schema_paths = ["./examples/schemas", &env_schema];
        for path in &schema_paths {
            if !path.is_empty() && std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }
        "./examples/schemas".to_string()
    });

    let current_data_file = data_file.clone().unwrap_or_else(|| {
        let env_data = std::env::var("UFORGE_DATA_FILE").unwrap_or_default();
        let data_paths = ["./examples/data/memory.json", &env_data];
        for path in &data_paths {
            if !path.is_empty() && std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }
        "./examples/data/memory.json".to_string()
    });

    let config = PathConfiguration {
        schema_dir,
        data_file,
        current_schema_dir,
        current_data_file,
    };

    Ok(ApiResponse::success(config))
}

#[tauri::command]
async fn set_path_configuration(
    schema_dir: Option<String>,
    data_file: Option<String>,
) -> Result<ApiResponse<String>, String> {
    // Set environment variables
    if let Some(dir) = schema_dir {
        std::env::set_var("UFORGE_SCHEMA_DIR", &dir);
        info!("Set UFORGE_SCHEMA_DIR to: {}", dir);
    }

    if let Some(file) = data_file {
        std::env::set_var("UFORGE_DATA_FILE", &file);
        info!("Set UFORGE_DATA_FILE to: {}", file);
    }

    Ok(ApiResponse::success(
        "Path configuration updated successfully".to_string(),
    ))
}

/// List all available schemas
#[tauri::command]
async fn list_schemas(
    state: State<'_, AppState>,
) -> Result<ApiResponse<SchemaListResponse>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        let schema_manager = kg.get_schema_manager();

        match schema_manager.list_schemas().await {
            Ok(schemas) => Ok(ApiResponse::success(SchemaListResponse { schemas })),
            Err(e) => {
                error!("Failed to list schemas: {}", e);
                Ok(ApiResponse::error(format!("Failed to list schemas: {}", e)))
            }
        }
    } else {
        Ok(ApiResponse::error(
            "Knowledge graph not initialized".to_string(),
        ))
    }
}

/// Get schema definition by name
#[tauri::command]
async fn get_schema(
    state: State<'_, AppState>,
    schema_name: String,
) -> Result<ApiResponse<SchemaResponse>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        let schema_manager = kg.get_schema_manager();

        match schema_manager.load_schema(&schema_name).await {
            Ok(schema) => {
                let response = SchemaResponse {
                    name: schema.name.clone(),
                    version: schema.version.clone(),
                    description: schema.description.clone(),
                    object_types: schema.object_types.keys().cloned().collect(),
                    edge_types: schema.edge_types.keys().cloned().collect(),
                };
                Ok(ApiResponse::success(response))
            }
            Err(e) => {
                error!("Failed to load schema '{}': {}", schema_name, e);
                Ok(ApiResponse::error(format!("Failed to load schema: {}", e)))
            }
        }
    } else {
        Ok(ApiResponse::error(
            "Knowledge graph not initialized".to_string(),
        ))
    }
}

/// Get schema statistics
#[tauri::command]
async fn get_schema_stats(
    state: State<'_, AppState>,
    schema_name: String,
) -> Result<ApiResponse<SchemaStats>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        let schema_manager = kg.get_schema_manager();

        match schema_manager.get_schema_stats(&schema_name).await {
            Ok(stats) => Ok(ApiResponse::success(stats)),
            Err(e) => {
                error!("Failed to get schema stats for '{}': {}", schema_name, e);
                Ok(ApiResponse::error(format!(
                    "Failed to get schema stats: {}",
                    e
                )))
            }
        }
    } else {
        Ok(ApiResponse::error(
            "Knowledge graph not initialized".to_string(),
        ))
    }
}

/// Validate an object against its schema
#[tauri::command]
async fn validate_object(
    state: State<'_, AppState>,
    object_id: String,
) -> Result<ApiResponse<ValidationResponse>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        // Convert string ID to Uuid
        let uuid = match Uuid::parse_str(&object_id) {
            Ok(uuid) => uuid,
            Err(_) => {
                return Ok(ApiResponse::error(format!(
                    "Invalid object ID format: {}",
                    object_id
                )));
            }
        };

        // Get the object first
        match kg.get_object(uuid) {
            Ok(Some(object)) => {
                let schema_manager = kg.get_schema_manager();

                match schema_manager.validate_object(&object).await {
                    Ok(validation_result) => {
                        let response = ValidationResponse {
                            valid: validation_result.valid,
                            errors: validation_result
                                .errors
                                .into_iter()
                                .map(|e| format!("{}: {}", e.property, e.message))
                                .collect(),
                            warnings: validation_result
                                .warnings
                                .into_iter()
                                .map(|w| format!("{}: {}", w.property, w.message))
                                .collect(),
                        };
                        Ok(ApiResponse::success(response))
                    }
                    Err(e) => {
                        error!("Failed to validate object '{}': {}", object_id, e);
                        Ok(ApiResponse::error(format!(
                            "Failed to validate object: {}",
                            e
                        )))
                    }
                }
            }
            Ok(None) => Ok(ApiResponse::error(format!(
                "Object '{}' not found",
                object_id
            ))),
            Err(e) => {
                error!("Failed to get object '{}': {}", object_id, e);
                Ok(ApiResponse::error(format!("Failed to get object: {}", e)))
            }
        }
    } else {
        Ok(ApiResponse::error(
            "Knowledge graph not initialized".to_string(),
        ))
    }
}

/// Load schemas from directory
#[tauri::command]
async fn load_schemas_from_directory(
    state: State<'_, AppState>,
    schema_dir: String,
    schema_name: Option<String>,
    schema_version: Option<String>,
) -> Result<ApiResponse<String>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        let name = schema_name.as_deref().unwrap_or("imported_schemas");
        let version = schema_version.as_deref().unwrap_or("1.0.0");

        match SchemaIngestion::load_schemas_from_directory(&schema_dir, name, version) {
            Ok(schema_definition) => {
                let schema_manager = kg.get_schema_manager();
                match schema_manager.save_schema(&schema_definition).await {
                    Ok(()) => {
                        let message = format!(
                            "Successfully loaded {} object types from directory: {}",
                            schema_definition.object_types.len(),
                            schema_dir
                        );
                        info!("{}", message);
                        Ok(ApiResponse::success(message))
                    }
                    Err(e) => {
                        error!("Failed to save schema to storage: {}", e);
                        Ok(ApiResponse::error(format!("Failed to save schema: {}", e)))
                    }
                }
            }
            Err(e) => {
                error!(
                    "Failed to load schemas from directory '{}': {}",
                    schema_dir, e
                );
                Ok(ApiResponse::error(format!("Failed to load schemas: {}", e)))
            }
        }
    } else {
        Ok(ApiResponse::error(
            "Knowledge graph not initialized".to_string(),
        ))
    }
}

/// Get detailed object type schema information
#[tauri::command]
async fn get_object_type_schema(
    state: State<'_, AppState>,
    schema_name: String,
    object_type: String,
) -> Result<ApiResponse<ObjectTypeSchemaResponse>, String> {
    let kg_guard = state.knowledge_graph.lock().await;

    if let Some(kg) = kg_guard.as_ref() {
        let schema_manager = kg.get_schema_manager();

        match schema_manager.load_schema(&schema_name).await {
            Ok(schema) => {
                if let Some(object_schema) = schema.object_types.get(&object_type) {
                    let properties: Vec<PropertyInfo> = object_schema
                        .properties
                        .iter()
                        .map(|(name, prop_schema)| PropertyInfo {
                            name: name.clone(),
                            property_type: prop_schema.property_type.name().to_string(),
                            description: prop_schema.description.clone(),
                            required: object_schema.required_properties.contains(name),
                            validation_rules: prop_schema.validation.as_ref().map(|v| {
                                PropertyValidation {
                                    min_length: v.min_length,
                                    max_length: v.max_length,
                                    min_value: v.min_value,
                                    max_value: v.max_value,
                                    pattern: v.pattern.clone(),
                                    allowed_values: v.allowed_values.clone(),
                                }
                            }),
                        })
                        .collect();

                    let response = ObjectTypeSchemaResponse {
                        name: object_type.clone(),
                        description: object_schema.description.clone(),
                        properties,
                        required_properties: object_schema.required_properties.clone(),
                        allowed_edges: object_schema.allowed_edges.clone(),
                    };

                    Ok(ApiResponse::success(response))
                } else {
                    Ok(ApiResponse::error(format!(
                        "Object type '{}' not found in schema '{}'",
                        object_type, schema_name
                    )))
                }
            }
            Err(e) => {
                error!("Failed to load schema '{}': {}", schema_name, e);
                Ok(ApiResponse::error(format!("Failed to load schema: {}", e)))
            }
        }
    } else {
        Ok(ApiResponse::error(
            "Knowledge graph not initialized".to_string(),
        ))
    }
}

fn main() {
    // Initialize logging with better configuration
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .with_thread_ids(true)
        .with_line_number(true)
        .init();

    info!("Starting u-forge.ai Tauri application");

    let session_id = Uuid::new_v4().to_string();
    info!("Session ID: {}", session_id);

    // Initialize app state
    let app_state = AppState {
        knowledge_graph: Arc::new(Mutex::new(None)),
        config: Arc::new(Mutex::new(AppConfig {
            db_path: PathBuf::from("./db"),
            cache_dir: PathBuf::from("./cache"),
            current_project: Some("default-project".to_string()),
        })),
        session_id,
    };

    tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            initialize_project,
            restore_project_connection,
            check_database_status,
            get_project_stats,
            search_knowledge_graph,
            create_object,
            create_relationship,
            get_graph_data,
            get_object_details,
            import_custom_data,
            import_default_data,
            get_path_configuration,
            set_path_configuration,
            list_schemas,
            get_schema,
            get_schema_stats,
            validate_object,
            load_schemas_from_directory,
            get_object_type_schema
        ])
        .setup(|app| {
            info!("Tauri application setup complete");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
