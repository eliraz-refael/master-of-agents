//! `project show|add` on the typical brain (today is Sunday 2026-11-15, noon in New York).

use std::path::{Path, PathBuf};

use jiff::civil::date;
use moa_core::Now;
use moa_core::change::Change;
use moa_core::config::{BrainName, Config};
use moa_core::effect::{Effect, Input, Publish, handle_checked, reply_of};
use moa_core::model::{ProjectId, ProjectStatus, SessionId};
use moa_core::op::{ConnId, Done, Op, ReqId};
use moa_core::publish::CoreEvent;
use moa_core::state::{CallerInfo, State};
use moa_testkit::brain::{TypicalBrain, run};
use moa_testkit::ids::*;

fn typical() -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    let st = b.state();
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

fn activity_texts(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Persist(c) => Some(c.iter()),
            _ => None,
        })
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

fn persisted(fx: &[Effect]) -> bool {
    fx.iter().any(|e| matches!(e, Effect::Persist(_)))
}

fn show(name: &str) -> Op {
    Op::ProjectShow { name: name.into() }
}

fn add(name: &str, path: &Path, summary: Option<&str>) -> Op {
    Op::ProjectAdd {
        name: name.into(),
        path: path.to_path_buf(),
        summary: summary.map(Into::into),
    }
}

fn sid(st: &State, conv: &str) -> SessionId {
    st.resolve(conv).unwrap()
}

// ---------- show ----------

