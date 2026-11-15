use assert_cmd::Command;

#[test]
fn version_prints_name_and_version() {
    Command::cargo_bin("moa")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout("moa 0.1.0\n");
}

#[test]
fn bare_moa_says_it_is_not_ready_and_points_to_the_spike() {
    // Until the switch-over, v1 must never be mistaken for the working moa.
    Command::cargo_bin("moa")
        .unwrap()
        .assert()
        .code(2)
        .stderr(predicates::str::contains("moa v1 is still being built"));
}
