use super::*;
use glam::Vec2;
use gpui::TestAppContext;
use parking_lot::Mutex;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tracing::{Event, Subscriber, field};
use tracing_subscriber::{Layer, layer::Context as LayerContext, prelude::*};
use u_forge_core::EdgeType;
use u_forge_graph_view::build_snapshot;

use crate::startup_tests::FakeLemonade;

struct Fixture {
    graph: Arc<KnowledgeGraph>,
    config: Arc<AppConfig>,
    server: FakeLemonade,
    _dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let mut config = AppConfig::load(&dir.path().join("config.toml")).unwrap();
        config.storage.db_path = dir.path().join("db");
        let graph = Arc::new(KnowledgeGraph::with_storage_config(&config.storage).unwrap());
        Self {
            graph,
            config: Arc::new(config),
            server: FakeLemonade::fresh(),
            _dir: dir,
        }
    }

    fn start(&self, snapshot: GraphSnapshot, cx: &mut TestAppContext) -> Entity<AppView> {
        cx.update(UiTheme::init);
        let view = cx.new(|cx| {
            AppView::new_profiled(
                snapshot,
                self.graph.clone(),
                self.graph.get_schema_manager(),
                self.config.data.import_file.clone(),
                self.config.data.schema_dir.clone(),
                self.config.clone(),
                Arc::new(tokio::runtime::Runtime::new().unwrap()),
                StartupTimeline::default(),
                Some(self.server.connection()),
                cx,
            )
        });
        cx.run_until_parked();
        view
    }

    fn add_node(&self, index: usize) -> ObjectMetadata {
        let object_type = if index.is_multiple_of(2) {
            "character"
        } else {
            "location"
        };
        let node = ObjectMetadata::new(object_type.into(), format!("Node {index}"));
        self.graph.add_object(node.clone()).unwrap();
        self.graph
            .save_layout(&[(node.id, index as f32, -(index as f32))])
            .unwrap();
        node
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SnapshotWork {
    metadata_reads: u64,
    full_reads: u64,
    spatial_builds: u64,
    spatial_entries: u64,
}

#[derive(Clone, Default)]
struct WorkCounter(Arc<Mutex<SnapshotWork>>);

impl<S: Subscriber> Layer<S> for WorkCounter {
    fn on_event(&self, event: &Event<'_>, _: LayerContext<'_, S>) {
        match event.metadata().name() {
            "graph_snapshot_metadata_read" => self.0.lock().metadata_reads += 1,
            "graph_snapshot_full_read" => self.0.lock().full_reads += 1,
            "graph_snapshot_spatial_build" => {
                let mut work = self.0.lock();
                work.spatial_builds += 1;
                event.record(&mut *work);
            }
            _ => {}
        }
    }
}

impl field::Visit for SnapshotWork {
    fn record_u64(&mut self, field: &field::Field, value: u64) {
        if field.name() == "entries" {
            self.spatial_entries += value;
        }
    }

    fn record_debug(&mut self, _: &field::Field, _: &dyn std::fmt::Debug) {}
}

fn measure_work(run: impl FnOnce()) -> SnapshotWork {
    let counter = WorkCounter::default();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(counter.clone()), run);
    std::mem::take(&mut *counter.0.lock())
}

fn flush_debounce(cx: &mut TestAppContext) {
    cx.run_until_parked();
    cx.background_executor
        .advance_clock(Duration::from_millis(16));
    cx.run_until_parked();
}

fn assert_matches_fresh(actual: &GraphSnapshot, graph: &KnowledgeGraph) {
    let fresh = build_snapshot(graph).unwrap();
    let ids = |snap: &GraphSnapshot| snap.id_to_idx.keys().copied().collect::<HashSet<_>>();
    assert_eq!(ids(actual), ids(&fresh));
    for (&id, &index) in &fresh.id_to_idx {
        let expected = &fresh.nodes[index];
        let node = &actual.nodes[actual.id_to_idx[&id]];
        assert_eq!(node.id, expected.id);
        assert_eq!(node.name, expected.name);
        assert_eq!(node.object_type, expected.object_type);
        assert_eq!(node.properties, expected.properties);
        assert_eq!(node.position, expected.position);
    }
    let edges = |snap: &GraphSnapshot| {
        let mut edges: Vec<_> = snap
            .edges
            .iter()
            .map(|edge| {
                (
                    snap.nodes[edge.source_idx].id.0,
                    snap.nodes[edge.target_idx].id.0,
                    edge.edge_type.clone(),
                    edge.weight.to_bits(),
                )
            })
            .collect();
        edges.sort_unstable();
        edges
    };
    assert_eq!(edges(actual), edges(&fresh));
    for (min, max) in [
        (Vec2::splat(-2000.0), Vec2::splat(2000.0)),
        (Vec2::new(30.0, -40.0), Vec2::new(40.0, -30.0)),
        (Vec2::splat(30.0), Vec2::splat(40.0)),
    ] {
        let hits = |snap: &GraphSnapshot| {
            let mut hits: Vec<_> = snap
                .nodes_in_viewport(min, max)
                .into_iter()
                .map(|idx| snap.nodes[idx].id.0)
                .collect();
            hits.sort_unstable();
            hits
        };
        assert_eq!(hits(actual), hits(&fresh));
    }
    assert_eq!(actual.type_counts, fresh.type_counts);
    assert_eq!(actual.legend_types, fresh.legend_types);
}

