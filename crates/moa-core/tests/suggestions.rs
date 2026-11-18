//! `suggest` and `suggestion yes|no|later|ls` on the typical brain, with the real moactl
//! grammar (today is Sunday 2026-11-15, noon in New York).

mod common;

use std::path::{Path, PathBuf};

use moa_core::change::Change;
use moa_core::config::BrainName;
use moa_core::effect::{Effect, Input, LogLevel, Publish, handle_checked};
use moa_core::model::{
    ProjectId, ProjectStatus, SuggestionId, SuggestionStatus, TaskId, TaskStatus,
};
use moa_core::op::{Answer, ConnId, Done, NoParams, Op, ReqId};
use moa_core::publish::CoreEvent;
use moa_core::state::{CallerInfo, Deps, LoadNote, State};
use moa_core::{Now, UtcMs};
use moa_testkit::brain::{TypicalBrain, run};
use moa_testkit::ids::*;

fn deps(b: &TypicalBrain) -> Deps {
    Deps {
        parse_action: Some(moa_proto::cli::parse_action),
        canonical: |p| p.to_path_buf(),
        home: b.home.clone(),
        ..Deps::default()
    }
}

fn typical() -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    let st = b.state_with(deps(&b));
    (b, st)
}

fn ok(st: &mut State, op: Op, now: &Now) -> Done {
    run(st, op, now).unwrap()
}

fn err(st: &mut State, op: Op, now: &Now) -> String {
    run(st, op, now).unwrap_err().0
}

fn effects(st: &mut State, op: Op, now: &Now) -> Vec<Effect> {
    handle_checked(
        st,
        Input::Client {
            conn: ConnId(1),
            req: ReqId(1),
            caller: CallerInfo::default(),
            op,
        },
        now.clone(),
    )
}

fn suggest(text: &str, actions: &[&str]) -> Op {
    Op::Suggest {
        text: text.into(),
        actions: actions.iter().map(|a| a.to_string()).collect(),
    }
}

fn answer(id: i64, a: Answer) -> Op {
    Op::SuggestionAnswer {
        id: Some(id),
        answer: a,
    }
}

fn row(st: &State, id: i64) -> moa_core::model::Suggestion {
    st.suggestions().find(|s| s.id.0 == id).unwrap().clone()
}

fn persists(fx: &[Effect]) -> Vec<&Vec<Change>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c),
            _ => None,
        })
        .collect()
}

fn activity_texts(fx: &[Effect]) -> Vec<String> {
    persists(fx)
        .into_iter()
        .flatten()
        .filter_map(|c| match c {
            Change::Activity(a) => Some(a.text.clone()),
            _ => None,
        })
        .collect()
}

fn ui_texts(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Publish(Publish::Event(CoreEvent::Ui { text })) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// A kind per effect, in order.
fn shape(fx: &[Effect]) -> Vec<&'static str> {
    fx.iter()
        .map(|e| match e {
            Effect::Persist(_) => "persist",
            Effect::Publish(Publish::Model(_)) => "model",
            Effect::Publish(Publish::Event(_)) => "event",
            Effect::Reply { .. } => "reply",
            _ => "other",
        })
        .collect()
}

fn startup(b: &TypicalBrain, deps: Deps) -> (State, Vec<Effect>) {
    let mut config = b.config();
    config.canonicalize_with(deps.canonical);
    let mut st = State::new(BrainName::default_brain(), config).with_deps(deps);
    let fx = handle_checked(&mut st, Input::Startup(Box::new(b.snapshot())), b.now());
    (st, fx)
}

#[test]
fn startup_flags_the_action_the_spike_could_not_run() {
    let b = TypicalBrain::new();
    let (mut st, fx) = startup(&b, deps(&b));
    let want = "an action doesn't parse as moactl";
    assert_eq!(row(&st, 1).actions_invalid.as_deref(), Some(want));
    assert_eq!(row(&st, 2).actions_invalid, None);
    assert_eq!(row(&st, 4).actions_invalid, None);
    // The flag is saved, before anything is published, and logged with an id only.
    let saved = persists(&fx);
    assert_eq!(saved.len(), 1);
    assert!(matches!(&saved[0][..], [Change::Suggestion(s)] if s.id == SuggestionId(1)));
    assert!(fx.iter().any(|e| matches!(e, Effect::Log(l)
        if l.level == LogLevel::Warn && l.message.contains("suggestion #1"))));
    let now = b.now();
    assert_eq!(
        err(&mut st, answer(1, Answer::Yes), &now),
        "suggestion #1 has actions moa can't run; answer no or later"
    );
    // No grammar, nothing flagged.
    let (st, fx) = startup(&b, Deps::default());
    assert_eq!(row(&st, 1).actions_invalid, None);
    assert!(persists(&fx).is_empty());
}

