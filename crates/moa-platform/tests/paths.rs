use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use moa_platform::{Paths, PathsError, TmuxSocket, ensure_private_dir, sandbox_root};

fn p(root: &str) -> Paths {
    Paths::with_root(Path::new(root))
}
fn vars(root: Option<&str>, home: Option<&str>, real: bool) -> Result<Paths, PathsError> {
    Paths::from_vars(root.map(Into::into), home.map(Into::into), None, 501, real)
}
fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap()
}

#[test]
fn moa_root_moves_every_path() {
    let x = p("/r");
    assert_eq!(x.config_dir(), Path::new("/r/.config/moa"));
    assert_eq!(x.config_file(), Path::new("/r/.config/moa/moa.toml"));
    assert_eq!(x.data_dir(), Path::new("/r/.local/share/moa"));
    assert_eq!(x.brain_dir("work"), Path::new("/r/.local/share/moa/work"));
    assert_eq!(
        x.db("default"),
        Path::new("/r/.local/share/moa/default/moa.db")
    );
    assert_eq!(
        x.backups("work"),
        Path::new("/r/.local/share/moa/work/backups")
    );
    assert_eq!(x.state_dir(), Path::new("/r/.local/state/moa"));
    assert_eq!(x.run_dir(), Path::new("/r/.local/state/moa/run"));
    assert_eq!(
        x.lock("work"),
        Path::new("/r/.local/state/moa/run/work.lock")
    );
    assert_eq!(
        x.start_lock("work"),
        Path::new("/r/.local/state/moa/run/work.start")
    );
    assert_eq!(
        x.socket("work").unwrap(),
        Path::new("/r/.local/state/moa/run/work.sock")
    );
    assert_eq!(
        x.spool("work"),
        Path::new("/r/.local/state/moa/spool/work.jsonl")
    );
    assert_eq!(x.log_dir(), Path::new("/r/.local/state/moa/logs"));
    assert_eq!(
        x.log("work"),
        Path::new("/r/.local/state/moa/logs/work.log")
    );
    assert_eq!(
        x.stderr_log("work"),
        Path::new("/r/.local/state/moa/logs/work.stderr.log")
    );
    assert_eq!(x.launch_agents_dir(), Path::new("/r/Library/LaunchAgents"));
    assert_eq!(x.systemd_user_dir(), Path::new("/r/.config/systemd/user"));
    assert!(x.is_sandboxed());
}

#[test]
fn moa_root_wins_over_home() {
    assert_eq!(
        vars(Some("/r"), Some("/home/u"), false).unwrap().home(),
        Path::new("/r")
    );
    let y = vars(None, Some("/home/u"), false).unwrap();
    assert_eq!(y.home(), Path::new("/home/u"));
    assert!(!y.is_sandboxed());
}

#[test]
fn bad_roots_and_no_home_are_refused() {
    assert_eq!(
        vars(Some("rel"), None, false).unwrap_err().to_string(),
        "MOA_ROOT must be a full path"
    );
    assert_eq!(
        vars(None, None, false).unwrap_err().to_string(),
        "can't tell your home folder: HOME isn't set"
    );
}

#[test]
fn the_real_home_is_off_limits_until_the_switch() {
    assert!(
        vars(Some("/r"), Some("/home/u"), false)
            .unwrap()
            .check_v1_allowed()
            .is_ok()
    );
    assert_eq!(
        vars(None, Some("/home/u"), false)
            .unwrap()
            .check_v1_allowed()
            .unwrap_err()
            .to_string(),
        "moa v1 isn't switched over yet; set MOA_ROOT to try it (see the try-it)"
    );
    assert!(
        vars(None, Some("/home/u"), true)
            .unwrap()
            .check_v1_allowed()
            .is_ok()
    );
}

#[test]
fn read_config_is_none_when_missing_and_text_when_present() {
    let t = tmp();
    let x = Paths::with_root(t.path());
    assert_eq!(x.read_config().unwrap(), None);
    std::fs::create_dir_all(x.config_dir()).unwrap();
    std::fs::write(x.config_file(), "a = 1\n").unwrap();
    assert_eq!(x.read_config().unwrap().as_deref(), Some("a = 1\n"));
}

