//! Running `moa` against a TempBrain: on a terminal, as a user would, or to the end. Every
//! tmux query goes through `TempBrain::tmux` (T23).
#![allow(dead_code)]

use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use moa_testkit::{FakeStep, TempBrain, Terminal, fake_claude, wait_for, write_script};
use serde_json::{Map, Value};

pub mod screen;

pub const MOA: &str = env!("CARGO_BIN_EXE_moa");
pub const B: &str = "default";
/// What the status line shows once moa's config is in.
pub const STATUS_LINE: &str = "^a keys";
/// How long a wait for something to show may take: every wait returns as soon as it does,
/// so this only lengthens a failure.
pub const SHOWS: Duration = Duration::from_secs(10);

/// `moa args…` on a terminal, with the root's environment plus `extra`.
pub fn open(tb: &TempBrain, args: &[&str], extra: &[(&str, &str)]) -> Terminal {
    let mut env = tb.env();
    env.extend(
        extra
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    );
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    Terminal::spawn(Path::new(MOA), &args, &env, 120, 30).unwrap()
}

/// `exe args…` to the end, with the root's environment plus `extra`.
pub fn run_exe(tb: &TempBrain, exe: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
    Command::new(exe)
        .args(args)
        .env_clear()
        .envs(tb.env())
        .envs(extra.iter().copied())
        .current_dir(tb.root())
        .timeout(Duration::from_secs(30))
        .output()
        .unwrap()
}

/// `moa args…` to the end, with the root's environment plus `extra`.
pub fn run(tb: &TempBrain, args: &[&str], extra: &[(&str, &str)]) -> Output {
    run_exe(tb, Path::new(MOA), args, extra)
}

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

pub fn tmux(tb: &TempBrain, args: &[&str]) -> String {
    tb.tmux(B, args)
        .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"))
}

/// The session each user's terminal (not a control client) is on.
pub fn user_clients(tb: &TempBrain) -> Vec<String> {
    let list = tmux(
        tb,
        &[
            "list-clients",
            "-F",
            "#{client_control_mode} #{session_name}",
        ],
    );
    list.lines()
        .filter_map(|l| l.strip_prefix("0 "))
        .map(str::to_string)
        .collect()
}

pub fn server_pid(tb: &TempBrain) -> String {
    tmux(tb, &["display-message", "-p", "#{pid}"])
}

