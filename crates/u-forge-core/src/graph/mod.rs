//! SQLite-backed knowledge graph storage.
mod storage;
mod nodes;
mod edges;
mod chunks;
mod fts;
mod traversal;
mod positions;

pub use storage::{KnowledgeGraphStorage, GraphStats, DEFAULT_EMBEDDING_CONTEXT_TOKENS, EMBEDDING_DIMENSIONS, HIGH_QUALITY_EMBEDDING_DIMENSIONS, MAX_CHUNK_TOKENS};
