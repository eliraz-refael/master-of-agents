//! The generated config against a real tmux, through the testkit's `Sandbox`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use moa_core::cockpit::panel_env_of;
use moa_core::config::{Form, UiConfig};
use moa_core::keymap::{Does, ctrl_a};
use moa_core::state::PanelLaunch;
use moa_core::tmux::cmd::{literal_format, quote};
use moa_core::tmux::ids::{ClientName, PaneId};
use moa_core::tmux::mailbox::{HookMsg, KeyAction, Mailbox};
use moa_testkit::{Sandbox, Terminal, tmux_path, wait_for};
use moa_tmux::Notification;
use moa_tmux::client::{CONNECT_WAIT, ConnectOptions, ControlClient, Event, Events, ServerInfo};
use moa_tmux::conf::{hooks, moa_tmux, tmux_conf, unhooks};

/// The home the fixture's popups open at.
const HOME: &str = "/r";

/// The fixture's panels: moa's program, sandboxed under `/r`.
fn launch() -> PanelLaunch {
    PanelLaunch {
        exe: "/opt/moa/bin/moa".into(),
        env: vec![("MOA_ROOT".into(), HOME.into())],
    }
}

fn ui(picker: Form) -> UiConfig {
    UiConfig {
        picker,
        ..UiConfig::default()
    }
}

/// `brain`'s file as the fixture's daemon writes it, with `ui`.
fn conf_with(brain: &str, ui: &UiConfig) -> String {
    moa_tmux(brain, ui, &launch(), Path::new(HOME))
}

/// The default brain's file, with the default `[ui]`.
fn conf() -> String {
    conf_with("default", &UiConfig::default())
}

#[test]
fn the_generated_config_is_stable() {
    insta::assert_snapshot!("moa_tmux_default", conf());
    insta::assert_snapshot!("moa_tmux_work", conf_with("work", &UiConfig::default()));
    insta::assert_snapshot!(
        "tmux_conf",
        tmux_conf(Path::new("/r/.local/share/moa/default/moa.tmux"))
    );
    insta::assert_snapshot!(
        "tmux_conf_odd_path",
        tmux_conf(Path::new("/r/it's here/moa.tmux"))
    );
}

/// A failed agent's tab shows a red cross in the current-window pill.
#[test]
fn the_current_window_pill_shows_failed() {
    assert!(
        conf().contains("#{?#{==:#{@moa_state},failed},#[fg=#d57780]✗ ,"),
        "the pill has no branch for failed"
    );
    // And tmux reads the branch as meant: the pill renders the cross.
    let sb = Sandbox::start().unwrap();
    load(&sb);
    sb.tmux(&["set-option", "-w", "@moa_state", "failed"])
        .unwrap();
    let left = sb
        .tmux(&["display-message", "-p", "#{T:status-left}"])
        .unwrap();
    assert!(left.contains("✗ "), "{left}");
}

/// V11: moa's file turns the mouse on, so tmux hands a panel its clicks, motion and wheel;
/// with `[ui] mouse = false` it says nothing about the mouse, leaving the user's own setting.
#[test]
fn moa_tmux_turns_the_mouse_on_unless_told_not_to() {
    let on = conf();
    let lines: Vec<&str> = on.lines().filter(|l| l.contains("mouse")).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("set -g  mouse on"), "{lines:?}");
    let off = conf_with(
        "default",
        &UiConfig {
            mouse: false,
            ..UiConfig::default()
        },
    );
    assert!(
        !off.lines().any(|l| l.contains("mouse")),
        "no mouse line at all"
    );
    // And tmux takes it.
    let sb = Sandbox::start().unwrap();
    load(&sb);
    assert_eq!(sb.tmux(&["show-options", "-gv", "mouse"]).unwrap(), "on");
}

/// Sources `tmux_conf` on a server whose home holds the user's three possible config files,
/// each appending its name to `@order`, with `XDG_CONFIG_HOME` as given (a path under the
/// home, or none).
fn order_with_xdg(xdg: Option<&str>) -> String {
    let sb = Sandbox::start().unwrap();
    let home = sb.home().to_path_buf();
    for (file, name) in [
        (".tmux.conf", "home"),
        (".config/tmux/tmux.conf", "dotconfig"),
        ("xdg/tmux/tmux.conf", "xdg"),
    ] {
        let path = home.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("set -ag @order '{name} '\n")).unwrap();
    }
    let moa = home.join("moa.tmux");
    std::fs::write(&moa, "set -ag @order 'moa '\n").unwrap();
    let conf = home.join("tmux.conf");
    std::fs::write(&conf, tmux_conf(&moa)).unwrap();
    if let Some(x) = xdg {
        let x = home.join(x).to_string_lossy().into_owned();
        sb.tmux(&["set-environment", "-g", "XDG_CONFIG_HOME", &x])
            .unwrap();
    }
    sb.tmux(&["source-file", &conf.to_string_lossy()]).unwrap();
    sb.tmux(&["show-options", "-gqv", "@order"]).unwrap()
}

#[test]
fn tmux_conf_loads_the_users_files_as_tmux_would_then_moas() {
    assert_eq!(order_with_xdg(None).trim(), "home dotconfig moa");
    assert_eq!(order_with_xdg(Some("xdg")).trim(), "home xdg dotconfig moa");
    // Named twice, loaded once, as tmux does.
    assert_eq!(order_with_xdg(Some(".config")).trim(), "home dotconfig moa");
}