#[test]
fn startup_leaves_a_suggestion_with_an_unknown_value_alone() {
    let b = TypicalBrain::new();
    let mut snap = b.snapshot();
    for key in ["#1", "#2"] {
        snap.unknown.push(LoadNote {
            table: "suggestions",
            key: key.into(),
            reason: "status",
        });
    }
    let d = deps(&b);
    let mut st = State::new(BrainName::default_brain(), b.config()).with_deps(d);
    let fx = handle_checked(&mut st, Input::Startup(Box::new(snap)), b.now());
    assert_eq!(row(&st, 1).actions_invalid, None);
    assert!(persists(&fx).is_empty());
    let now = b.now();
    assert_eq!(
        err(&mut st, answer(1, Answer::Yes), &now),
        "suggestion #1 has a value this build doesn't know; update moa"
    );
    assert_eq!(
        err(&mut st, answer(1, Answer::No), &now),
        "suggestion #1 has a value this build doesn't know; update moa"
    );
    assert_eq!(row(&st, 1).status, SuggestionStatus::Open);
    // #2's action is valid, so without the guard it would run and commit, and only then the
    // suggestion's own write would be refused.
    let s1 = st.resolve(S1_WORKING).unwrap();
    let title = st.session(s1).unwrap().title.clone();
    let fx = effects(&mut st, answer(2, Answer::Yes), &now);
    assert_eq!(
        moa_core::effect::reply_of(&fx)
            .unwrap()
            .as_ref()
            .unwrap_err()
            .0,
        "suggestion #2 has a value this build doesn't know; update moa"
    );
    assert_eq!(st.session(s1).unwrap().title, title);
    assert_ne!(title.as_deref(), Some("New name"));
    assert!(persists(&fx).is_empty());
}

#[test]
fn imported_suggestion_two_runs() {
    let (b, mut st) = typical();
    let now = b.now();
    let d = ok(&mut st, answer(2, Answer::Yes), &now);
    assert_eq!(d.text, "done");
    let s1 = st.resolve(S1_WORKING).unwrap();
    assert_eq!(st.session(s1).unwrap().title.as_deref(), Some("New name"));
    let s = row(&st, 2);
    assert_eq!(s.status, SuggestionStatus::Accepted);
    assert_eq!(s.resolved_at, Some(now.wall));
    assert_eq!(s.result.as_deref(), Some("done"));
}

#[test]
fn suggest_validates_and_refuses_duplicates() {
    let (b, mut st) = typical();
    let now = b.now();
    let start = "Start the board?";
    let fx = effects(
        &mut st,
        suggest(start, &["task set 1 --status active"]),
        &now,
    );
    assert_eq!(
        moa_core::effect::reply_of(&fx)
            .unwrap()
            .as_ref()
            .unwrap()
            .text,
        "suggestion #5: the user sees it with yes / later / no buttons"
    );
    assert_eq!(activity_texts(&fx), ["suggests #5: Start the board?"]);
    let s = row(&st, 5);
    assert_eq!(s.status, SuggestionStatus::Open);
    assert_eq!(s.actions_raw, None);
    assert_eq!(s.created_at, now.wall);
    assert_eq!(s.actions[0].verb, "task");

    let fx = effects(
        &mut st,
        suggest(start, &["task set 1 --status active"]),
        &now,
    );
    assert_eq!(
        moa_core::effect::reply_of(&fx)
            .unwrap()
            .as_ref()
            .unwrap()
            .text,
        "already suggested (#5)"
    );
    assert!(persists(&fx).is_empty());

    assert_eq!(
        err(&mut st, suggest("Tidy", &["rm -rf x"]), &now),
        "--do 'rm -rf x': a suggestion can run moactl archive, close, group, note, project, set, task"
    );
    assert!(
        err(&mut st, suggest("Tidy", &["task 1 --status done"]), &now)
            .starts_with("--do 'task 1 --status done': invalid value '1'")
    );
    assert!(
        err(&mut st, suggest("Tidy", &["note 'oops"]), &now).starts_with("--do 'note 'oops': ")
    );
    assert_eq!(st.suggestions().count(), 5);
}

#[test]
fn suggest_without_the_grammar_refuses_actions() {
    let b = TypicalBrain::new();
    let mut st = b.state();
    let now = b.now();
    assert_eq!(
        err(&mut st, suggest("Do it", &["note x"]), &now),
        "suggestions need the moactl grammar"
    );
    // Without actions it is only text.
    assert!(run(&mut st, suggest("Just a thought", &[]), &now).is_ok());
}

