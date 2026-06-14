//! Tokenizer for the BM25-lite inverted index.
//!
//! Kept separate from `store.rs` so the token logic can be tested without redb.

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "in", "of", "to", "and", "or", "for", "be", "with",
    "as", "at", "by", "from", "it", "its", "that", "this", "was",
];

/// Maximum byte length of a single token. Longer tokens are silently skipped
/// to prevent DoS via a crafted 8 KiB single-"word" value.
const MAX_TOKEN_BYTES: usize = 64;

/// Tokenize `text` into lowercase, non-stopword, length-bounded terms.
///
/// Steps:
/// 1. Lowercase via `str::to_lowercase()` (std only, no unicode crate).
/// 2. Split on non-alphanumeric characters.
/// 3. Drop empty tokens, tokens > 64 bytes, and stopwords.
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| {
            !t.is_empty()
                && t.len() <= MAX_TOKEN_BYTES
                && !STOPWORDS.contains(t)
        })
        .map(|t| t.to_string())
        .collect()
}

/// Count how many times each token in `terms` appears in `tokens`.
pub fn term_frequencies(tokens: &[String], terms: &[String]) -> Vec<usize> {
    terms
        .iter()
        .map(|term| tokens.iter().filter(|t| *t == term).count())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_lowercases_and_splits() {
        let tokens = tokenize("Hello, World! Rust-lang");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"lang".to_string()));
    }

    #[test]
    fn tokenize_drops_stopwords() {
        let tokens = tokenize("the quick brown fox");
        assert!(!tokens.contains(&"the".to_string()));
        assert!(tokens.contains(&"quick".to_string()));
        assert!(tokens.contains(&"brown".to_string()));
        assert!(tokens.contains(&"fox".to_string()));
    }

    #[test]
    fn all_stopword_query_returns_empty_no_panic() {
        let tokens = tokenize("the a an is in");
        assert!(tokens.is_empty(), "all stopwords should produce empty token list");
    }

    #[test]
    fn token_length_capped_silently() {
        // A 128-byte "word" must be skipped; shorter words around it must pass.
        let long_token = "x".repeat(128);
        let text = format!("normal {} word", long_token);
        let tokens = tokenize(&text);
        assert!(tokens.contains(&"normal".to_string()));
        assert!(tokens.contains(&"word".to_string()));
        assert!(!tokens.iter().any(|t| t.len() > MAX_TOKEN_BYTES));
    }

    #[test]
    fn term_frequencies_counts_correctly() {
        let tokens = tokenize("rust is fast and rust is safe");
        let terms = vec!["rust".to_string(), "fast".to_string()];
        let freqs = term_frequencies(&tokens, &terms);
        assert_eq!(freqs[0], 2, "rust appears twice");
        assert_eq!(freqs[1], 1, "fast appears once");
    }
}
