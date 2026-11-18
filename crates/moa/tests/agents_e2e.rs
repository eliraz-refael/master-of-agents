//! Adopt and restart end to end: the real `moa` binaries, a TempBrain's own terminal server,
//! and fake-claude (named `claude`, so process reads take it for Claude). An agent typed by
//! hand into a `moactl term` tab is listed untracked, adopted, restarted under moa, or left;
//! none of it, nor resume, focus, close, term or sleep, launches anything from the daemon.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use moa_testkit::{
    FakeStep, TempBrain, Terminal, fake_claude_named_claude, wait_for, write_script,
};
use serde_json::{Map, Value};

use common::*;

/// The conversation the hand-started agent runs.
const CONV: &str = "00000000-0000-4000-8000-000000000042";

/// A tab as `list-panes` shows it.
#[derive(Clone, Debug, PartialEq)]
struct Tab {
    pane: String,
    /// `@moa_session`.
    session: String,
    /// `@moa_adopted`.
    adopted: bool,
    /// `@moa_term`.
    term: bool,
    /// `@moa_label`.
    label: String,
    start: String,
}

/// A root whose `acct-a` launches fake-claude named `claude`, and `moa` on a terminal,
/// linked to its daemon.
struct Rig {
    // Dropped before `tb`, as a test's own terminal is.
    term: Terminal,
    tb: TempBrain,
}

fn rig() -> Rig {
    rig_with(|_| {})
}

/// [`rig`], with `more` run on the root before `moa` starts.
fn rig_with(more: impl FnOnce(&TempBrain)) -> Rig {
    let tb = TempBrain::new(None);
    let claude = fake_claude_named_claude(tb.root());
    moa_toml_with_launcher(&tb, &claude);
    more(&tb);
    let term = open(&tb, &[], &[]);
    came_up(&tb, &term);
    Rig { term, tb }
}

fn start(source: &str) -> FakeStep {
    FakeStep::Hook {
        event: "SessionStart".into(),
        fields: Map::from_iter([("source".to_string(), Value::String(source.into()))]),
    }
}

impl Rig {
    /// `moactl args…` to the end.
    fn moactl(&self, args: &[&str]) -> std::process::Output {
        run_exe(&self.tb, &moactl(&self.tb), args, &[("MOA_BRAIN", B)])
    }

