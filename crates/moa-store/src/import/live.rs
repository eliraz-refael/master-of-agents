//! How the spike reads a session's state and "why" (`picture.py`: `LIVE_SQL`, `live()` and
//! `asks()`). The importer follows these rules exactly so a session looks the same the
//! moment after the switch.

use moa_core::live::{asks, last_line, strip};

/// The newest prompt, stop or waiting event that isn't an idle ping.
pub(super) struct Latest<'a> {
    pub kind: &'a str,
    pub text: Option<&'a str>,
}

/// `live()`'s `why`: one line of context for a session.
///
/// This departs from picture.py in one place. v1 doesn't store the kind of the latest event,
/// and slice 2 derives "asks you" by applying [`asks`] to the stored why. The spike shows
/// "question" only when the latest event is a `stop` (`last_kind == "stop"`), so outside the
/// working and needs-permission cases the why carries text only when the latest event is a
/// stop, and is empty otherwise. Then "asks(why)" is true exactly when the spike shows "question".
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
    } else if kind != Some("stop") || text.is_empty() {
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
        // Only a stop's text is carried, so "asks you" can't appear for a cut-off prompt.
        assert_eq!(why("idle", None, "prompt", Some("can you check X?")), "");
        assert_eq!(why("idle", None, "waiting", Some("May I?")), "");
        assert_eq!(why("ended", None, "prompt", Some("hello")), "");
        assert!(asks(&why("idle", None, "stop", Some("Want me to go on?"))));
        assert_eq!(why("ended", None, "stop", Some("Sketched")), "Sketched");
    }
}
