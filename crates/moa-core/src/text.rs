//! Small helpers for text that moa shows or logs.

/// The first `n` characters of `text`.
pub fn cut(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}