/// Writes `text` as the sandbox's `moa.tmux`.
fn write_conf(sb: &Sandbox, text: &str) -> PathBuf {
    let path = sb.home().join("moa.tmux");
    std::fs::write(&path, text).unwrap();
    path
}

/// Sources `text` on the sandbox's server, which takes it without a word.
fn load_text(sb: &Sandbox, text: &str) {
    let path = write_conf(sb, text);
    let out = sb.tmux(&["source-file", &path.to_string_lossy()]).unwrap();
    assert_eq!(out, "");
}

fn load(sb: &Sandbox) {
    load_text(sb, &conf());
}

/// The keys bound in the `moa` table, from tmux's own listing: the word after `-T moa`, with
/// tmux's quoting taken off.
fn moa_keys(sb: &Sandbox) -> Vec<String> {
    let keys = sb.tmux(&["list-keys", "-T", "moa"]).unwrap();
    keys.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            f.by_ref().find(|w| *w == "moa")?;
            f.next().map(|k| k.replace(['\'', '\\'], ""))
        })
        .collect()
}

/// The key table and the generated file agree: every key the table names is bound (`Move`
/// names the arrows, which `it_sources_cleanly_on_this_tmux` checks). Read from tmux's
/// listing, not the file's text: the template spells `[` bare, `quote` gives `'['`.
#[test]
fn every_table_key_is_bound() {
    for form in [Form::Popup, Form::Tab] {
        let sb = Sandbox::start().unwrap();
        load_text(&sb, &conf_with("default", &ui(form)));
        let bound = moa_keys(&sb);
        let table: Vec<&str> = ctrl_a(form)
            .into_iter()
            .flatten()
            .filter(|b| b.does != Does::Move)
            .map(|b| b.key)
            .collect();
        assert!(!table.is_empty());
        for k in table {
            assert!(
                bound.iter().any(|b| b == k),
                "{form:?}: {k} isn't bound in: {bound:?}"
            );
        }
    }
}

/// The line binding `key` in the `moa` table, as the file writes it.
fn bind_line(text: &str, key: &str) -> String {
    let head = format!("bind -T moa {:<7} ", quote(key));
    text.lines()
        .find_map(|l| l.strip_prefix(&head))
        .unwrap_or_else(|| panic!("no binding for {key}"))
        .to_string()
}

/// The daemon key `find`, from a key.
const FIND_KEY: &str = "if -F '#{@moa_ctl}' { run -C \"display-message -c '#{@moa_ctl}' 'moa:key find ##{pane_id} ##{session_id} ##{client_name}'\" } { display-message -d 4000 \"moa isn't running: run moa\" }";

/// S8: `/` opens the picker in a popup that tmux runs itself, guarded as the daemon keys
/// are, with the panels' environment and moa's program; `s` is the daemon key `find`,
/// which opens the full tab.
#[test]
fn the_picker_key_is_a_popup_by_default() {
    let text = conf();
    assert_eq!(
        bind_line(&text, "/"),
        "if -F '#{@moa_ctl}' { display-popup -EE -w 80% -h 70% -d /r \
         -T \"#[fg=#88c0d0 bold] ⌕ find a session \" -e MOA_BRAIN=default \
         -e MOA_NO_AUTOSTART=1 -e MOA_ROOT=/r /opt/moa/bin/moa panel picker --once \
         --brain default } { display-message -d 4000 \"moa isn't running: run moa\" }"
    );
    assert_eq!(bind_line(&text, "s"), FIND_KEY);
}

#[test]
fn picker_tab_swaps_the_keys() {
    let text = conf_with("default", &ui(Form::Tab));
    assert_eq!(bind_line(&text, "/"), FIND_KEY);
    let s = bind_line(&text, "s");
    assert!(s.contains("display-popup -EE -w 80% -h 70%"), "{s}");
    assert!(s.contains("panel picker --once --brain default"), "{s}");
    assert_eq!(s, bind_line(&conf(), "/"));
}

/// S10: `?` opens help in a popup that tmux runs itself, guarded as the daemon keys are:
/// 84 columns wide, or the whole width on a narrower terminal (tmux refuses a popup wider
/// than the terminal, and `-w` takes no format). With `[ui] picker = "tab"`, `?` is the
/// daemon key `help`, which opens the full tab.
#[test]
fn the_help_key_is_a_popup_by_default() {
    let popup = |size: &str| {
        format!(
            "display-popup -EE {size} -d /r -T \"#[fg=#88c0d0 bold] keys and help \" \
             -e MOA_BRAIN=default -e MOA_NO_AUTOSTART=1 -e MOA_ROOT=/r /opt/moa/bin/moa \
             panel help --once --brain default"
        )
    };
    let want = format!(
        "if -F '#{{@moa_ctl}}' {{ if -F '#{{e|<:#{{client_width}},84}}' {{ {} }} {{ {} }} }} \
         {{ display-message -d 4000 \"moa isn't running: run moa\" }}",
        popup("-w 100% -h 90%"),
        popup("-w 84 -h 90%"),
    );
    let text = conf();
    assert_eq!(bind_line(&text, "?"), want);
    // The menu's item is the same: the fixture's paths hold no `#` to double.
    assert_eq!(
        menu_item(&text, "Keys and help"),
        format!("{} {}", quote("?"), quote(&want))
    );

    let help = FIND_KEY.replace("moa:key find", "moa:key help");
    let text = conf_with("default", &ui(Form::Tab));
    assert_eq!(bind_line(&text, "?"), help);
    assert_eq!(
        menu_item(&text, "Keys and help"),
        format!(
            "{} {}",
            quote("?"),
            quote(&help.replace("##{pane_id}", "-"))
        )
    );
}