#[test]
fn private_dirs_are_made_0700_and_open_ones_refused() {
    let t = tmp();
    let root = t.path();
    ensure_private_dir(&root.join("a/b")).unwrap();
    assert!(root.join("a").is_dir());
    let mode = std::fs::metadata(root.join("a/b"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    // idempotent
    ensure_private_dir(&root.join("a/b")).unwrap();

    let c = root.join("c");
    std::fs::create_dir(&c).unwrap();
    std::fs::set_permissions(&c, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = ensure_private_dir(&c).unwrap_err();
    let c = c.display();
    assert_eq!(
        err.to_string(),
        format!("{c} is open to other users; moa won't use it (chmod 700 {c})")
    );

    let d = root.join("d");
    symlink(root.join("a/b"), &d).unwrap();
    let err = ensure_private_dir(&d).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{} is a symlink; moa won't use it", d.display())
    );
}

#[test]
fn a_long_socket_path_falls_back_to_tmpdir() {
    let long = format!("/{}", "a".repeat(100));
    let x = Paths::from_vars(
        Some(long.clone().into()),
        None,
        Some("/t".into()),
        501,
        false,
    )
    .unwrap();
    let s = x.socket("default").unwrap();
    assert!(s.starts_with("/t/moa-501/") && s.to_str().unwrap().ends_with("-default.sock"));
    assert!(s.as_os_str().len() <= 103);
    let other = Paths::from_vars(
        Some(format!("{long}b").into()),
        None,
        Some("/t".into()),
        501,
        false,
    )
    .unwrap();
    assert_ne!(other.socket("default").unwrap(), s);
}

#[test]
fn the_limit_is_103_bytes() {
    // run/<b>.sock = root + "/.local/state/moa/run/" + "x" + ".sock"
    let tail = "/.local/state/moa/run/x.sock".len();
    let at = |len: usize| {
        let root = format!("/{}", "r".repeat(len - tail - 1));
        let x = Paths::from_vars(
            Some(root.clone().into()),
            None,
            Some("/t".into()),
            501,
            false,
        )
        .unwrap();
        (root, x.socket("x").unwrap())
    };
    let (root, s) = at(103);
    assert_eq!(s.as_os_str().len(), 103);
    assert!(s.starts_with(&root));
    let (_, s) = at(104);
    assert!(s.starts_with("/t/moa-501/"));
}

#[test]
fn a_fallback_that_is_still_too_long_is_refused() {
    let tmpdir = format!("/{}", "t".repeat(100));
    let x = Paths::from_vars(
        Some(format!("/{}", "a".repeat(100)).into()),
        None,
        Some(tmpdir.into()),
        501,
        false,
    )
    .unwrap();
    assert_eq!(
        x.socket("default").unwrap_err().to_string(),
        "the socket path is too long even under TMPDIR; set a shorter TMPDIR"
    );
}

#[test]
fn tmpdir_defaults_to_slash_tmp() {
    let x = Paths::from_vars(
        Some(format!("/{}", "a".repeat(100)).into()),
        None,
        None,
        7,
        false,
    )
    .unwrap();
    assert!(x.socket("d").unwrap().starts_with("/tmp/moa-7/"));
}

#[test]
fn tmpdir_is_the_given_absolute_one_else_slash_tmp() {
    let with = |t: Option<&str>| {
        Paths::from_vars(Some("/r".into()), None, t.map(Into::into), 7, false)
            .unwrap()
            .tmpdir()
            .to_path_buf()
    };
    assert_eq!(with(Some("/var/t")), Path::new("/var/t"));
    assert_eq!(with(Some("relative")), Path::new("/tmp"));
    assert_eq!(with(None), Path::new("/tmp"));
    assert_eq!(p("/r").tmpdir(), Path::new("/tmp"));
}

#[test]
fn a_trailing_slash_does_not_hide_a_symlink() {
    let t = tmp();
    let real = t.path().join("real");
    ensure_private_dir(&real).unwrap();
    let link = t.path().join("link");
    symlink(&real, &link).unwrap();
    let slashed = format!("{}/", link.display());
    let err = ensure_private_dir(Path::new(&slashed)).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{} is a symlink; moa won't use it", link.display())
    );
}

#[test]
fn a_dangling_symlink_is_refused_as_a_symlink() {
    let t = tmp();
    let link = t.path().join("dangling");
    symlink(t.path().join("nowhere"), &link).unwrap();
    let err = ensure_private_dir(&link).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{} is a symlink; moa won't use it", link.display())
    );
}

