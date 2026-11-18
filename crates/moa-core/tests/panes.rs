//! Pane listings as reconcile reads them: `PANE_INFO` and `PANE_START` lines, tmux's quoting
//! of a start command, R4's test for a binding that still holds, and R32's stores.

use std::path::{Path, PathBuf};

use moa_core::UtcMs;
use moa_core::config::{AccountName, BrainName, Config};
use moa_core::model::{PaneBinding, SessionId};
use moa_core::state::{Deps, State};
use moa_core::tmux::ids::{Generation, PaneId};
use moa_core::tmux::panes::{
    PANE_INFO, PANE_START, PaneInfo, Tag, holds, list_panes, parse_panes, split_start_command,
};

const SID: &str = "00000000-0000-4000-8000-000000000001";
const OTHER: &str = "00000000-0000-4000-8000-000000000002";

fn sid() -> SessionId {
    SID.parse().unwrap()
}

fn lines(l: &[&str]) -> Vec<String> {
    l.iter().map(|s| s.to_string()).collect()
}

fn words(w: &[&str]) -> Vec<String> {
    lines(w)
}

fn pane(n: u32) -> PaneId {
    PaneId(n)
}

#[test]
fn the_listings_put_the_folder_and_the_start_command_last() {
    assert!(PANE_INFO.starts_with("I\t#{pane_id}\t"));
    assert!(PANE_INFO.ends_with("\t#{pane_current_path}"));
    assert_eq!(PANE_START, "S\t#{pane_id}\t#{pane_start_command}");
    let cmds = list_panes();
    let argv: Vec<&[String]> = cmds.iter().map(|c| c.argv()).collect();
    assert_eq!(
        argv,
        [
            &words(&["-a", "-F", PANE_INFO])[..],
            &words(&["-a", "-F", PANE_START])
        ]
    );
    assert!(
        cmds.iter()
            .all(|c| c.name() == "list-panes" && c.check().is_ok())
    );
}

#[test]
fn pane_info_parses_with_empty_options_and_spaces_in_the_folder() {
    let info = lines(&[
        "I\t%1\t101\t0\t\t\t\t/h/private/a b",
        &format!("I\t%2\t102\t1\t1\t\t{SID}\t/h"),
        "I\t%3\t103\t0\t\t1\tjunk\t/h/tab\there",
    ]);
    let start = lines(&[
        "S\t%1\t/opt/claude --resume abc",
        "S\t%2\t",
        "S\t%3\t\"a b\" c",
    ]);
    assert_eq!(
        parse_panes(&info, &start),
        [
            PaneInfo {
                pane: pane(1),
                pid: 101,
                dead: false,
                term: false,
                adopted: false,
                tag: Tag::Unset,
                cwd: "/h/private/a b".into(),
                start: words(&["/opt/claude", "--resume", "abc"]),
            },
            PaneInfo {
                pane: pane(2),
                pid: 102,
                dead: true,
                term: true,
                adopted: false,
                tag: Tag::Session(sid()),
                cwd: "/h".into(),
                start: Vec::new(),
            },
            PaneInfo {
                pane: pane(3),
                pid: 103,
                dead: false,
                term: false,
                adopted: true,
                tag: Tag::Other,
                cwd: "/h/tab\there".into(),
                start: words(&["a b", "c"]),
            },
        ]
    );
}

#[test]
fn a_start_line_that_doesnt_parse_loses_only_its_pane() {
    let info = lines(&[
        "I\t%1\t101\t0\t\t\t\t/h",
        "I\t%2\t102\t0\t\t\t\t/h",
        "I\t%3\t103\t0\t\t\t\t/h",
        // Lines that don't parse are skipped.
        "I\t%x\t104\t0\t\t\t\t/h",
        "I\t%5\t+105\t0\t\t\t\t/h",
        "I\t%6\t106\t0\t\t\t",
        "P %7 @1 $0 1 1 0 0 ",
        "",
    ]);
    let start = lines(&[
        "junk",
        "S %1 /bin/sh",
        "S\t%2\tbash -l",
        "S\t%3\t\"unbalanced",
    ]);
    let got = parse_panes(&info, &start);
    let summary: Vec<(PaneId, Vec<String>)> = got.into_iter().map(|p| (p.pane, p.start)).collect();
    assert_eq!(
        summary,
        [
            (pane(1), Vec::new()),
            (pane(2), words(&["bash", "-l"])),
            (pane(3), Vec::new()),
        ]
    );
}

