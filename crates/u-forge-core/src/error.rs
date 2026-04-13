//! Application-level error types for u-forge.ai HTTP handler boundaries.
//!
//! Internal code uses [`anyhow::Error`] throughout.  This module provides
//! [`AppError`], a typed enum that converts `anyhow` errors to appropriate
//! HTTP responses at the axum handler boundary.
//!
//! # Phase 3 note
//!
//! The `impl IntoResponse for AppError` block is intentionally omitted here —
//! it will be added in Phase 3 when `axum` is introduced as a dependency.
//! The `From<anyhow::Error>` impl is present so handlers can use `?` with
//! `anyhow`-returning functions once the axum dependency is wired.

/// Application-level error returned by axum HTTP handlers.
///
/// Convert any `anyhow::Error` via the `From` impl (or `?` operator) and let
/// the `IntoResponse` impl (added in Phase 3) translate it to an HTTP status
/// code and JSON body.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The requested resource was not found (HTTP 404).
    #[error("Not found: {0}")]
    NotFound(String),
    /// The request was malformed or contained invalid data (HTTP 400).
    #[error("Bad request: {0}")]
    BadRequest(String),
    /// An unexpected internal error occurred (HTTP 500).
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

// TODO Phase 3: add `impl axum::response::IntoResponse for AppError` here
// once `axum` is added as a dependency.  Map:
//   NotFound    → StatusCode::NOT_FOUND + JSON body
//   BadRequest  → StatusCode::BAD_REQUEST + JSON body
//   Internal    → StatusCode::INTERNAL_SERVER_ERROR + JSON body (sanitised)
