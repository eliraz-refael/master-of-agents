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

/// Whether an ask's text loses `c` when it is cleaned (A2): a control character other than
/// `\n` and `\t`, which takes DEL and the C1 controls too. An `ESC` would end a bracketed
/// paste early, a `\r` would send it. The one test: tmux's `set-buffer` check uses it too.
pub fn is_unpastable(c: char) -> bool {
    c.is_control() && c != '\n' && c != '\t'
}
