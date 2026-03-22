//! FTS5 query sanitisation — strips characters that cause FTS5 syntax errors.

/// Prepare a free-text query for SQLite FTS5.
///
/// FTS5 has its own query syntax and rejects many characters that appear
/// naturally in prose (e.g. `?`, `!`, `'`, `(`, `)`).  This function:
///
/// 1. Keeps only alphanumeric characters, spaces, and the hyphen `-`
///    (hyphens are common in proper nouns and are safe inside tokens).
/// 2. Collapses runs of whitespace to a single space and trims the result.
/// 3. Returns `None` when the sanitised string is empty, so callers can
///    skip the FTS stage rather than sending an empty query to SQLite.
///
/// The original query is **not** modified — it is still used verbatim for
/// embedding and reranking, where punctuation is meaningful.
pub(super) fn fts5_sanitize(query: &str) -> Option<String> {
    let sanitized: String = query
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect();

    let collapsed = sanitized
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");

    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fts5_sanitize_strips_question_mark() {
        assert_eq!(
            fts5_sanitize("Who founded the Foundation?"),
            Some("Who founded the Foundation".to_string())
        );
    }

    #[test]
    fn test_fts5_sanitize_strips_punctuation() {
        assert_eq!(
            fts5_sanitize("What happened to the Galactic Empire!"),
            Some("What happened to the Galactic Empire".to_string())
        );
    }

    #[test]
    fn test_fts5_sanitize_strips_parentheses_and_apostrophes() {
        assert_eq!(
            fts5_sanitize("psychohistory (Hari's plan)"),
            Some("psychohistory Hari s plan".to_string())
        );
    }

    #[test]
    fn test_fts5_sanitize_preserves_hyphen() {
        assert_eq!(
            fts5_sanitize("well-known mathematician"),
            Some("well-known mathematician".to_string())
        );
    }

    #[test]
    fn test_fts5_sanitize_collapses_whitespace() {
        // Multiple spaces/punctuation between words collapse to a single space.
        let result = fts5_sanitize("empire,  collapse!  foundation");
        assert_eq!(result, Some("empire collapse foundation".to_string()));
    }

    #[test]
    fn test_fts5_sanitize_empty_string_returns_none() {
        assert_eq!(fts5_sanitize(""), None);
    }

    #[test]
    fn test_fts5_sanitize_only_punctuation_returns_none() {
        assert_eq!(fts5_sanitize("??? !!! ..."), None);
    }

    #[test]
    fn test_fts5_sanitize_plain_keywords_unchanged() {
        assert_eq!(
            fts5_sanitize("empire foundation terminus"),
            Some("empire foundation terminus".to_string())
        );
    }
}
