use tempfile::TempDir;
use u_forge_core::{
    ChunkType, EdgeTypeSchema, KnowledgeGraph, ObjectTypeSchema, PropertySchema, SchemaDefinition,
    StorageConfig, ingest::import_data_only,
};

fn save_schema(graph: &KnowledgeGraph) {
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
}

#[tokio::test]
async fn same_file_retry_indexes_reused_node_after_failed_diagnostics() {
    let dir = TempDir::new().unwrap();
    let graph = KnowledgeGraph::new(dir.path()).unwrap();
    save_schema(&graph);
    let input = dir.path().join("world.jsonl");
    std::fs::write(&input, concat!(
        r#"{"entitytype":"node","id":"town","nodetype":"location","properties":{"name":"Westhaven"}}"#,
        "\n",
        r#"{"entitytype":"node","id":"invalid","nodetype":"location","properties":{}}"#,
    )).unwrap();
    let obstruction = dir.path().join(".u-forge-import-diagnostics");
    std::fs::write(&obstruction, "").unwrap();
    assert!(
        import_data_only(&graph, &input)
            .await
            .unwrap_err()
            .to_string()
            .contains("diagnostics directory")
    );
    assert_eq!(graph.get_stats().unwrap().chunk_count, 0);
    std::fs::remove_file(obstruction).unwrap();
    // An index-write failure must roll back the ticket acknowledgement too.
    let db = rusqlite::Connection::open(dir.path().join("knowledge.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_import BEFORE INSERT ON chunks BEGIN SELECT RAISE(ABORT, 'forced index failure'); END;").unwrap();
    assert!(
        import_data_only(&graph, &input)
            .await
            .unwrap_err()
            .to_string()
            .contains("forced index failure")
    );
    db.execute_batch("DROP TRIGGER reject_import").unwrap();
    let retry = import_data_only(&graph, &input).await.unwrap();
    assert_eq!(
        (
            retry.objects_created,
            retry.objects_reused,
            retry.object_records_skipped
        ),
        (0, 1, 1)
    );
    assert_eq!(retry.chunks_indexed, 1);
    let hits = graph.search_chunks_fts("Westhaven", 4).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        import_data_only(&graph, &input)
            .await
            .unwrap()
            .chunks_indexed,
        0
    );
    assert_eq!(graph.search_chunks_fts("Westhaven", 4).unwrap(), hits);
}

#[tokio::test]
async fn retry_after_diagnostics_failure_recovers_committed_index_work() {
    let dir = TempDir::new().unwrap();
    let graph = KnowledgeGraph::new(dir.path()).unwrap();
    save_schema(&graph);
    let input = dir.path().join("world.jsonl");
    let town =
        r#"{"entitytype":"node","id":"town","nodetype":"location","properties":{"name":"Town"}}"#;
    let port =
        r#"{"entitytype":"node","id":"port","nodetype":"location","properties":{"name":"Port"}}"#;
    let edge = r#"{"entitytype":"edge","from":"Port","to":"Town","edgeType":"borders"}"#;
    std::fs::write(&input, town).unwrap();
    import_data_only(&graph, &input).await.unwrap();
    let obstruction = dir.path().join(".u-forge-import-diagnostics");
    std::fs::write(&obstruction, "not a directory").unwrap();
    // Reused Town emits a diagnostic after Port and its edge commit.
    std::fs::write(&input, [town, port, edge].join("\n")).unwrap();
    let error = import_data_only(&graph, &input).await.unwrap_err();
    assert!(error.to_string().contains("diagnostics directory"));
    assert_eq!(graph.get_stats().unwrap().node_count, 2);
    assert!(graph.search_chunks_fts("Port", 4).unwrap().is_empty());
    drop(graph);
    let graph = KnowledgeGraph::new(dir.path()).unwrap();
    std::fs::remove_file(obstruction).unwrap();
    // A corrected retry need not replay the edge to repair both endpoints.
    std::fs::write(&input, port).unwrap();
    let retry = import_data_only(&graph, &input).await.unwrap();
    assert_eq!(retry.objects_reused, 1);
    assert_eq!(retry.chunks_indexed, 2);
    assert_eq!(graph.search_chunks_fts("borders", 4).unwrap().len(), 2);
    let repeat = import_data_only(&graph, &input).await.unwrap();
    assert_eq!(repeat.chunks_indexed, 0);
}

#[tokio::test]
async fn unchanged_location_does_not_append_chunks() {
    let dir = TempDir::new().unwrap();
    let graph = KnowledgeGraph::with_storage_config(&StorageConfig {
        db_path: dir.path().into(),
        embedding_dimensions: 4,
        high_quality_embedding_dimensions: 8,
    })
    .unwrap();
    save_schema(&graph);
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
