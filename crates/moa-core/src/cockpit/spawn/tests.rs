//! L7 and L18: panels started parked, one `new-window` each, and every pane that started
//! tagged, recorded and counted whatever its neighbours' answers. Here rather than in
//! `tests/`: until the cockpit queue (L17) sends spawns, only the crate can.

use std::path::PathBuf;
use std::time::Duration;

use jiff::tz::TimeZone;

use super::*;
use crate::config::{BrainName, Config};
use crate::core::handle;
use crate::effect::{Down, Input, LogLevel, TmuxIn, TmuxToken};
use crate::state::{CallerInfo, Deps, PanelLaunch};
use crate::tmux::ids::{ClientName, Generation};
use crate::{Now, UtcMs};

const STOPPED: &str = "panel stopped · Ctrl-a r brings it back";

fn now() -> Now {
    Now::at(
        UtcMs(1_000_000),
        Duration::from_secs(1),
        TimeZone::posix("UTC0").unwrap(),
    )
}

fn connected() -> TmuxIn {
    TmuxIn::Connected {
        conn: 1,
        generation: Generation {
            pid: 10,
            start: 1000,
        },
        version: "3.5a".into(),
        ctl: ClientName("client-9".into()),
        socket: "moa".into(),
    }
}

/// A started core whose panels are `/x/moa` with `env`, at home `/h`, its link up.
fn linked(env: &[(&str, &str)]) -> State {
    let deps = Deps {
        home: PathBuf::from("/h"),
        panel: PanelLaunch {
            exe: PathBuf::from("/x/moa"),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        },
        ..Deps::default()
    };
    let mut st = State::new(BrainName::default_brain(), Config::default()).with_deps(deps);
    handle(&mut st, Input::Startup(Box::default()), now());
    handle(&mut st, Input::Tmux(connected()), now());
    st
}

/// Each command as one line of its words, unquoted.
fn words(cmds: &[Cmd]) -> Vec<String> {
    cmds.iter()
        .map(|c| format!("{} {}", c.name(), c.argv().join(" ")))
        .collect()
}

/// The spawn sent for `views`: its query number and its commands' words, each checked.
fn sent(st: &mut State, views: &[View]) -> (u64, Vec<String>) {
    match spawn(st, views.to_vec(), 7) {
        Some(Effect::Tmux {
            conn: 1,
            cmds,
            token: Some(TmuxToken::Query(n)),
        }) => {
            for c in &cmds {
                assert_eq!(c.check(), Ok(()), "{}", c.line());
            }
            (n, words(&cmds))
        }
        other => panic!("not a spawn on connection 1: {other:?}"),
    }
}

fn answer(n: u64, results: Answers) -> TmuxIn {
    TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Query(n),
        results,
    }
}

fn pane(id: &str) -> Result<Vec<String>, String> {
    Ok(vec![id.to_string()])
}

/// L7's four tags on `pane` for `view`, as words.
fn tags(pane: &str, view: &str, label: &str) -> Vec<String> {
    vec![
        format!("set-option -p -t {pane} @moa_view {view}"),
        format!("set-option -p -t {pane} @moa_label {label}"),
        format!("set-option -p -t {pane} remain-on-exit on"),
        format!("set-option -p -t {pane} remain-on-exit-format {STOPPED}"),
    ]
}

/// The untokened commands in `fx`, as words.
fn tagged(fx: &[Effect]) -> Vec<Vec<String>> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                conn: 1,
                cmds,
                token: Some(TmuxToken::Tags),
            } => Some(words(cmds)),
            _ => None,
        })
        .collect()
}

fn logs(fx: &[Effect]) -> Vec<(LogLevel, String)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some((l.level, l.message.clone())),
            _ => None,
        })
        .collect()
}

/// Lesson 5: every panel moa starts gets `panel_env_of`'s environment, the one moa's tmux
/// file gives its popups.
#[test]
fn a_panels_env_is_panel_env_of() {
    let st = linked(&[("MOA_ROOT", "/r")]);
    let env = panel_env_of("default", &st.deps().panel);
    assert_eq!(
        env,
        [
            "-e",
            "MOA_BRAIN=default",
            "-e",
            "MOA_NO_AUTOSTART=1",
            "-e",
            "MOA_ROOT=/r"
        ]
    );
    let line = spawn_cmds(&st, &[View::Tree])[0].line();
    assert!(line.contains(&env.join(" ")), "{line}");
    let tail = respawn_tail(&st, View::Tree);
    assert!(tail.windows(env.len()).any(|w| w == env), "{tail:?}");
}

