//! Slice 7 end to end: asks between two sessions `moa open` started, and to moa, through the
//! real `moactl` run from inside a session's tab, as Claude runs it from a Bash tool call.
//! Each session's agent is `fake-claude` (its `Run` step runs `moactl` with the tab's
//! environment and shows what it printed in the tab); moa's agent is `stub-acp`; the clock is
//! the real one. Every wait is for what a tab, the console or the store shows.

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use moa_platform::ensure_private_dir;
use moa_testkit::{
    FakeStep, StubStep, TempBrain, Terminal, stub_moa_toml, wait_for, write_stub_script,
};
use serde_json::{Value, json};

use common::*;

/// Harbor's answer: more than one line, as a turn's last message often is.
const RENEWED: &str = "Renewed through May.\n\nThe invoice is in ~/private/Harbor.";
const DOMAIN: &str = "Is the domain renewed?";
const GREEN: &str = "Is the Kite build green?";
/// moa's answer to the user's hello.
const HELLO: &str = "hello there";
/// moa's answer to Harbor.
const MOA_SAYS: &str = "Green: 212 tests pass.";

/// The terminal: tall enough that the console holds a hello, an ask's card and moa's answer
/// at once, so none scrolls off while a test reads them.
const SIZE: (u16, u16) = (120, 50);

/// A root whose moa runs the stub with `steps`, opening sessions with fake-claude, and `moa`
/// on a terminal with the cockpit built.
struct Cockpit {
    // Dropped before `tb`, as a test's own terminal is.
    term: Terminal,
    tb: TempBrain,
    /// The console's pane.
    console: String,
}

fn cockpit(moa: &[StubStep]) -> Cockpit {
    let base = stub_moa_toml();
    let toml = base.replacen(
        "moa_account = \"acct-a\"\n",
        "moa_account = \"acct-a\"\nopen_account = \"acct-a\"\n",
        1,
    );
    assert_ne!(toml, base, "open_account isn't set: {base}");
    let tb = TempBrain::new(Some(&toml));
    // moa opened the day already: attaching opens nothing here (D2).
    tb.opened_today();
    let agent = tb.paths().agent_dir(B);
    ensure_private_dir(&agent).expect("the agent folder");
    write_stub_script(&agent, moa);
    let term = cockpit_up_sized(&tb, SIZE);
    let console = cockpit_panes(&tb)
        .into_iter()
        .find(|p| p.tag == "console")
        .expect("the console's pane")
        .id;
    shows(&tb, &console, &["ask moa, or / for commands"]);
    Cockpit { term, tb, console }
}

/// A session `moa open` started, its tab and its folder.
struct Agent {
    id: String,
    pane: String,
    dir: std::path::PathBuf,
}

impl Cockpit {
    /// `moactl open --bg <root>/work/<name> --title <title>`, its agent playing `steps` after
    /// its start; once moa has that start, so whatever moa types next lands in its prompt box.
    #[track_caller]
    fn open(&self, name: &str, title: &str, steps: &[FakeStep]) -> Agent {
        let mut all = vec![start_hook()];
        all.extend_from_slice(steps);
        let opened = open_session_with(&self.tb, &self.term, name, title, None, &all);
        let started = wait_for(SHOWS, || {
            (events(&self.tb, &opened.id, "session.started") > 0).then_some(())
        });
        assert!(
            started.is_some(),
            "{title}'s start never came\n{}",
            what_happened(&self.tb, &self.term)
        );
        let pane = session_pane(&self.tb, &self.term, &opened.id);
        Agent {
            id: opened.id,
            pane,
            dir: opened.dir,
        }
    }

    /// The user says hello in the console and moa answers [`HELLO`] (the stub's first turn):
    /// moa's agent is up, so nothing an ask does later has to start it.
    #[track_caller]
    fn moa_up(&self) {
        tmux(&self.tb, &["send-keys", "-t", &self.console, "-l", "hello"]);
        tmux(&self.tb, &["send-keys", "-t", &self.console, "Enter"]);
        self.shows(&self.console, &[HELLO, "● ready"]);
    }

