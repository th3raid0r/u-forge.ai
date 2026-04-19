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
    build_hq_embed_queue, embed_all_chunks, rechunk_and_embed, EmbeddingOutcome, EmbeddingPlan,
    EmbeddingProgress, EmbeddingResult, EmbeddingTarget,
};
pub use pipeline::{setup_and_index, SetupResult};
