//! Running `moa` against a TempBrain: on a terminal, as a user would, or to the end. Every
//! tmux query goes through `TempBrain::tmux` (T23).
#![allow(dead_code)]

use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use moa_testkit::{
    FakeStep, StubStep, TempBrain, Terminal, fake_claude, stub_log, stub_moa_toml, wait_for,
    write_script,
};
use serde_json::{Map, Value};

pub mod screen;

pub const MOA: &str = env!("CARGO_BIN_EXE_moa");
pub const B: &str = "default";
/// What the status line shows once moa's config is in.
pub const STATUS_LINE: &str = "^a keys";
/// How long a wait for something to show may take: every wait returns as soon as it does,
/// so this only lengthens a failure.
pub const SHOWS: Duration = Duration::from_secs(10);

/// The size `open` gives a terminal: columns, rows.
pub const TERM_SIZE: (u16, u16) = (120, 30);

/// `moa args…` on a terminal ([`TERM_SIZE`]), with the root's environment plus `extra`.
pub fn open(tb: &TempBrain, args: &[&str], extra: &[(&str, &str)]) -> Terminal {
    open_sized(tb, TERM_SIZE, args, extra)
}

/// `moa args…` on a terminal of `size` (columns, rows), with the root's environment plus
/// `extra`.
pub fn open_sized(
    tb: &TempBrain,
    size: (u16, u16),
    args: &[&str],
    extra: &[(&str, &str)],
) -> Terminal {
    open_exe(tb, Path::new(MOA), size, args, extra)
}

/// `exe args…` on a terminal of `size` (columns, rows), with the root's environment plus
/// `extra`.
pub fn open_exe(
    tb: &TempBrain,
    exe: &Path,
    size: (u16, u16),
    args: &[&str],
    extra: &[(&str, &str)],
) -> Terminal {
    let mut env = tb.env();
    env.extend(
        extra
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    );
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let (cols, rows) = size;
    Terminal::spawn(exe, &args, &env, cols, rows).unwrap()
}

/// A copy of `moa` at `<root>/install/moa`: an install of its own, as a user's is, which
/// [`rebuild`] can replace. The root's [`moactl`] is linked to it from now on, so a
/// `moactl` call is this install's too.
pub fn install_copy(tb: &TempBrain) -> PathBuf {
    let dir = tb.root().join("install");
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("moa");
    std::fs::copy(MOA, &exe).unwrap();
    let ctl = moactl(tb);
    std::fs::remove_file(&ctl).unwrap();
    symlink(&exe, &ctl).unwrap();
    exe
}

/// A new build at `exe`: a copy of `moa` written beside it, given a later time (a build id
/// hashes it), and renamed over it, never written into a running program (macOS kills a
/// process whose signed program changes in place).
pub fn rebuild(exe: &Path) {
    let next = exe.with_extension("next");
    std::fs::copy(MOA, &next).unwrap();
    let later = std::fs::metadata(exe).unwrap().modified().unwrap() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&next)
        .unwrap()
        .set_modified(later)
        .unwrap();
    std::fs::rename(&next, exe).unwrap();
}

/// What a panel says while moa is another build it can't follow.
pub const MISMATCH: &str = "from another build of moa";

/// The cockpit run from an install of its own ([`install_copy`]), with a projects panel in
/// a tab of its own started from it too, beside the tree, console and plan panels moa placed.
pub struct Installed {
    // Dropped before `tb`, as a test's own terminal is.
    pub term: Terminal,
    pub exe: PathBuf,
    /// Every panel's pane, the projects panel's last.
    pub panes: Vec<String>,
    pub tb: TempBrain,
}

/// [`installed_in`] a root with the fake account, on a terminal of [`TERM_SIZE`].
pub fn installed() -> Installed {
    let tb = TempBrain::new(None);
    moa_toml_with_fake_account(&tb);
    installed_in(tb, TERM_SIZE)
}

/// The cockpit on `tb`, a root already set up, run from an install of its own on a terminal
/// of `size` ([`Installed`]).
#[track_caller]
pub fn installed_in(tb: TempBrain, size: (u16, u16)) -> Installed {
    let exe = install_copy(&tb);
    let term = cockpit_up_from(&tb, &exe, size);
    let projects = panel_tab_from(&tb, &exe, "projects");
    shows(&tb, &projects, &["▦ projects"]);
    let mut panes: Vec<String> = cockpit_panes(&tb).into_iter().map(|p| p.id).collect();
    panes.push(projects);
    Installed {
        term,
        exe,
        panes,
        tb,
    }
}

