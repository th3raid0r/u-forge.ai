//! Data ingestion pipelines and utilities.
//!
//! The canonical flow is:
//! 1. Call [`setup_and_index`] to load schemas, import data, and build FTS5 indexes
//! 2. Call [`embed_all_chunks`] to compute embeddings for semantic search
//! 3. Optionally call [`build_hq_embed_queue`] for high-quality embeddings
//!
//! # Modules
//! * [`data`] — low-level JSON import via [`DataIngestion`]
//! * [`pipeline`] — high-level orchestration: [`setup_and_index`]
//! * [`embedding`] — batch embedding: [`embed_all_chunks`], [`build_hq_embed_queue`]
pub mod data;
pub mod embedding;
pub mod pipeline;

pub use data::{DataIngestion, IngestionStats, JsonEntry};
pub use embedding::{
    EmbeddingOutcome, EmbeddingPlan, EmbeddingProgress, EmbeddingResult, EmbeddingTarget,
    build_hq_embed_queue, build_hq_embed_queue_with_connection, embed_all_chunks,
    embed_all_chunks_with_cancellation, rechunk_and_embed, rechunk_and_embed_with_cancellation,
};
pub use pipeline::{
    SetupResult, import_data_only, import_data_only_with_cancellation, import_schemas_and_data,
    import_schemas_and_data_with_cancellation, setup_and_index, setup_and_index_with_cancellation,
};
