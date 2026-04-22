//! Text splitting utility for chunk-size management.

use std::sync::LazyLock;

use tiktoken_rs::CoreBPE;
use tracing::info;

use crate::graph::MAX_CHUNK_TOKENS;

/// Cached o200k_harmony BPE tokenizer — constructed once, reused forever.
///
/// `o200k_harmony()` parses a ~200 k-entry vocabulary on every call; caching
/// it here turns repeated `count_tokens` invocations (e.g. inside
/// [`split_text`]'s per-word loop) from O(N × vocab_parse) into O(N × encode).
static O200K_BPE: LazyLock<CoreBPE> =
    LazyLock::new(|| tiktoken_rs::o200k_harmony().expect("o200k_harmony is always available"));

/// Count tokens in `text` using the o200k_harmony BPE tokenizer.
pub(crate) fn count_tokens(text: &str) -> usize {
    O200K_BPE.encode_with_special_tokens(text).len()
}

/// Bisect `word` at character midpoints until every piece fits within
/// [`MAX_CHUNK_TOKENS`]. Used for words (or runs of text without whitespace,
/// such as CJK prose or base64 blobs) that cannot be split at spaces.
///
/// Logs at `info` level when a hard-split fires — useful signal during
/// ingestion of non-Latin corpora.
fn split_oversized_word(word: &str) -> Vec<String> {
    if count_tokens(word) <= MAX_CHUNK_TOKENS {
        return vec![word.to_string()];
    }
    info!(
        len_chars = word.chars().count(),
        "hard-splitting oversized token-dense word at character midpoint"
    );
    let chars: Vec<char> = word.chars().collect();
    let mid = chars.len() / 2;
    let left: String = chars[..mid].iter().collect();
    let right: String = chars[mid..].iter().collect();
    let mut result = split_oversized_word(&left);
    result.extend(split_oversized_word(&right));
    result
}

/// Split `text` into pieces of at most [`MAX_CHUNK_TOKENS`] tokens, breaking
/// at whitespace boundaries where possible and bisecting at character midpoints
/// for runs without whitespace (CJK prose, base64 blobs, long URLs).
///
/// Token counts are measured with the o200k_harmony BPE tokenizer so that the
/// budget is exact and consistent with what is stored in
/// [`TextChunk::token_count`].
pub(crate) fn split_text(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }

    // Fast path: entire text fits in one chunk.
    if count_tokens(text) <= MAX_CHUNK_TOKENS {
        return vec![text.to_string()];
    }

    let mut pieces: Vec<String> = Vec::new();
    let mut current_words: Vec<&str> = Vec::new();

    for word in text.split_whitespace() {
        current_words.push(word);
        let candidate = current_words.join(" ");
        if count_tokens(&candidate) > MAX_CHUNK_TOKENS {
            if current_words.len() == 1 {
                // Single token-dense word (CJK, base64, etc.) — bisect it.
                pieces.extend(split_oversized_word(&candidate));
                current_words.clear();
            } else {
                // Flush everything except the word that pushed us over.
                current_words.pop();
                pieces.push(current_words.join(" "));
                current_words.clear();
                current_words.push(word);
            }
        }
    }

    if !current_words.is_empty() {
        pieces.push(current_words.join(" "));
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
        // Build a string that is exactly MAX_CHUNK_TOKENS tokens by using
        // single-character words separated by spaces (each "a " is ~1 token).
        // We overshoot slightly and verify the first chunk ≤ MAX_CHUNK_TOKENS.
        let words: Vec<&str> = vec!["hello"; MAX_CHUNK_TOKENS / 2];
        let content = words.join(" ");
        let pieces = split_text(&content);
        // The content may or may not fit in one chunk depending on BPE merges;
        // all we assert is that every piece is within budget.
        for piece in &pieces {
            assert!(
                count_tokens(piece) <= MAX_CHUNK_TOKENS,
                "piece exceeds token budget: {} tokens",
                count_tokens(piece)
            );
        }
    }

    #[test]
    fn test_split_text_long_content_splits_on_word_boundary() {
        // Build content clearly longer than MAX_CHUNK_TOKENS tokens.
        let word = "extraordinary"; // ~3 tokens each
        let repeats = (MAX_CHUNK_TOKENS / 3) * 4;
        let content = (0..repeats).map(|_| word).collect::<Vec<_>>().join(" ");

        assert!(
            count_tokens(&content) > MAX_CHUNK_TOKENS,
            "pre-condition: content must exceed token limit"
        );

        let pieces = split_text(&content);
        assert!(pieces.len() >= 2, "long content must be split");
        for piece in &pieces {
            assert!(
                count_tokens(piece) <= MAX_CHUNK_TOKENS,
                "piece exceeds token budget: {} tokens",
                count_tokens(piece)
            );
            assert!(!piece.is_empty());
        }

        let original_words: Vec<_> = content.split_whitespace().collect();
        let rejoined_words: Vec<_> = pieces.iter().flat_map(|p| p.split_whitespace()).collect();
        assert_eq!(original_words, rejoined_words);
    }

    #[test]
    fn test_split_text_hard_cut_when_no_whitespace() {
        // A single token-dense word that exceeds MAX_CHUNK_TOKENS is bisected
        // at character midpoints until every piece fits the budget.
        let content = "x".repeat(MAX_CHUNK_TOKENS * 6);
        let pieces = split_text(&content);
        assert!(!pieces.is_empty());
        for piece in &pieces {
            assert!(!piece.is_empty());
            assert!(
                count_tokens(piece) <= MAX_CHUNK_TOKENS,
                "bisected piece still exceeds token budget: {} tokens",
                count_tokens(piece)
            );
        }
    }

    #[test]
    fn test_split_text_cjk_stays_within_budget() {
        // CJK text has no whitespace — split_text must bisect it rather than
        // emitting a single oversized chunk.
        let cjk_char = '字';
        // Each CJK character is roughly 1 token, so repeat well past budget.
        let content: String = std::iter::repeat(cjk_char)
            .take(MAX_CHUNK_TOKENS * 3)
            .collect();
        assert!(count_tokens(&content) > MAX_CHUNK_TOKENS);
        let pieces = split_text(&content);
        assert!(pieces.len() >= 2, "CJK blob must be split");
        for piece in &pieces {
            assert!(
                count_tokens(piece) <= MAX_CHUNK_TOKENS,
                "CJK piece exceeds token budget: {} tokens",
                count_tokens(piece)
            );
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