/// S10 on this tmux: help's popup opens from the key and from the menu on a terminal
/// narrower than 84 columns, exactly 84, and wider; a popup wider than the terminal would
/// be refused, and never run moa. It is the terminal's width under 84 columns and 84
/// otherwise: inside its border, two columns fewer.
#[test]
fn the_help_popup_opens_at_any_width() {
    for (cols, inside) in [(60, 58), (84, 82), (100, 82)] {
        let sb = Sandbox::start().unwrap();
        let odd = OddPaths::new(&sb);
        let text = moa_tmux("default", &UiConfig::default(), &odd.launch(), &odd.home);
        load_text(&sb, &text);
        sb.tmux(&["set-option", "-g", "@moa_ctl", "not-a-client"])
            .unwrap();
        let term = attach_sized(&sb, cols, 30);
        term.press(b"\x01?");
        assert_eq!(
            odd.ran_as_told(&sb, &term, "help"),
            inside,
            "{cols}: the key"
        );
        open_menu(&term, b"\x01 ");
        term.press(b"?");
        assert_eq!(
            odd.ran_as_told(&sb, &term, "help"),
            inside,
            "{cols}: the menu"
        );
    }
}

/// The menu's items for the picker, by key: the text after the label, its key and its
/// command (quoted), from the tall menu.
fn menu_item(text: &str, label: &str) -> String {
    let head = format!("\"{label}\" ");
    let items: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix(&head))
        .collect();
    // Each item is in the short menu and the tall one, in each of the three bindings.
    assert_eq!(items.len(), 6, "{label}: {items:?}");
    items[0].trim_end_matches(" \\").to_string()
}

/// S8: the menu offers both forms. `/` opens the one `[ui] picker` names, as the key does;
/// `s` the other.
#[test]
fn the_menu_offers_both_forms() {
    let find = |key: &str| format!("{key} {}", quote(&FIND_KEY.replace("##{pane_id}", "-")));
    let text = conf();
    let popup = menu_item(&text, "Find a session…");
    assert!(popup.starts_with("/ 'if -F "), "{popup}");
    assert!(popup.contains("display-popup -EE -w 80% -h 70%"), "{popup}");
    assert!(popup.contains("panel picker --once"), "{popup}");
    assert_eq!(menu_item(&text, "Find a session, full tab…"), find("s"));

    let text = conf_with("default", &ui(Form::Tab));
    assert_eq!(menu_item(&text, "Find a session…"), find("/"));
    let s = menu_item(&text, "Find a session, popup…");
    assert_eq!(s.strip_prefix("s "), popup.strip_prefix("/ "));
}

/// The popup's `-e` flags are the panels' environment (`panel_env_of`), which `spawn_cmds`
/// gives every panel moa starts: one source (lesson 5).
#[test]
fn the_popup_env_is_the_panels_env() {
    let text = conf();
    let line = bind_line(&text, "/");
    let words: Vec<&str> = line.split(' ').collect();
    let flags: Vec<String> = words
        .windows(2)
        .filter(|w| w[0] == "-e")
        .flat_map(|w| ["-e".to_string(), w[1].to_string()])
        .collect();
    assert_eq!(flags, panel_env_of("default", &launch()));
}

/// A popup's command line as the sandbox's tmux runs it: the fake `moa` at a path with a
/// space, a quote and a `#`, at a home with a `#`, writes its pid, its terminal's size, its
/// folder, its environment and its arguments next to itself, then ends, and tmux closes the
/// popup.
struct OddPaths {
    exe: PathBuf,
    home: PathBuf,
    ran: PathBuf,
}

