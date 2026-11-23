//! Slice 6b end to end: the console panel of the cockpit on a TempBrain, with `stub-acp` as
//! moa's agent. Keys go to the console's pane as tmux `send-keys`; what moa did is read back
//! from the pane's screen and from the store. Every wait is for what the screen or the store
//! shows, never for a fixed time.

mod common;

use moa_platform::ensure_private_dir;
use moa_testkit::{StubStep, TempBrain, Terminal, stub_moa_toml, wait_for, write_stub_script};

use common::*;

/// A root whose moa runs the stub with `steps`, and `moa` on a terminal with the cockpit
/// built.
struct Console {
    // Dropped before `tb`, as a test's own terminal is.
    term: Terminal,
    tb: TempBrain,
    /// The console's pane.
    pane: String,
}

fn console(steps: &[StubStep]) -> Console {
    let tb = TempBrain::new(Some(&stub_moa_toml()));
    let agent = tb.paths().agent_dir(B);
    ensure_private_dir(&agent).expect("the agent folder");
    write_stub_script(&agent, steps);
    let term = cockpit_up(&tb);
    let pane = cockpit_panes(&tb)
        .into_iter()
        .find(|p| p.tag == "console")
        .expect("the console's pane")
        .id;
    let c = Console { term, tb, pane };
    // Keys sent before the panel draws its input line could be lost.
    c.shows(&["ask moa, or / for commands"]);
    c
}

impl Console {
    /// Waits until the console shows every one of `texts`.
    #[track_caller]
    fn shows(&self, texts: &[&str]) {
        shows(&self.tb, &self.pane, texts);
    }

    /// tmux `send-keys` to the console, each of `keys` a key name or text.
    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", &self.pane];
        args.extend_from_slice(keys);
        tmux(&self.tb, &args);
    }

    /// `text` typed as is, then Enter.
    fn say(&self, text: &str) {
        tmux(&self.tb, &["send-keys", "-t", &self.pane, "-l", text]);
        self.keys(&["Enter"]);
    }

    /// The feed's kinds and texts, oldest first, as `kind text`.
    fn feed(&self) -> Vec<String> {
        let o = run(
            &self.tb,
            &[
                "sql",
                "SELECT kind || ' ' || json_extract(body, '$.text') FROM feed ORDER BY id",
            ],
            &[],
        );
        assert!(o.status.success(), "{}{}", out(&o), err(&o));
        out(&o).lines().skip(1).map(str::to_string).collect()
    }

    /// Waits until a feed row is `row`.
    #[track_caller]
    fn feed_has(&self, row: &str) {
        let seen = wait_for(SHOWS, || self.feed().iter().any(|r| r == row).then_some(()));
        assert!(
            seen.is_some(),
            "the feed never had {row:?}: {:?}\n{}",
            self.feed(),
            what_happened(&self.tb, &self.term)
        );
    }
}

fn say(text: &str, id: &str) -> StubStep {
    StubStep::Say {
        text: text.into(),
        id: Some(id.into()),
    }
}

fn end() -> StubStep {
    StubStep::End { stop: None }
}

#[test]
fn typing_in_the_console_reaches_moa() {
    let c = console(&[say("hello from the stub", "m1"), end()]);

    c.say("hello");

    c.shows(&["› hello", "◆ moa", "hello from the stub", "● ready"]);
}

#[test]
fn a_button_runs_its_command() {
    let c = console(&[say("try `/ls`", "m1"), end()]);
    c.say("what can I do");
    c.shows(&["try [/ls]", "● ready"]);
    let ls = run(&c.tb, &["ls"], &[]);
    assert!(ls.status.success(), "{}{}", out(&ls), err(&ls));

    // The card's own title line, which the button's `[/ls]` isn't.
    let card = "▸ /ls";
    assert!(!screen_of(&c.tb, &c.pane).contains(card));

    // The first Tab focuses the newest text button, which is `/ls`.
    c.keys(&["Tab", "Enter"]);

    // The card carries `moactl ls`'s own text.
    let want = out(&ls);
    let first = want.lines().next().expect("ls prints a header");
    c.shows(&[card, first.trim_end()]);
}

#[test]
fn a_typed_command_runs() {
    let c = console(&[]);

    c.say("/ls");

    let ls = run(&c.tb, &["ls"], &[]);
    assert!(ls.status.success(), "{}{}", out(&ls), err(&ls));
    let want = out(&ls);
    let first = want.lines().next().expect("ls prints a header");
    c.shows(&["▸ /ls", first.trim_end()]);
}

#[test]
fn esc_cancels_a_hang() {
    let c = console(&[StubStep::Hang]);
    c.say("take your time");
    c.shows(&["thinking… Esc cancels"]);

    c.keys(&["Escape"]);

    c.shows(&["● ready", "cancelled"]);
    assert!(!screen_of(&c.tb, &c.pane).contains("thinking…"));
}

#[test]
fn closing_the_console_leaves_the_turn() {
    let c = console(&[
        StubStep::Wait {
            file: "gate".into(),
        },
        say("still here", "m1"),
        end(),
    ]);
    c.say("are you there");
    // The turn is open, and the stub only ends it once the gate file exists, so it is
    // in `Wait` until the gate opens below.
    c.feed_has("you are you there");
    c.shows(&["thinking… Esc cancels"]);

    tmux(&c.tb, &["kill-pane", "-t", &c.pane]);
    assert!(!cockpit_panes(&c.tb).iter().any(|p| p.id == c.pane));
    std::fs::write(c.tb.paths().agent_dir(B).join("gate"), "").expect("open the gate");

    c.feed_has("moa still here");
}
