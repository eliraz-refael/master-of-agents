//! Slice 9: the daemon reads `moa.toml` again when it is saved (L8, §6.15.4): a good one
//! replaces the config, and a `[ui]` change rewrites moa's tmux config and has the terminal
//! server source it. An editor's several steps are one save.

mod common;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::create_server;
use common::rig::{Rig, Up, WAIT};
use moa_core::publish::Topic;
use moa_testkit::{stub_moa_toml, wait_for};
use serde_json::{Value, json};

/// Saves `text` as `moa.toml` the way an editor does: written beside it, renamed over it.
fn save(rig: &Rig, text: &str) {
    let file = rig.paths.config_file();
    let new = file.with_extension("toml.new");
    std::fs::write(&new, text).expect("the new moa.toml is written");
    std::fs::rename(&new, &file).expect("it is put in place");
}

/// How many saves of `moa.toml` the daemon read, and how many times it had the terminal
/// server source moa's tmux config again. Read from its counters, not its log: the log is
/// the process's first daemon's.
fn counts(up: &Up) -> (u64, u64) {
    let stats = up.core.stats();
    (
        stats.configs_read.load(Ordering::SeqCst),
        stats.tmux_sources.load(Ordering::SeqCst),
    )
}

/// The `brain` row as published now.
fn brain_row(up: &Up) -> Value {
    let model = up.core.model();
    let model = model.borrow();
    model.topics[&Topic::Brain]["brain"].clone()
}

fn text(path: &Path) -> String {
    std::fs::read_to_string(path).expect("the file reads")
}

/// `[ui] mouse` turned on in `moa.toml` while moa runs: the daemon writes moa's tmux config
/// again and the server sources it, once, with no re-attach and no restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editing_moa_toml_reloads() {
    let rig = Rig::with_toml(&format!("{}[ui]\nmouse = false\n", stub_moa_toml()), &[]);
    let up = rig.start().await;
    create_server(&rig.tb, &rig.paths);
    up.link();
    let moa_tmux = rig.paths.moa_tmux("default");
    let mouse = || rig.tmux(&["show-options", "-gv", "mouse"]);
    assert!(!text(&moa_tmux).contains("mouse on"));
    // moa says nothing of the mouse yet, so the server has the user's setting: off here.
    rig.tmux(&["set-option", "-g", "mouse", "off"]);

    save(&rig, &stub_moa_toml());
    wait_for(WAIT, || (mouse() == "on").then_some(()))
        .unwrap_or_else(|| panic!("the server never sourced the new config: {:?}", counts(&up)));
    assert!(text(&moa_tmux).contains("set -g  mouse on"));
    assert_eq!(counts(&up), (1, 1));
    up.stop().await;
}

/// Two saves within the settle are read once, as the second left the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saving_twice_reloads_once() {
    let rig = Rig::new(&[]);
    let up = rig.start().await;
    assert_eq!(brain_row(&up)["picker"], json!("popup"));

    save(&rig, &format!("{}[ui]\nmouse = false\n", stub_moa_toml()));
    save(
        &rig,
        &format!("{}[ui]\npicker = \"tab\"\n", stub_moa_toml()),
    );
    wait_for(WAIT, || {
        (brain_row(&up)["picker"] == json!("tab")).then_some(())
    })
    .unwrap_or_else(|| panic!("the save was never read: {}", brain_row(&up)));
    // Quiet for longer than the settle: no second read comes.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(counts(&up).0, 1);
    up.stop().await;
}