impl OddPaths {
    fn new(sb: &Sandbox) -> OddPaths {
        let dir = sb.home().join("a b/it's/#1");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("moa");
        // Written aside, then moved: `ran` is never seen half written.
        std::fs::write(
            &exe,
            "#!/bin/sh\nd=$(dirname \"$0\")\nprintf '%s\\n' \"$$\" \"$(stty size)\" \"$(pwd -P)\" \"$MOA_BRAIN\" \
             \"$MOA_NO_AUTOSTART\" \"$MOA_ROOT\" \"$@\" > \"$d/ran.part\"\n\
             mv \"$d/ran.part\" \"$d/ran\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = sb.home().join("h#2");
        std::fs::create_dir_all(&home).unwrap();
        OddPaths {
            ran: dir.join("ran"),
            exe,
            home,
        }
    }

    fn launch(&self) -> PanelLaunch {
        PanelLaunch {
            exe: self.exe.clone(),
            env: vec![("MOA_ROOT".into(), self.home.to_string_lossy().into_owned())],
        }
    }

    /// Waits for the fake `moa`'s run of `view` and checks what it was given; then for tmux
    /// to reap it, which closes the popup, so the next key reaches tmux rather than the
    /// popup. The popup's width inside its border.
    #[track_caller]
    fn ran_as_told(&self, sb: &Sandbox, term: &Terminal, view: &str) -> u16 {
        let got = wait_for(Duration::from_secs(10), || {
            std::fs::read_to_string(&self.ran).ok()
        });
        let got = got.unwrap_or_else(|| {
            panic!(
                "the popup never ran moa; the terminal showed: {}",
                term.tail(600)
            )
        });
        let (pid, got) = got.split_once('\n').unwrap();
        let (size, got) = got.split_once('\n').unwrap();
        let cols = size.split_whitespace().nth(1).and_then(|c| c.parse().ok());
        let cols = cols.unwrap_or_else(|| panic!("no size from `stty size`: {size:?}"));
        let home = std::fs::canonicalize(&self.home).unwrap();
        let want = format!(
            "{}\ndefault\n1\n{}\npanel\n{view}\n--once\n--brain\ndefault\n",
            home.display(),
            self.home.display()
        );
        assert_eq!(got, want);
        std::fs::remove_file(&self.ran).unwrap();
        let pid: u32 = pid.parse().unwrap();
        let reaped = wait_for(Duration::from_secs(10), || {
            // tmux 3.4 and 3.5 built with utempter can lose the exit until tmux reaps a
            // child of its own (`every_hook_reaches_the_mailbox`).
            let _ = sb.tmux(&["run-shell", "true"]);
            (!moa_platform::pid_alive(pid)).then_some(())
        });
        assert!(reaped.is_some(), "tmux never reaped the popup's moa");
        cols
    }
}

/// S8's quoting, on this tmux: the program and its arguments, the environment and the
/// folder reach the popup's program as they are, from the key and from the menu (whose
/// command tmux expands once more, so the menu's copy doubles every `#` again).
#[test]
fn a_path_with_spaces_quotes_and_hashes_survives() {
    let sb = Sandbox::start().unwrap();
    let odd = OddPaths::new(&sb);
    let text = moa_tmux("default", &UiConfig::default(), &odd.launch(), &odd.home);
    // The key's `-d` is kept literal (`literal_format`), and the menu's copy doubles every
    // `#` once more: in the folder, the environment and the program.
    let key = bind_line(&text, "/");
    let item = menu_item(&text, "Find a session…");
    let home = odd.home.to_string_lossy();
    assert!(key.contains(&format!("-d {}", quote(&literal_format(&home)))));
    assert!(key.contains("=default -e MOA_NO_AUTOSTART=1"), "{key}");
    assert_eq!(key.matches("h#2").count(), 1, "{key}");
    assert_eq!(key.matches("h##2").count(), 1, "the folder: {key}");
    assert!(key.contains("/#1/moa"), "{key}");
    assert_eq!(item.matches("h####2").count(), 1, "the folder: {item}");
    assert_eq!(item.matches("h##2").count(), 1, "the environment: {item}");
    assert!(
        item.contains("/##1/moa") && !item.contains("####1"),
        "{item}"
    );
    load_text(&sb, &text);
    let listed = sb.tmux(&["list-keys", "-T", "moa", "/"]).unwrap();
    assert!(listed.contains("#1/moa"), "{listed}");
    assert!(!listed.contains("##1/moa"), "{listed}");

    sb.tmux(&["set-option", "-g", "@moa_ctl", "not-a-client"])
        .unwrap();
    let term = attach(&sb);
    term.press(b"\x01/");
    odd.ran_as_told(&sb, &term, "picker");
    open_menu(&term, b"\x01 ");
    term.press(b"/");
    odd.ran_as_told(&sb, &term, "picker");
}

/// S8: a popup whose moa fails (a broken `moa.toml`, a brain that isn't there) stays open
/// once it has ended, so what it said is seen: `-EE` closes the popup only on success.
#[test]
fn a_popup_whose_moa_fails_stays_open() {
    let sb = Sandbox::start().unwrap();
    let dir = sb.home().join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("moa");
    // Its pid is written aside, then moved: `pid` is never seen half written.
    std::fs::write(
        &exe,
        "#!/bin/sh\nd=$(dirname \"$0\")\necho \"$$\" > \"$d/pid.part\"\nmv \"$d/pid.part\" \"$d/pid\"\n\
         echo 'moa: no brain called kite'\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let launch = PanelLaunch {
        exe,
        env: Vec::new(),
    };
    load_text(
        &sb,
        &moa_tmux("default", &UiConfig::default(), &launch, sb.home()),
    );
    sb.tmux(&["set-option", "-g", "@moa_ctl", "not-a-client"])
        .unwrap();
    let term = attach(&sb);
    let said = "no brain called kite";
    term.press(b"\x01/");
    assert!(
        term.wait_for_text(said, Duration::from_secs(10)),
        "the popup never ran moa; the terminal showed: {}",
        term.tail(600)
    );
    let pid = wait_for(Duration::from_secs(10), || {
        std::fs::read_to_string(dir.join("pid"))
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()
    })
    .expect("no pid");
    let reaped = wait_for(Duration::from_secs(10), || {
        // tmux 3.4 and 3.5 built with utempter can lose the exit until tmux reaps a child
        // of its own (`every_hook_reaches_the_mailbox`).
        let _ = sb.tmux(&["run-shell", "true"]);
        (!moa_platform::pid_alive(pid)).then_some(())
    });
    assert!(reaped.is_some(), "tmux never reaped the popup's moa");
    // Ended and reaped, it is still drawn: a redraw of the terminal draws it again.
    term.mark();
    sb.tmux(&["refresh-client", "-t", &term.tty()]).unwrap();
    assert!(
        term.wait_for_text(said, Duration::from_secs(10)),
        "the popup closed; the terminal showed: {}",
        term.tail(600)
    );
}

/// Each key the status line's hint names (`/ find · c console · …`, the arrows among them)
/// is a key in the table, or `Space` (the menu).
#[test]
fn the_status_hint_names_only_table_keys() {
    let template = include_str!("../src/moa.tmux.tpl");
    let left = template
        .lines()
        .find(|l| l.starts_with("set -g  status-left "))
        .expect("no status-left line");
    // The hint is the one styled run whose text lists keys: `key what · key what · …`.
    let hints: Vec<&str> = left
        .split("#[")
        .filter_map(|run| run.split_once(']').map(|(_, text)| text.trim()))
        .filter(|text| text.contains(" · "))
        .collect();
    assert_eq!(hints.len(), 1, "{hints:?}");
    let named: Vec<&str> = hints[0]
        .split(" · ")
        .map(|item| item.split_whitespace().next().unwrap_or(""))
        .collect();
    assert!(named.len() >= 9, "{named:?}");
    let table: Vec<&str> = ctrl_a(Form::Popup)
        .into_iter()
        .flatten()
        .map(|b| b.key)
        .collect();
    for k in named {
        assert!(
            k == "Space" || table.contains(&k),
            "the hint names {k:?}, which isn't in the table"
        );
    }
}

#[test]
fn it_sources_cleanly_on_this_tmux() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    assert_eq!(
        sb.tmux(&["show-options", "-gv", "status-interval"])
            .unwrap(),
        "0"
    );
    // The keys outside the table (`every_table_key_is_bound` checks the table's): the
    // arrows and `h j k l`, and the menu's two.
    let bound = moa_keys(&sb);
    let want = [
        "Left", "Right", "Up", "Down", "h", "j", "k", "l", "Space", "C-a",
    ];
    for k in want {
        assert!(
            bound.iter().any(|b| b == k),
            "{k} isn't bound in: {bound:?}"
        );
    }
    // The prefix table's own bindings.
    let prefix = sb.tmux(&["list-keys", "-T", "prefix"]).unwrap();
    for k in ["Space", "m", "C"] {
        let found = prefix.lines().any(|l| {
            let mut f = l.split_whitespace();
            f.by_ref().find(|w| *w == "prefix").is_some() && f.next() == Some(k)
        });
        assert!(found, "prefix {k} isn't bound");
    }
}

