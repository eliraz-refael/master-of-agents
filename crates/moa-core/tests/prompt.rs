//! `Ctrl-a n` (S6): the folder, then the title, asked in tmux's status line one at a time
//! (S5), each answer back through the mailbox; then the session opens as `moactl open` would.

mod common;

use std::fs;
use std::path::Path;

use moa_core::change::Change;
use moa_core::effect::{Effect, LogLine, TmuxIn, TmuxToken};
use moa_core::model::SessionId;
use moa_core::state::{Deps, State};
use moa_core::tmux::cmd::Cmd;
use moa_core::tmux::ids::ClientName;
use moa_core::tmux::notify::Notification;
use moa_testkit::brain::TypicalBrain;

use common::*;

/// Brain `default` (folder `~/private`, opening with `acct-a`).
const TOML: &str = r#"
[brains.default]
folders = ["~/private"]
open_account = "acct-a"

[accounts.acct-a]
kind = "claude"
store = "~/.claude-a"
launcher = "/opt/claude"
"#;

const KEY: &str = "moa:key new %3 $0 /dev/pts/3";
const NOTHING_WAITS: &str = "a prompt answer arrived that nothing waits for";

fn client() -> ClientName {
    ClientName("/dev/pts/3".into())
}

fn ctl() -> ClientName {
    ClientName("client-9".into())
}

/// The typical brain under `toml`, with real folder checks, linked on tmux `version`.
fn linked(toml: &str, version: &str) -> (TypicalBrain, State) {
    let b = TypicalBrain::new();
    fs::create_dir_all(b.home.join("private")).unwrap();
    let deps = Deps {
        session_plugin: PLUGIN.into(),
        home: b.home.clone(),
        is_dir: Path::is_dir,
        ..Deps::default()
    };
    let snapshot = b.snapshot();
    let mut st = loaded_with(&b, toml, deps, snapshot);
    connect(&b, &mut st, 1, version);
    (b, st)
}

fn connect(b: &TypicalBrain, st: &mut State, conn: u64, version: &str) {
    feed(
        b,
        st,
        TmuxIn::Connected {
            conn,
            generation: generation(),
            version: version.into(),
            ctl: ctl(),
            socket: "moa".into(),
        },
    );
}

fn message(b: &TypicalBrain, st: &mut State, text: &str) -> Vec<Effect> {
    feed(
        b,
        st,
        TmuxIn::Notification(Notification::Message(text.into())),
    )
}

fn key(b: &TypicalBrain, st: &mut State) -> Vec<Effect> {
    message(b, st, KEY)
}

/// The prompts sent in `fx`, each with its token.
fn prompts(fx: &[Effect]) -> Vec<(u64, Cmd)> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds, token: None, ..
            } => Some(cmds),
            _ => None,
        })
        .flatten()
        .filter(|c| c.name() == "command-prompt")
        .map(|c| {
            let template = c.argv().last().unwrap();
            let token = template
                .split("moa:prompt ")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .and_then(|t| t.parse().ok())
                .unwrap_or_else(|| panic!("{template}"));
            (token, c.clone())
        })
        .collect()
}

/// The token of the one prompt sent in `fx`, which asks `label`, prefilled with `prefill`.
fn the_prompt(fx: &[Effect], label: &str, prefill: &str) -> u64 {
    match &prompts(fx)[..] {
        [(token, cmd)] => {
            assert_eq!(*cmd, Cmd::prompt(&client(), label, prefill, &ctl(), *token));
            *token
        }
        other => panic!("not one prompt: {other:?}"),
    }
}

/// The notices `fx` shows the key's client.
fn notices(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Tmux {
                cmds, token: None, ..
            } => Some(cmds),
            _ => None,
        })
        .flatten()
        .filter(|c| c.name() == "display-message" && c.argv().get(1) == Some(&client().0))
        .map(|c| c.argv().last().unwrap().clone())
        .collect()
}

fn opens(fx: &[Effect]) -> usize {
    fx.iter()
        .filter(|e| {
            matches!(
                e,
                Effect::Tmux {
                    token: Some(TmuxToken::Open(_)),
                    ..
                }
            )
        })
        .count()
}

fn logged(fx: &[Effect], text: &str) -> bool {
    fx.iter()
        .any(|e| matches!(e, Effect::Log(LogLine { message, .. }) if message == text))
}

fn answer(b: &TypicalBrain, st: &mut State, token: u64, text: &str) -> Vec<Effect> {
    message(b, st, &format!("moa:prompt {token} {text}"))
}

/// `Ctrl-a n`, answered `folder`: the title prompt's token.
fn to_title(b: &TypicalBrain, st: &mut State, folder: &str) -> u64 {
    let fx = key(b, st);
    let token = the_prompt(&fx, "new session · folder:", "~/private/");
    let fx = answer(b, st, token, folder);
    the_prompt(&fx, "title:", "")
}

