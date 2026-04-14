//! Text splitting utility for chunk-size management.

use crate::graph::MAX_CHUNK_TOKENS;

/// Split `text` into pieces of at most [`MAX_CHUNK_TOKENS`] tokens, breaking
/// only at whitespace boundaries.
///
/// Uses the same `len / 3` heuristic as [`estimate_token_count`] so that the
/// token budget is always consistent with what is stored in [`TextChunk::token_count`].
///
/// [`estimate_token_count`]: crate::types
pub(crate) fn split_text(text: &str) -> Vec<String> {
    // 3 chars per token mirrors estimate_token_count in types.rs.
    let max_chars = MAX_CHUNK_TOKENS * 3;

    if text.len() <= max_chars {
        let trimmed = text.trim().to_string();
        return if trimmed.is_empty() {
            vec![]
        } else {
            vec![trimmed]
        };
    }

    let mut pieces: Vec<String> = Vec::new();
    let mut remaining = text.trim();

    while !remaining.is_empty() {
        if remaining.len() <= max_chars {
            pieces.push(remaining.to_string());
            break;
        }

        // Find the last whitespace at or before the max_chars boundary so we
        // never cut through a word.
        let boundary = &remaining[..max_chars];
        let split_at = boundary.rfind(char::is_whitespace).unwrap_or(max_chars); // no whitespace found → hard cut

        let piece = remaining[..split_at].trim().to_string();
        if !piece.is_empty() {
            pieces.push(piece);
        }

        // Advance past the split point, skipping leading whitespace.
        remaining = remaining[split_at..].trim_start();
    }

    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_text_short_content_is_not_split() {
        let pieces = split_text("A short description.");
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], "A short description.");
    }

    #[test]
    fn test_split_text_empty_and_whitespace_produce_no_pieces() {
        assert!(split_text("").is_empty());
        assert!(split_text("   \n\t  ").is_empty());
    }

    #[test]
    fn test_split_text_exact_boundary_is_not_split() {
        let max_chars = MAX_CHUNK_TOKENS * 3;
        let content = "a".repeat(max_chars);
        let pieces = split_text(&content);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].len(), max_chars);
    }

    #[test]
    fn test_split_text_long_content_splits_on_word_boundary() {
        let max_chars = MAX_CHUNK_TOKENS * 3;
        // Build content that is 3× the character limit so it must split into ≥2 pieces.
        let word = "x".repeat(399);
        let repeats = (max_chars * 3 / (word.len() + 1)) + 1;
        let content = (0..repeats).map(|_| word.as_str()).collect::<Vec<_>>().join(" ");
        assert!(
            content.len() > max_chars,
            "pre-condition: content must exceed limit"
        );

        let pieces = split_text(&content);
        assert!(pieces.len() >= 2, "long content must be split");
        for piece in &pieces {
            assert!(
                piece.len() <= max_chars,
                "piece too long ({} chars): {:?}",
                piece.len(),
                &piece[..piece.len().min(40)]
            );
            assert!(!piece.is_empty());
        }
        let rejoined = pieces.join(" ");
        let original_words: Vec<_> = content.split_whitespace().collect();
        let rejoined_words: Vec<_> = rejoined.split_whitespace().collect();
        assert_eq!(original_words, rejoined_words);
    }

    #[test]
    fn test_split_text_hard_cut_when_no_whitespace() {
        let max_chars = MAX_CHUNK_TOKENS * 3;
        let content = "z".repeat(max_chars * 2 + 1);
        let pieces = split_text(&content);
        assert!(pieces.len() >= 2, "must hard-cut oversized no-whitespace content");
        for piece in &pieces {
            assert!(piece.len() <= max_chars);
            assert!(!piece.is_empty());
        }
    }

    #[test]
    fn test_split_text_leading_trailing_whitespace_is_trimmed() {
        let pieces = split_text("  hello world  ");
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], "hello world");
    }
}
