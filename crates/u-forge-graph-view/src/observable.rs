// ObservableGraph — compatibility wrapper around the core mutation stream.

use std::ops::Deref;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;
use u_forge_core::{EdgeType, GraphChange, KnowledgeGraph, ObjectId, ObjectMetadata};

/// Backward-compatible name for the core committed-change event.
pub type GraphEvent = GraphChange;

/// A thin compatibility wrapper around `Arc<KnowledgeGraph>` and its core
/// [`GraphEvent`] stream.
///
/// Read-only methods are available via `Deref<Target = KnowledgeGraph>` —
/// call them directly without going through the wrapper.
pub struct ObservableGraph {
    inner: Arc<KnowledgeGraph>,
}

impl ObservableGraph {
    /// Wrap an existing shared `KnowledgeGraph`.
    pub fn new(graph: Arc<KnowledgeGraph>) -> Self {
        Self { inner: graph }
    }

    /// Subscribe to graph mutation events.
    ///
    /// Subscribers receive a [`broadcast::Receiver`]. Lagged receivers (that
    /// fall behind by more than 64 events) will receive a
    /// [`broadcast::error::RecvError::Lagged`] error and should do a full
    /// snapshot rebuild rather than applying incremental events.
    pub fn subscribe(&self) -> broadcast::Receiver<GraphEvent> {
        self.inner.subscribe_changes()
    }

    /// Persist a new object, returning its [`ObjectId`].
    /// Emits an object-upsert change on success.
    pub fn add_object(&self, metadata: ObjectMetadata) -> Result<ObjectId> {
        self.inner.add_object(metadata)
    }

    /// Overwrite an existing object's metadata.
    /// Emits an object-upsert change on success.
    pub fn update_object(&self, metadata: ObjectMetadata) -> Result<()> {
        self.inner.update_object(metadata)
    }

    /// Delete an object and all its edges.
    /// Emits an object-deleted change on success.
    pub fn delete_object(&self, id: ObjectId) -> Result<()> {
        self.inner.delete_object(id)
    }

    /// Create a typed relationship between two objects.
    /// Emits an edge-upsert change on success.
    pub fn connect_objects(&self, from: ObjectId, to: ObjectId, edge_type: EdgeType) -> Result<()> {
        self.inner.connect_objects(from, to, edge_type)
    }

    /// Create a relationship using a plain string edge type.
    /// Emits an edge-upsert change on success.
    pub fn connect_objects_str(&self, from: ObjectId, to: ObjectId, edge_type: &str) -> Result<()> {
        self.inner.connect_objects_str(from, to, edge_type)
    }
}

impl Deref for ObservableGraph {
    type Target = KnowledgeGraph;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
