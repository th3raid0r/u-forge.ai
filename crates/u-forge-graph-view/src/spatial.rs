use rstar::{AABB, PointDistance, RTreeObject};
use u_forge_core::ObjectId;

/// Entry in the R-tree spatial index. Stores the node's `ObjectId` and its
/// 2D position.
///
/// Using `ObjectId` instead of a `usize` index decouples the index from node
/// ordering across snapshot rebuilds. The node's index into `GraphSnapshot::nodes` is resolved on demand via
/// `GraphSnapshot::id_to_idx`.
#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub id: ObjectId,
    pub position: [f32; 2],
}

impl PartialEq for NodeEntry {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl RTreeObject for NodeEntry {
    type Envelope = AABB<[f32; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_point(self.position)
    }
}

impl PointDistance for NodeEntry {
    fn distance_2(&self, point: &[f32; 2]) -> f32 {
        let dx = self.position[0] - point[0];
        let dy = self.position[1] - point[1];
        dx * dx + dy * dy
    }
}
