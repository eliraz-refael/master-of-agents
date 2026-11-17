use assert_cmd::Command;
use moa_testkit::TempBrain;

#[test]
fn version_prints_name_and_version() {
    Command::cargo_bin("moa")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout("moa 0.1.0\n");
}

/// T21: until the switch-over, the front door opens only a sandbox, even with
/// `MOA_REAL_HOME=1`, and starts nothing.
#[test]
fn moa_without_the_sandbox_refuses() {
    let tb = TempBrain::new(None);
    for real in [None, Some("1")] {
        let mut cmd = Command::cargo_bin("moa").unwrap();
        cmd.env_clear()
            .env("HOME", tb.root())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("SHELL", "/bin/sh");
        if let Some(v) = real {
            cmd.env("MOA_REAL_HOME", v);
        }
        cmd.assert()
            .code(1)
            .stdout("")
            .stderr("moa v1 isn't switched over yet; try moa --sandbox\n");
        // No daemon (no socket, no lock) and no terminal server under the root.
        let state = tb.root().join(".local/state");
        assert!(!state.exists(), "{} was created", state.display());
    }
}
