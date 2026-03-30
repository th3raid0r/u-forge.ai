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
#[derive(Debug)]
pub enum AppError {
    /// The requested resource was not found (HTTP 404).
    NotFound(String),
    /// The request was malformed or contained invalid data (HTTP 400).
    BadRequest(String),
    /// An unexpected internal error occurred (HTTP 500).
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        AppError::Internal(err)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NotFound(msg) => write!(f, "Not found: {msg}"),
            AppError::BadRequest(msg) => write!(f, "Bad request: {msg}"),
            AppError::Internal(err) => write!(f, "Internal error: {err}"),
        }
    }
}

// TODO Phase 3: add `impl axum::response::IntoResponse for AppError` here
// once `axum` is added as a dependency.  Map:
//   NotFound    → StatusCode::NOT_FOUND + JSON body
//   BadRequest  → StatusCode::BAD_REQUEST + JSON body
//   Internal    → StatusCode::INTERNAL_SERVER_ERROR + JSON body (sanitised)