#[test]
fn spawning_runs_moa_panel_with_its_view_and_brain() {
    let mut st = linked(&[("MOA_ROOT", "/r")]);
    let (n, cmds) = sent(&mut st, &[View::Tree]);
    assert_eq!(
        cmds,
        [
            "new-window -d -P -F #{pane_id} -c /h -t =_moa: -n tree -e MOA_BRAIN=default -e MOA_NO_AUTOSTART=1 \
          -e MOA_ROOT=/r /x/moa panel tree --brain default"
        ]
    );
    assert_eq!(st.panel_starts, 0);

    let fx = handle(&mut st, Input::Tmux(answer(n, vec![pane("%12")])), now());
    assert_eq!(tagged(&fx), [tags("%12", "tree", "▤ sessions")]);
    assert_eq!(st.panel_starts, 1);
    // The topology knows the panel's view before any listing shows it.
    assert_eq!(st.tmux.topology.view_of(PaneId(12)), Some("tree"));
    // The tags save nothing: they go out at once, never held behind a commit that may fail.
    let tags = fx
        .iter()
        .find(|e| matches!(e, Effect::Tmux { .. }))
        .unwrap();
    assert!(tags.runs_at_once() && !tags.is_heard(), "{tags:?}");
}

/// The tags' answer comes back only to be checked: a refusal is logged, and nothing else
/// happens.
#[test]
fn a_tag_tmux_refused_is_logged() {
    let mut st = linked(&[]);
    let tags = |results| TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Tags,
        results,
    };
    let fx = handle(
        &mut st,
        Input::Tmux(tags(vec![Ok(Vec::new()), Ok(Vec::new())])),
        now(),
    );
    assert!(logs(&fx).is_empty(), "{fx:?}");
    let fx = handle(
        &mut st,
        Input::Tmux(tags(vec![
            Ok(Vec::new()),
            Err("can't find pane: %12".into()),
        ])),
        now(),
    );
    assert_eq!(
        logs(&fx),
        [(
            LogLevel::Warn,
            "tmux didn't tag a panel moa started: can't find pane: %12".to_string()
        )]
    );
}

#[test]
fn outside_the_sandbox_no_moa_root_is_passed() {
    let mut st = linked(&[]);
    let (_, cmds) = sent(&mut st, &[View::Console]);
    assert_eq!(
        cmds,
        [
            "new-window -d -P -F #{pane_id} -c /h -t =_moa: -n console -e MOA_BRAIN=default -e MOA_NO_AUTOSTART=1 \
          /x/moa panel console --brain default"
        ]
    );
}

#[test]
fn a_panels_argv_names_its_view_and_brain() {
    let st = linked(&[]);
    assert_eq!(
        panel_argv(&st, View::EmptySlot),
        ["/x/moa", "panel", "empty-slot", "--brain", "default"]
    );
}

#[test]
fn one_failed_spawn_still_tags_the_others() {
    let mut st = linked(&[]);
    let views = [View::Console, View::Tree, View::Today];
    let (n, cmds) = sent(&mut st, &views);
    assert_eq!(cmds.len(), 3, "one command per panel: {cmds:?}");

    let reply = answer(
        n,
        vec![pane("%1"), Err("create window failed".into()), pane("%3")],
    );
    // What the queue does with a spawn's answer (L17): the link hands it back per command.
    let [
        (
            Asked::Spawn {
                views: asked,
                seq: 7,
            },
            Some(answers),
        ),
    ] = &st.tmux.answered(&reply)[..]
    else {
        panic!("one spawn answered");
    };
    assert_eq!(asked, &views);
    let (fx, failed) = spawned(&mut st, asked, answers.clone());
    assert_eq!(failed, [View::Tree]);
    let mut both = tags("%1", "console", "◆ moa");
    both.extend(tags("%3", "today", "☀ today"));
    assert_eq!(
        tagged(&fx),
        [both],
        "every started pane, in one Tmux effect"
    );
    assert_eq!(st.panel_starts, 2);
    assert_eq!(st.tmux.topology.view_of(PaneId(1)), Some("console"));
    assert_eq!(st.tmux.topology.view_of(PaneId(2)), None);
    assert_eq!(st.tmux.topology.view_of(PaneId(3)), Some("today"));
    assert_eq!(
        logs(&fx),
        [(
            LogLevel::Warn,
            "the tree panel couldn't start: create window failed".to_string()
        )]
    );
}

