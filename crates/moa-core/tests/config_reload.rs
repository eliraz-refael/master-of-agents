//! Slice 9: `moa.toml`, `layouts.json`, the scope file and moa's constitution reload while
//! the daemon runs (L8, §6.15.4). The daemon's shell reads `moa.toml` and hands the core the
//! config whole, or its error; the other files are only said to have changed, and the core
//! reads them as it always does.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use common::central::{acps, ended, first_turn, hear, opened_today, prompt, toml};
use common::*;
use moa_core::central::wire::{AcpCmd, ThreadHow};
use moa_core::config::{BrainName, Config};
use moa_core::config_reload::{INSTRUCTIONS_CHANGED, kept_the_last, layouts_kept};
use moa_core::effect::{Effect, Input, Publish, TimerKey, handle_checked};
use moa_core::model::EventKind;
use moa_core::op::{NoParams, Op};
use moa_core::publish::{CoreEvent, Topic};
use moa_core::state::{Deps, State};
use moa_core::{Now, UtcMs};
use moa_testkit::brain::TypicalBrain;
use serde_json::{Value, json};

thread_local! {
    /// The files the test's `read_text` sees, by path; a path not here is no file.
    static FILES: RefCell<BTreeMap<PathBuf, String>> = const { RefCell::new(BTreeMap::new()) };
}

fn read_text(path: &Path) -> io::Result<Option<String>> {
    Ok(FILES.with(|f| f.borrow().get(path).cloned()))
}

/// Puts `text` in `path`, as a save does.
fn save(path: &Path, text: &str) {
    FILES.with(|f| f.borrow_mut().insert(path.to_path_buf(), text.to_string()));
}

const LAYOUTS: &str = "/tmp/moa-test-config/layouts.json";
const CONSTITUTION: &str = "/tmp/moa-test-config/constitution.md";
const SCOPE: &str = "/tmp/moa-test-config/default.md";

fn deps(b: &TypicalBrain) -> Deps {
    Deps {
        home: b.home.clone(),
        read_text,
        layouts_file: PathBuf::from(LAYOUTS),
        constitution_file: PathBuf::from(CONSTITUTION),
        scope_file: PathBuf::from(SCOPE),
        ..Deps::default()
    }
}

/// `moa.toml` for the default brain with `accounts` (each a Claude account with a
/// launcher), and `extra` after it.
fn moa_toml(accounts: &[&str], extra: &str) -> String {
    let mut text = String::from("[brains.default]\nfolders = [\"~/private\"]\n");
    for a in accounts {
        text.push_str(&format!(
            "[accounts.{a}]\nkind = \"claude\"\nstore = \"~/.claude-{a}\"\n\
             launcher = \"/opt/claude/bin/claude\"\n"
        ));
    }
    text.push_str(extra);
    text
}

/// What the daemon's shell hands over for `text`: the config, or its error's line.
fn read(b: &TypicalBrain, text: &str) -> Result<Config, String> {
    Config::from_file_text(Some(text), &b.home, |p| p.to_path_buf()).map_err(|e| e.to_string())
}

fn reload(b: &TypicalBrain, st: &mut State, text: &str) -> Vec<Effect> {
    let config = read(b, text);
    handle_checked(st, Input::Config(config), b.now())
}

/// The typical brain under `toml`, its link up.
fn linked(toml: &str) -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    let mut st = loaded_with(&b, toml, deps(&b), b.snapshot());
    link_up(&b, &mut st);
    (b, st)
}

fn errors(fx: &[Effect]) -> Vec<&str> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Event(CoreEvent::Error { message })) => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

fn tmux_confs(fx: &[Effect]) -> usize {
    fx.iter().filter(|e| matches!(e, Effect::TmuxConf)).count()
}

/// The rows of `topic` the last publish in `fx` carries.
fn published(fx: &[Effect], topic: Topic) -> Option<Value> {
    fx.iter().rev().find_map(|e| match e {
        Effect::Publish(Publish::Model(m)) => m
            .topics
            .get(&topic)
            .map(|rows| serde_json::to_value(&**rows).unwrap()),
        _ => None,
    })
}

fn open_with(b: &TypicalBrain, account: &str) -> Op {
    Op::SessionOpen {
        path: b.home.join("private/lantern"),
        account: Some(account.into()),
        title: None,
        group: None,
        prompt: None,
        bg: true,
    }
}

