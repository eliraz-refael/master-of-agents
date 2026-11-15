//! How the spike reads a session's state and "why" (`picture.py`: `LIVE_SQL`, `live()` and
//! `asks()`). The importer follows these rules exactly so a session looks the same the
//! moment after the switch.

/// Python's `str.strip()` whitespace: Unicode whitespace plus the four separators `\x1c`..`\x1f`.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\x0b'
            | '\x0c'
            | '\x1c'
            | '\x1d'
            | '\x1e'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// The last line of `text`, as Python's `splitlines()[-1:]` gives it (empty for no text).
fn last_line(text: &str) -> &str {
    let text = strip(text);
    match text.rfind(is_line_break) {
        Some(i) => {
            let c = text[i..].chars().next().map_or(1, char::len_utf8);
            &text[i + c..]
        }
        None => text,
    }
}

/// `waiting` events with this text are Claude's idle ping, never "needs you". The spike
/// matches with SQL `LIKE`, which ignores ASCII case.
pub(super) fn is_idle_ping(text: Option<&str>) -> bool {
    text.is_some_and(|t| t.to_ascii_lowercase().contains("waiting for your input"))
}

/// `asks()`: did a reply end by asking something?
pub(super) fn asks(text: &str) -> bool {
    last_line(text)
        .trim_end_matches([' ', '*', '_', '`', ')', '"', '\''])
        .ends_with('?')
}

/// The newest prompt, stop or waiting event that isn't an idle ping.
pub(super) struct Latest<'a> {
    pub kind: &'a str,
    pub text: Option<&'a str>,
}

/// `live()`'s `why`: one line of context for a session.
pub(super) fn activity_why(
    state: Option<&str>,
    last_prompt: Option<&str>,
    latest: Option<&Latest>,
) -> String {
    let text = strip(latest.and_then(|l| l.text).unwrap_or(""));
    let kind = latest.map(|l| l.kind);
    let why = if state == Some("working") {
        last_prompt.unwrap_or("").replace('\n', " ")
    } else if state == Some("waiting") && kind == Some("waiting") {
        let one = text.replace('\n', " ");
        if one.is_empty() {
            "needs you".into()
        } else {
            one
        }
    } else if kind == Some("stop") && asks(text) {
        last_line(text).trim_matches([' ', '*', '_']).to_string()
    } else if text.is_empty() {
        String::new()
    } else {
        last_line(text)
            .trim_matches([' ', '*', '_', '#'])
            .to_string()
    };
    if why.starts_with('<') {
        String::new()
    } else {
        why
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn why(state: &str, prompt: Option<&str>, kind: &str, text: Option<&str>) -> String {
        activity_why(Some(state), prompt, Some(&Latest { kind, text }))
    }

    #[test]
    fn asks_reads_the_last_line_without_markdown() {
        assert!(asks("Shall I go on?"));
        assert!(asks("intro\n**Shall I go on?**  \n"));
        assert!(asks("(really?)"));
        assert!(asks("\"ok?\""));
        assert!(!asks("Done."));
        assert!(!asks("Why?\nbecause"));
        assert!(!asks(""));
    }

    #[test]
    fn why_follows_picture_py() {
        assert_eq!(why("working", Some("a\nb"), "prompt", Some("x")), "a b");
        assert_eq!(why("working", None, "stop", Some("x")), "");
        assert_eq!(
            why("waiting", None, "waiting", Some(" May I?\nplease ")),
            "May I? please"
        );
        assert_eq!(why("waiting", None, "waiting", None), "needs you");
        assert_eq!(why("idle", None, "stop", Some("a\n**Go on?**")), "Go on?");
        assert_eq!(why("idle", None, "stop", Some("a\n## Done_ *")), "Done");
        assert_eq!(why("idle", None, "stop", Some("<command>x")), "");
        assert_eq!(why("idle", None, "stop", Some("   ")), "");
        assert_eq!(activity_why(Some("idle"), Some("p"), None), "");
        assert_eq!(why("ended", None, "stop", Some("Sketched")), "Sketched");
    }

    #[test]
    fn idle_pings_ignore_case() {
        assert!(is_idle_ping(Some("Claude is waiting for your input")));
        assert!(is_idle_ping(Some("WAITING FOR YOUR INPUT")));
        assert!(!is_idle_ping(Some("needs permission")));
        assert!(!is_idle_ping(None));
    }

    #[test]
    fn last_line_splits_like_python() {
        assert_eq!(last_line("a\r\nb"), "b");
        assert_eq!(last_line("a\u{2028}b "), "b");
        assert_eq!(last_line("a\n\n"), "a");
    }
}