#[gpui::test]
fn graph_refresh_coalesces_renames_without_replacing_the_index(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let nodes: Vec<_> = (0..1000).map(|index| fixture.add_node(index)).collect();
    for (index, node) in nodes.iter().enumerate() {
        for offset in [1, 2] {
            fixture
                .graph
                .connect_objects(
                    node.id,
                    nodes[(index + offset) % nodes.len()].id,
                    EdgeType::new("knows"),
                )
                .unwrap();
        }
    }
    let mut node = nodes[0].clone();
    let id = node.id;
    let snapshot = build_snapshot(&fixture.graph).unwrap();
    let original_entry = snapshot.spatial_index.iter().next().unwrap() as *const _;
    let view = fixture.start(snapshot, cx);

    let work = measure_work(|| {
        node.name = "Intermediate".into();
        fixture.graph.update_object(node.clone()).unwrap();
        cx.run_until_parked(); // First event is waiting in the debounce window.
        node.name = "Final".into();
        node.properties = json!({"description": "Edited"});
        fixture.graph.update_object(node).unwrap();
        flush_debounce(cx);
    });
    assert_eq!(
        work,
        SnapshotWork {
            metadata_reads: 1,
            ..Default::default()
        }
    );
    cx.read(|cx| {
        let snapshot = view.read(cx).state.snapshot.read();
        assert_eq!(snapshot.nodes[snapshot.id_to_idx[&id]].name, "Final");
        assert_eq!(
            snapshot.spatial_index.iter().next().unwrap() as *const _,
            original_entry
        );
        assert_matches_fresh(&snapshot, &fixture.graph);
    });
}

#[gpui::test]
fn editor_save_refreshes_metadata_once_through_the_subscriber(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let node = fixture.add_node(0);
    let view = fixture.start(build_snapshot(&fixture.graph).unwrap(), cx);
    let work = measure_work(|| {
        view.update(cx, |view, cx| {
            view.node_editor.update(cx, |editor, cx| {
                editor.open_or_focus_tab(node.id, cx);
                let tab = &mut editor.tabs[editor.active_tab.unwrap()];
                tab.edited_values
                    .insert("name".into(), json!("Renamed in Details"));
                tab.dirty = true;
            });
            view.do_save_editor(false, cx);
        });
        flush_debounce(cx);
    });
    assert_eq!(
        work,
        SnapshotWork {
            metadata_reads: 1,
            ..Default::default()
        }
    );
    cx.read(|cx| {
        let snapshot = view.read(cx).state.snapshot.read();
        assert_eq!(
            snapshot.nodes[snapshot.id_to_idx[&node.id]].name,
            "Renamed in Details"
        );
        assert_matches_fresh(&snapshot, &fixture.graph);
    });
}

#[gpui::test]
fn graph_refresh_rebuilds_for_mixed_batches_and_recovers_after_lag(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let mut first = fixture.add_node(0);
    let second = fixture.add_node(1);
    let view = fixture.start(build_snapshot(&fixture.graph).unwrap(), cx);
    let work = measure_work(|| {
        first.name = "Renamed before connecting".into();
        fixture.graph.update_object(first.clone()).unwrap();
        cx.run_until_parked();
        fixture
            .graph
            .connect_objects(first.id, second.id, EdgeType::new("knows"))
            .unwrap();
        first.name = "Renamed after connecting".into();
        fixture.graph.update_object(first.clone()).unwrap();
        flush_debounce(cx);
    });
    assert_eq!(
        work,
        SnapshotWork {
            metadata_reads: 0,
            full_reads: 3,
            spatial_builds: 1,
            spatial_entries: 2
        }
    );
    cx.read(|cx| assert_matches_fresh(&view.read(cx).state.snapshot.read(), &fixture.graph));

    let work = measure_work(|| {
        fixture.graph.delete_object(second.id).unwrap();
        // Overflow the receiver before it is polled, dropping the only event
        // that reported the deletion (and the cascaded deletion of the edge).
        for index in 0..300 {
            first.name = format!("Rename {index}");
            fixture.graph.update_object(first.clone()).unwrap();
        }
        flush_debounce(cx);
    });
    assert_eq!(
        work,
        SnapshotWork {
            metadata_reads: 0,
            full_reads: 3,
            spatial_builds: 1,
            spatial_entries: 1
        }
    );
    cx.read(|cx| {
        let state = &view.read(cx).state;
        assert_eq!(state.graph_lag_recoveries, 1);
        assert_matches_fresh(&state.snapshot.read(), &fixture.graph);
    });
}

#[gpui::test]
fn graph_refresh_rebuilds_for_unknown_ids_and_type_changes(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let snapshot = build_snapshot(&fixture.graph).unwrap();
    let mut node = fixture.add_node(0); // Not represented in the initial snapshot.
    let view = fixture.start(snapshot, cx);
    let work = measure_work(|| {
        node.name = "Unknown ID".into();
        fixture.graph.update_object(node.clone()).unwrap();
        flush_debounce(cx);
    });
    assert_eq!(
        work,
        SnapshotWork {
            metadata_reads: 0,
            full_reads: 3,
            spatial_builds: 1,
            spatial_entries: 1
        }
    );
    cx.read(|cx| assert_matches_fresh(&view.read(cx).state.snapshot.read(), &fixture.graph));

    let work = measure_work(|| {
        node.object_type = "location".into();
        fixture.graph.update_object(node).unwrap();
        flush_debounce(cx);
    });
    assert_eq!(
        work,
        SnapshotWork {
            metadata_reads: 1,
            full_reads: 3,
            spatial_builds: 1,
            spatial_entries: 1
        }
    );
    cx.read(|cx| assert_matches_fresh(&view.read(cx).state.snapshot.read(), &fixture.graph));
}