#[test]
fn yes_runs_every_action_in_one_transaction() {
    let (b, mut st) = typical();
    let now = b.now();
    ok(
        &mut st,
        suggest(
            "Do the lot",
            &[
                "task set 1 --status active",
                "note 'board ordered' --project Lantern",
                "task set 99 --status done",
                "close 3f9a1c52",
            ],
        ),
        &now,
    );
    let notes = st.notes().count();
    let fx = effects(&mut st, answer(5, Answer::Yes), &now);
    let reply = moa_core::effect::reply_of(&fx).unwrap().as_ref().unwrap();
    assert_eq!(
        reply.text,
        "done; but task set 99 --status done: no task #99; close 3f9a1c52: moa isn't open in a terminal yet; run moa first"
    );
    let t1 = st.tasks().find(|t| t.id == TaskId(1)).unwrap();
    assert_eq!(t1.status, TaskStatus::Active);
    assert_eq!(st.notes().count(), notes + 1);
    assert!(st.notes().any(|n| n.text == "board ordered"));
    assert_eq!(shape(&fx), ["persist", "model", "event", "reply"]);
    let saved = persists(&fx);
    assert_eq!(saved.len(), 1);
    let c = saved[0];
    assert!(c.iter().any(|c| matches!(c, Change::Task(_))));
    assert!(c.iter().any(|c| matches!(c, Change::Note(_))));
    assert!(c.iter().any(|c| matches!(c, Change::Suggestion(_))));
    assert!(c.iter().any(|c| matches!(c, Change::Event(_))));
    assert!(
        activity_texts(&fx)
            .iter()
            .any(|t| t.starts_with("noted (fact): board ordered"))
    );
    let failed = "task set 99 --status done: no task #99; close 3f9a1c52: moa isn't open in a terminal yet; run moa first";
    assert_eq!(
        row(&st, 5).result.as_deref(),
        Some(failed),
        "the failures are the result"
    );
    // The task's own ui event (none: set doesn't log one) isn't needed; the suggestion's is.
    assert!(ui_texts(&fx).contains(&format!(
        "accepted suggestion #5: Do the lot (failed: {failed})"
    )));
}

/// `<home>/link/x` is `<home>/private/x`.
fn through_link(p: &Path) -> PathBuf {
    p.components()
        .map(|c| {
            if c.as_os_str() == "link" {
                "private".into()
            } else {
                c.as_os_str().to_owned()
            }
        })
        .collect()
}

#[test]
fn an_action_path_is_canonicalised_like_a_typed_one() {
    let b = TypicalBrain::new();
    let d = Deps {
        canonical: through_link,
        ..deps(&b)
    };
    let mut st = b.state_with(d);
    let now = b.now();
    let typed = b.home.join("link");
    let action = format!("project Shore --add {}", typed.display());
    ok(&mut st, suggest("Add Shore?", &[&action]), &now);
    assert_eq!(ok(&mut st, answer(5, Answer::Yes), &now).text, "done");
    let p = st.projects().find(|p| p.name == "Shore").unwrap();
    assert_eq!(p.path.as_deref(), b.home.join("private").to_str());
    assert_eq!(p.status, ProjectStatus::Active);
    assert!(p.id > ProjectId(0));
}

#[test]
fn project_add_matches_an_imported_path_typed_the_same_way() {
    let b = TypicalBrain::new();
    let private = b.home.join("private");
    std::fs::create_dir_all(private.join("shore")).unwrap();
    std::os::unix::fs::symlink(&private, b.home.join("link")).unwrap();
    let mut snap = b.snapshot();
    let at = UtcMs(1);
    snap.projects.push(moa_core::model::Project {
        id: ProjectId(50),
        name: "Shore".into(),
        path: Some(b.home.join("link").join("shore").display().to_string()),
        status: ProjectStatus::Active,
        summary: None,
        last_activity_on: None,
        created_at: at,
        updated_at: at,
    });
    let d = Deps {
        canonical: |p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()),
        ..deps(&b)
    };
    let mut config = b.config();
    config.canonicalize_with(d.canonical);
    let mut st = State::new(BrainName::default_brain(), config).with_deps(d);
    handle_checked(&mut st, Input::Startup(Box::new(snap)), b.now());
    let now = b.now();
    let e = err(
        &mut st,
        Op::ProjectAdd {
            name: "Other".into(),
            path: b.home.join("link").join("shore"),
            summary: None,
        },
        &now,
    );
    assert!(
        e.starts_with("there's already a project named 'Other' or for "),
        "{e}"
    );
}

