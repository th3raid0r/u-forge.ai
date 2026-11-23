use tempfile::TempDir;
use u_forge_core::{KnowledgeGraph, ingest::import_data_only};

#[tokio::test]
async fn unchanged_location_does_not_append_chunks() {
    let dir = TempDir::new().unwrap();
    let graph = KnowledgeGraph::new(dir.path()).unwrap();
    let input = dir.path().join("locations.jsonl");
    std::fs::write(
        &input,
        r#"{"entitytype":"node","id":"00000000-0000-0000-0000-000000000001","nodetype":"location","properties":{"name":"Harbor"}}"#,
    )
    .unwrap();
    let initial = import_data_only(&graph, &input).await.unwrap();
    assert_eq!(initial.chunks_indexed, 1);
    let repeat = import_data_only(&graph, &input).await.unwrap();
    assert_eq!(repeat.objects_reused, 1);
    assert_eq!(repeat.chunks_indexed, 0);
    assert_eq!(graph.get_stats().unwrap().chunk_count, 1);
}