fn opts(sb: &Sandbox) -> ConnectOptions {
    ConnectOptions {
        tmux: tmux_path().unwrap_or_else(|e| panic!("{e}")),
        socket: sb.socket_args(),
        env: sb.env(),
        wait: CONNECT_WAIT,
    }
}

async fn connect(sb: &Sandbox) -> (ControlClient, Events, ServerInfo) {
    ControlClient::connect(&opts(sb))
        .await
        .unwrap_or_else(|e| panic!("connect failed: {e}"))
}

fn attach(sb: &Sandbox) -> Terminal {
    attach_sized(sb, 120, 30)
}

fn attach_sized(sb: &Sandbox, cols: u16, rows: u16) -> Terminal {
    let tmux = tmux_path().unwrap_or_else(|e| panic!("{e}"));
    let mut args: Vec<std::ffi::OsString> = vec!["-u".into()];
    args.extend(sb.socket_args());
    args.extend(["attach".into(), "-t".into(), "=sandbox".into()]);
    let term = Terminal::spawn(&tmux, &args, &sb.env(), cols, rows).unwrap();
    let tty = term.tty();
    let seen = wait_for(Duration::from_secs(5), || {
        let list = sb.tmux(&["list-clients", "-F", "#{client_name}"]).ok()?;
        list.lines().any(|l| l == tty).then_some(())
    });
    assert!(seen.is_some(), "the terminal never attached");
    // Listed is not drawn: keys typed before the first draw can be lost. The status line
    // is moa's when its config is loaded, else tmux's own, which starts with `[sandbox]`.
    let left = sb.tmux(&["show-options", "-gqv", "status-left"]).unwrap();
    let drawn = if left.contains("◆ moa") {
        "◆ moa"
    } else {
        "[sandbox]"
    };
    assert!(
        term.wait_for_text(drawn, Duration::from_secs(5)),
        "the status line never drew; the terminal showed: {}",
        term.tail(600)
    );
    term
}

/// The next `moa:` message, parsed, with no time limit.
async fn recv_mailbox(events: &mut Events) -> Mailbox {
    while let Some(ev) = events.recv().await {
        if let Event::Notification(Notification::Message(m)) = ev
            && m.starts_with("moa:")
        {
            return Mailbox::parse(&m).unwrap_or_else(|e| panic!("{m:?}: {e:?}"));
        }
    }
    panic!("the events ended")
}

/// The next `moa:` message, parsed.
async fn next_mailbox(events: &mut Events) -> Mailbox {
    tokio::time::timeout(Duration::from_secs(5), recv_mailbox(events))
        .await
        .expect("no moa message within 5 s")
}

/// The next hook message that `pick` takes, skipping other hooks (a new window also
/// changes a layout). `what` names the hook a failure waited for.
async fn next_hook<T>(events: &mut Events, what: &str, pick: impl Fn(HookMsg) -> Option<T>) -> T {
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match recv_mailbox(events).await {
                Mailbox::Hook(h) => {
                    if let Some(v) = pick(h) {
                        return v;
                    }
                }
                other => panic!("waiting for {what}: {other:?}"),
            }
        }
    })
    .await;
    found.unwrap_or_else(|_| panic!("no {what} hook within 5 s"))
}

/// Waits until `client` is back in the root key table, so the next Ctrl-a starts a fresh key.
fn wait_for_root(sb: &Sandbox, client: &ClientName) {
    let back = wait_for(Duration::from_secs(5), || {
        let list = sb
            .tmux(&["list-clients", "-F", "#{client_name} #{client_key_table}"])
            .ok()?;
        list.lines()
            .any(|l| l == format!("{} root", client.0))
            .then_some(())
    });
    assert!(
        back.is_some(),
        "the client never went back to the root keys"
    );
}

