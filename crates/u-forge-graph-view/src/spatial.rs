use rstar::{PointDistance, RTreeObject, AABB};

/// Entry in the R-tree spatial index. Stores the node's index into
/// `GraphSnapshot::nodes` and its 2D position.
#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub index: usize,
    pub position: [f32; 2],
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
