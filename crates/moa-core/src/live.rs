//! The display-state model: what a session looks like in the console, computed from its row.
//! One implementation, shared by every crate. The text helpers follow the spike's
//! `picture.py` (Python `strip()` and `splitlines()` rules).

use std::cmp::Reverse;

use serde::Serialize;

use crate::UtcMs;
use crate::model::{Activity, Lifecycle, Location, Session};

/// How a session shows. The declared order is the urgency order (most urgent first).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayState {
    Waiting,
    Question,
    Failed,
    Done,
    Working,
    Idle,
    Asleep,
    Ended,
    Archived,
}

impl DisplayState {
    pub const ALL: &'static [DisplayState] = &[
        DisplayState::Waiting,
        DisplayState::Question,
        DisplayState::Failed,
        DisplayState::Done,
        DisplayState::Working,
        DisplayState::Idle,
        DisplayState::Asleep,
        DisplayState::Ended,
        DisplayState::Archived,
    ];

    /// The lowercase name: what `@moa_state` holds.
    pub fn word(self) -> &'static str {
        match self {
            DisplayState::Waiting => "waiting",
            DisplayState::Question => "question",
            DisplayState::Failed => "failed",
            DisplayState::Done => "done",
            DisplayState::Working => "working",
            DisplayState::Idle => "idle",
            DisplayState::Asleep => "asleep",
            DisplayState::Ended => "ended",
            DisplayState::Archived => "archived",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            DisplayState::Waiting => "⚠",
            DisplayState::Question => "?",
            DisplayState::Failed => "✗",
            DisplayState::Done => "✓",
            DisplayState::Working => "◐",
            DisplayState::Idle => "●",
            DisplayState::Asleep => "💤",
            DisplayState::Ended => "○",
            DisplayState::Archived => "",
        }
    }
}

/// One line of context shown beside the state.
pub type Why = String;

/// Keeps long text readable: over 2400 characters, the first 600, a `[…]` line and the last
/// 1800. It counts characters, not bytes.
pub fn cut_middle(text: &str) -> String {
    const HEAD: usize = 600;
    const TAIL: usize = 1800;
    let total = text.chars().count();
    if total <= HEAD + TAIL {
        return text.to_string();
    }
    let head: String = text.chars().take(HEAD).collect();
    let tail: String = text.chars().skip(total - TAIL).collect();
    format!("{head}\n[…]\n{tail}")
}

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

