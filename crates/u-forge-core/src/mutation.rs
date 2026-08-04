//! Mutation events emitted by [`crate::KnowledgeGraph`].

use crate::{ObjectId, types::Edge};

/// Schema-authoritative write commands accepted by [`crate::KnowledgeGraph`].
#[derive(Debug, Clone)]
pub enum GraphMutation {
    UpsertObject(crate::ObjectMetadata),
    DeleteObject(ObjectId),
    UpsertEdge(Edge),
    DeleteEdge {
        from: ObjectId,
        to: ObjectId,
        edge_type: String,
    },
    ClearData,
    ClearSchemas,
    ClearAll,
}

/// A committed graph change. Events are emitted only after storage succeeds.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphChange {
    ObjectUpserted {
        id: ObjectId,
        created: bool,
    },
    ObjectDeleted {
        id: ObjectId,
    },
    EdgeUpserted(Edge),
    EdgeDeleted {
        from: ObjectId,
        to: ObjectId,
        edge_type: String,
    },
    DataCleared,
    SchemasCleared,
    AllCleared,
}