impl Installed {
    /// Each pane's process.
    pub fn pids(&self) -> Vec<String> {
        let pid_of = |pane: &str| {
            tmux(
                &self.tb,
                &["display-message", "-p", "-t", pane, "#{pane_pid}"],
            )
        };
        self.panes.iter().map(|p| pid_of(p)).collect()
    }

    /// Waits until the daemon runs `build` and every panel is linked to it: each pane has
    /// its `panels` row, which only a panel on the daemon's build has (it says which panel
    /// it is after a matching hello). Then no pane says it is from another build.
    #[track_caller]
    pub fn all_linked_on(&self, build: &str) {
        let linked = |rows: &[Value]| {
            self.panes
                .iter()
                .all(|p| rows.iter().any(|r| r["pane"] == p.as_str()))
        };
        let seen = wait_for(SHOWS, || {
            let upgraded = status_json(&self.tb)["build"] == build;
            (upgraded && linked(&panels_on(&self.tb, build))).then_some(())
        });
        assert!(
            seen.is_some(),
            "not every panel linked on {build}: {:?}\n{:#?}\n{}\n{}",
            self.panes,
            panels_on(&self.tb, build),
            std::fs::read_to_string(self.tb.paths().log_dir().join("panel.log"))
                .unwrap_or_default(),
            what_happened(&self.tb, &self.term)
        );
        for pane in &self.panes {
            let screen = screen_of(&self.tb, pane);
            assert!(!screen.contains(MISMATCH), "{pane}:\n{screen}");
        }
    }
}