#[test]
fn an_answer_moa_cant_read_starts_no_panel() {
    let mut st = linked(&[]);
    let views = [View::Tree, View::Plan, View::Status];
    // Two lines, no pane id, and no answer at all for the third.
    let answers = vec![Ok(vec!["%1".into(), "%2".into()]), Ok(vec!["x".into()])];
    let (fx, failed) = spawned(&mut st, &views, answers);
    assert_eq!(failed, views);
    assert!(tagged(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.panel_starts, 0);
    let why = |view: &str, why: &str| {
        (
            LogLevel::Warn,
            format!("the {view} panel couldn't start: {why}"),
        )
    };
    // A window tmux opened is said to be there, with what tmux answered.
    let opened = |view: &str, answer: &str| {
        (
            LogLevel::Warn,
            format!("the {view} panel's window opened, but tmux answered {answer}, not one tab id"),
        )
    };
    assert_eq!(
        logs(&fx),
        [
            opened("tree", r#"["%1", "%2"]"#),
            opened("plan", r#"["x"]"#),
            why("status", "tmux didn't answer"),
        ]
    );
}

/// An error with no words still says why, never ending on a bare colon.
#[test]
fn a_failure_tmux_gave_no_words_for_still_says_why() {
    let mut st = linked(&[]);
    let views = [View::Tree, View::Plan, View::Today];
    let answers = vec![
        Err(String::new()),
        Err("\n".into()),
        Err("  no space for new pane  \nmore".into()),
    ];
    let (fx, failed) = spawned(&mut st, &views, answers);
    assert_eq!(failed, views);
    let why = |view: &str, why: &str| {
        (
            LogLevel::Warn,
            format!("the {view} panel couldn't start: {why}"),
        )
    };
    assert_eq!(
        logs(&fx),
        [
            why("tree", "tmux gave no reason"),
            why("plan", "tmux gave no reason"),
            why("today", "no space for new pane"),
        ]
    );
}

#[test]
fn a_spawn_the_link_lost_tags_nothing() {
    let mut st = linked(&[]);
    sent(&mut st, &[View::Tree, View::Plan]);
    let fx = handle(&mut st, Input::Tmux(TmuxIn::Down(Down::Lost)), now());
    assert!(tagged(&fx).is_empty(), "{fx:?}");
    assert_eq!(st.panel_starts, 0);
    assert_eq!(st.tmux.queries().count(), 0);
    // tmux may have started them all the same: the log says so.
    assert_eq!(
        logs(&fx),
        [(
            LogLevel::Warn,
            "moa lost its terminal before tmux said whether these panels started, so they \
             may be running: tree, plan"
                .to_string()
        )]
    );
}

/// A listing asked before the tags went out (the new window's own notification asks one)
/// doesn't show the view yet: the topology keeps it until one does, or the pane is gone.
#[test]
fn a_listing_from_before_the_tags_keeps_the_view() {
    let mut st = linked(&[]);
    let (n, _) = sent(&mut st, &[View::Tree, View::Plan]);
    handle(
        &mut st,
        Input::Tmux(answer(n, vec![pane("%12"), pane("%13")])),
        now(),
    );
    let listing = |panes: &[&str]| TmuxIn::Replies {
        conn: 1,
        token: TmuxToken::Refresh,
        results: vec![
            Ok(vec!["S $1 _moa".into()]),
            Ok(panes.iter().map(|p| p.to_string()).collect()),
            Ok(Vec::new()),
            Ok(Vec::new()),
        ],
    };
    // Only %12 is listed, untagged; %13 isn't there.
    handle(
        &mut st,
        Input::Tmux(listing(&["P %12 @1 $1 1 1 0 0  0 "])),
        now(),
    );
    let topology = &st.tmux.topology;
    assert_eq!(topology.view_of(PaneId(12)), Some("tree"));
    assert_eq!(topology.view_of(PaneId(13)), None);
    // A listing that shows a tag takes over from then on.
    handle(
        &mut st,
        Input::Tmux(listing(&["P %12 @1 $1 1 1 0 0  0 plan"])),
        now(),
    );
    assert_eq!(st.tmux.topology.view_of(PaneId(12)), Some("plan"));
    handle(
        &mut st,
        Input::Tmux(listing(&["P %12 @1 $1 1 1 0 0  0 "])),
        now(),
    );
    assert_eq!(st.tmux.topology.view_of(PaneId(12)), None);
}

#[test]
fn status_reports_panel_starts() {
    let mut st = linked(&[]);
    let status = |st: &State| st.status(&CallerInfo::default(), UtcMs(1_000_000));
    assert_eq!(status(&st).panel_starts, 0);
    let (n, _) = sent(&mut st, &[View::Tree, View::Plan]);
    handle(
        &mut st,
        Input::Tmux(answer(n, vec![pane("%12"), pane("%13")])),
        now(),
    );
    assert_eq!(status(&st).panel_starts, 2);
}
