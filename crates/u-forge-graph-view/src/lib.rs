// u-forge-graph-view — framework-agnostic graph view model and layout engine.
//
// Converts raw KnowledgeGraph data into a structure optimized for frame-rate
// rendering. Both GPUI and egui UI frontends share this crate.

mod layout;
mod observable;
mod snapshot;
mod spatial;

pub use layout::{
    LayoutIterationMetrics, LayoutMetrics, force_directed_layout, force_directed_layout_with_fixed,
};
pub use observable::{GraphEvent, ObservableGraph};
pub use snapshot::{
    EdgeView, GraphSnapshot, LodLevel, NodeView, build_snapshot, build_snapshot_incremental,
};
pub use spatial::NodeEntry;