pub fn status_json(tb: &TempBrain) -> Value {
    let o = run(tb, &["status", "--json"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    serde_json::from_str(&out(&o)).unwrap()
}

pub fn shutdown(tb: &TempBrain) {
    let o = run(tb, &["shutdown"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
}

/// `moactl`, linked to `moa` as an install would.
pub fn moactl(tb: &TempBrain) -> PathBuf {
    let bin = tb.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("moactl");
    if !path.exists() {
        symlink(MOA, &path).unwrap();
    }
    path
}

/// Waits until `term` shows the status line and the daemon reports its terminal link. A
/// failure names the caller's line, and shows the terminal and the daemon's log.
#[track_caller]
pub fn came_up(tb: &TempBrain, term: &Terminal) {
    assert!(
        term.wait_for_text(STATUS_LINE, SHOWS),
        "no status line\n{}",
        what_happened(tb, term)
    );
    let linked = wait_for(SHOWS, || status_json(tb)["tmux"].is_object().then_some(()));
    assert!(
        linked.is_some(),
        "not linked: {}\n{}",
        status_json(tb),
        what_happened(tb, term)
    );
}

/// For a failure message: what `term` showed last, what `moa status` says, and the tail of
/// the daemon's log.
pub fn what_happened(tb: &TempBrain, term: &Terminal) -> String {
    let status = Command::new(MOA)
        .arg("status")
        .env_clear()
        .envs(tb.env())
        .current_dir(tb.root())
        .timeout(Duration::from_secs(5))
        .output()
        .map_or_else(|e| e.to_string(), |o| out(&o) + &err(&o));
    format!(
        "the terminal showed: {}\nmoa status: {status}\n{}",
        term.tail(1500),
        tb.log_tail(B, 60)
    )
}

/// The root's `moa.toml`: brain `default` on `<root>/work`, opening sessions with `acct-a`,
/// a Claude account whose store is `<root>/.claude-a` and whose launcher is fake-claude, so
/// the real `claude` is never looked for. Both folders are made.
pub fn moa_toml_with_fake_account(tb: &TempBrain) {
    moa_toml_with_launcher(tb, &fake_claude());
}

/// [`moa_toml_with_fake_account`], with `launcher` as `acct-a`'s launcher.
pub fn moa_toml_with_launcher(tb: &TempBrain, launcher: &Path) {
    let work = tb.root().join("work");
    let store = tb.root().join(".claude-a");
    for dir in [&work, &store] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let toml = format!(
        "[brains.default]\nfolders = [\"{}\"]\nopen_account = \"acct-a\"\n\n\
         [accounts.acct-a]\nkind = \"claude\"\nstore = \"{}\"\nlauncher = \"{}\"\n",
        work.display(),
        store.display(),
        launcher.display()
    );
    let paths = tb.paths();
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(paths.config_file(), toml).unwrap();
}

/// Session `id`'s row in `moa ls --json`, or `None` while it isn't listed.
pub fn ls_row(tb: &TempBrain, id: &str) -> Option<Value> {
    let o = run(tb, &["--json", "ls"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let rows: Value = serde_json::from_str(&out(&o)).unwrap();
    rows.as_array()?.iter().find(|r| r["id"] == id).cloned()
}

/// Session `id`'s `display` in `moa ls --json`, or `None` while it isn't listed.
pub fn ls_display(tb: &TempBrain, id: &str) -> Option<String> {
    ls_row(tb, id)?["display"].as_str().map(str::to_string)
}

/// Waits until the user clients are `want`.
pub fn clients_become(tb: &TempBrain, want: &[&str]) {
    let seen = wait_for(SHOWS, || {
        let c = user_clients(tb);
        (c == want).then_some(())
    });
    assert!(seen.is_some(), "clients: {:?}", user_clients(tb));
}

/// A `moactl term` tab in `dir` where the test types fake-claude (named `claude`) on
/// conversation `conv`, as a user starts an agent by hand: `CLAUDE_CONFIG_DIR` set to
/// `acct-a`'s store, `--resume conv`, and no moa environment, so it sends no hooks. Its
/// script is `dir`'s. Returns the tab's `%N` once the agent has written its transcript (R34),
/// so it is running. The root's `bin/claude` must already be fake-claude
/// (`moa_testkit::fake_claude_named_claude`), as the launcher of a root these tests use is.
///
/// The tab runs the server's `default-shell`, `/bin/sh` here ([`TempBrain::env`]), with
/// `HOME` the root, so no developer's shell config takes part; the typed line is plain POSIX.
#[track_caller]
pub fn fake_shell_agent(tb: &TempBrain, dir: &Path, conv: &str) -> String {
    let shell = tmux(tb, &["show-options", "-gv", "default-shell"]);
    assert_eq!(shell, "/bin/sh", "a terminal tab would run another shell");
    let claude = tb.root().join("bin/claude");
    assert!(claude.exists(), "no fake-claude named claude in the root");
    let o = run_exe(
        tb,
        &moactl(tb),
        &["--json", "term", &dir.display().to_string()],
        &[("MOA_BRAIN", B)],
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    let data: Value = serde_json::from_str(&out(&o)).unwrap();
    let pane = data["pane"]
        .as_str()
        .unwrap_or_else(|| panic!("{data}"))
        .to_string();
    let store = tb.root().join(".claude-a");
    let line = format!(
        "CLAUDE_CONFIG_DIR='{}' '{}' --resume {conv}",
        store.display(),
        claude.display()
    );
    tmux(tb, &["send-keys", "-t", &pane, "-l", &line]);
    tmux(tb, &["send-keys", "-t", &pane, "Enter"]);
    let transcript = store
        .join("projects")
        .join(moa_agents::claude::project_slot(&moa_platform::canonical(
            dir,
        )))
        .join(format!("{conv}.jsonl"));
    let started = wait_for(SHOWS, || transcript.is_file().then_some(()));
    assert!(
        started.is_some(),
        "fake-claude never wrote {}\n{}",
        transcript.display(),
        tab_shows(tb, &pane)
    );
    pane
}

/// For a failure message: what tab `pane` shows, every tab with what it runs, and the tail of
/// the daemon's log.
pub fn tab_shows(tb: &TempBrain, pane: &str) -> String {
    let shown = tb
        .tmux(B, &["capture-pane", "-p", "-t", pane])
        .unwrap_or_else(|e| format!("({e})"));
    let tabs = tb
        .tmux(
            B,
            &[
                "list-panes",
                "-a",
                "-F",
                "#{pane_id} pid=#{pane_pid} dead=#{pane_dead} #{pane_current_command} \
                 session=#{@moa_session} term=#{@moa_term}",
            ],
        )
        .unwrap_or_else(|e| format!("({e})"));
    format!(
        "tab {pane} shows:\n{}\ntabs:\n{tabs}\n{}",
        shown.trim_end(),
        tb.log_tail(B, 60)
    )
}

/// `format` for the client on `term`'s pty, or `None` while it isn't attached.
pub fn client_field(tb: &TempBrain, term: &Terminal, format: &str) -> Option<String> {
    let list = tmux(
        tb,
        &["list-clients", "-F", &format!("#{{client_name}} {format}")],
    );
    let prefix = format!("{} ", term.tty());
    list.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .map(str::to_string)
}

/// Waits until `term`'s client is in key table `table`.
#[track_caller]
pub fn key_table_becomes(tb: &TempBrain, term: &Terminal, table: &str) {
    let seen = wait_for(SHOWS, || {
        (client_field(tb, term, "#{client_key_table}").as_deref() == Some(table)).then_some(())
    });
    assert!(
        seen.is_some(),
        "the keys never became {table}: {:?}",
        client_field(tb, term, "#{client_key_table}")
    );
}

/// Ctrl-a, once the last key is done, then waits for moa's keys. Ctrl-a also clears the last
/// notice, so the mark that follows sees only what the next key draws.
#[track_caller]
pub fn ctrl_a(tb: &TempBrain, term: &Terminal) {
    key_table_becomes(tb, term, "root");
    term.press(b"\x01");
    key_table_becomes(tb, term, "moa");
    term.mark();
}

/// A tab running `moa panel <view>` in the root's terminal server, out of sight; its pane.
pub fn panel_tab(tb: &TempBrain, view: &str) -> String {
    let cmd = format!("'{MOA}' panel {view}");
    tmux(
        tb,
        &[
            "new-window",
            "-d",
            "-t",
            "=moa:",
            "-P",
            "-F",
            "#{pane_id}",
            &cmd,
        ],
    )
}

/// What `pane` shows, as text.
pub fn screen_of(tb: &TempBrain, pane: &str) -> String {
    tmux(tb, &["capture-pane", "-p", "-t", pane])
}

/// The terminal cell, 1-based as a mouse report gives it, three columns into the row of
/// `pane` that shows `line_text` (the first cell of a framed view's body). Placed from tmux's
/// own `#{pane_left}`/`#{pane_top}` and the row's place in `capture-pane`.
///
/// Only the framed view's list rows are searched: below the frame's top line and the view's
/// header line, and above the blank line, the footer and the frame's bottom line. A footer
/// message or prompt that names `line_text` is never the row clicked.
#[track_caller]
pub fn cell_of(tb: &TempBrain, pane: &str, line_text: &str) -> (u32, u32) {
    let at = tmux(
        tb,
        &[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_left} #{pane_top} #{pane_height}",
        ],
    );
    let at: Vec<u32> = at.split(' ').map(|n| n.parse().unwrap()).collect();
    let [left, top, height] = at[..] else {
        panic!("{pane}'s place: {at:?}");
    };
    // The frame's top line (0) and the header (1); the body ends where the blank line
    // before the footer begins (`height - 3`).
    let rows = 2..usize::try_from(height.saturating_sub(3)).unwrap();
    let screen = screen_of(tb, pane);
    let row = screen
        .lines()
        .enumerate()
        .find(|(i, l)| rows.contains(i) && l.contains(line_text))
        .map(|(i, _)| i)
        .unwrap_or_else(|| panic!("{pane}'s rows don't show {line_text:?}:\n{screen}"));
    (left + 3, top + u32::try_from(row).unwrap() + 1)
}

/// A left press and release on `term`, on the row of `pane` that shows `line_text`. The
/// panel's tab is brought up for the attached client first, so the click lands on it.
#[track_caller]
pub fn click(tb: &TempBrain, term: &Terminal, pane: &str, line_text: &str) {
    tmux(tb, &["select-window", "-t", pane]);
    let (x, y) = cell_of(tb, pane, line_text);
    term.press(format!("\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m").as_bytes());
}

/// The rows of the `panels` topic, as a client subscribed to it sees them now.
pub fn panels(tb: &TempBrain) -> Vec<Value> {
    use moa_client::{Client, ClientOptions, OnMismatch};
    use moa_core::config::BrainName;
    use moa_core::publish::Topic;
    use moa_core::state::CallerInfo;
    let opts = ClientOptions {
        paths: tb.paths(),
        brain: BrainName::default_brain(),
        client: "e2e",
        caller: CallerInfo::default(),
        autostart: None,
        on_mismatch: OnMismatch::Accept,
        build: moa_platform::build_id_of(Path::new(MOA)).unwrap(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let Ok(mut client) = Client::connect(opts).await else {
            return Vec::new();
        };
        let Ok(snapshot) = client.subscribe(&[Topic::Panels]).await else {
            return Vec::new();
        };
        snapshot
            .topics
            .get(&Topic::Panels)
            .cloned()
            .unwrap_or_default()
    })
}

/// A pane as [`panes_of`] lists it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PaneRow {
    pub id: String,
    /// 0 once the pane's process is gone.
    pub pid: u32,
    /// `@moa_view`: the panel's view, or `agent` for the agent in the spot.
    pub tag: String,
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
    pub window: String,
    pub dead: bool,
    pub active: bool,
}

/// Every pane of the session `target` names (`=moa`, `=_moa`), in tmux's order: by tab,
/// then top to bottom and left to right within it.
pub fn panes_of(tb: &TempBrain, target: &str) -> Vec<PaneRow> {
    let list = tmux(
        tb,
        &[
            "list-panes",
            "-s",
            "-t",
            target,
            "-F",
            "#{pane_id}\t#{pane_pid}\t#{@moa_view}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t\
             #{pane_height}\t#{pane_dead}\t#{pane_active}\t#{window_name}",
        ],
    );
    list.lines()
        .map(|l| {
            let f: Vec<&str> = l.splitn(10, '\t').collect();
            let n = |i: usize| f[i].parse::<u32>().unwrap_or_else(|_| panic!("{l}"));
            PaneRow {
                id: f[0].to_string(),
                pid: f[1].parse().unwrap_or(0),
                tag: f[2].to_string(),
                left: n(3),
                top: n(4),
                width: n(5),
                height: n(6),
                dead: f[7] == "1",
                active: f[8] == "1",
                window: f[9].to_string(),
            }
        })
        .collect()
}

/// The cockpit's panes: the tab named `moa` in session `moa`.
pub fn cockpit_panes(tb: &TempBrain) -> Vec<PaneRow> {
    panes_of(tb, "=moa")
        .into_iter()
        .filter(|p| p.window == "moa")
        .collect()
}

/// `moa` on a terminal (120x30), once the daemon has built the cockpit on the first attach:
/// its tab holds the tree, console and plan panels. The user's own `~/.tmux.conf` gets a
/// line saying the terminal draws true colour first, as a user's would for theirs, so tmux
/// sends moa's colours as they are, not cut to 256, on every tmux version.
#[track_caller]
pub fn cockpit_up(tb: &TempBrain) -> Terminal {
    let mut conf = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tb.root().join(".tmux.conf"))
        .unwrap();
    std::io::Write::write_all(
        &mut conf,
        b"set -as terminal-features 'xterm-256color:RGB'\n",
    )
    .unwrap();
    let term = open(tb, &[], &[]);
    came_up(tb, &term);
    let built = wait_for(SHOWS, || {
        let tags: Vec<String> = cockpit_panes(tb).into_iter().map(|p| p.tag).collect();
        (tags == ["tree", "console", "plan"]).then_some(())
    });
    assert!(
        built.is_some(),
        "no cockpit: {:#?}\n{}",
        cockpit_panes(tb),
        what_happened(tb, &term)
    );
    term
}

/// The selection bar's background, `sel` (`#5e81ac`), as `capture-pane -e` writes it.
pub const SEL_BG: &str = "48;2;94;129;172";

/// `text` without its escape sequences, skipped whole as `screen::Screen` skips them.
pub fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // `ESC [ params final`: up to the final byte.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            // OSC, DCS, APC, PM: up to BEL or ST (`ESC \`).
            Some(']' | 'P' | '_' | '^') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // A charset designation: one more character.
            Some('(' | ')' | '*' | '+' | '#') => {
                chars.next();
            }
            // Any other escape is two characters, both gone now.
            _ => {}
        }
    }
    out
}

/// `pane`'s rows with their colours, as `capture-pane -e` writes them.
pub fn colours(tb: &TempBrain, pane: &str) -> Vec<String> {
    let shown = tmux(tb, &["capture-pane", "-p", "-e", "-t", pane]);
    shown.lines().map(str::to_string).collect()
}

/// The words a list row shows: its runs of letters, digits and `-_./~`, less its age
/// (`now`, `5m`, `2h`, `3d`), which changes by itself. Glyphs, spinner frames, tree guides
/// and the frame are no words.
pub fn row_words(row: &str) -> Vec<String> {
    let age = |w: &str| {
        w == "now"
            || w.strip_suffix(['m', 'h', 'd'])
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    row.split(|c: char| !(c.is_alphanumeric() || "-_./~".contains(c)))
        .filter(|w| !w.is_empty() && !age(w))
        .map(str::to_string)
        .collect()
}

/// What tells the item on a row from another: [`row_words`] less the counts that change
/// while it stays the same item (a group's members and its `live` count). A working
/// session's spinner, an age ticking or a group's count going up is never another item.
pub fn item_of(row: &str) -> Vec<String> {
    row_words(row)
        .into_iter()
        .filter(|w| w.chars().any(char::is_alphabetic) && w != "live")
        .collect()
}

/// Whether `row` names `text`: `text`'s words are among the row's, whole and in a row, so
/// `Kite` never matches `Kitesurf`.
pub fn names(row: &str, text: &str) -> bool {
    let want = row_words(text);
    !want.is_empty() && row_words(row).windows(want.len()).any(|w| w == want)
}

/// The selection bar in `rows` (`capture-pane -e`'s): its row, and that row as text.
fn bar_of(rows: &[String]) -> Option<(usize, String)> {
    let at = rows.iter().position(|l| l.contains(SEL_BG))?;
    Some((at, plain(&rows[at])))
}

/// Presses `j` or `k` in `pane` until the selection bar is on the row naming `text`.
///
/// The bar is judged by the item it is on, never by the row's number alone: rows can
/// change places under it (the tree lists sessions newest first, so a session whose agent
/// starts moves up), and the selection follows its item there (V8). A key has landed once
/// the bar's row or its item ([`item_of`]) changed; a spinner, an age or a count that
/// changes by itself is no move.
#[track_caller]
pub fn select_row(tb: &TempBrain, pane: &str, text: &str) {
    for _ in 0..20 {
        // Where the bar is, and the row naming `text`.
        let found = wait_for(SHOWS, || {
            let rows = colours(tb, pane);
            let bar = bar_of(&rows)?;
            let want = rows.iter().position(|l| names(&plain(l), text))?;
            Some((bar, want))
        });
        let Some(((bar, on), want)) = found else {
            panic!(
                "{pane} draws no bar, or no {text:?}
{}",
                tab_shows(tb, pane)
            );
        };
        if names(&on, text) {
            return;
        }
        let key = if want > bar { "j" } else { "k" };
        tmux(tb, &["send-keys", "-t", pane, key]);
        // For a failure: each screen the panel showed meanwhile, as `<ms> <bar row>: <rows>`.
        let started = std::time::Instant::now();
        let mut seen: Vec<(String, String)> = Vec::new();
        let moved = wait_for(SHOWS, || {
            let rows = colours(tb, pane);
            let now = bar_of(&rows);
            let shown = format!(
                "{:?}: {:?}",
                now.as_ref().map(|(at, _)| at),
                rows.iter().map(|l| plain(l)).collect::<Vec<_>>()
            );
            if seen.last().is_none_or(|(_, s)| *s != shown) {
                seen.push((format!("{}ms", started.elapsed().as_millis()), shown));
            }
            now.filter(|(at, row)| (*at, item_of(row)) != (bar, item_of(&on)))
        });
        if moved.is_none() {
            // The report itself never panics: each thing it can't read, it says why.
            let ask = |format: &str| {
                tb.tmux(B, &["display-message", "-p", "-t", pane, format])
                    .unwrap_or_else(|e| format!("({e})"))
            };
            let pid = ask("#{pane_pid}");
            let ps = std::process::Command::new("ps")
                .args(["-o", "stat=,%cpu=,time=", "-p", &pid])
                .output()
                .map_or_else(|e| e.to_string(), |o| out(&o));
            let mode = ask("in_mode=#{pane_in_mode}");
            let screens: Vec<String> = seen.iter().map(|(t, s)| format!("{t} {s}")).collect();
            panic!(
                "{key} left the bar on row {bar}, not {text:?}'s {want}\nits screens:\n{}\n\
                 the panel ({pid}): {} {mode}\n{}",
                screens.join("\n"),
                ps.trim(),
                tab_shows(tb, pane)
            );
        }
    }
    panic!("the bar never reached {text:?}");
}

/// `moactl args…` for brain `default`, which must succeed; its stdout. A failure shows what
/// `term` and the daemon say.
#[track_caller]
pub fn moactl_ok(tb: &TempBrain, term: &Terminal, args: &[&str]) -> String {
    let o = run_exe(tb, &moactl(tb), args, &[("MOA_BRAIN", B)]);
    assert!(
        o.status.success(),
        "moactl {args:?}: {}{}\n{}",
        out(&o),
        err(&o),
        what_happened(tb, term)
    );
    out(&o)
}

/// A session `moactl open` started, and where.
pub struct Opened {
    pub id: String,
    pub dir: PathBuf,
}

/// fake-claude's `SessionStart` hook, as a new session's agent sends it.
pub fn start_hook() -> FakeStep {
    FakeStep::Hook {
        event: "SessionStart".into(),
        fields: Map::from_iter([("source".to_string(), Value::String("startup".into()))]),
    }
}

/// `moactl open --bg <root>/work/<name> --title <title>` (and `--group` when given), with
/// fake-claude starting and hanging there.
#[track_caller]
pub fn open_session(
    tb: &TempBrain,
    term: &Terminal,
    name: &str,
    title: &str,
    group: Option<&str>,
) -> Opened {
    open_session_with(
        tb,
        term,
        name,
        title,
        group,
        &[start_hook(), FakeStep::Hang],
    )
}

/// [`open_session`], with fake-claude playing `steps`.
#[track_caller]
pub fn open_session_with(
    tb: &TempBrain,
    term: &Terminal,
    name: &str,
    title: &str,
    group: Option<&str>,
    steps: &[FakeStep],
) -> Opened {
    let dir = tb.root().join("work").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    write_script(&dir, steps);
    let path = dir.display().to_string();
    let mut args = vec!["--json", "open", "--bg", &path, "--title", title];
    if let Some(group) = group {
        args.extend(["--group", group]);
    }
    let data: Value = serde_json::from_str(&moactl_ok(tb, term, &args)).unwrap();
    let id = data["id"].as_str().unwrap_or_else(|| panic!("{data}"));
    Opened {
        id: id.to_string(),
        dir: moa_platform::canonical(&dir),
    }
}

/// The pane of the tab running session `id`, once there is one.
#[track_caller]
pub fn session_pane(tb: &TempBrain, term: &Terminal, id: &str) -> String {
    let found = wait_for(SHOWS, || {
        let list = tmux(
            tb,
            &["list-panes", "-a", "-F", "#{pane_id} #{@moa_session}"],
        );
        list.lines().find_map(|l| {
            let (pane, session) = l.split_once(' ')?;
            (session == id).then(|| pane.to_string())
        })
    });
    found.unwrap_or_else(|| panic!("no tab runs {id}\n{}", what_happened(tb, term)))
}

/// Waits until `pane` shows every one of `texts`.
#[track_caller]
pub fn shows(tb: &TempBrain, pane: &str, texts: &[&str]) {
    let seen = wait_for(SHOWS, || {
        let screen = screen_of(tb, pane);
        texts.iter().all(|t| screen.contains(t)).then_some(())
    });
    assert!(
        seen.is_some(),
        "{pane} never showed {texts:?}\n{}",
        tab_shows(tb, pane)
    );
}
