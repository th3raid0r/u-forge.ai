// u-forge-graph-view — framework-agnostic graph view model and layout engine.
//
// Converts raw KnowledgeGraph data into a structure optimized for frame-rate
// rendering. Both GPUI and egui UI frontends share this crate.

mod layout;
mod snapshot;
mod spatial;

pub use layout::force_directed_layout;
pub use snapshot::{build_snapshot, EdgeView, GraphSnapshot, LodLevel, NodeView};
pub use spatial::NodeEntry;