    /// The daemon's launches.
    fn launches(&self) -> u64 {
        status_json(&self.tb)["launches"].as_u64().unwrap()
    }

    /// The rows of `query`, as `moa sql` prints them, less its header.
    fn sql(&self, query: &str) -> Vec<String> {
        let o = run(&self.tb, &["sql", query], &[]);
        assert!(o.status.success(), "{}{}", out(&o), err(&o));
        out(&o).lines().skip(1).map(str::to_string).collect()
    }

    /// Waits until `pane` shows each of `texts`.
    #[track_caller]
    fn shows(&self, pane: &str, texts: &[&str]) {
        shows(&self.tb, pane, texts);
    }

    /// Waits until a session's tab has printed each of `texts`: its screen and the lines
    /// scrolled above it, each line whole however the tab's width wrapped it (`-J`).
    #[track_caller]
    fn printed(&self, pane: &str, texts: &[&str]) {
        let all = || {
            tmux(
                &self.tb,
                &["capture-pane", "-p", "-J", "-S", "-", "-t", pane],
            )
        };
        let seen = wait_for(SHOWS, || {
            let printed = all();
            texts.iter().all(|t| printed.contains(t)).then_some(())
        });
        assert!(
            seen.is_some(),
            "{pane} never printed {texts:?}:\n{}\n{}",
            all(),
            what_happened(&self.tb, &self.term)
        );
    }

    /// The feed's id of the newest row of `kind` whose body's text is `text`.
    fn feed_id(&self, kind: &str, text: &str) -> i64 {
        let ids = self.sql(&format!(
            "SELECT max(id) FROM feed WHERE kind = '{kind}' \
             AND json_extract(body, '$.text') = '{}'",
            text.replace('\'', "''")
        ));
        ids.first()
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| panic!("no {kind} row {text:?}: {ids:?}"))
    }
}