fn active_pane(sb: &Sandbox) -> PaneId {
    let out = sb
        .tmux(&["list-panes", "-t", "=sandbox:", "-F", "#{pane_id}"])
        .unwrap();
    out.parse().unwrap_or_else(|e| panic!("{out:?}: {e}"))
}

async fn with_ctl(sb: &Sandbox) -> (ControlClient, Events, ServerInfo) {
    let (client, events, info) = connect(sb).await;
    sb.tmux(&["set-option", "-g", "@moa_ctl", &info.client.0])
        .unwrap();
    (client, events, info)
}

/// The table's daemon keys, with what each tells the daemon, as the default `[ui]` has them.
fn daemon_keys() -> Vec<(&'static str, KeyAction)> {
    ctrl_a(UiConfig::default().picker)
        .into_iter()
        .flatten()
        .filter_map(|b| match b.does {
            Does::Moa(a) => Some((b.key, a)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_daemon_key_reaches_the_mailbox() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let (client, mut events, _) = with_ctl(&sb).await;
    let term = attach(&sb);
    let pane = active_pane(&sb);
    let tty = ClientName(term.tty());
    for (key, action) in daemon_keys() {
        term.press(b"\x01");
        term.press(key.as_bytes());
        match next_mailbox(&mut events).await {
            Mailbox::Key {
                action: got,
                pane: p,
                client: c,
                ..
            } => {
                assert_eq!(got, action);
                assert_eq!(p, Some(pane));
                assert_eq!(c, tty);
            }
            other => panic!("{action:?}: {other:?}"),
        }
        wait_for_root(&sb, &tty);
    }
    client.close().await;
}

/// Types `open` and waits until the menu is drawn, so the next key lands in it.
fn open_menu(term: &Terminal, open: &[u8]) {
    term.mark();
    term.press(open);
    assert!(
        term.wait_for_text("Big picture", Duration::from_secs(5)),
        "the menu never showed; the terminal showed: {}",
        term.tail(600)
    );
}

/// The menu fits the terminal, down to 80×24. tmux 3.4 never opens a menu taller than the
/// terminal and 3.8 draws the status line over its end, so below the tall menu's height (27
/// rows, `Terminals…` being in it only) and the status line's (2) the short one opens, without the separators and the
/// note on moving between splits. At each height its last item is drawn.
#[test]
fn the_menu_fits_a_small_terminal() {
    for (rows, tall) in [
        (24, false),
        (26, false),
        (27, false),
        (28, false),
        (29, true),
    ] {
        let sb = Sandbox::start().unwrap();
        load(&sb);
        let term = attach_sized(&sb, 80, rows);
        open_menu(&term, b"\x01 ");
        assert!(
            term.wait_for_text("Send Ctrl-a to the app", Duration::from_secs(5)),
            "{rows} rows: the menu's last item wasn't drawn; the terminal showed: {}",
            term.tail(600)
        );
        let note = String::from_utf8_lossy(&term.output()).contains("move between splits");
        assert_eq!(note, tall, "{rows} rows: the note on moving between splits");
        // And its items run: with no moa, the help item says so.
        term.mark();
        term.press(b"?");
        assert!(
            term.wait_for_text("moa isn't running: run moa", Duration::from_secs(5)),
            "{rows} rows: the help item didn't run; the terminal showed: {}",
            term.tail(600)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_menu_and_its_items_reach_the_mailbox() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let (client, mut events, _) = with_ctl(&sb).await;
    let term = attach(&sb);
    let tty = ClientName(term.tty());
    // Both ways in: Ctrl-a Space and a second Ctrl-a.
    for open in [&b"\x01 "[..], &b"\x01\x01"[..]] {
        open_menu(&term, open);
        term.press(b"c");
        match next_mailbox(&mut events).await {
            Mailbox::Key {
                action,
                pane,
                client,
                ..
            } => {
                assert_eq!(action, KeyAction::Console);
                assert_eq!(pane, None);
                assert_eq!(client, tty);
            }
            other => panic!("{other:?}"),
        }
    }
    // Every other daemon item, `s` (find, full tab) among them.
    for (key, action) in daemon_keys()
        .into_iter()
        .filter(|(_, a)| *a != KeyAction::Console)
    {
        open_menu(&term, b"\x01 ");
        term.press(key.as_bytes());
        match next_mailbox(&mut events).await {
            Mailbox::Key {
                action: got, pane, ..
            } => {
                assert_eq!(got, action);
                assert_eq!(pane, None);
            }
            other => panic!("{action:?}: {other:?}"),
        }
    }
    client.close().await;
}

/// The current tab's panes, each with its terminal mark and its label, as tmux has them.
fn marks(sb: &Sandbox) -> Vec<(PaneId, String, String)> {
    let out = sb
        .tmux(&[
            "list-panes",
            "-t",
            "=sandbox:",
            "-F",
            "#{pane_id}\t#{@moa_term}\t#{@moa_label}",
        ])
        .unwrap();
    out.lines()
        .filter_map(|l| {
            let [pane, term, label] = l.splitn(3, '\t').collect::<Vec<_>>()[..] else {
                return None;
            };
            Some((pane.parse().ok()?, term.to_string(), label.to_string()))
        })
        .collect()
}

/// Waits for the tab to hold `n` panes, and answers the newest one's mark and label.
fn split_to(sb: &Sandbox, term: &Terminal, n: usize) -> (String, String) {
    let found = wait_for(Duration::from_secs(10), || {
        let panes = marks(sb);
        (panes.len() == n).then(|| panes.into_iter().max_by_key(|(p, _, _)| p.0))?
    });
    let Some((_, mark, label)) = found else {
        panic!(
            "no split to {n} panes: {:?}; the terminal showed: {}",
            marks(sb),
            term.tail(600)
        );
    };
    (mark, label)
}

/// `|` and `-` open a shell that moa lists with its terminals: the new pane has the
/// terminal mark and the `⌨ shell` label, from a key and from the menu alike.
#[test]
fn the_split_keys_mark_terminals() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let term = attach(&sb);
    let tty = ClientName(term.tty());
    let shell = ("1".to_string(), "⌨ shell".to_string());
    term.press(b"\x01|");
    assert_eq!(split_to(&sb, &term, 2), shell, "Ctrl-a |");
    wait_for_root(&sb, &tty);
    open_menu(&term, b"\x01 ");
    term.press(b"-");
    assert_eq!(split_to(&sb, &term, 3), shell, "the menu's -");
}

/// What a plain key runs, as tmux lists it with the quoting taken off: what the table's
/// `Does` stands for. `None` for the keys that go to the daemon or open a popup, which
/// `every_daemon_key_reaches_the_mailbox` and the popup tests cover, and for the two that
/// are no command of their own.
fn runs(does: Does) -> Option<&'static str> {
    match does {
        Does::SplitBeside => Some(
            "split-window -h -c #{pane_current_path} ; set -p @moa_label ⌨ shell ; set -p @moa_term 1",
        ),
        Does::SplitBelow => Some(
            "split-window -v -c #{pane_current_path} ; set -p @moa_label ⌨ shell ; set -p @moa_term 1",
        ),
        Does::BackToMoa => Some("select-window -t moa:moa"),
        Does::Zoom => Some("resize-pane -Z"),
        Does::PrevTab => Some("previous-window"),
        Does::NextTab => Some("next-window"),
        Does::SendCtrlA => Some("send-keys C-a"),
        Does::Moa(_) | Does::Popup(_) | Does::Move | Does::Menu => None,
    }
}

/// The words of a command as tmux lists it: `"…"` (with `\` escapes), `'…'`, or bare (with
/// `\` escapes too: tmux lists the key `$` as `\$`).
fn listed_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let mut word = String::new();
        match c {
            ' ' | '\n' => continue,
            '\'' => word.extend(chars.by_ref().take_while(|&c| c != '\'')),
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => word.extend(chars.next()),
                        '"' => break,
                        _ => word.push(c),
                    }
                }
            }
            _ => {
                let mut c = Some(c);
                while let Some(ch) = c {
                    match ch {
                        '\\' => word.extend(chars.next()),
                        _ => word.push(ch),
                    }
                    c = chars.next_if(|&c| c != ' ' && c != '\n');
                }
            }
        }
        words.push(word);
    }
    words
}