    /// `moactl args…`, which must succeed; its stdout.
    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        let o = self.moactl(args);
        assert!(
            o.status.success(),
            "moactl {args:?}: {}{}\n{}",
            out(&o),
            err(&o),
            what_happened(&self.tb, &self.term)
        );
        out(&o)
    }

    /// `<root>/work/<name>`, made, with `steps` as fake-claude's script there.
    fn folder(&self, name: &str, steps: &[FakeStep]) -> PathBuf {
        let dir = self.tb.root().join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        write_script(&dir, steps);
        dir
    }

    /// `moactl open <dir>`; the session's id.
    fn open(&self, dir: &Path) -> String {
        let text = self.ok(&["--json", "open", &dir.display().to_string()]);
        let data: Value = serde_json::from_str(&text).unwrap();
        data["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{data}"))
            .to_string()
    }

    /// Waits until session `id`, opened in `dir`, has its transcript in acct-a's store (R34),
    /// so its agent has started; the transcript's path. The session shows idle from its open
    /// on, so that alone doesn't say so.
    #[track_caller]
    fn written(&self, dir: &Path, id: &str) -> PathBuf {
        let row = ls_row(&self.tb, id).unwrap_or_else(|| panic!("{id} isn't listed"));
        let conv = row["conversation"].as_str().unwrap();
        let slot = moa_agents::claude::project_slot(&moa_platform::canonical(dir));
        let path = self
            .tb
            .root()
            .join(".claude-a/projects")
            .join(slot)
            .join(format!("{conv}.jsonl"));
        let written = wait_for(SHOWS, || path.is_file().then_some(()));
        assert!(
            written.is_some(),
            "fake-claude never wrote its transcript\n{}",
            what_happened(&self.tb, &self.term)
        );
        path
    }

    /// Waits until `moactl ls` lists tab `pane` as untracked.
    #[track_caller]
    fn listed_untracked(&self, pane: &str) {
        let want = format!("UNTRACKED tab {pane}: acct-a in ");
        let seen = wait_for(SHOWS, || {
            let text = self.ok(&["ls"]);
            text.lines().any(|l| l.starts_with(&want)).then_some(())
        });
        assert!(
            seen.is_some(),
            "never listed untracked: {}\n{}",
            self.ok(&["ls"]),
            tab_shows(&self.tb, pane)
        );
    }

    /// `moactl adopt pane`; the adopted session's id.
    #[track_caller]
    fn adopt(&self, pane: &str) -> String {
        let text = self.ok(&["adopt", pane]);
        assert!(
            text.starts_with(&format!("adopted {CONV} (acct-a) in ")),
            "{text}"
        );
        let rows: Value = serde_json::from_str(&self.ok(&["--json", "ls"])).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["conversation"] == CONV)
            .unwrap_or_else(|| panic!("no session runs {CONV}: {rows}"))
            .clone();
        assert_eq!(row["tracked"], false, "{row}");
        row["id"].as_str().unwrap().to_string()
    }

    /// Waits until session `id`'s row in `ls --json` satisfies `want`.
    #[track_caller]
    fn row_becomes(&self, id: &str, what: &str, want: impl Fn(&Value) -> bool) {
        let seen = wait_for(SHOWS, || ls_row(&self.tb, id).filter(|r| want(r)));
        assert!(
            seen.is_some(),
            "never {what}: {:?}\n{}",
            ls_row(&self.tb, id),
            what_happened(&self.tb, &self.term)
        );
    }

    fn shows(&self, id: &str, display: &str) {
        self.row_becomes(id, display, |r| r["display"] == display);
    }

    /// Every tab.
    fn tabs(&self) -> Vec<Tab> {
        tmux(
            &self.tb,
            &[
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}\t#{@moa_session}\t#{@moa_adopted}\t#{@moa_term}\t#{@moa_label}\t\
                 #{pane_start_command}",
            ],
        )
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(6, '\t').collect();
            let [pane, session, adopted, term, label, start] = f[..] else {
                return None;
            };
            Some(Tab {
                pane: pane.into(),
                session: session.into(),
                adopted: adopted == "1",
                term: term == "1",
                label: label.into(),
                start: start.into(),
            })
        })
        .collect()
    }

    /// Tab `pane`, which must be there.
    #[track_caller]
    fn tab(&self, pane: &str) -> Tab {
        let tabs = self.tabs();
        let tab = tabs.iter().find(|t| t.pane == pane);
        tab.unwrap_or_else(|| panic!("no tab {pane}: {tabs:?}"))
            .clone()
    }

    /// Waits until terminal tab `pane` carries session `id`'s adoption tags, or none when
    /// `None`. Either way it stays the terminal `fake_shell_agent` opened in `work/b`: still
    /// marked one, with its label.
    #[track_caller]
    fn still_a_terminal(&self, pane: &str, id: Option<&str>) {
        let tagged = wait_for(SHOWS, || {
            let t = self.tab(pane);
            (t.session == id.unwrap_or("") && t.adopted == id.is_some()).then_some(t)
        });
        let t = tagged.unwrap_or_else(|| {
            panic!(
                "{pane}'s tags never became {id:?}: {:?}\n{}",
                self.tab(pane),
                tab_shows(&self.tb, pane)
            )
        });
        assert!(t.term, "{t:?}");
        assert_eq!(t.label, "⌨ b", "{t:?}");
    }

    fn launches(&self) -> u64 {
        status_json(&self.tb)["launches"].as_u64().unwrap()
    }
}

#[test]
fn an_agent_typed_in_a_shell_can_be_adopted_and_restarted() {
    let r = rig();
    let dir = r.folder("b", &[FakeStep::Hang]);
    let pane = fake_shell_agent(&r.tb, &dir, CONV);
    r.listed_untracked(&pane);

    let id = r.adopt(&pane);
    r.still_a_terminal(&pane, Some(&id));
    // The text row says how to track it, by its handle.
    let ls = r.ok(&["ls"]);
    let handle = ls
        .lines()
        .find_map(|l| l.split("  adopted: moactl restart ").nth(1))
        .and_then(|rest| rest.strip_suffix(" tracks it"))
        .unwrap_or_else(|| panic!("no restart line: {ls}"))
        .to_string();

    // fake-claude reads its script from its folder at each launch.
    write_script(&dir, &[start("resume"), FakeStep::Hang]);
    let text = r.ok(&["restart", &handle]);
    assert!(text.starts_with("restarted "), "{text}");

    // The old tab is gone with its agent, and a new one runs the same conversation.
    let replaced = wait_for(SHOWS, || {
        let tabs = r.tabs();
        let old = tabs.iter().any(|t| t.pane == pane);
        let new = tabs
            .iter()
            .find(|t| t.session == id && t.start.contains(&format!("--resume {CONV}")));
        (!old).then_some(())?;
        new.map(|t| t.pane.clone())
    });
    let new = replaced.unwrap_or_else(|| {
        panic!(
            "not replaced: {:?}\n{}",
            r.tabs(),
            what_happened(&r.tb, &r.term)
        )
    });
    assert_ne!(new, pane);

    // Its SessionStart came through moa's hooks: tracked, and idle.
    let context = dir.join(".fake-claude.context");
    let started = wait_for(SHOWS, || context.exists().then_some(()));
    assert!(started.is_some(), "{}", tab_shows(&r.tb, &new));
    r.row_becomes(&id, "idle and tracked", |row| {
        row["display"] == "idle" && row["tracked"] == true && row["conversation"] == CONV
    });
}