/// `text` with each age (`5m ago`, `2h ago`) as `now`, as `peers` and `asks` print an age
/// under a minute: a slow run whose age ticked over still reads the same.
fn ages_as_now(text: &str) -> String {
    let is_span = |w: &str| {
        w.len() > 1
            && w.ends_with(['m', 'h', 'd'])
            && w[..w.len() - 1].bytes().all(|b| b.is_ascii_digit())
    };
    text.split('\n')
        .map(|line| {
            let words: Vec<&str> = line.split(' ').collect();
            let mut out = Vec::new();
            let mut i = 0;
            while i < words.len() {
                if is_span(words[i]) && words.get(i + 1) == Some(&"ago") {
                    out.push("now");
                    i += 2;
                } else {
                    out.push(words[i]);
                    i += 1;
                }
            }
            out.join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `moactl args…`, run from the session's tab ([`FakeStep::Run`]). The tab's `PATH` is the
/// user's, which has no `moactl` here, so it is named by its full path.
fn moactl_step(tb: &TempBrain, args: &[&str]) -> FakeStep {
    let mut argv = vec![moactl(tb).display().to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    FakeStep::Run { argv }
}

/// Waits for the gate `file` in the agent's folder ([`FakeStep::Wait`]).
fn gate(file: &str) -> FakeStep {
    FakeStep::Wait { file: file.into() }
}

/// Opens the gate `file` in `dir`.
fn open_gate(dir: &Path, file: &str) {
    std::fs::write(dir.join(file), "").expect("open the gate");
}

/// What `moactl` printed and how it exited, for each `Run` of the agent in `dir`, in order:
/// whole lines only, so a line still being written isn't read.
fn runs(dir: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(dir.join(".fake-claude.run")).unwrap_or_default();
    text.split_inclusive('\n')
        .filter_map(|l| l.strip_suffix('\n'))
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l:?}: {e}")))
        .collect()
}

/// The `n`th (from 0) `Run` of the agent in `dir`, once it is done.
#[track_caller]
fn ran(dir: &Path, n: usize) -> Value {
    let done = wait_for(SHOWS, || runs(dir).get(n).cloned());
    done.unwrap_or_else(|| panic!("run {n} in {} never ended: {:?}", dir.display(), runs(dir)))
}

fn say(text: &str, id: &str) -> StubStep {
    StubStep::Say {
        text: text.into(),
        id: Some(id.into()),
    }
}

/// The roadmap's "done when": Kite asks Harbor and prints Harbor's answer; Harbor asks moa,
/// the console shows the ask's card, then moa's answer, which Harbor prints. None of it
/// launches anything.
#[test]
fn two_sessions_and_moa_exchange_asks() {
    // The stub plays a turn per prompt: the user's hello, then Harbor's ask.
    let c = cockpit(&[
        say(HELLO, "m0"),
        StubStep::End { stop: None },
        say(MOA_SAYS, "m1"),
        StubStep::End { stop: None },
    ]);
    let harbor = c.open(
        "harbor",
        "Harbor",
        &[
            FakeStep::Answer {
                reply: RENEWED.into(),
            },
            gate("ask-moa"),
            moactl_step(&c.tb, &["ask", "moa", GREEN, "--timeout", "30"]),
        ],
    );
    let kite = c.open(
        "kite",
        "Kite",
        &[
            gate("ask"),
            moactl_step(&c.tb, &["ask", "Harbor", DOMAIN, "--timeout", "30"]),
        ],
    );
    c.moa_up();
    let before = c.launches();

    // Kite asks Harbor: Harbor's prompt box gets the framed ask, and Kite prints the answer.
    open_gate(&kite.dir, "ask");
    c.printed(
        &kite.pane,
        &[
            "Renewed through May.",
            "The invoice is in ~/private/Harbor.",
            "exit 0",
        ],
    );
    let asked = ran(&kite.dir, 0);
    assert_eq!(asked["code"], 0, "{asked}");
    assert_eq!(asked["stdout"], format!("{RENEWED}\n"), "{asked}");
    assert_eq!(
        c.sql("SELECT id, status FROM asks"),
        ["1\tanswered"],
        "{asked}"
    );
    // Harbor's prompt was the framed ask, as moa typed it (A13); its line breaks as `⏎`, so
    // `moa sql` prints it on one line.
    assert_eq!(
        c.sql(&format!(
            "SELECT replace(last_prompt, char(10), '⏎') AS p FROM sessions WHERE id = '{}'",
            harbor.id
        )),
        [format!(
            "[moa · message from Kite · ask #1] {DOMAIN}⏎⏎\
             (Your reply this turn goes back to Kite as the answer.)"
        )]
    );

    // Harbor asks moa: the console shows the ask's card, then moa's answer; Harbor prints it.
    open_gate(&harbor.dir, "ask-moa");
    let card = format!("Harbor · #2: {GREEN}");
    let row = "· moa answers Harbor's ask #2";
    c.shows(&c.console, &["▸ an ask for moa", &card, row, MOA_SAYS]);
    let screen = screen_of(&c.tb, &c.console);
    let at = |text: &str| screen.find(text).unwrap_or_else(|| panic!("{screen}"));
    assert!(at(&card) < at(row), "{screen}");
    assert!(at(row) < at(MOA_SAYS), "{screen}");
    // The same order in the feed: the card, the ask's prompt, then moa's answer.
    let prompt = format!(
        "(moa · Harbor asks you, ask #2, not typed by the user)\n{GREEN}\n\n\
         Your reply this turn goes back to Harbor as the answer."
    );
    let (card_at, prompt_at) = (c.feed_id("card", &card), c.feed_id("you", &prompt));
    assert!(card_at < prompt_at, "{card_at} {prompt_at}");
    assert!(prompt_at < c.feed_id("moa", MOA_SAYS), "{prompt_at}");
    c.printed(&harbor.pane, &[MOA_SAYS, "exit 0"]);
    let asked = ran(&harbor.dir, 0);
    assert_eq!(asked["code"], 0, "{asked}");
    assert_eq!(asked["stdout"], format!("{MOA_SAYS}\n"), "{asked}");
    assert_eq!(
        c.sql("SELECT id, status, reply FROM asks WHERE id = 2"),
        [format!("2\tanswered\t{MOA_SAYS}")]
    );
    let prompts = stub_logged(&c.tb, "session/prompt");
    let sent: Vec<&str> = prompts
        .iter()
        .filter_map(|p| p["params"]["prompt"][0]["text"].as_str())
        .collect();
    assert_eq!(sent[1..], [prompt]);

    assert_eq!(c.launches(), before);
}

/// `moactl peers` and `moactl asks` from Kite's tab, after Kite asked Harbor: A11's columns.
#[test]
fn peers_and_asks_read_well() {
    let c = cockpit(&[]);
    let harbor = c.open(
        "harbor",
        "Harbor",
        &[FakeStep::Answer {
            reply: RENEWED.into(),
        }],
    );
    let kite = c.open(
        "kite",
        "Kite",
        &[
            gate("ask"),
            moactl_step(&c.tb, &["ask", "Harbor", DOMAIN, "--timeout", "30"]),
            gate("list"),
            moactl_step(&c.tb, &["peers"]),
            moactl_step(&c.tb, &["asks"]),
            moactl_step(&c.tb, &["--json", "peers"]),
        ],
    );
    open_gate(&kite.dir, "ask");
    c.printed(&kite.pane, &["Renewed through May.", "exit 0"]);
    // A handle is the first 8 of the session's conversation (`State::handle_of`).
    let handles = c.sql(&format!(
        "SELECT substr(conversation_id, 1, 8) FROM conversations WHERE session_id = '{}'",
        harbor.id
    ));
    let [harbor_handle] = handles.as_slice() else {
        panic!("one conversation: {handles:?}");
    };

    open_gate(&kite.dir, "list");
    c.printed(
        &kite.pane,
        &[
            &format!("{harbor_handle:<8}  Harbor"),
            "#1  Kite → Harbor  answered",
            "\"label\":\"Harbor\"",
        ],
    );
    // moa's agent hasn't started: nothing asked it anything yet, so it is `off`.
    let peers = ran(&kite.dir, 1);
    assert_eq!(peers["code"], 0, "{peers}");
    assert_eq!(
        ages_as_now(peers["stdout"].as_str().unwrap_or_default()),
        format!(
            "{:<8}  {:<20} {:<9} {:<24} -\n{harbor_handle:<8}  {:<20} {:<9} {:<24} asked now\n",
            "moa", "moa", "off", "-", "Harbor", "idle", "~/work/harbor"
        ),
        "{peers}"
    );

    let asks = ran(&kite.dir, 2);
    assert_eq!(asks["code"], 0, "{asks}");
    assert_eq!(
        ages_as_now(asks["stdout"].as_str().unwrap_or_default()),
        format!("#1  Kite → Harbor  answered  now  {DOMAIN}\n"),
        "{asks}"
    );

    let json = ran(&kite.dir, 3);
    let rows: Value = serde_json::from_str(json["stdout"].as_str().unwrap()).unwrap();
    let rows = rows.as_array().unwrap_or_else(|| panic!("{json}"));
    let labels: Vec<&Value> = rows.iter().map(|r| &r["label"]).collect();
    assert_eq!(labels, [&json!("moa"), &json!("Harbor")], "{json}");
    let harbor_row = &rows[1];
    assert_eq!(harbor_row["id"], json!(harbor.id), "{json}");
    assert_eq!(harbor_row["handle"], json!(harbor_handle), "{json}");
    assert_eq!(harbor_row["state"], "idle", "{json}");
    assert_eq!(harbor_row["folder"], "~/work/harbor", "{json}");
    assert!(harbor_row["last_asked"].is_i64(), "{json}");
    let keys: BTreeSet<&str> = harbor_row
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let want = BTreeSet::from([
        "id",
        "handle",
        "label",
        "state",
        "folder",
        "last_asked",
        "agent_kind",
        "project",
        "cwd",
        "doing",
    ]);
    assert_eq!(keys, want, "{json}");
}
