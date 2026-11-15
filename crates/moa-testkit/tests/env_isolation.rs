#[test]
fn parent_env_never_reaches_the_sandbox() {
    #[allow(unsafe_code)] // roadmap P15: tests may set the env in a one-test binary
    // SAFETY: this test binary has exactly one test, so no other thread reads the env.
    unsafe {
        std::env::set_var("MOA_BRAIN", "leak");
        std::env::set_var("CLAUDE_CONFIG_DIR", "/leak");
        std::env::set_var("TMUX", "/tmp/leak,1,0");
    }
    let sb = moa_testkit::Sandbox::start().unwrap();
    let global = sb.tmux(&["show-environment", "-g"]).unwrap();
    for bad in ["MOA_", "CLAUDE_", "TMUX="] {
        assert!(
            !global.lines().any(|l| l.starts_with(bad)),
            "{bad} leaked:\n{global}"
        );
    }
    // and a process in a pane doesn't see them either: dump its env to a file (no typing,
    // no screen scraping, no dependence on the user's shell)
    let out = sb.home().join("env.txt");
    sb.tmux(&[
        "new-window",
        "-d",
        &format!("env > '{}.tmp' && mv '{0}.tmp' '{0}'", out.display()),
    ])
    .unwrap();
    let env = moa_testkit::wait_for(std::time::Duration::from_secs(5), || {
        std::fs::read_to_string(&out).ok()
    })
    .expect("the pane never wrote its env");
    assert!(
        !env.lines()
            .any(|l| l.starts_with("MOA_BRAIN=") || l.starts_with("CLAUDE_CONFIG_DIR=")),
        "a pane saw a leaked variable:\n{env}"
    );
    assert!(
        !env.lines()
            .any(|l| l.starts_with("TMUX=") && l.contains("leak")),
        "a pane saw the leaked TMUX:\n{env}"
    );
    assert!(
        env.lines()
            .any(|l| l == format!("HOME={}", sb.home().display()))
    );
}