/// How tmux 3.4+ prints each argument (`args_escape`), and the argument it was.
#[test]
fn split_start_command_reads_tmux_quoting() {
    let cases: &[(&str, &[&str])] = &[
        (
            "/opt/claude --resume abc",
            &["/opt/claude", "--resume", "abc"],
        ),
        (r#""a b""#, &["a b"]),
        (r#""it's""#, &["it's"]),
        (r#"'x"y'"#, &[r#"x"y"#]),
        (r#""say \"hi\"""#, &[r#"say "hi""#]),
        (r##""#c""##, &["#c"]),
        (r"\#", &["#"]),
        (r"one\ntwo", &["one\ntwo"]),
        (r#""\$HOME""#, &["$HOME"]),
        (r"a\\b", &[r"a\b"]),
        (r"\001x", &["\u{1}x"]),
        (r"\t\r\b\a\v\f\0", &["\t\r\u{8}\u{7}\u{b}\u{c}\0"]),
        (r"\303\251", &["é"]),
        (r"\~/x", &["~/x"]),
        (r#""\~ y""#, &["~ y"]),
        ("''", &[""]),
        ("a  b", &["a", "b"]),
        ("", &[]),
        // Not what tmux prints: nothing is trusted.
        (r#""unbalanced"#, &[]),
        ("'unbalanced", &[]),
        (r"trailing\", &[]),
    ];
    for (line, want) in cases {
        assert_eq!(split_start_command(line), words(want), "{line}");
    }
}

fn binding(generation: Generation) -> PaneBinding {
    PaneBinding {
        session_id: sid(),
        server_pid: generation.pid,
        server_start: generation.start,
        socket: "moa".into(),
        pane_id: "%3".into(),
        pane_pid: 99,
        bound_at: UtcMs(1),
        tracked: true,
    }
}

fn listed(tag: Tag, dead: bool) -> PaneInfo {
    PaneInfo {
        pane: pane(3),
        pid: 99,
        dead,
        term: false,
        adopted: false,
        tag,
        cwd: "/h".into(),
        start: Vec::new(),
    }
}

#[test]
fn holds_follows_r4() {
    let g = Generation {
        pid: 10,
        start: 1000,
    };
    let b = binding(g);
    let other = OTHER.parse().unwrap();
    assert!(holds(&b, g, Some(&listed(Tag::Unset, false))));
    assert!(holds(&b, g, Some(&listed(Tag::Session(sid()), false))));
    // Another session's tag, a tag that isn't a session, a dead pane, or no pane at all.
    assert!(!holds(&b, g, Some(&listed(Tag::Session(other), false))));
    assert!(!holds(&b, g, Some(&listed(Tag::Other, false))));
    assert!(!holds(&b, g, Some(&listed(Tag::Unset, true))));
    assert!(!holds(&b, g, None));
    // Another server's pane ids name other panes.
    for g2 in [
        Generation {
            pid: 10,
            start: 1001,
        },
        Generation {
            pid: 11,
            start: 1000,
        },
    ] {
        assert!(!holds(&b, g2, Some(&listed(Tag::Unset, false))), "{g2:?}");
    }
    // The listing must be of the binding's own pane.
    let elsewhere = PaneInfo {
        pane: pane(4),
        ..listed(Tag::Unset, false)
    };
    assert!(!holds(&b, g, Some(&elsewhere)));
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// R32: no `CLAUDE_CONFIG_DIR` means `<home>/.claude`, and an account's store is found by its
/// canonical folder, however either was spelled.
#[test]
fn store_of_maps_no_variable_to_the_default_folder() {
    // Under /tmp, which is a link on macOS: the config's spelling isn't the canonical one.
    let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".claude-b")).unwrap();
    let toml = r#"
[accounts.mine]
kind = "claude"
store = "~/.claude"

[accounts.acct-b]
kind = "claude"
store = "~/.claude-b"
"#;
    let config = Config::parse(toml, &home).unwrap();
    let deps = Deps {
        canonical,
        home: home.clone(),
        ..Deps::default()
    };
    let st = State::new(BrainName::default_brain(), config).with_deps(deps);

    let default = st.store_of(None);
    assert_eq!(default, canonical(&home.join(".claude")));
    let spelled = format!("{}/.claude/", home.display());
    assert_eq!(st.store_of(Some(&spelled)), default);
    assert_eq!(st.store_of(Some("")), default);

    let mine = AccountName::parse("mine").unwrap();
    assert_eq!(st.account_of_store(&default), Some(mine.clone()));
    assert_eq!(st.account_of_store(&home.join(".claude")), Some(mine));
    let b = st.store_of(Some(&format!("{}/.claude-b", home.display())));
    assert_eq!(st.account_of_store(&b), AccountName::parse("acct-b").ok());
    assert_eq!(st.account_of_store(&home.join(".claude-c")), None);
}