/// A command with its inner quoting taken off, and `##` read as `#`, as a menu item's
/// command is expanded once before it runs.
fn unquoted(text: &str) -> String {
    text.replace(['\'', '"', '\\'], "").replace("##", "#")
}

/// Every menu item in tmux's listing of the menu's binding, each as its key and its
/// command: the short menu's and the tall one's.
fn menu_items_listed(listed: &str) -> Vec<(String, String)> {
    let words = listed_words(listed);
    let mut items = Vec::new();
    let mut at = 0;
    while let Some(start) = words[at..].iter().position(|w| w == "display-menu") {
        at += start + 1;
        while matches!(words[at].as_str(), "-T" | "-x" | "-y") {
            at += 2;
        }
        while words[at] != "}" {
            // An empty label is a separator, alone; any other item is a label, a key and
            // a command (a greyed note's are empty).
            if words[at].is_empty() {
                at += 1;
                continue;
            }
            items.push((words[at + 1].clone(), words[at + 2].clone()));
            at += 3;
        }
    }
    items
}

/// The template's plain keys and the menu's copies of them, which are written apart, as
/// this tmux reads them: each runs what the table's `Does` says, so neither drifts.
#[test]
fn the_plain_keys_do_what_the_table_says() {
    let sb = Sandbox::start().unwrap();
    load(&sb);
    let menu = menu_items_listed(&sb.tmux(&["list-keys", "-T", "moa", "Space"]).unwrap());
    let mut checked = Vec::new();
    for b in ctrl_a(UiConfig::default().picker).into_iter().flatten() {
        let Some(want) = runs(b.does) else {
            continue;
        };
        let listed = sb.tmux(&["list-keys", "-T", "moa", b.key]).unwrap();
        let words = listed_words(&listed);
        assert_eq!(words[1..4], ["-T", "moa", b.key], "{listed}");
        let key = unquoted(&words[4..].join(" "));
        assert!(key.ends_with(want), "{}: {key}", b.key);
        let copies: Vec<String> = menu
            .iter()
            .filter(|(k, _)| k == b.key)
            .map(|(_, cmd)| unquoted(cmd))
            .collect();
        assert_eq!(
            copies.len(),
            2,
            "{}: the short menu's and the tall one's",
            b.key
        );
        for copy in copies {
            assert!(copy.ends_with(want), "{}: the menu's {copy}", b.key);
        }
        checked.push(b.key);
    }
    assert_eq!(checked, ["|", "-", "m", "z", "[", "]", "a"]);
}

