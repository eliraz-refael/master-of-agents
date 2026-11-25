//! Slices 6b and 6c end to end: the console panel of the cockpit on a TempBrain, with
//! `stub-acp` as moa's agent. Keys go to the console's pane as tmux `send-keys`; what moa did
//! is read back from the pane's screen, the status line and the store. Every wait is for what
//! the screen or the store shows, never for a fixed time.

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
    console_with(&stub_moa_toml(), steps)
}

/// [`console`], with `toml` as the root's `moa.toml`.
fn console_with(toml: &str, steps: &[StubStep]) -> Console {
    let tb = TempBrain::new(Some(toml));
    // moa opened the day already: attaching opens nothing here (D2).
    tb.opened_today();
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

    /// Waits until the console shows `text`, its words in order whatever the lines it wraps
    /// over ([`names`]).
    #[track_caller]
    fn shows_wrapped(&self, text: &str) {
        let seen = wait_for(SHOWS, || {
            names(&screen_of(&self.tb, &self.pane), text).then_some(())
        });
        assert!(
            seen.is_some(),
            "{} never showed {text:?}\n{}",
            self.pane,
            tab_shows(&self.tb, &self.pane)
        );
    }

    /// Waits for the stub's answer to its permission request; its `optionId`.
    #[track_caller]
    fn outcome(&self) -> String {
        let outcome = wait_for(SHOWS, || stub_logged(&self.tb, "permission.outcome").pop());
        let outcome = outcome.unwrap_or_else(|| {
            panic!(
                "the stub's request was never answered\n{}",
                what_happened(&self.tb, &self.term)
            )
        });
        assert_eq!(outcome["params"]["outcome"], "selected", "{outcome}");
        outcome["params"]["optionId"]
            .as_str()
            .unwrap_or_else(|| panic!("{outcome}"))
            .to_string()
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

/// The request the stub makes: tidy the Harbor, once, always or not.
fn ask() -> StubStep {
    StubStep::Ask {
        title: "tidy the Harbor".into(),
        options: vec![
            ("yes".into(), "allow_once".into()),
            ("always".into(), "allow_always".into()),
            ("no".into(), "reject_once".into()),
        ],
    }
}

/// What the console shows while the stub's request waits.
const ASKS: [&str; 5] = [
    "⚠ moa asks: tidy the Harbor",
    "[y once]",
    "[a always]",
    "[n no]",
    "⚠ answer y/a/n",
];

#[test]
fn y_allows_a_permission() {
    let c = console(&[ask(), say("tidied", "m1"), end()]);
    c.say("tidy up");
    c.shows(&ASKS);

    c.keys(&["y"]);

    assert_eq!(c.outcome(), "yes");
    c.shows(&["✓ allowed once: tidy the Harbor", "tidied", "● ready"]);
    // A key answers as the card's button does, and leaves no line of its own.
    let screen = screen_of(&c.tb, &c.pane);
    assert!(!screen.contains("/permission"), "{screen}");
    assert!(!screen.contains("moa asks"), "{screen}");
}

#[test]
fn n_refuses_and_moa_goes_on() {
    let c = console(&[ask(), say("left it as it is", "m1"), end()]);
    c.say("tidy up");
    c.shows(&ASKS);

    c.keys(&["n"]);

    assert_eq!(c.outcome(), "no");
    let refused = "✗ refused: tidy the Harbor";
    c.shows(&[refused, "left it as it is", "● ready"]);
    let screen = screen_of(&c.tb, &c.pane);
    let at = |text: &str| screen.find(text).unwrap_or_else(|| panic!("{screen}"));
    assert!(at(refused) < at("left it as it is"), "{screen}");
    assert!(!screen.contains("/permission"), "{screen}");
}

#[test]
fn moa_needs_you_without_a_console() {
    let c = console(&[ask(), say("left it as it is", "m1"), end()]);
    let status = || tmux(&c.tb, &["show-options", "-gv", "@moa_status"]);
    let brain = [("MOA_BRAIN", B)];
    tmux(&c.tb, &["kill-pane", "-t", &c.pane]);
    assert!(!cockpit_panes(&c.tb).iter().any(|p| p.id == c.pane));

    let said = std::thread::scope(|s| {
        let asker = s.spawn(|| run(&c.tb, &["console", "prompt", "tidy up"], &brain));
        let needs = wait_for(SHOWS, || status().contains("moa needs you").then_some(()));
        assert!(
            needs.is_some(),
            "the status line: {}\n{}",
            status(),
            what_happened(&c.tb, &c.term)
        );

        let o = run(&c.tb, &["console", "answer", "no"], &brain);
        assert!(o.status.success(), "{}{}", out(&o), err(&o));
        assert_eq!(out(&o), "refused\n");
        let cleared = wait_for(SHOWS, || {
            (!status().contains("moa needs you")).then_some(())
        });
        assert!(cleared.is_some(), "the status line: {}", status());
        asker.join().unwrap()
    });
    assert!(said.status.success(), "{}{}", out(&said), err(&said));
    assert_eq!(out(&said), "left it as it is\n");
    assert_eq!(c.outcome(), "no");
}

#[test]
fn a_hard_hang_shows_the_watchdog_card() {
    let toml = format!(
        "{}\n[moa]\nwatchdog_seconds = 2\nwatchdog_kill_seconds = 1\n",
        stub_moa_toml()
    );
    let c = console_with(&toml, &[StubStep::HangHard]);
    c.say("take your time");

    // Each of the card's texts holds the one before, so none is missed.
    c.shows(&["▸ moa's turn was stuck"]);
    c.shows_wrapped("moa's turn had no news for 2 s, so moa cancelled it.");
    c.shows_wrapped("It still hadn't stopped 1 s later, so moa's agent was restarted");
    c.shows_wrapped("so moa's agent was restarted and today's thread reopened.");
    c.shows(&["● ready"]);
    assert_eq!(stub_logged(&c.tb, "session/resume").len(), 1);
}
