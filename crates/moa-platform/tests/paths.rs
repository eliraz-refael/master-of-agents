use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use moa_platform::{Paths, PathsError, ensure_private_dir};

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
