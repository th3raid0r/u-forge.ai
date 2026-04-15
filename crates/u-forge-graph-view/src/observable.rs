// ObservableGraph — wraps Arc<KnowledgeGraph> and emits broadcast events on mutations.
//
// The core KnowledgeGraph is unchanged; this wrapper intercepts mutating calls,
// forwards them, then broadcasts the corresponding GraphEvent to all subscribers.
// The UI subscribes to trigger incremental snapshot refreshes rather than full rebuilds.

use std::ops::Deref;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;
use u_forge_core::{EdgeType, KnowledgeGraph, ObjectId, ObjectMetadata};

/// Events emitted by [`ObservableGraph`] after a successful mutation.
#[derive(Debug, Clone)]
pub enum GraphEvent {
    NodeAdded(ObjectId),
    NodeUpdated(ObjectId),
    NodeDeleted(ObjectId),
    EdgeAdded { source: ObjectId, target: ObjectId },
    EdgeDeleted { source: ObjectId, target: ObjectId },
}

/// A thin wrapper around `Arc<KnowledgeGraph>` that broadcasts [`GraphEvent`]
/// notifications after every successful mutation.
///
/// Read-only methods are available via `Deref<Target = KnowledgeGraph>` —
/// call them directly without going through the wrapper.
pub struct ObservableGraph {
    inner: Arc<KnowledgeGraph>,
    sender: broadcast::Sender<GraphEvent>,
}

impl ObservableGraph {
    /// Wrap an existing shared `KnowledgeGraph`.
    pub fn new(graph: Arc<KnowledgeGraph>) -> Self {
        let (sender, _) = broadcast::channel(64);
        Self { inner: graph, sender }
    }

    /// Subscribe to graph mutation events.
    ///
    /// Subscribers receive a [`broadcast::Receiver`]. Lagged receivers (that
    /// fall behind by more than 64 events) will receive a
    /// [`broadcast::error::RecvError::Lagged`] error and should do a full
    /// snapshot rebuild rather than applying incremental events.
    pub fn subscribe(&self) -> broadcast::Receiver<GraphEvent> {
        self.sender.subscribe()
    }

    /// Persist a new object, returning its [`ObjectId`].
    /// Emits [`GraphEvent::NodeAdded`] on success.
    pub fn add_object(&self, metadata: ObjectMetadata) -> Result<ObjectId> {
        let id = self.inner.add_object(metadata)?;
        let _ = self.sender.send(GraphEvent::NodeAdded(id));
        Ok(id)
    }

    /// Overwrite an existing object's metadata.
    /// Emits [`GraphEvent::NodeUpdated`] on success.
    pub fn update_object(&self, metadata: ObjectMetadata) -> Result<()> {
        let id = metadata.id;
        self.inner.update_object(metadata)?;
        let _ = self.sender.send(GraphEvent::NodeUpdated(id));
        Ok(())
    }

    /// Delete an object and all its edges.
    /// Emits [`GraphEvent::NodeDeleted`] on success.
    pub fn delete_object(&self, id: ObjectId) -> Result<()> {
        self.inner.delete_object(id)?;
        let _ = self.sender.send(GraphEvent::NodeDeleted(id));
        Ok(())
    }

    /// Create a typed relationship between two objects.
    /// Emits [`GraphEvent::EdgeAdded`] on success.
    pub fn connect_objects(
        &self,
        from: ObjectId,
        to: ObjectId,
        edge_type: EdgeType,
    ) -> Result<()> {
        self.inner.connect_objects(from, to, edge_type)?;
        let _ = self.sender.send(GraphEvent::EdgeAdded { source: from, target: to });
        Ok(())
    }

    /// Create a relationship using a plain string edge type.
    /// Emits [`GraphEvent::EdgeAdded`] on success.
    pub fn connect_objects_str(&self, from: ObjectId, to: ObjectId, edge_type: &str) -> Result<()> {
        self.inner.connect_objects_str(from, to, edge_type)?;
        let _ = self.sender.send(GraphEvent::EdgeAdded { source: from, target: to });
        Ok(())
    }
}

impl Deref for ObservableGraph {
    type Target = KnowledgeGraph;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
