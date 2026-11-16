use moa_platform::{Spawner, spawn};

#[test]
fn spawner_counts_every_launch() {
    let before = spawn::launches();
    Spawner
        .spawn(&mut std::process::Command::new("true"))
        .unwrap()
        .wait()
        .unwrap();
    let _ = Spawner.spawn(&mut std::process::Command::new("/no/such/program"));
    assert_eq!(spawn::launches(), before + 2);
}
