//! Test helpers shared across all integration-test modules.
//!
//! This module is compiled **only** when running tests (`#[cfg(test)]`).
//!
//! # Integration test toggle
//!
//! All integration tests that need a live Lemonade Server use
//! [`integration_test_url`] (or the [`require_integration_url`] macro) to
//! obtain the server URL.  Behaviour is controlled by the
//! **`UFORGE_INTEGRATION_TESTS`** environment variable:
//!
//! | Value       | Behaviour                                              |
//! |-------------|--------------------------------------------------------|
//! | *unset*     | Probe localhost; skip silently if unreachable (default) |
//! | `"require"` | Probe localhost; **panic** if unreachable               |
//! | `"skip"`    | Always skip, even if a server is reachable              |
//! | any URL     | Use that URL directly; **panic** if unreachable         |
//!
//! ## Usage
//!
//! ```ignore
//! use crate::test_helpers::require_integration_url;
//!
//! #[tokio::test]
//! async fn test_something() {
//!     let url = require_integration_url!();
//!     // ... test body ...
//! }
//! ```

/// Returns the resolved Lemonade URL if integration tests should run.
///
/// See the [module docs](self) for the `UFORGE_INTEGRATION_TESTS` contract.
pub(crate) async fn integration_test_url() -> Option<String> {
    match std::env::var("UFORGE_INTEGRATION_TESTS").as_deref() {
        Ok("skip") => {
            eprintln!("SKIP: UFORGE_INTEGRATION_TESTS=skip");
            None
        }
        Ok("require") => {
            let url = crate::lemonade::resolve_lemonade_url()
                .await
                .expect(
                    "UFORGE_INTEGRATION_TESTS=require but no Lemonade Server reachable \
                     (tried localhost:8000 and LEMONADE_URL)",
                );
            Some(url)
        }
        Ok(url) if !url.is_empty() => {
            // Treat as an explicit URL — verify reachability.
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap_or_default();

            let health = format!(
                "{}/health",
                url.trim_end_matches('/')
            );

            let reachable = client
                .get(&health)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);

            assert!(
                reachable,
                "UFORGE_INTEGRATION_TESTS={url} but health check ({health}) failed",
            );
            Some(url.to_string())
        }
        _ => {
            // Unset or empty: legacy behaviour — probe and skip silently.
            crate::lemonade::resolve_lemonade_url().await
        }
    }
}

/// Convenience macro that replaces the skip-guard boilerplate.
///
/// Expands to an early `return` when no server is available (in default/skip
/// mode) or panics when the env var demands a server that isn't there.
macro_rules! require_integration_url {
    () => {
        match $crate::test_helpers::integration_test_url().await {
            Some(url) => url,
            None => {
                eprintln!("SKIP: no Lemonade Server reachable");
                return;
            }
        }
    };
}
pub(crate) use require_integration_url;
