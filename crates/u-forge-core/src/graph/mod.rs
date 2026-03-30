//! SQLite-backed knowledge graph storage.
mod storage;
mod nodes;
mod edges;
mod chunks;
mod fts;
mod traversal;

pub use storage::{KnowledgeGraphStorage, GraphStats, DEFAULT_EMBEDDING_CONTEXT_TOKENS, EMBEDDING_DIMENSIONS, MAX_CHUNK_TOKENS, ENABLE_HIGH_QUALITY_EMBEDDING};
