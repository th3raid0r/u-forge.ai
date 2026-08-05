use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use glam::Vec2;
use u_forge_core::ObjectId;
use u_forge_graph_view::{EdgeView, NodeView, force_directed_layout};

fn graph(node_count: usize) -> (Vec<NodeView>, Vec<EdgeView>) {
    let nodes = (0..node_count)
        .map(|index| NodeView {
            id: ObjectId::new_v4(),
            name: format!("Node {index}"),
            object_type: "benchmark".to_string(),
            position: Vec2::ZERO,
            properties: serde_json::Value::Object(Default::default()),
        })
        .collect::<Vec<_>>();
    let edges = (0..node_count)
        .map(|index| EdgeView {
            source_idx: index,
            target_idx: (index + 1) % node_count,
            edge_type: "next".to_string(),
            weight: 1.0,
        })
        .chain((0..node_count).step_by(8).map(|index| EdgeView {
            source_idx: index,
            target_idx: (index + node_count / 4) % node_count,
            edge_type: "cross".to_string(),
            weight: 1.0,
        }))
        .collect();
    (nodes, edges)
}

fn benchmark_layout(c: &mut Criterion) {
    let mut group = c.benchmark_group("force_directed_layout");
    group.sample_size(10);
    for node_count in [128, 4_096] {
        let (nodes, edges) = graph(node_count);
        group.bench_with_input(
            BenchmarkId::from_parameter(node_count),
            &node_count,
            |b, _| {
                b.iter_batched(
                    || nodes.clone(),
                    |mut nodes| force_directed_layout(&mut nodes, &edges),
                    criterion::BatchSize::SmallInput,
                )
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_layout);
criterion_main!(benches);