/// A daemon key and the popup's both say so; the popup opens nothing.
#[test]
fn keys_say_moa_isnt_running_without_a_ctl() {
    for keys in [&b"\x01c"[..], &b"\x01/"[..]] {
        let sb = Sandbox::start().unwrap();
        load(&sb);
        let term = attach(&sb);
        term.mark();
        term.press(keys);
        assert!(
            term.wait_for_text("moa isn't running: run moa", Duration::from_secs(5)),
            "{keys:?}: no notice; the terminal showed: {}",
            term.tail(600)
        );
    }
}

fn ctl_option(sb: &Sandbox) -> String {
    sb.tmux(&["show-options", "-gqv", "@moa_ctl"]).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ctl_option_is_cleared_when_the_ctl_dies() {
    let sb = Sandbox::start().unwrap();
    let (client, _events, info) = with_ctl(&sb).await;
    for c in hooks(&info.client) {
        client.run(&c).await.unwrap();
    }
    assert_eq!(ctl_option(&sb), info.client.0);
    let killed = std::process::Command::new("kill")
        .args(["-9", &client.child_pid().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let cleared = wait_for(Duration::from_secs(2), || {
        ctl_option(&sb).is_empty().then_some(())
    });
    assert!(cleared.is_some(), "@moa_ctl is still {:?}", ctl_option(&sb));
}

#[tokio::test(flavor = "multi_thread")]
async fn user_hooks_at_other_indexes_survive() {
    let sb = Sandbox::start().unwrap();
    sb.tmux(&["set-hook", "-g", "pane-exited[5]", "set-option -g @user 1"])
        .unwrap();
    let (client, _events, info) = connect(&sb).await;
    for c in hooks(&info.client) {
        client.run(&c).await.unwrap();
    }
    // Session hooks and window hooks are listed apart.
    let shown = |sb: &Sandbox| {
        let s = sb.tmux(&["show-hooks", "-g"]).unwrap();
        s + "\n" + &sb.tmux(&["show-hooks", "-gw"]).unwrap()
    };
    let before = shown(&sb);
    assert!(before.contains("pane-exited[90]"), "{before}");
    assert!(before.contains("client-detached[90]"), "{before}");
    for c in unhooks() {
        client.run(&c).await.unwrap();
    }
    let shown = shown(&sb);
    assert!(shown.contains("pane-exited[5]"), "{shown}");
    assert!(!shown.contains("[90]"), "{shown}");
    client.close().await;
}

/// A new window, in the background, whose command exits at once; its pane.
fn new_window(sb: &Sandbox) -> PaneId {
    let out = sb
        .tmux(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "=sandbox:",
            "exit 0",
        ])
        .unwrap();
    out.parse().unwrap_or_else(|e| panic!("{out:?}: {e}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_hook_reaches_the_mailbox() {
    let sb = Sandbox::start().unwrap();
    let (client, mut events, info) = connect(&sb).await;
    for c in hooks(&info.client) {
        client.run(&c).await.unwrap();
    }

    // A window whose command exits.
    let pane = new_window(&sb);
    next_hook(&mut events, "pane-exited", |h| {
        (h == HookMsg::PaneExited(pane)).then_some(())
    })
    .await;

    // The same, kept on screen. tmux 3.4 and 3.5 built with utempter (Linux distro
    // packages) can lose this pane's SIGCHLD: when the pty closes before the child is
    // reaped, tmux closes the pane and runs the utmp helper with SIGCHLD set to SIG_DFL,
    // and a SIGCHLD that lands then is dropped (3.6 raises it again after the helper).
    // The pane is then dead with no exit status, so no pane-died until tmux next reaps a
    // child. Once the pane is dead, a run-shell job makes it reap.
    sb.tmux(&["set-option", "-g", "remain-on-exit", "on"])
        .unwrap();
    let pane = new_window(&sb);
    let target = pane.to_string();
    let dead = wait_for(Duration::from_secs(5), || {
        let d = sb
            .tmux(&["display-message", "-p", "-t", &target, "#{pane_dead}"])
            .ok()?;
        (d == "1").then_some(())
    });
    assert!(dead.is_some(), "{pane} never died");
    sb.tmux(&["run-shell", "true"]).unwrap();
    next_hook(&mut events, "pane-died", |h| {
        (h == HookMsg::PaneDied(pane)).then_some(())
    })
    .await;

    // A split changes the layout.
    sb.tmux(&["split-window", "-d", "-t", "=sandbox:"]).unwrap();
    next_hook(&mut events, "layout", |h| {
        matches!(h, HookMsg::Layout(_)).then_some(())
    })
    .await;

    // A terminal attaching.
    let term = attach(&sb);
    let c = next_hook(&mut events, "client-attached", |h| match h {
        HookMsg::ClientAttached(c) => Some(c),
        _ => None,
    })
    .await;
    assert_eq!(c.0, term.tty());
    client.close().await;
}

#[test]
fn menu_items_say_moa_isnt_running_without_a_ctl() {
    // A daemon item and the popup's.
    for key in [b"c", b"/"] {
        let sb = Sandbox::start().unwrap();
        load(&sb);
        let term = attach(&sb);
        open_menu(&term, b"\x01 ");
        term.mark();
        term.press(key);
        assert!(
            term.wait_for_text("moa isn't running: run moa", Duration::from_secs(5)),
            "{key:?}: no notice; the terminal showed: {}",
            term.tail(600)
        );
    }
}

#[test]
fn the_hook_commands_are_allowed_ones() {
    let ctl = ClientName("client-9".into());
    for c in hooks(&ctl).into_iter().chain(unhooks()) {
        assert_eq!(c.check(), Ok(()), "{}", c.line());
        assert!(!c.line().contains('\n'));
    }
}