#[test]
fn ctrl_a_n_asks_the_folder_with_the_brains_first_folder() {
    let (b, mut st) = linked(TOML, "3.8");
    let fx = key(&b, &mut st);
    let tmux: Vec<&Effect> = fx
        .iter()
        .filter(|e| matches!(e, Effect::Tmux { .. }))
        .collect();
    assert_eq!(tmux.len(), 1, "{fx:?}");
    let token = the_prompt(&fx, "new session · folder:", "~/private/");
    assert_eq!(
        Cmd::prompt(
            &client(),
            "new session · folder:",
            "~/private/",
            &ctl(),
            token
        )
        .line(),
        format!(
            "command-prompt -b -t /dev/pts/3 -p 'new session · folder:' -I '~/private/' \
             'display-message -c client-9 -l \"moa:prompt {token} %%%\"'"
        )
    );
    assert!(notices(&fx).is_empty(), "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
}

#[test]
fn a_missing_folder_is_refused_and_not_made() {
    let (b, mut st) = linked(TOML, "3.8");
    let token = the_prompt(&key(&b, &mut st), "new session · folder:", "~/private/");
    let fx = answer(&b, &mut st, token, "~/nope");
    assert_eq!(notices(&fx), ["moa: no folder ~/nope"]);
    assert!(prompts(&fx).is_empty(), "{fx:?}");
    assert_eq!(opens(&fx), 0, "{fx:?}");
    assert!(changes(&fx).is_empty(), "{fx:?}");
    assert!(!b.home.join("nope").exists());
    // That ended it: the same answer again finds nothing waiting.
    let fx = answer(&b, &mut st, token, "~/private");
    assert!(prompts(&fx).is_empty(), "{fx:?}");
    assert!(logged(&fx, NOTHING_WAITS), "{fx:?}");
}

#[test]
fn a_folder_needs_tilde_or_slash() {
    let (b, mut st) = linked(TOML, "3.8");
    let token = the_prompt(&key(&b, &mut st), "new session · folder:", "~/private/");
    let fx = answer(&b, &mut st, token, "private");
    assert_eq!(notices(&fx), ["moa: a folder starts with ~ or /"]);
    assert!(prompts(&fx).is_empty(), "{fx:?}");
    assert_eq!(opens(&fx), 0, "{fx:?}");
}

/// The session `fx` opened: its id, after checking one `new-window` went out.
fn opened(fx: &[Effect]) -> SessionId {
    let (id, cmds) = the_open(fx);
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].name(), "new-window");
    id
}

#[test]
fn the_title_comes_second_and_opens() {
    let (b, mut st) = linked(TOML, "3.8");
    let token = to_title(&b, &mut st, "~/private");
    let fx = answer(&b, &mut st, token, "kite work");
    let id = opened(&fx);
    assert!(notices(&fx).is_empty(), "{fx:?}");
    let s = st.session(id).unwrap();
    let folder = b.home.join("private").display().to_string();
    assert_eq!(s.cwd.as_deref(), Some(folder.as_str()));
    assert_eq!(
        (s.title.as_deref(), s.title_locked, s.group_path.as_deref()),
        (Some("kite work"), true, None)
    );
    assert_eq!(s.account.as_deref(), Some("acct-a"));
    assert!(changes(&fx).iter().any(|c| matches!(c, Change::Session(_))));

    // tmux says which tab it has: the key's client is told by notice, and nobody by reply.
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(id),
            results: vec![Ok(vec!["P %7 4242".into()])],
        },
    );
    assert_eq!(
        notices(&fx),
        [format!("moa: opened kite work with acct-a in {folder}")]
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    assert!(st.binding(id).is_some());
    // As `moactl open` without `--bg`: the tab comes to the screen. This brain has no
    // cockpit, so the select goes straight out.
    let select = Cmd::new("select-window").args(["-t", "%7"]);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Tmux { cmds, .. } if cmds.contains(&select))),
        "{fx:?}"
    );
}

#[test]
fn an_empty_folder_answer_is_cancelled() {
    // With tmux's trailing space, and without it.
    for line in ["moa:prompt {t} ", "moa:prompt {t}", "moa:prompt {t}   "] {
        let (b, mut st) = linked(TOML, "3.8");
        let token = the_prompt(&key(&b, &mut st), "new session · folder:", "~/private/");
        let fx = message(&b, &mut st, &line.replace("{t}", &token.to_string()));
        assert_eq!(notices(&fx), ["moa: cancelled"], "{line:?}");
        assert!(prompts(&fx).is_empty(), "{fx:?}");
        assert_eq!(opens(&fx), 0, "{fx:?}");
        assert!(changes(&fx).is_empty(), "{fx:?}");
    }
}

#[test]
fn an_empty_title_opens_without_one() {
    let (b, mut st) = linked(TOML, "3.8");
    let token = to_title(&b, &mut st, "~/private/");
    // tmux may trim the space after the token.
    let fx = message(&b, &mut st, &format!("moa:prompt {token}"));
    let s = st.session(opened(&fx)).unwrap();
    assert_eq!(
        (s.title.as_deref(), s.title_locked),
        (Some("private"), false)
    );
}

