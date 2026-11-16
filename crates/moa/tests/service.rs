//! `moa service install|uninstall`: the real binary, only ever against a `/tmp` root, where
//! the file is written but nothing is registered.

use std::process::{Command, Output};

use moa_testkit::TempBrain;

const MOA: &str = env!("CARGO_BIN_EXE_moa");

fn moa(tb: &TempBrain, args: &[&str]) -> Output {
    Command::new(MOA)
        .args(args)
        .env_clear()
        .envs(tb.env())
        .current_dir(tb.root())
        .output()
        .unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).trim().to_string()
}

fn service_file(tb: &TempBrain, brain: &str) -> std::path::PathBuf {
    let paths = tb.paths();
    if cfg!(target_os = "macos") {
        paths
            .launch_agents_dir()
            .join(format!("moa.daemon.{brain}.plist"))
    } else {
        paths
            .systemd_user_dir()
            .join(format!("moa-{brain}.service"))
    }
}

#[test]
fn install_and_uninstall_a_named_brain() {
    let tb = TempBrain::new(Some("[brains.work]\nfolders = []\n"));
    let o = moa(
        &tb,
        &[
            "service",
            "install",
            "--brain",
            "work",
            "--exe",
            "/opt/moa/bin/moa",
        ],
    );
    assert!(o.status.success(), "{}", err(&o));
    let file = service_file(&tb, "work");
    assert!(file.starts_with(tb.root()));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("/opt/moa/bin/moa"));
    assert!(text.contains("work"));
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.contains("sandbox"), "{out}");

    let o = moa(&tb, &["service", "uninstall", "--brain", "work"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(!file.exists());
}

#[test]
fn the_brain_must_be_in_the_config() {
    let tb = TempBrain::new(None);
    let o = moa(
        &tb,
        &["service", "install", "--brain", "nope", "--exe", "/opt/moa"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o),
        format!(
            "there's no brain named nope in {}",
            tb.paths().config_file().display()
        )
    );
}

#[test]
fn a_nix_store_exe_is_refused() {
    let tb = TempBrain::new(None);
    let o = moa(
        &tb,
        &["service", "install", "--exe", "/nix/store/abc-moa/bin/moa"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).starts_with("that moa is a Nix store path"),
        "{}",
        err(&o)
    );
}

#[test]
fn without_moa_root_it_is_the_d20_line() {
    let tb = TempBrain::new(None);
    // HOME is the temp root and MOA_REAL_HOME is unset, so nothing is written or spawned.
    let o = Command::new(MOA)
        .args(["service", "install", "--exe", "/opt/moa"])
        .env_clear()
        .env("HOME", tb.root())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(tb.root())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o),
        "moa v1 isn't switched over yet; set MOA_ROOT to try it (see the try-it)"
    );
    assert!(!tb.root().join("Library").exists());
    assert!(!tb.root().join(".config/systemd").exists());
}
