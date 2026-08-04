//! SQLite-backed knowledge graph storage.
mod chunks;
mod edges;
mod fts;
mod nodes;
mod positions;
mod storage;
mod traversal;

pub use storage::{
    DEFAULT_EMBEDDING_CONTEXT_TOKENS, EMBEDDING_DIMENSIONS, GraphStats,
    HIGH_QUALITY_EMBEDDING_DIMENSIONS, KnowledgeGraphStorage, MAX_CHUNK_TOKENS,
};