#[test]
fn an_adopted_agent_that_exits_shows_asleep() {
    let r = rig();
    let dir = r.folder("b", &[FakeStep::Hang]);
    let pane = fake_shell_agent(&r.tb, &dir, CONV);
    r.listed_untracked(&pane);
    let id = r.adopt(&pane);
    r.still_a_terminal(&pane, Some(&id));

    // The user stops it; the shell stays, a terminal again with no tag of moa's.
    tmux(&r.tb, &["send-keys", "-t", &pane, "C-c"]);
    r.shows(&id, "asleep");
    r.still_a_terminal(&pane, None);
}

#[test]
fn resume_close_sleep_adopt_launch_nothing_in_the_daemon() {
    let r = rig();
    let a = r.folder("a", &[start("startup"), FakeStep::Hang]);
    let id = r.open(&a);
    r.shows(&id, "idle");
    r.written(&a, &id);
    let before = r.launches();

    r.ok(&["sleep", &id]);
    r.shows(&id, "asleep");
    write_script(&a, &[start("resume"), FakeStep::Hang]);
    let text = r.ok(&["resume", &id]);
    assert!(text.starts_with("resumed "), "{text}");
    r.shows(&id, "idle");
    r.ok(&["focus", &id]);
    assert_eq!(r.ok(&["close", &id]), "closed\n");
    r.shows(&id, "ended");
    r.ok(&["term", &r.tb.root().join("work").display().to_string()]);

    let b = r.folder("b", &[FakeStep::Hang]);
    let pane = fake_shell_agent(&r.tb, &b, CONV);
    r.listed_untracked(&pane);
    let adopted = r.adopt(&pane);
    write_script(&b, &[start("resume"), FakeStep::Hang]);
    r.ok(&["restart", &adopted]);
    r.row_becomes(&adopted, "tracked", |row| row["tracked"] == true);

    assert_eq!(r.launches(), before);
}

#[test]
fn a_resume_in_the_wrong_account_is_refused_end_to_end() {
    // acct-b, a second account with a store of its own.
    let r = rig_with(|tb| {
        let store_b = tb.root().join(".claude-b");
        fs::create_dir_all(&store_b).unwrap();
        let file = tb.paths().config_file();
        let toml = fs::read_to_string(&file).unwrap()
            + &format!(
                "\n[accounts.acct-b]\nkind = \"claude\"\nstore = \"{}\"\nlauncher = \"{}\"\n",
                store_b.display(),
                tb.root().join("bin/claude").display()
            );
        fs::write(&file, toml).unwrap();
    });
    let store_b = r.tb.root().join(".claude-b");

    // Recorded under acct-a, the brain's open account, and asleep.
    let dir = r.folder("c", &[start("startup"), FakeStep::Hang]);
    let id = r.open(&dir);
    r.shows(&id, "idle");
    let row = ls_row(&r.tb, &id).unwrap();
    assert_eq!(row["account"], "acct-a", "{row}");
    let conv = row["conversation"].as_str().unwrap().to_string();
    let in_a = r.written(&dir, &id);
    r.ok(&["sleep", &id]);
    r.shows(&id, "asleep");
    let gone = wait_for(SHOWS, || {
        (!r.tabs().iter().any(|t| t.session == id)).then_some(())
    });
    assert!(gone.is_some(), "its tab is still there: {:?}", r.tabs());

    // Its transcript is only in acct-b's folder.
    fs::remove_file(&in_a).unwrap();
    let in_b = store_b
        .join("projects")
        .join(in_a.parent().unwrap().file_name().unwrap());
    fs::create_dir_all(&in_b).unwrap();
    fs::write(
        in_b.join(format!("{conv}.jsonl")),
        "{\"type\":\"summary\",\"summary\":\"fake\"}\n",
    )
    .unwrap();

    let tabs = r.tabs();
    let o = r.moactl(&["resume", &id]);
    assert!(!o.status.success(), "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains(
            "its conversation is in acct-b's folder, not acct-a's; \
             moa won't resume it under the wrong account"
        ),
        "{}{}",
        out(&o),
        err(&o)
    );
    // The refusal comes before any tab is asked for.
    assert_eq!(r.tabs(), tabs);
    assert_eq!(ls_display(&r.tb, &id).as_deref(), Some("asleep"));
}
