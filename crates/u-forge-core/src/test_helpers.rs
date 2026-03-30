//! Test helpers shared across all integration-test modules.
//!
//! This module is compiled **only** when running tests (`#[cfg(test)]`).
//! It re-exports production utilities under test-friendly names so that
//! test modules stay concise:
//!
//! ```ignore
//! use crate::test_helpers::lemonade_url;
//!
//! let Some(url) = lemonade_url().await else {
//!     eprintln!("Skipping: no Lemonade Server reachable");
//!     return;
//! };
//! ```

pub(crate) use crate::lemonade::resolve_lemonade_url as lemonade_url;
