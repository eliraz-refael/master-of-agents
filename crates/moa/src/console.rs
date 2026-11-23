//! `moa console`: talk to moa's own agent from the command line. `prompt` says something and
//! prints the answer, `cancel` stops a turn under way, `feed` prints the conversation so far.

use std::process::ExitCode;

use clap::{Args, Subcommand};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_core::op::Op;
use moa_core::publish::{FeedRow, MoaRow, Topic};
use serde_json::Value;

use crate::ctl::{BrainArgs, finish, running, send_routed};

#[derive(Args)]
pub struct ConsoleArgs {
    #[command(subcommand)]
    pub(crate) verb: Verb,
}

#[derive(Subcommand)]
pub(crate) enum Verb {
    /// Say something to moa and print its answer once its turn ends.
    Prompt {
        #[command(flatten)]
        brain: BrainArgs,
        /// What to say; the words are joined with spaces.
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Stop moa's turn under way.
    Cancel {
        #[command(flatten)]
        brain: BrainArgs,
    },
    /// Print the last lines of the conversation, and whether moa is answering.
    Feed {
        #[command(flatten)]
        brain: BrainArgs,
        /// How many lines to show.
        #[arg(short = 'n', default_value_t = 20)]
        lines: usize,
    },
}

pub fn console(args: &ConsoleArgs) -> ExitCode {
    finish(match &args.verb {
        Verb::Prompt { brain, text } => prompt(brain, text.join(" ")),
        Verb::Cancel { brain } => cancel(brain),
        Verb::Feed { brain, lines } => feed(brain, *lines),
    })
}

fn prompt(brain: &BrainArgs, text: String) -> Result<(), String> {
    let done = send_routed(
        brain.brain.as_deref(),
        Op::ConsolePrompt { text, wait: true },
    )?;
    println!("{}", done.text);
    Ok(())
}

fn cancel(brain: &BrainArgs) -> Result<(), String> {
    let done = send_routed(
        brain.brain.as_deref(),
        Op::ConsoleCancel(Default::default()),
    )?;
    println!("{}", done.text);
    Ok(())
}

fn feed(brain: &BrainArgs, n: usize) -> Result<(), String> {
    running(brain, false, |_, mut client| async move {
        let snap = client
            .subscribe(&[Topic::Feed])
            .await
            .map_err(|e| e.to_string())?;
        let values = snap.topics.get(&Topic::Feed).map_or(&[][..], |v| &v[..]);
        let mut rows = Vec::new();
        let mut state = None;
        for v in values {
            if v.get("key").and_then(Value::as_str) == Some("state") {
                state = serde_json::from_value::<MoaRow>(v.clone()).ok();
            } else if let Ok(row) = serde_json::from_value::<FeedRow>(v.clone()) {
                rows.push(row);
            }
        }
        rows.sort_by_key(|r| r.id);
        let skip = rows.len().saturating_sub(n);
        for line in render(&rows[skip..], &TimeZone::system()) {
            println!("{line}");
        }
        if let Some(s) = state {
            println!("{}", phase_line(&s));
        }
        Ok(())
    })
}

fn phase_line(s: &MoaRow) -> String {
    match &s.why {
        Some(why) => format!("moa is {}: {why}", s.phase),
        None => format!("moa is {}", s.phase),
    }
}

/// One line per row, oldest first, as `HH:MM` (in `tz`) and what was said. Multi-line text
/// shows its first line and a `…`.
pub fn render(rows: &[FeedRow], tz: &TimeZone) -> Vec<String> {
    rows.iter()
        .map(|r| {
            let at = Timestamp::from_millisecond(r.at)
                .map(|t| tz.to_datetime(t).strftime("%H:%M").to_string())
                .unwrap_or_else(|_| "--:--".into());
            let text = r.body.get("text").and_then(Value::as_str).unwrap_or("");
            match r.kind.as_str() {
                "you" => format!("{at} you  {}", first_line(text)),
                "moa" => format!("{at} moa  {}", first_line(text)),
                "thought" => format!("{at} ~ {}", first_line(text)),
                "tool" => {
                    let title = r.body.get("title").and_then(Value::as_str).unwrap_or("");
                    let state = r.state.as_deref().unwrap_or("pending");
                    format!("{at} ⚙ {title} · {state}")
                }
                "plan" => {
                    let n = r.body.get("entries").and_then(Value::as_array);
                    format!("{at} plan, {} steps", n.map_or(0, Vec::len))
                }
                "error" => format!("{at} ✗ {}", first_line(text)),
                _ => format!("{at} · {}", first_line(text)),
            }
        })
        .collect()
}

fn first_line(text: &str) -> String {
    let mut lines = text.trim().lines();
    let first = lines.next().unwrap_or("");
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_string()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(id: i64, kind: &str, body: Value, state: Option<&str>) -> FeedRow {
        FeedRow {
            id,
            thread: "t1".into(),
            at: 1_700_000_000_000 + id * 60_000,
            kind: kind.into(),
            message_id: None,
            automatic: false,
            body,
            state: state.map(Into::into),
        }
    }

    /// A prompt is routed as a write (starts moa, refuses an unlisted account); the brain pick
    /// itself needs a config and folder, so it is covered by `pick_brain`'s own tests.
    #[test]
    fn a_prompt_is_a_write() {
        assert!(
            Op::ConsolePrompt {
                text: "hi".into(),
                wait: true
            }
            .writes()
        );
    }

    #[test]
    fn render_says_each_kind_on_one_line() {
        let tz = TimeZone::UTC; // 1_700_000_000 s is 22:13:20 UTC
        let rows = [
            row(1, "you", json!({"text": "hello"}), None),
            row(2, "moa", json!({"text": "first\nsecond"}), None),
            row(
                3,
                "tool",
                json!({"title": "Read Lantern"}),
                Some("completed"),
            ),
            row(4, "system", json!({"text": "new thread"}), None),
            row(5, "error", json!({"text": "it broke"}), None),
        ];
        assert_eq!(
            render(&rows, &tz),
            [
                "22:14 you  hello",
                "22:15 moa  first …",
                "22:16 ⚙ Read Lantern · completed",
                "22:17 · new thread",
                "22:18 ✗ it broke",
            ]
        );
    }
}