#[test]
fn project_show_matches_the_spike_layout() {
    let (b, mut st) = typical();
    let d = ok(&mut st, show("Lantern"), &b.now());
    // The temp dir is random; the layout is what the snapshot pins.
    let text = d.text.replace(b.home.to_str().unwrap(), "<home>");
    assert!(
        text.starts_with(
            "Lantern [active] <home>/private/lantern  last activity 2026-11-15\nA small lamp controller\n"
        ),
        "{text}"
    );
    insta::assert_snapshot!("project_show_lantern", text);
    assert_eq!(d.data["project"]["name"], "Lantern");
    assert_eq!(d.data["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(d.data["sessions"].as_array().unwrap().len(), 4);
    assert_eq!(d.data["notes"].as_array().unwrap().len(), 2);
}

#[test]
fn project_show_by_substring() {
    let (b, mut st) = typical();
    let now = b.now();
    let d = ok(&mut st, show("tide"), &now);
    let text = d.text.replace(b.home.to_str().unwrap(), "<home>");
    assert!(
        text.starts_with("Tidepool [idea] <home>/private/tidepool  last activity -\n\n"),
        "{text}"
    );
    assert!(text.contains("\n  task #3 Survey the tide flats [blocked] blocked on a permit"));
    assert!(text.contains("\n  note (thread-summary) Talked through the harbour layout"));
    // An exact name beats a substring match; case doesn't matter.
    assert!(
        ok(&mut st, show("DRIFTWOOD"), &now)
            .text
            .starts_with("Driftwood [paused] -  last activity 2026-10-20\nShelved for now")
    );
    // The lowest id wins among several substring matches.
    assert!(ok(&mut st, show("t"), &now).text.starts_with("Lantern "));
}

#[test]
fn no_project_matches() {
    let (b, mut st) = typical();
    assert_eq!(
        err(&mut st, show("nope"), &b.now()),
        "no project matches 'nope'"
    );
    assert_eq!(err(&mut st, show(""), &b.now()), "no project matches ''");
}

// ---------- add ----------

/// The Typical sessions all work in `<home>/private/lantern`.
fn lantern(b: &TypicalBrain) -> PathBuf {
    b.home.join("private").join("lantern")
}

#[test]
fn project_add_links_sessions_and_logs() {
    let (b, mut st) = typical();
    let now = b.now();
    assert!(
        st.sessions()
            .all(|s| s.cwd.as_deref() == lantern(&b).to_str())
    );
    let harbour = lantern(&b).join("harbour");
    let fx = effects(&mut st, add("Harbour", &harbour, Some("Boats")), &now);
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap().text,
        format!("project Harbour added for {}", harbour.display())
    );
    assert_eq!(
        ui_texts(&fx),
        [format!("new project Harbour ({})", harbour.display())]
    );
    assert!(activity_texts(&fx).contains(&"new project Harbour (active)".to_string()));
    let p = st.projects().find(|p| p.name == "Harbour").unwrap();
    assert_eq!(p.id.0, 4);
    assert_eq!(p.status, ProjectStatus::Active);
    assert_eq!(p.summary.as_deref(), Some("Boats"));
    assert_eq!(p.last_activity_on, Some(date(2026, 11, 15)));
    assert_eq!((p.created_at, p.updated_at), (now.wall, now.wall));
    assert_eq!(p.path.as_deref(), harbour.to_str());

    // A fresh brain: the whole of `private` takes the three sessions with no project.
    let (b, mut st) = typical();
    let private = b.home.join("private");
    let fx = effects(&mut st, add("Shore", &private, None), &now);
    let d = reply_of(&fx).unwrap().as_ref().unwrap();
    assert_eq!(
        d.text,
        format!(
            "project Shore added for {}; 3 sessions linked",
            private.display()
        )
    );
    let want: Vec<SessionId> = [S2_IDLE, S7_ELSEWHERE, S8_CODEX]
        .iter()
        .map(|c| sid(&st, c))
        .collect();
    let mut got: Vec<SessionId> = serde_json::from_value(d.data["linked"].clone()).unwrap();
    got.sort();
    let mut want_sorted = want.clone();
    want_sorted.sort();
    assert_eq!(got, want_sorted);
    let texts = activity_texts(&fx);
    for c in [S2_IDLE, S7_ELSEWHERE, S8_CODEX] {
        let line = format!("linked session {} → Shore", &c[..8]);
        assert_eq!(texts.iter().filter(|t| **t == line).count(), 1, "{line}");
        assert_eq!(
            st.session(sid(&st, c)).unwrap().project_id.map(|p| p.0),
            Some(4)
        );
    }
    // S1 keeps Lantern.
    assert_eq!(
        st.session(sid(&st, S1_WORKING))
            .unwrap()
            .project_id
            .map(|p| p.0),
        Some(1)
    );
    assert!(texts.contains(&"new project Shore (active)".to_string()));
    assert_eq!(
        ui_texts(&fx),
        [format!("new project Shore ({})", private.display())]
    );
    assert_eq!(texts.len(), 5);
}

#[test]
fn one_linked_session_reads_in_the_singular() {
    let b = TypicalBrain::new();
    let mut snapshot = b.snapshot();
    // Only S2 is left without a project.
    let s2 = b.state().resolve(S2_IDLE).unwrap();
    for s in &mut snapshot.sessions {
        if s.id != s2 && s.project_id.is_none() {
            s.project_id = Some(ProjectId(1));
        }
    }
    let mut st = State::new(BrainName::default_brain(), b.config());
    handle_checked(&mut st, Input::Startup(Box::new(snapshot)), b.now());
    let path = b.home.join("private");
    let text = ok(&mut st, add("Shore", &path, None), &b.now()).text;
    assert_eq!(
        text,
        format!(
            "project Shore added for {}; 1 session linked",
            path.display()
        )
    );
}

#[test]
fn project_add_checks_the_folder_by_whole_components() {
    let (b, mut st) = typical();
    let now = b.now();
    let outside = b.home.join("private2").join("x");
    let fx = effects(&mut st, add("Out", &outside, None), &now);
    assert_eq!(
        reply_of(&fx).unwrap().as_ref().unwrap_err().0,
        format!(
            "{} isn't in this brain's folders; this brain doesn't track it",
            outside.display()
        )
    );
    assert!(!persisted(&fx));
    assert!(st.projects().all(|p| p.name != "Out"));
    // Not absolute, or climbing out with `..`, doesn't count either.
    for bad in [
        PathBuf::from("private/x"),
        b.home.join("private/../private2"),
    ] {
        assert!(
            err(&mut st, add("Out", &bad, None), &now).ends_with("this brain doesn't track it")
        );
    }
    // The folder itself is fine.
    let private = b.home.join("private");
    assert!(run(&mut st, add("Whole", &private, None), &now).is_ok());
}

#[test]
fn a_brain_with_no_folders_says_so() {
    let b = TypicalBrain::new();
    let mut st = State::new(BrainName::default_brain(), Config::default());
    handle_checked(&mut st, Input::Startup(Box::new(b.snapshot())), b.now());
    assert_eq!(
        err(
            &mut st,
            add("Any", &b.home.join("private/a"), None),
            &b.now()
        ),
        "the default brain has no folders yet; add them to moa.toml"
    );
}

#[test]
fn project_add_refuses_duplicates() {
    let (b, mut st) = typical();
    let now = b.now();
    let fresh = b.home.join("private").join("fresh");
    let e = format!(
        "there's already a project named 'lantern' or for {}",
        fresh.display()
    );
    assert_eq!(err(&mut st, add("lantern", &fresh, None), &now), e);
    // The same name in other letters, and in upper case.
    assert!(err(&mut st, add("TIDEPOOL", &fresh, None), &now).starts_with("there's already"));
    let theirs = lantern(&b);
    assert_eq!(
        err(&mut st, add("New name", &theirs, None), &now),
        format!(
            "there's already a project named 'New name' or for {}",
            theirs.display()
        )
    );
    assert_eq!(st.projects().count(), 3);
    // A refusal changes nothing: the same add with a free name and path works.
    assert!(run(&mut st, add("Fresh", &fresh, None), &now).is_ok());
}

#[test]
fn a_project_needs_a_name() {
    let (b, mut st) = typical();
    assert_eq!(
        err(
            &mut st,
            add("  ", &b.home.join("private/z"), None),
            &b.now()
        ),
        "a project needs a name"
    );
}