/// A good `moa.toml` replaces the config whole: an account it adds can open a session at
/// once, and the `brain` row lists it.
#[test]
fn a_good_moa_toml_replaces_the_config() {
    let (b, mut st) = linked(&moa_toml(&["acct-a"], ""));
    let fx = send(&b, &mut st, 1, open_with(&b, "acct-b"));
    match &replies(&fx)[..] {
        [(_, Err(e))] => assert_eq!(e.0, "there's no account called acct-b"),
        other => panic!("a refusal, got {other:?}"),
    }

    let fx = reload(&b, &mut st, &moa_toml(&["acct-a", "acct-b"], ""));
    assert_eq!(errors(&fx), Vec::<&str>::new());
    assert!(changes(&fx).is_empty(), "a config saves nothing: {fx:?}");
    assert_eq!(
        published(&fx, Topic::Brain).unwrap()["brain"]["accounts"],
        json!(["acct-a", "acct-b"])
    );
    // `[ui]` is as it was: the tmux config stays.
    assert_eq!(tmux_confs(&fx), 0, "{fx:?}");
    assert!(st.config().accounts.keys().any(|a| a.as_str() == "acct-b"));

    let fx = send(&b, &mut st, 2, open_with(&b, "acct-b"));
    the_open(&fx);
}

/// A `[ui]` change has the shell write moa's tmux config again and source it: once.
#[test]
fn a_ui_change_rewrites_the_tmux_conf() {
    let (b, mut st) = linked(&moa_toml(&["acct-a"], ""));
    let fx = reload(&b, &mut st, &moa_toml(&["acct-a"], "[ui]\nmouse = false\n"));
    assert_eq!(tmux_confs(&fx), 1, "{fx:?}");
    assert!(!st.config().ui.mouse);
    // The same again changes nothing.
    let fx = reload(&b, &mut st, &moa_toml(&["acct-a"], "[ui]\nmouse = false\n"));
    assert_eq!(tmux_confs(&fx), 0, "{fx:?}");
}

/// Review Focus 5: `moa.toml` saved half-edited is refused whole, with one error line, and
/// the last good config stays; the next good save applies.
#[test]
fn a_broken_moa_toml_keeps_the_last() {
    let (b, mut st) = linked(&moa_toml(&["acct-a"], ""));
    let before = st.config().clone();
    let half = moa_toml(&["acct-a", "acct-b"], "[ui]\nmouse = \"of\n");

    let fx = reload(&b, &mut st, &half);
    assert_eq!(
        errors(&fx),
        [
            "moa.toml has an error, so moa kept the last one: line 12: invalid basic string, \
          expected `\"`"
        ]
    );
    assert_eq!(
        kept_the_last("moa.toml: [ui] mouse must be true or false"),
        "moa.toml has an error, so moa kept the last one: [ui] mouse must be true or false"
    );
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert_eq!(tmux_confs(&fx), 0, "{fx:?}");
    assert_eq!(st.config(), &before);
    let fx = send(&b, &mut st, 1, open_with(&b, "acct-b"));
    assert!(matches!(&replies(&fx)[..], [(_, Err(_))]), "{fx:?}");

    let fx = reload(&b, &mut st, &moa_toml(&["acct-a", "acct-b"], ""));
    assert_eq!(errors(&fx), Vec::<&str>::new());
    let fx = send(&b, &mut st, 2, open_with(&b, "acct-b"));
    the_open(&fx);
}

/// A `moa.toml` that drops the brain this daemon runs is refused as a broken one is: the
/// daemon can't run a brain the file doesn't have.
#[test]
fn a_moa_toml_without_this_brain_is_refused() {
    let b = TypicalBrain::new();
    let work = BrainName::parse("work").unwrap();
    let text = "[brains.work]\nfolders = [\"~/office\"]\n";
    let config = Config::parse(text, &b.home).unwrap();
    let mut st = State::new(work, config).with_deps(deps(&b));
    handle_checked(&mut st, Input::Startup(Box::new(b.snapshot())), b.now());
    let before = st.config().clone();

    let fx = reload(&b, &mut st, "[brains.home]\nfolders = [\"~/private\"]\n");
    assert_eq!(
        errors(&fx),
        ["moa.toml has an error, so moa kept the last one: it has no brain named work"]
    );
    assert_eq!(st.config(), &before);
}