#[test]
fn no_and_later_and_already_answered() {
    let (b, mut st) = typical();
    let now = b.now();
    let fx = effects(&mut st, answer(2, Answer::No), &now);
    assert_eq!(
        moa_core::effect::reply_of(&fx)
            .unwrap()
            .as_ref()
            .unwrap()
            .text,
        "rejected"
    );
    assert_eq!(
        ui_texts(&fx),
        ["rejected suggestion #2: Rename the working session"]
    );
    let s = row(&st, 2);
    assert_eq!(s.status, SuggestionStatus::Rejected);
    assert_eq!(s.resolved_at, Some(now.wall));
    // No was no: the action did not run.
    let s1 = st.resolve(S1_WORKING).unwrap();
    assert_ne!(st.session(s1).unwrap().title.as_deref(), Some("New name"));

    assert_eq!(ok(&mut st, answer(1, Answer::Later), &now).text, "later");
    assert_eq!(row(&st, 1).status, SuggestionStatus::Later);
    // Later can still be answered.
    assert_eq!(ok(&mut st, answer(4, Answer::Later), &now).text, "later");
    assert_eq!(ok(&mut st, answer(1, Answer::No), &now).text, "rejected");

    assert_eq!(
        err(&mut st, answer(2, Answer::Yes), &now),
        "suggestion #2 is already rejected"
    );
    assert_eq!(
        err(&mut st, answer(3, Answer::No), &now),
        "suggestion #3 is already rejected"
    );
    for op in [
        Op::SuggestionAnswer {
            id: None,
            answer: Answer::Yes,
        },
        answer(99, Answer::Yes),
    ] {
        assert_eq!(err(&mut st, op, &now), "moactl suggestion yes|no|later ID");
    }
}

#[test]
fn suggestion_ls_matches_the_spike_layout() {
    let (b, mut st) = typical();
    let now = b.now();
    let text = ok(&mut st, Op::SuggestionLs(NoParams), &now).text;
    insta::assert_snapshot!("suggestion_ls_matches_the_spike_layout", text);
}

/// `close` runs from a suggestion through the same transaction (R15): the session ends,
/// and its tab is killed once the whole answer is saved.
#[test]
fn a_suggestion_can_close_a_session() {
    let toml = "[brains.default]\nfolders = [\"~/work\"]\nopen_account = \"acct-a\"\n\n\
                [accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.claude-a\"\nlauncher = \"/opt/claude\"\n";
    let b = TypicalBrain::new();
    let deps = Deps {
        session_plugin: PathBuf::from(common::PLUGIN),
        ..deps(&b)
    };
    let mut st = common::loaded_with(&b, toml, deps, b.snapshot());
    common::link_up(&b, &mut st);
    handle_checked(&mut st, Input::StartDrained, b.now());
    common::listed(&mut st, &b, &[], &[]);
    let open = Op::SessionOpen {
        path: b.home.join("work/lantern"),
        account: None,
        title: None,
        group: None,
        prompt: None,
        bg: true,
    };
    let id = common::the_open(&common::send(&b, &mut st, 1, open)).0;
    common::feed(
        &b,
        &mut st,
        moa_core::effect::TmuxIn::Replies {
            conn: 1,
            token: moa_core::effect::TmuxToken::Open(id),
            results: vec![Ok(vec!["P %7 4242".into()])],
        },
    );
    let now = b.now();
    let handle = st.handle_of(id);
    let made = ok(
        &mut st,
        suggest("Close lantern", &[&format!("close {handle}")]),
        &now,
    );
    let n = made.data["id"].as_i64().unwrap();

    let fx = effects(&mut st, answer(n, Answer::Yes), &now);
    assert_eq!(
        moa_core::effect::reply_of(&fx)
            .unwrap()
            .as_ref()
            .unwrap()
            .text,
        "done"
    );
    let s = st.session(id).unwrap();
    assert_eq!(s.lifecycle, moa_core::model::Lifecycle::Ended);
    assert!(s.asleep_at.is_none());
    let kill = moa_core::tmux::cmd::Cmd::new("kill-pane").args(["-t", "%7"]);
    let killed = fx
        .iter()
        .position(
            |e| matches!(e, Effect::Tmux { conn: 1, cmds, token: None } if cmds == std::slice::from_ref(&kill)),
        )
        .unwrap_or_else(|| panic!("the tab is killed: {fx:?}"));
    let saved = fx
        .iter()
        .position(|e| matches!(e, Effect::Persist(_)))
        .unwrap();
    assert!(saved < killed, "{fx:?}");
    assert_eq!(persists(&fx).len(), 1, "one transaction: {fx:?}");
}
