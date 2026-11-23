use tempfile::TempDir;
use u_forge_core::{
    ChunkType, EdgeTypeSchema, KnowledgeGraph, ObjectTypeSchema, PropertySchema, SchemaDefinition,
    StorageConfig, ingest::import_data_only,
};

#[tokio::test]
async fn unchanged_location_does_not_append_chunks() {
    let dir = TempDir::new().unwrap();
    let graph = KnowledgeGraph::with_storage_config(&StorageConfig {
        db_path: dir.path().into(),
        embedding_dimensions: 4,
        high_quality_embedding_dimensions: 8,
    })
    .unwrap();
    let mut schema = SchemaDefinition::new("world".into(), "1".into(), "fixture".into());
    schema.add_object_type(
        "location".into(),
        ObjectTypeSchema::new("location".into(), "location".into())
            .with_property("name".into(), PropertySchema::string("name"))
            .with_required_property("name".into()),
    );
    schema.add_edge_type(
        "borders".into(),
        EdgeTypeSchema::new("borders".into(), "borders".into())
            .with_source_types(vec!["location".into()])
            .with_target_types(vec!["location".into()]),
    );
    graph.get_schema_manager().save_schema(&schema).unwrap();
    let input = dir.path().join("locations.jsonl");
    let record = |i| {
        format!(
            r#"{{"entitytype":"node","id":"00000000-0000-0000-0000-{i:012}","nodetype":"location","properties":{{"name":"Harbor{i}"}}}}"#
        )
    };
    std::fs::write(&input, (0..1000).map(record).collect::<Vec<_>>().join("\n")).unwrap();
    let initial = import_data_only(&graph, &input).await.unwrap();
    assert_eq!(initial.chunks_indexed, 1000);
    let harbor = graph.find_by_name_only("Harbor0").unwrap()[0].id;
    let note = graph
        .add_text_chunk(harbor, "Handwritten".into(), ChunkType::UserNote)
        .unwrap()[0];
    // Counting stand-in for an embedder: fill both pending lanes once.
    let mut calls = 0;
    for c in graph.get_unembedded_chunks().unwrap() {
        graph.upsert_chunk_embedding(c.id, &[1.0; 4]).unwrap();
        calls += 1;
    }
    for c in graph.get_unembedded_chunks_hq().unwrap() {
        graph.upsert_chunk_embedding_hq(c.id, &[1.0; 8]).unwrap();
        calls += 1;
    }
    assert_eq!(calls, 2002);
    let snapshot = || {
        let mut hits = graph.search_chunks_fts("location", 1100).unwrap();
        hits.sort_by_key(|hit| hit.0.to_string());
        hits
    };
    let before = snapshot();
    assert_eq!(before.len(), 1000);
    std::fs::write(&input, record(0)).unwrap();
    let repeat = import_data_only(&graph, &input).await.unwrap();
    assert_eq!(repeat.objects_reused, 1);
    assert_eq!(repeat.chunks_indexed, 0);
    assert_eq!(snapshot(), before);
    assert!(graph.get_unembedded_chunks().unwrap().is_empty());
    assert!(graph.get_unembedded_chunks_hq().unwrap().is_empty());
    std::fs::write(
        &input,
        r#"{"entitytype":"edge","from":"Harbor0","to":"Harbor1","edgeType":"borders"}"#,
    )
    .unwrap();
    let edge = import_data_only(&graph, &input).await.unwrap();
    assert_eq!(edge.relationships_created, 1);
    assert_eq!(edge.chunks_indexed, 2);
    let after = snapshot();
    let peer = graph.find_by_name_only("Harbor1").unwrap()[0].id;
    let unrelated = |hits: &Vec<(u_forge_core::ChunkId, u_forge_core::ObjectId, String)>| {
        hits.iter()
            .filter(|hit| hit.1 != harbor && hit.1 != peer)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(unrelated(&before), unrelated(&after));
    let line = graph.search_chunks_fts("borders", 10).unwrap();
    assert_eq!(line.len(), 2);
    for id in [harbor, peer] {
        assert!(
            line.iter()
                .any(|hit| hit.1 == id && hit.2.contains("Harbor0 borders Harbor1"))
        );
    }
    assert_eq!(
        graph.search_chunks_fts("Handwritten", 10).unwrap()[0].0,
        note
    );
    assert_eq!(graph.get_stats().unwrap().chunk_count, 1001);
    for pending in [
        graph.get_unembedded_chunks().unwrap(),
        graph.get_unembedded_chunks_hq().unwrap(),
    ] {
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|c| line.iter().any(|hit| hit.0 == c.id)));
    }
}
