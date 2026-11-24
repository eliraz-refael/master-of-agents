//! Small helpers for text that moa shows or logs.

/// `s` on one line, every control character a space: a tag tmux prints in a list (a
/// `@moa_label`, last on its line) then never prints a line of its own, and a heading in
/// moa's prompts stays one line.
pub fn one_line(s: &str) -> String {
    s.replace(char::is_control, " ")
}

/// The first `n` characters of `text`.
pub fn cut(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}
