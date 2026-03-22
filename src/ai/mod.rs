//! AI provider abstractions: embedding and transcription.
pub mod embeddings;
pub mod transcription;

pub use embeddings::{
    EmbeddingManager, EmbeddingModelInfo, EmbeddingProvider, EmbeddingProviderType,
    LemonadeProvider,
};
pub use transcription::{
    LemonadeTranscriptionProvider, TranscriptionManager, TranscriptionProvider,
    mime_for_filename,
};