#[test]
fn an_empty_answer_with_tmuxs_trailing_space_is_empty() {
    let (b, mut st) = linked(TOML, "3.8");
    let token = to_title(&b, &mut st, "~/private");
    let fx = answer(&b, &mut st, token, "");
    let s = st.session(opened(&fx)).unwrap();
    assert_eq!(
        (s.title.as_deref(), s.title_locked),
        (Some("private"), false)
    );
    // Trailing spaces never reach a title.
    let token = to_title(&b, &mut st, "~/private  ");
    let fx = answer(&b, &mut st, token, "kite  ");
    let s = st.session(opened(&fx)).unwrap();
    assert_eq!(s.title.as_deref(), Some("kite"));
}

/// tmux ignores a `command-prompt` while one is open on the client: the answer that comes
/// may be the first's.
#[test]
fn a_second_prompt_keeps_the_first_token() {
    let (b, mut st) = linked(TOML, "3.8");
    let first = the_prompt(&key(&b, &mut st), "new session · folder:", "~/private/");
    let second = the_prompt(&key(&b, &mut st), "new session · folder:", "~/private/");
    assert_ne!(first, second);
    let fx = answer(&b, &mut st, first, "~/private");
    the_prompt(&fx, "title:", "");
    assert!(!logged(&fx, NOTHING_WAITS), "{fx:?}");
}

#[test]
fn seventeen_prompts_drop_the_oldest() {
    let (b, mut st) = linked(TOML, "3.8");
    let tokens: Vec<u64> = (0..17)
        .map(|_| the_prompt(&key(&b, &mut st), "new session · folder:", "~/private/"))
        .collect();
    let fx = answer(&b, &mut st, tokens[0], "~/private");
    assert!(prompts(&fx).is_empty(), "{fx:?}");
    assert!(logged(&fx, NOTHING_WAITS), "{fx:?}");
    let fx = answer(&b, &mut st, tokens[1], "~/private");
    the_prompt(&fx, "title:", "");
}

/// The templates name the old control client: a new connection forgets every prompt.
#[test]
fn a_reconnect_clears_the_prompts() {
    let (b, mut st) = linked(TOML, "3.8");
    let token = the_prompt(&key(&b, &mut st), "new session · folder:", "~/private/");
    connect(&b, &mut st, 2, "3.8");
    let fx = answer(&b, &mut st, token, "~/private");
    assert!(prompts(&fx).is_empty(), "{fx:?}");
    assert!(logged(&fx, NOTHING_WAITS), "{fx:?}");
}

#[test]
fn a_key_started_open_that_tmux_refuses_is_told_by_notice() {
    let (b, mut st) = linked(TOML, "3.8");
    let token = to_title(&b, &mut st, "~/private");
    let id = opened(&answer(&b, &mut st, token, "kite"));
    let fx = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(id),
            results: vec![Err("create window failed: index in use".to_string())],
        },
    );
    assert_eq!(
        notices(&fx),
        ["moa: couldn't open a tab for it; see moa's log"]
    );
    assert!(replies(&fx).is_empty(), "{fx:?}");
    // Told once: nothing waits for the tab any more, so a second answer tells nobody.
    let again = feed(
        &b,
        &mut st,
        TmuxIn::Replies {
            conn: 1,
            token: TmuxToken::Open(id),
            results: vec![Ok(vec!["P %8 4243".into()])],
        },
    );
    assert!(notices(&again).is_empty(), "{again:?}");
    assert!(replies(&again).is_empty(), "{again:?}");
    assert!(st.binding(id).is_none());
}

/// tmux 3.4 alone stores a typed `$` as `\$`: only there does moa take it back.
#[test]
fn dollar_unescapes_only_on_3_4() {
    for (version, want) in [
        ("3.4", "$HOME"),
        ("next-3.4", "$HOME"),
        ("3.8", r"\$HOME"),
        ("3.5a", r"\$HOME"),
    ] {
        let (b, mut st) = linked(TOML, version);
        let token = to_title(&b, &mut st, "~/private");
        let fx = answer(&b, &mut st, token, r"\$HOME");
        let s = st.session(opened(&fx)).unwrap();
        assert_eq!(s.title.as_deref(), Some(want), "{version}");
    }
}

/// `-I` splits on `,` and expands `#{…}`: such a folder isn't offered.
#[test]
fn a_prefill_with_a_comma_falls_back() {
    for folder in ["~/a,b", "~/a#b"] {
        let toml = TOML.replace("~/private", folder);
        let (b, mut st) = linked(&toml, "3.8");
        the_prompt(&key(&b, &mut st), "new session · folder:", "~/");
    }
}

/// A brain with no folders offers home.
#[test]
fn no_folder_prefills_home() {
    let toml = TOML.replace(r#"folders = ["~/private"]"#, "folders = []");
    let (b, mut st) = linked(&toml, "3.8");
    the_prompt(&key(&b, &mut st), "new session · folder:", "~/");
}