/// Python's `str.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// The last line of `text`, as Python's `splitlines()[-1:]` gives it (empty for no text).
pub fn last_line(text: &str) -> &str {
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
pub fn is_idle_ping(text: Option<&str>) -> bool {
    text.is_some_and(|t| t.to_ascii_lowercase().contains("waiting for your input"))
}

/// `asks()`: did a reply end by asking something?
pub fn asks(text: &str) -> bool {
    last_line(text)
        .trim_end_matches([' ', '*', '_', '`', ')', '"', '\''])
        .ends_with('?')
}

/// A session that ended and was put to sleep (resumable), not archived.
pub fn is_asleep(s: &Session) -> bool {
    s.lifecycle == Lifecycle::Ended && s.asleep_at.is_some() && s.archived_at.is_none()
}

fn one_line(why: &str) -> String {
    why.replace('\n', " ")
}

fn or_else(s: String, fallback: &str) -> String {
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// Computes how a session shows and the line of context beside it. `now` is unused until
/// slice 4 adds "quiet".
///
/// A non-archived `elsewhere` session shows as `Ended` with why `elsewhere` (slice 8 owns a
/// fuller state). Ended and asleep sessions keep their own state, so `Asleep` stays exactly
/// [`is_asleep`].
pub fn display_state(s: &Session, _now: UtcMs) -> (DisplayState, Why) {
    let stored = s.activity_why.as_deref().unwrap_or("");
    let (state, why) = if s.archived_at.is_some() {
        (DisplayState::Archived, String::new())
    } else if s.lifecycle == Lifecycle::Ended {
        if s.asleep_at.is_some() {
            (DisplayState::Asleep, String::new())
        } else {
            (DisplayState::Ended, String::new())
        }
    } else if s.location == Location::Elsewhere {
        (DisplayState::Ended, "elsewhere".to_string())
    } else {
        match s.activity {
            Activity::NeedsPermission => (
                DisplayState::Waiting,
                or_else(one_line(stored), "needs you"),
            ),
            Activity::NeedsAnswer => (
                DisplayState::Question,
                or_else(one_line(stored), "asks you"),
            ),
            Activity::Failed => (DisplayState::Failed, one_line(stored)),
            Activity::Working => (DisplayState::Working, one_line(stored)),
            Activity::Idle => {
                if asks(stored) {
                    (
                        DisplayState::Question,
                        last_line(stored).trim_matches([' ', '*', '_']).to_string(),
                    )
                } else {
                    let line = last_line(stored)
                        .trim_matches([' ', '*', '_', '#'])
                        .to_string();
                    let unseen = matches!(
                        (s.last_turn_end_at, s.seen_at),
                        (Some(end), Some(seen)) if end > seen
                    );
                    if unseen {
                        (DisplayState::Done, line)
                    } else {
                        (DisplayState::Idle, line)
                    }
                }
            }
        }
    };
    if why.starts_with('<') {
        (state, String::new())
    } else {
        (state, why)
    }
}

/// Sort order for the list: state (urgency), then newest activity first (`None` last).
pub fn sort_key(s: &Session, state: DisplayState) -> (DisplayState, Reverse<i64>) {
    (state, Reverse(s.activity_at.map_or(i64::MIN, |t| t.0)))
}

/// The state word the spike's output shows, and the export's mapping.
pub fn spike_state(s: &Session) -> &'static str {
    if s.archived_at.is_some() {
        "archived"
    } else if s.location == Location::Elsewhere {
        "elsewhere"
    } else if s.lifecycle == Lifecycle::Ended {
        "ended"
    } else {
        match s.activity {
            Activity::Working => "working",
            Activity::NeedsPermission | Activity::NeedsAnswer => "waiting",
            Activity::Idle | Activity::Failed => "idle",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, SessionId};
    use proptest::prelude::*;

    #[test]
    fn cut_middle_keeps_head_and_tail_by_characters() {
        let at_limit = "é".repeat(2400);
        assert_eq!(cut_middle(&at_limit), at_limit);
        assert_eq!(
            cut_middle(&"é".repeat(2401)),
            format!("{}\n[…]\n{}", "é".repeat(600), "é".repeat(1800))
        );
    }

    fn session() -> Session {
        Session {
            id: SessionId(uuid::Uuid::from_u128(1)),
            agent_kind: AgentKind::Claude,
            account: None,
            cwd: None,
            start_cwd: None,
            project_id: None,
            title: None,
            title_locked: false,
            group_path: None,
            lifecycle: Lifecycle::Live,
            activity: Activity::Idle,
            activity_at: None,
            activity_why: None,
            last_prompt: None,
            last_turn_end_at: None,
            seen_at: None,
            asleep_at: None,
            archived_at: None,
            location: Location::Here,
            resume_failed: false,
            created_at: UtcMs(0),
            last_seen_at: None,
        }
    }

    fn live(activity: Activity, why: &str) -> Session {
        Session {
            activity,
            activity_why: Some(why.to_string()),
            ..session()
        }
    }

    fn shown(s: &Session) -> (DisplayState, String) {
        display_state(s, UtcMs(100))
    }

    fn row(state: DisplayState, why: &str) -> (DisplayState, String) {
        (state, why.to_string())
    }

    #[test]
    fn the_table() {
        use DisplayState::*;
        let archived = Session {
            archived_at: Some(UtcMs(1)),
            ..session()
        };
        assert_eq!(shown(&archived), row(Archived, ""));
        let asleep = Session {
            lifecycle: Lifecycle::Ended,
            asleep_at: Some(UtcMs(1)),
            ..session()
        };
        assert_eq!(shown(&asleep), row(Asleep, ""));
        let ended = Session {
            lifecycle: Lifecycle::Ended,
            ..session()
        };
        assert_eq!(shown(&ended), row(Ended, ""));
        assert_eq!(
            shown(&live(Activity::NeedsPermission, "May I?\nplease")),
            row(Waiting, "May I? please")
        );
        assert_eq!(
            shown(&live(Activity::NeedsPermission, "")),
            row(Waiting, "needs you")
        );
        assert_eq!(
            shown(&live(Activity::NeedsAnswer, "Which one?")),
            row(Question, "Which one?")
        );
        assert_eq!(
            shown(&live(Activity::NeedsAnswer, "")),
            row(Question, "asks you")
        );
        assert_eq!(
            shown(&live(Activity::Failed, "boom\nbang")),
            row(Failed, "boom bang")
        );
        assert_eq!(
            shown(&live(Activity::Working, "can you fix it?")),
            row(Working, "can you fix it?")
        );
        assert_eq!(
            shown(&live(Activity::Idle, "Ready to merge?**")),
            row(Question, "Ready to merge?")
        );
        let done = Session {
            last_turn_end_at: Some(UtcMs(10)),
            seen_at: Some(UtcMs(5)),
            ..live(Activity::Idle, "intro\n## Shipped_ *")
        };
        assert_eq!(shown(&done), row(Done, "Shipped"));
        assert_eq!(
            shown(&live(Activity::Idle, "all fine\n**ok**")),
            row(Idle, "ok")
        );
        assert_eq!(shown(&live(Activity::Idle, "")), row(Idle, ""));
    }

    #[test]
    fn archived_beats_everything() {
        let s = Session {
            archived_at: Some(UtcMs(1)),
            activity: Activity::NeedsPermission,
            activity_why: Some("May I?".into()),
            ..session()
        };
        assert_eq!(shown(&s), row(DisplayState::Archived, ""));
        let e = Session {
            lifecycle: Lifecycle::Ended,
            asleep_at: Some(UtcMs(1)),
            location: Location::Elsewhere,
            ..s
        };
        assert_eq!(shown(&e), row(DisplayState::Archived, ""));
        assert!(!is_asleep(&e));
    }

    #[test]
    fn an_elsewhere_session_shows_as_ended_with_why_elsewhere() {
        let s = Session {
            location: Location::Elsewhere,
            activity: Activity::NeedsPermission,
            activity_why: Some("May I?".into()),
            ..session()
        };
        assert_eq!(shown(&s), row(DisplayState::Ended, "elsewhere"));
        assert_eq!(spike_state(&s), "elsewhere");
        let archived = Session {
            archived_at: Some(UtcMs(1)),
            ..s
        };
        assert_eq!(shown(&archived), row(DisplayState::Archived, ""));
    }

    #[test]
    fn unseen_turn_is_done_and_no_seen_is_idle() {
        let s = Session {
            last_turn_end_at: Some(UtcMs(10)),
            seen_at: Some(UtcMs(5)),
            ..live(Activity::Idle, "done.")
        };
        assert_eq!(shown(&s), row(DisplayState::Done, "done."));
        let no_seen = Session {
            seen_at: None,
            ..s.clone()
        };
        assert_eq!(shown(&no_seen), row(DisplayState::Idle, "done."));
        let seen = Session {
            seen_at: Some(UtcMs(10)),
            ..s
        };
        assert_eq!(shown(&seen), row(DisplayState::Idle, "done."));
    }

    #[test]
    fn markup_lines_are_blanked() {
        assert_eq!(
            shown(&live(Activity::Idle, "<command-name>/clear")),
            row(DisplayState::Idle, "")
        );
        assert_eq!(
            shown(&live(Activity::Working, "<x>")),
            row(DisplayState::Working, "")
        );
        assert_eq!(
            shown(&live(Activity::NeedsPermission, "<x>")),
            row(DisplayState::Waiting, "")
        );
    }

    #[test]
    fn sorts_by_state_then_recency() {
        let at = |t: Option<i64>| Session {
            activity_at: t.map(UtcMs),
            ..session()
        };
        let mut v = [
            (at(Some(5)), DisplayState::Idle, "idle-5"),
            (at(None), DisplayState::Waiting, "wait-none"),
            (at(Some(9)), DisplayState::Idle, "idle-9"),
            (at(Some(1)), DisplayState::Waiting, "wait-1"),
            (at(Some(2)), DisplayState::Archived, "arch"),
            (at(None), DisplayState::Idle, "idle-none"),
        ];
        v.sort_by_key(|(s, st, _)| sort_key(s, *st));
        let names: Vec<_> = v.iter().map(|x| x.2).collect();
        assert_eq!(
            names,
            [
                "wait-1",
                "wait-none",
                "idle-9",
                "idle-5",
                "idle-none",
                "arch"
            ]
        );
    }

    #[test]
    fn spike_state_table() {
        let ended = Session {
            lifecycle: Lifecycle::Ended,
            ..session()
        };
        assert_eq!(spike_state(&ended), "ended");
        let archived = Session {
            archived_at: Some(UtcMs(1)),
            ..ended.clone()
        };
        assert_eq!(spike_state(&archived), "archived");
        let elsewhere = Session {
            location: Location::Elsewhere,
            ..ended
        };
        assert_eq!(spike_state(&elsewhere), "elsewhere");
        for (a, word) in [
            (Activity::Working, "working"),
            (Activity::NeedsPermission, "waiting"),
            (Activity::NeedsAnswer, "waiting"),
            (Activity::Idle, "idle"),
            (Activity::Failed, "idle"),
        ] {
            assert_eq!(spike_state(&live(a, "")), word);
        }
    }

    #[test]
    fn glyphs_and_order() {
        let glyphs: Vec<_> = DisplayState::ALL.iter().map(|s| s.glyph()).collect();
        assert_eq!(glyphs, ["⚠", "?", "✗", "✓", "◐", "●", "💤", "○", ""]);
        assert!(DisplayState::ALL.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn states_serialize_as_lowercase_words() {
        insta::assert_json_snapshot!(DisplayState::ALL);
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

    fn opt(on: bool, v: i64) -> Option<UtcMs> {
        on.then_some(UtcMs(v))
    }

    proptest! {
        #[test]
        fn every_combination_has_one_consistent_state(
            ended in any::<bool>(),
            activity in prop::sample::select(Activity::ALL),
            elsewhere in any::<bool>(),
            archived in any::<bool>(),
            asleep in any::<bool>(),
            seen in any::<bool>(),
            turn in any::<bool>(),
            why in prop::sample::select(vec!["", "ok", "why?", "<x>"]),
        ) {
            let s = Session {
                lifecycle: if ended { Lifecycle::Ended } else { Lifecycle::Live },
                activity,
                location: if elsewhere { Location::Elsewhere } else { Location::Here },
                archived_at: opt(archived, 1),
                asleep_at: opt(asleep, 2),
                seen_at: opt(seen, 5),
                last_turn_end_at: opt(turn, 10),
                activity_why: Some(why.to_string()),
                ..session()
            };
            let (state, text) = display_state(&s, UtcMs(100));
            prop_assert_eq!(state == DisplayState::Archived, archived);
            if state <= DisplayState::Idle {
                prop_assert!(!ended && !archived && !elsewhere);
            }
            prop_assert_eq!(state == DisplayState::Asleep, is_asleep(&s));
            prop_assert_eq!(
                state == DisplayState::Waiting,
                !ended && !archived && !elsewhere && activity == Activity::NeedsPermission
            );
            prop_assert!(!text.starts_with('<'));
        }
    }
}