/// `exe build-id`: the build `exe` is.
pub fn build_of(tb: &TempBrain, exe: &Path) -> String {
    let o = run_exe(tb, exe, &["build-id"], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    out(&o).trim().to_string()
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

/// `moactl`, linked to `moa` as an install would (to the root's own install, once
/// [`install_copy`] made one).
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
    panel_tab_from(tb, Path::new(MOA), view)
}

/// [`panel_tab`], run from `exe`.
pub fn panel_tab_from(tb: &TempBrain, exe: &Path, view: &str) -> String {
    let cmd = format!("'{}' panel {view}", exe.display());
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
    panels_on(tb, &moa_platform::build_id_of(Path::new(MOA)).unwrap())
}

/// [`panels`], asked by a client on `build`: none unless the daemon runs that build. A row
/// is a panel linked to the daemon (it said which panel it is after a matching hello).
pub fn panels_on(tb: &TempBrain, build: &str) -> Vec<Value> {
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
        build: build.to_string(),
        upgrade_wait: moa_client::UPGRADE_WAIT,
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

/// `moa` on a terminal ([`TERM_SIZE`]), once the daemon has built the cockpit on the first
/// attach: its tab holds the tree, console and plan panels. The user's own `~/.tmux.conf`
/// gets a line saying the terminal draws true colour first, as a user's would for theirs, so
/// tmux sends moa's colours as they are, not cut to 256, on every tmux version.
#[track_caller]
pub fn cockpit_up(tb: &TempBrain) -> Terminal {
    cockpit_up_sized(tb, TERM_SIZE)
}

/// [`cockpit_up`] on a terminal of `size` (columns, rows).
#[track_caller]
pub fn cockpit_up_sized(tb: &TempBrain, size: (u16, u16)) -> Terminal {
    cockpit_up_from(tb, Path::new(MOA), size)
}

/// [`cockpit_up_sized`], with `moa` run from `exe`: its daemon, and the panels it places,
/// run from there too.
#[track_caller]
pub fn cockpit_up_from(tb: &TempBrain, exe: &Path, size: (u16, u16)) -> Terminal {
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
    let term = open_exe(tb, exe, size, &[], &[]);
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
/// fake-claude starting and hanging there; once moa has its start hook.
///
/// A hook moves the session's last-seen time, and so its place in the tree, which lists the
/// newest first. In by now, it lands while the session is the newest anyway, so the tree's
/// order never changes under a test that then moves through it ([`select_row`]).
#[track_caller]
pub fn open_session(
    tb: &TempBrain,
    term: &Terminal,
    name: &str,
    title: &str,
    group: Option<&str>,
) -> Opened {
    let opened = open_session_with(
        tb,
        term,
        name,
        title,
        group,
        &[start_hook(), FakeStep::Hang],
    );
    let started = wait_for(SHOWS, || {
        (events(tb, &opened.id, "session.started") > 0).then_some(())
    });
    assert!(
        started.is_some(),
        "{title}'s start hook never came\n{}",
        what_happened(tb, term)
    );
    opened
}

/// How many `events` rows of `kind` session `id` has, read as `moa sql` reads them.
pub fn events(tb: &TempBrain, id: &str, kind: &str) -> usize {
    let query =
        format!("select count(*) from events where session_id = '{id}' and kind = '{kind}'");
    let o = run(tb, &["sql", &query], &[]);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    out(&o).lines().nth(1).unwrap().parse().unwrap()
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

/// Waits until `pane` shows `text`, its words in order whatever the lines it wraps over
/// ([`names`]).
#[track_caller]
pub fn shows_wrapped(tb: &TempBrain, pane: &str, text: &str) {
    let seen = wait_for(SHOWS, || names(&screen_of(tb, pane), text).then_some(()));
    assert!(
        seen.is_some(),
        "{pane} never showed {text:?}\n{}",
        tab_shows(tb, pane)
    );
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

/// Ctrl-c, typed on a terminal.
pub const CTRL_C: &[u8] = b"\x03";
/// The title tmux draws on the picker's popup's border.
pub const PICKER_TITLE: &str = "⌕ find a session";
/// Esc, Enter and Ctrl-u (which empties tmux's prompt), typed on a terminal.
pub const ESC: &[u8] = b"\x1b";
pub const ENTER: &[u8] = b"\r";
pub const CTRL_U: &[u8] = b"\x15";
/// What the picker's full tab shows: its footer hint.
pub const PICKER_SHOWS: &str = "esc close";
/// What help shows, as a popup or a tab: its first heading.
pub const HELP_SHOWS: &str = "Ctrl-a keys";
/// The title tmux draws on help's popup's border.
pub const HELP_TITLE: &str = "keys and help";

/// Whether a `panel.log` line's text says how a panel ended: `stopped…`, or `panicked…`
/// (which a test must see to fail on it). A note such as `can't watch the terminal…` isn't.
pub fn is_end(what: &str) -> bool {
    what.starts_with("stopped") || what.starts_with("panicked")
}

/// How many of `view`'s popups are connected to moa: `panels` rows showing it in no tab.
pub fn popups(tb: &TempBrain, view: &str) -> usize {
    panels(tb)
        .iter()
        .filter(|row| row["view"] == view && row["pane"].is_null())
        .count()
}

/// What `panel.log` says of `view`'s popups, in order: each line's end.
pub fn popup_log(tb: &TempBrain, view: &str) -> Vec<String> {
    let log = std::fs::read_to_string(tb.paths().log_dir().join("panel.log")).unwrap_or_default();
    let mine = format!("panel {view} (popup) in -: ");
    log.lines()
        .filter_map(|l| l.split_once(&mine).map(|(_, w)| w.to_string()))
        .collect()
}

/// Waits until tmux has closed the popup titled `title` on `term`, as the user sees it: its
/// border, which tmux draws until it reaps the popup's moa, is off the screen. Then the
/// next key reaches tmux rather than the popup. No process is looked up: `ps` isn't there
/// in every place the tests run (nix's build sandbox).
#[track_caller]
pub fn popup_closes(tb: &TempBrain, term: &Terminal, title: &str) {
    let closed = wait_for(SHOWS, || {
        // tmux 3.4 and 3.5 built with utempter can lose the exit until tmux reaps a child
        // of its own; the daemon nudges, and so does the test, unordered with it.
        tmux(tb, &["run-shell", "true"]);
        (!screen::Screen::of(term).text().contains(title)).then_some(())
    });
    assert!(
        closed.is_some(),
        "tmux never closed the popup {title:?}; the terminal shows:\n{}",
        screen::Screen::of(term).text()
    );
}

/// Waits until `term`'s screen shows each of `texts`, or with `shown` false until it shows
/// none of them: what tmux draws over the panes (a popup, the status line), which
/// `capture-pane` can't read.
#[track_caller]
pub fn screen_shows(term: &Terminal, texts: &[&str], shown: bool) {
    let seen = wait_for(SHOWS, || {
        let text = screen::Screen::of(term).text();
        let ok = if shown {
            texts.iter().all(|t| text.contains(t))
        } else {
            !texts.iter().any(|t| text.contains(t))
        };
        ok.then_some(())
    });
    assert!(
        seen.is_some(),
        "the terminal {} {texts:?}:\n{}",
        if shown { "never showed" } else { "still shows" },
        screen::Screen::of(term).text()
    );
}

/// What the popup titled `title` on `term`'s screen holds inside its border, row by row:
/// the columns between the corners around the title, from the row below it to the bottom
/// corner. `None` while no popup with that title is drawn. What the tabs behind it show,
/// beside or below it, is never read.
pub fn popup_text(term: &Terminal, title: &str) -> Option<String> {
    let rows: Vec<Vec<char>> = screen::Screen::of(term)
        .text()
        .lines()
        .map(|l| l.chars().collect())
        .collect();
    let want: Vec<char> = title.chars().collect();
    let (top, at) = rows.iter().enumerate().find_map(|(y, row)| {
        let x = row.windows(want.len()).position(|w| w == want.as_slice())?;
        Some((y, x))
    })?;
    let row = &rows[top];
    let left = row[..at].iter().rposition(|c| matches!(c, '╭' | '┌'))?;
    let right = at + row[at..].iter().position(|c| matches!(c, '╮' | '┐'))?;
    let mut inside = Vec::new();
    for row in &rows[top + 1..] {
        if matches!(row.get(left), Some('╰' | '└')) {
            return Some(inside.join("\n"));
        }
        inside.push(row.get(left + 1..right)?.iter().collect::<String>());
    }
    None
}

/// Waits until the popup titled `title` on `term` shows each of `texts` inside its border.
#[track_caller]
pub fn popup_shows(term: &Terminal, title: &str, texts: &[&str]) {
    let seen = wait_for(SHOWS, || {
        let inside = popup_text(term, title)?;
        texts.iter().all(|t| inside.contains(t)).then_some(())
    });
    assert!(
        seen.is_some(),
        "no popup {title:?} showed {texts:?}; the terminal shows:\n{}",
        screen::Screen::of(term).text()
    );
}

/// `press` opens a sheet (S9): the terminal's tab becomes one whose panel shows each of
/// `says`, linked into session `moa` from `_moa`. `close`, typed, closes it: the terminal is
/// back on the tab it was on, and session `moa` no longer has the sheet's tab.
#[track_caller]
pub fn a_sheet_opens_and_closes(
    tb: &TempBrain,
    term: &Terminal,
    press: impl Fn(),
    says: &[&str],
    close: &[u8],
) {
    let before = client_field(tb, term, "#{window_id}");
    press();
    let sheet = wait_for(SHOWS, || {
        let pane = client_field(tb, term, "#{pane_id}")?;
        let screen = screen_of(tb, &pane);
        says.iter()
            .all(|t| screen.contains(t))
            .then(|| client_field(tb, term, "#{window_id}"))
            .flatten()
    });
    let Some(sheet) = sheet else {
        let pane = client_field(tb, term, "#{pane_id}").unwrap_or_default();
        panic!(
            "no sheet showed {says:?}; {pane} shows:\n{}\n{}",
            tab_shows(tb, &pane),
            what_happened(tb, term)
        )
    };
    term.press(close);
    let closed = wait_for(SHOWS, || {
        let tabs = tmux(tb, &["list-windows", "-t", "=moa:", "-F", "#{window_id}"]);
        let back = client_field(tb, term, "#{window_id}") == before;
        (back && !tabs.lines().any(|w| w == sheet)).then_some(())
    });
    assert!(
        closed.is_some(),
        "the sheet {sheet} never closed\n{}",
        what_happened(tb, term)
    );
}

/// `press` opens help's popup (S10) on `term`: it draws help over the tab, each of `shows`
/// inside its border, as a panel moa hears from. `q` closes it: the panel goes, its moa ends
/// well (`panel.log` says it started, then quit) and tmux closes the popup, so the next key
/// reaches tmux; the terminal is still on the tab it was on.
#[track_caller]
pub fn the_help_popup_opens_and_closes(
    tb: &TempBrain,
    term: &Terminal,
    press: impl Fn(),
    shows: &[&str],
) {
    let before = client_field(tb, term, "#{window_id}");
    let logged = popup_log(tb, "help").len();
    press();
    let up = wait_for(SHOWS, || (popups(tb, "help") == 1).then_some(()));
    assert!(
        up.is_some(),
        "help's popup never connected: {:?}\n{}",
        panels(tb),
        what_happened(tb, term)
    );
    popup_shows(term, HELP_TITLE, shows);
    term.press(b"q");
    let gone = wait_for(SHOWS, || (popups(tb, "help") == 0).then_some(()));
    assert!(
        gone.is_some(),
        "help's popup never closed\n{}",
        what_happened(tb, term)
    );
    popup_closes(tb, term, HELP_TITLE);
    assert_eq!(
        popup_log(tb, "help")[logged..],
        ["started", "stopped: quit"]
    );
    assert_eq!(client_field(tb, term, "#{window_id}"), before);
}

/// A root with the fake account, and `moa` on a terminal with the cockpit built.
pub struct Rig {
    // Dropped before `tb`, as a test's own terminal is.
    pub term: Terminal,
    pub tb: TempBrain,
}

pub fn rig() -> Rig {
    rig_with(TERM_SIZE, "")
}

/// [`rig`] on a terminal of `size`, with `more` added to the root's `moa.toml`.
pub fn rig_with(size: (u16, u16), more: &str) -> Rig {
    let tb = TempBrain::new(None);
    moa_toml_with_fake_account(&tb);
    let file = tb.paths().config_file();
    let toml = std::fs::read_to_string(&file).unwrap() + more;
    std::fs::write(&file, toml).unwrap();
    let term = cockpit_up_sized(&tb, size);
    Rig { term, tb }
}

impl Rig {
    /// Ctrl-a `key` on the terminal.
    pub fn key(&self, key: &str) {
        ctrl_a(&self.tb, &self.term);
        self.term.press(key.as_bytes());
    }

    /// Waits until the terminal says `text`, since the last key.
    #[track_caller]
    pub fn says(&self, text: &str) {
        assert!(
            self.term.wait_for_text(text, SHOWS),
            "the terminal never said {text:?}\n{}",
            what_happened(&self.tb, &self.term)
        );
    }

    /// Waits until the terminal's screen shows each of `texts`, or with `shown` false none.
    #[track_caller]
    pub fn on_screen(&self, texts: &[&str], shown: bool) {
        screen_shows(&self.term, texts, shown);
    }

    /// The daemon's launches and its panel starts.
    pub fn counts(&self) -> (u64, u64) {
        let s = status_json(&self.tb);
        let n = |k: &str| s[k].as_u64().unwrap_or_else(|| panic!("no {k}: {s}"));
        (n("launches"), n("panel_starts"))
    }

    /// A session `moactl open --bg` started in `<root>/work/<name>`, titled `title`; its id.
    #[track_caller]
    pub fn session(&self, name: &str, title: &str) -> String {
        open_session(&self.tb, &self.term, name, title, None).id
    }

    /// The pane the terminal shows.
    pub fn client_pane(&self) -> Option<String> {
        client_field(&self.tb, &self.term, "#{pane_id}")
    }

    /// `Ctrl-a /`: the picker's popup draws each of `shows` inside its border, and its panel
    /// is connected to moa.
    #[track_caller]
    pub fn open_picker_popup(&self, shows: &[&str]) {
        self.key("/");
        let up = wait_for(SHOWS, || (popups(&self.tb, "picker") == 1).then_some(()));
        assert!(
            up.is_some(),
            "the picker's popup never connected: {:?}\n{}",
            panels(&self.tb),
            what_happened(&self.tb, &self.term)
        );
        popup_shows(&self.term, PICKER_TITLE, shows);
    }
}

/// The newest activity rows' text.
pub fn activity(tb: &TempBrain) -> String {
    let o = run(
        tb,
        &["sql", "select text from activity order by id desc limit 20"],
        &[],
    );
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    out(&o)
}

/// The lines `stub-acp`, as moa's agent, logged for `method`.
pub fn stub_logged(tb: &TempBrain, method: &str) -> Vec<Value> {
    stub_log(&tb.paths().agent_dir(B))
        .into_iter()
        .filter(|l| l["method"] == method)
        .collect()
}

/// [`stub_moa_toml`], opening sessions with `acct-a` too: moa's agent is the stub, and each
/// session's is fake-claude.
pub fn stub_moa_toml_opening_sessions() -> String {
    let base = stub_moa_toml();
    let toml = base.replacen(
        "moa_account = \"acct-a\"\n",
        "moa_account = \"acct-a\"\nopen_account = \"acct-a\"\n",
        1,
    );
    assert_ne!(toml, base, "open_account isn't set: {base}");
    toml
}

/// The stub's answer chunk `text`, with message id `id`.
pub fn say(text: &str, id: &str) -> StubStep {
    StubStep::Say {
        text: text.into(),
        id: Some(id.into()),
    }
}

/// The end of the stub's turn.
pub fn end() -> StubStep {
    StubStep::End { stop: None }
}
