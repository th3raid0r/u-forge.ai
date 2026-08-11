//! Typed errors exposed across u-forge subsystem boundaries.
//!
//! Internal fallible operations generally use [`anyhow::Error`]. Dimension and
//! embedding-space errors remain typed because callers discriminate them.
//! [`AppError`] is a generic application error enum; no HTTP framework or
//! response conversion is part of the current workspace.

/// Returned by [`crate::graph::KnowledgeGraphStorage::new`] when the
/// on-disk embedding dimensions differ from the configured dimensions.
///
/// This indicates the embedding model was changed without recreating the
/// database.  The caller should either re-index the database or use the
/// previous configured dimensions — no auto-migration is performed.
#[derive(Debug, thiserror::Error)]
#[error(
    "embedding dimension mismatch for vec table '{table}': \
     database has {stored}-dim embeddings but the current model produces {expected}-dim. \
     Re-index the database or pin the previous embedding model."
)]
pub struct EmbeddingDimensionMismatch {
    /// The `vec0` virtual table whose schema diverges (e.g. `"chunks_vec"`).
    pub table: String,
    /// Dimensions recorded in the database at creation time.
    pub stored: usize,
    /// Dimensions expected by the current configuration.
    pub expected: usize,
}

/// Returned when vectors already stored in a lane were produced by a
/// different embedding provider set than the current queue.
#[derive(Debug, thiserror::Error)]
#[error(
    "embedding space mismatch for lane '{lane}': database fingerprint is '{stored}', current fingerprint is '{current}'. Re-index this lane before semantic search."
)]
pub struct EmbeddingSpaceMismatch {
    pub lane: String,
    pub stored: String,
    pub current: String,
}

/// Returned for a legacy populated vector lane that has no recorded identity.
#[derive(Debug, thiserror::Error)]
#[error(
    "embedding space for lane '{lane}' is unidentified but already contains vectors. Re-index this lane once to record its model fingerprint."
)]
pub struct UnidentifiedEmbeddingSpace {
    pub lane: String,
}

/// Generic application-level error categories.
///
/// The enum currently has no transport-specific response conversion.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The requested resource was not found.
    #[error("Not found: {0}")]
    NotFound(String),
    /// The request was malformed or contained invalid data.
    #[error("Bad request: {0}")]
    BadRequest(String),
    /// An unexpected internal error occurred.
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}