#[test]
fn a_plain_file_is_refused_as_not_a_folder() {
    let t = tmp();
    let f = t.path().join("file");
    std::fs::write(&f, b"x").unwrap();
    let err = ensure_private_dir(&f).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{} isn't a folder; moa won't use it", f.display())
    );
}

#[test]
fn an_empty_home_is_no_home() {
    assert_eq!(
        vars(None, Some(""), false).unwrap_err().to_string(),
        "can't tell your home folder: HOME isn't set"
    );
}

#[test]
fn an_empty_or_relative_tmpdir_falls_back_to_tmp() {
    let root = format!("/{}", "a".repeat(100));
    for bad in ["", "rel/tmp"] {
        let x =
            Paths::from_vars(Some(root.clone().into()), None, Some(bad.into()), 7, false).unwrap();
        assert!(
            x.socket("d").unwrap().starts_with("/tmp/moa-7/"),
            "TMPDIR={bad:?}"
        );
    }
}

#[test]
fn tmux_socket_is_inside_the_root_when_sandboxed() {
    let x = p("/r");
    let s = x.tmux_socket("work").unwrap();
    assert_eq!(
        s,
        TmuxSocket::Path("/r/.local/state/moa/run/tmux-work".into())
    );
    assert_eq!(
        s.args(),
        [
            std::ffi::OsString::from("-S"),
            "/r/.local/state/moa/run/tmux-work".into()
        ]
    );
    assert_eq!(s.path(Some(Path::new("/x")), 1), s.path(None, 2));
    assert_eq!(
        x.tmux_conf("work"),
        Path::new("/r/.local/share/moa/work/tmux.conf")
    );
    assert_eq!(
        x.moa_tmux("work"),
        Path::new("/r/.local/share/moa/work/moa.tmux")
    );
}

#[test]
fn real_brains_keep_the_spike_names() {
    let x = vars(None, Some("/home/u"), true).unwrap();
    assert_eq!(
        x.tmux_socket("default").unwrap(),
        TmuxSocket::Name("moa".into())
    );
    let w = x.tmux_socket("work").unwrap();
    assert_eq!(w, TmuxSocket::Name("moa-work".into()));
    assert_eq!(
        w.args(),
        [std::ffi::OsString::from("-L"), "moa-work".into()]
    );
}

#[test]
fn a_long_sandbox_socket_falls_back() {
    let long = format!("/{}", "a".repeat(100));
    let x = Paths::from_vars(Some(long.into()), None, Some("/t".into()), 501, false).unwrap();
    let Ok(TmuxSocket::Path(s)) = x.tmux_socket("default") else {
        panic!("no path socket")
    };
    assert!(s.starts_with("/t/moa-501/") && s.to_str().unwrap().ends_with("-tmux-default"));
    assert!(s.as_os_str().len() <= 103);
    let tiny = Paths::from_vars(
        Some(format!("/{}", "a".repeat(100)).into()),
        None,
        Some(format!("/{}", "t".repeat(100)).into()),
        501,
        false,
    )
    .unwrap();
    assert!(matches!(
        tiny.tmux_socket("default"),
        Err(PathsError::SocketTooLong)
    ));
}

#[test]
fn name_sockets_follow_tmux_tmpdir() {
    let s = TmuxSocket::Name("moa".into());
    assert_eq!(s.path(None, 501), Path::new("/tmp/tmux-501/moa"));
    assert_eq!(
        s.path(Some(Path::new("/x/y")), 7),
        Path::new("/x/y/tmux-7/moa")
    );
}

#[test]
fn io_errors_name_the_path() {
    let t = tmp();
    let f = t.path().join("file");
    std::fs::write(&f, b"x").unwrap();
    let under = f.join("sub");
    let err = ensure_private_dir(&under).unwrap_err();
    assert!(matches!(err, PathsError::Io(_)));
    assert!(
        err.to_string().contains(&under.display().to_string()),
        "{err}"
    );
}

#[test]
fn the_sandbox_lives_under_local_state() {
    assert_eq!(
        sandbox_root(Path::new("/home/u")),
        Path::new("/home/u/.local/state/moa-sandbox")
    );
}
