//! Service registration: the file text, the exe rule, and that a sandbox never registers.

use std::ffi::OsString;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use moa_platform::service::{self, ServiceError};
use moa_platform::{Paths, spawn};

fn unsandboxed(real_home_ok: bool) -> Paths {
    // Pure text generation (or a refusal): nothing is written or run with this.
    Paths::from_vars(
        None,
        Some(OsString::from("/home/u")),
        None,
        1000,
        real_home_ok,
    )
    .unwrap()
}

#[test]
fn the_plist_and_unit_text() {
    let sandbox = Paths::with_root(Path::new("/r"));
    let real = unsandboxed(true);
    let exe = Path::new("/opt/moa bin/moa&co");
    let plist = |p: &Paths| service::launchd_plist(exe, "work", p);
    let unit = |p: &Paths| service::systemd_unit(exe, "work", p);
    insta::assert_snapshot!("plist_sandboxed", plist(&sandbox));
    insta::assert_snapshot!("unit_sandboxed", unit(&sandbox));
    insta::assert_snapshot!("plist_unsandboxed", plist(&real));
    insta::assert_snapshot!("unit_unsandboxed", unit(&real));
    let pct = Path::new("/opt/50%/moa");
    insta::assert_snapshot!("unit_percent", service::systemd_unit(pct, "work", &sandbox));
    let quoted = Path::new("/opt/a\"b\\c/moa");
    insta::assert_snapshot!(
        "unit_quotes",
        service::systemd_unit(quoted, "work", &sandbox)
    );
}

#[test]
fn the_label() {
    assert_eq!(service::label("work"), "moa.daemon.work");
}

#[test]
fn the_stable_exe_rule() {
    let home = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    // No profile link: this program.
    assert_eq!(
        service::stable_exe(None, home.path()).unwrap(),
        std::env::current_exe().unwrap()
    );
    let store = home.path().join("nix/store/abc-moa/bin");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("moa"), "").unwrap();
    let bin = home.path().join(".nix-profile/bin");
    std::fs::create_dir_all(&bin).unwrap();
    symlink(store.join("moa"), bin.join("moa")).unwrap();
    assert_eq!(
        service::stable_exe(None, home.path()).unwrap(),
        home.path().join(".nix-profile/bin/moa")
    );
}

#[test]
fn a_nix_store_path_is_refused() {
    let home = Path::new("/r");
    let err = service::stable_exe(Some(Path::new("/nix/store/abc-moa/bin/moa")), home).unwrap_err();
    assert!(matches!(err, ServiceError::NixStore));
    assert_eq!(
        err.to_string(),
        "that moa is a Nix store path that changes on upgrade; run it from your profile \
         (~/.nix-profile/bin/moa) or pass --exe"
    );
    let ok = Path::new("/r/.nix-profile/bin/moa");
    assert_eq!(service::stable_exe(Some(ok), home).unwrap(), ok);
}

#[test]
fn a_sandboxed_install_writes_but_never_registers() {
    let tmp = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let paths = Paths::with_root(tmp.path());
    let exe = Path::new("/opt/moa/bin/moa");
    let before = spawn::launches();
    let out = service::install(&paths, "work", exe).unwrap();
    assert!(!out.registered);
    assert!(out.message.contains("sandbox"), "{}", out.message);
    assert!(out.file.starts_with(tmp.path()));
    let want = if cfg!(target_os = "macos") {
        service::launchd_plist(exe, "work", &paths)
    } else {
        service::systemd_unit(exe, "work", &paths)
    };
    assert_eq!(std::fs::read_to_string(&out.file).unwrap(), want);
    let mode = std::fs::metadata(&out.file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o644);

    let again = service::install(&paths, "work", exe).unwrap();
    assert!(
        again.message.contains("already installed"),
        "{}",
        again.message
    );
    assert!(!again.registered);

    let gone = service::uninstall(&paths, "work").unwrap();
    assert!(!out.file.exists());
    assert!(gone.message.contains("sandbox"), "{}", gone.message);
    let none = service::uninstall(&paths, "work").unwrap();
    assert_eq!(none.message, "not installed");
    assert_eq!(spawn::launches(), before);
}

#[test]
fn an_unsandboxed_paths_without_the_switch_is_refused_by_the_gate() {
    // Only the gate is asked; install and uninstall are never called on unsandboxed Paths.
    assert!(unsandboxed(false).check_v1_allowed().is_err());
}

#[test]
fn an_exe_must_be_a_full_path_and_is_cleaned_before_the_store_check() {
    let home = Path::new("/r");
    let err = service::stable_exe(Some(Path::new("bin/moa")), home).unwrap_err();
    assert!(matches!(err, ServiceError::NotAbsolute(_)));
    assert_eq!(err.to_string(), "--exe must be a full path, not bin/moa");
    let sneaky = Path::new("/opt/../nix/store/abc-moa/bin/moa");
    let err = service::stable_exe(Some(sneaky), home).unwrap_err();
    assert!(matches!(err, ServiceError::NixStore));
    assert_eq!(
        service::stable_exe(Some(Path::new("/opt/./x/../moa")), home).unwrap(),
        Path::new("/opt/moa")
    );
}