/// `layouts.json` saved by hand is read again and published; a broken one keeps the last
/// good layouts, with one error line.
#[test]
fn layouts_reload_and_publish() {
    let (b, mut st) = linked(&moa_toml(&["acct-a"], ""));
    save(
        Path::new(LAYOUTS),
        r#"{"layouts": {"mine": {"panel": "tree"}}}"#,
    );
    let fx = handle_checked(&mut st, Input::LayoutsChanged, b.now());
    let rows = published(&fx, Topic::Layouts).expect("layouts published");
    assert!(rows.get("mine").is_some(), "{rows}");
    assert_eq!(errors(&fx), Vec::<&str>::new());

    save(Path::new(LAYOUTS), r#"{"layouts": {"mine": "#);
    let fx = handle_checked(&mut st, Input::LayoutsChanged, b.now());
    let why = moa_core::layout::Layouts::load(Some(r#"{"layouts": {"mine": "#))
        .file_error
        .unwrap();
    assert_eq!(errors(&fx), [layouts_kept(&why).as_str()]);
    assert_eq!(
        layouts_kept("x"),
        "layouts.json has an error, so moa kept the last one: x"
    );
    assert_eq!(published(&fx, Topic::Layouts), None, "{fx:?}");
    // What the panels have is the last good file's.
    let model = st.model();
    let shown = &model.topics[&Topic::Layouts];
    assert!(
        shown.contains_key("mine") && !shown.contains_key(""),
        "{shown:?}"
    );
}

/// moa's instructions apply from its next thread, never mid-thread: the thread under way
/// keeps the system text it was opened with, a line says so, and `/new`'s thread is told
/// the new constitution.
#[test]
fn instructions_apply_from_the_next_thread() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    opened_today(&mut snapshot, &b.now());
    let mut st = loaded_with(&b, &toml("acct-a"), deps(&b), snapshot);
    first_turn(&mut st, &b.now(), "th-1", false);
    hear(&b, &mut st, ended(1, 1));

    save(Path::new(CONSTITUTION), "# You are moa, now terse.");
    save(Path::new(SCOPE), "This brain keeps the garden.");
    let fx = handle_checked(&mut st, Input::InstructionsChanged, b.now());
    let said: Vec<&str> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Event(CoreEvent::Ui { text })) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(said, [INSTRUCTIONS_CHANGED]);
    assert_eq!(
        INSTRUCTIONS_CHANGED,
        "moa's instructions changed; they apply from its next thread (/new starts one)"
    );
    // Its `ui` event and activity row are saved before anyone hears of it.
    let ui = changes(&fx).into_iter().any(|c| {
        matches!(c, moa_core::change::Change::Event(e)
            if e.kind.0 == EventKind::UI && e.text.as_deref() == Some(INSTRUCTIONS_CHANGED))
    });
    assert!(ui, "{fx:?}");
    assert!(acps(&fx).is_empty(), "nothing goes to moa's agent: {fx:?}");

    // The thread under way goes on as it was opened.
    let fx = prompt(&b, &mut st, 2, "and now?");
    assert!(
        matches!(&acps(&fx)[..], [AcpCmd::Prompt { thread, .. }] if thread == "th-1"),
        "{fx:?}"
    );
    hear(&b, &mut st, ended(1, 2));

    send(&b, &mut st, 3, Op::ConsoleNew(NoParams));
    let fx = prompt(&b, &mut st, 4, "hello again");
    let told = acps(&fx).into_iter().find_map(|c| match c {
        AcpCmd::Open {
            how:
                ThreadHow::New {
                    system_prompt: Some(text),
                    ..
                },
            ..
        } => Some(text.clone()),
        _ => None,
    });
    let told = told.unwrap_or_else(|| panic!("a new thread: {fx:?}"));
    assert!(told.starts_with("# You are moa, now terse."), "{told}");
    assert!(told.contains("This brain keeps the garden."), "{told}");
}

/// The audit: a deadline moa armed from moa.toml moves with a reload, at the reload itself.
/// moa's turn under way is watched for 300 s by default; a reload that makes it 60 s arms the
/// watchdog for 60 s after the turn's last news.
#[test]
fn a_reload_moves_the_timers_it_armed() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    opened_today(&mut snapshot, &b.now());
    let mut st = loaded_with(&b, &toml("acct-a"), deps(&b), snapshot);
    let now = b.now();
    first_turn(&mut st, &now, "th-1", false);
    let later = Now {
        wall: UtcMs(now.wall.0 + 1_000),
        ..now.clone()
    };
    let text = format!("{}[moa]\nwatchdog_seconds = 60\n", toml("acct-a"));
    let config = read(&b, &text);
    let fx = handle_checked(&mut st, Input::Config(config), later);
    assert_eq!(
        timers(&fx, TimerKey::Watchdog),
        [&Effect::SetTimer {
            key: TimerKey::Watchdog,
            at: UtcMs(now.wall.0 + 60_000),
        }],
        "{fx:?}"
    );
}
