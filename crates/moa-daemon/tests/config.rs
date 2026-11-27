//! Slice 9: the daemon reads `moa.toml` again when it is saved (L8, §6.15.4): a good one
//! replaces the config, and a `[ui]` change rewrites moa's tmux config and has the terminal
//! server source it. An editor's several steps are one save.

mod common;

use std::path::Path;
use std::sync::atomic::Ordering;

use common::create_server;
use common::rig::{Rig, Up, WAIT};
use moa_core::config_reload::{INSTRUCTIONS_CHANGED, MOA_TOML_GONE};
use moa_core::publish::{CoreEvent, Topic};
use moa_daemon::SETTLE;
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

/// Waits for something not to happen: well past the settle, so a read it would have let
/// through has been made.
async fn quiet() {
    tokio::time::sleep(SETTLE * 3).await;
}

/// Two saves within the settle, with a gap between them shorter than it, are read once, as
/// the second left the file. Without the settle the first would be read on its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saving_twice_reloads_once() {
    let rig = Rig::new(&[]);
    let up = rig.start().await;
    assert_eq!(brain_row(&up)["picker"], json!("popup"));

    save(&rig, &format!("{}[ui]\nmouse = false\n", stub_moa_toml()));
    tokio::time::sleep(SETTLE * 3 / 10).await;
    save(
        &rig,
        &format!("{}[ui]\npicker = \"tab\"\n", stub_moa_toml()),
    );
    wait_for(WAIT, || {
        (brain_row(&up)["picker"] == json!("tab")).then_some(())
    })
    .unwrap_or_else(|| panic!("the save was never read: {}", brain_row(&up)));
    quiet().await;
    assert_eq!(counts(&up).0, 1);
    up.stop().await;
}

/// `moa.toml` removed while moa runs keeps the last config, and says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_moa_toml_keeps_the_last() {
    let rig = Rig::new(&[]);
    let up = rig.start().await;
    let mut events = up.core.events();
    let accounts = brain_row(&up)["accounts"].clone();
    assert_eq!(accounts, json!(["acct-a"]));

    std::fs::remove_file(rig.paths.config_file()).expect("moa.toml is removed");
    let said = wait_for(WAIT, || {
        loop {
            match events.try_recv() {
                Ok(CoreEvent::Error { message }) => break Some(message),
                Ok(_) => continue,
                Err(_) => break None,
            }
        }
    });
    assert_eq!(said.as_deref(), Some(MOA_TOML_GONE));
    assert_eq!(brain_row(&up)["accounts"], accounts);
    assert_eq!(counts(&up).0, 1);
    up.stop().await;
}

/// `layouts.json` saved by hand is published; the scope file and the constitution saved
/// together say one line, that they apply from moa's next thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn layouts_and_instructions_reload() {
    let rig = Rig::new(&[]);
    let up = rig.start().await;
    let dir = rig.paths.config_dir();
    let layouts = || {
        let model = up.core.model();
        let model = model.borrow();
        model.topics[&Topic::Layouts].contains_key("mine")
    };
    assert!(!layouts());
    std::fs::write(
        dir.join("layouts.json"),
        r#"{"layouts": {"mine": {"panel": "tree"}}}"#,
    )
    .expect("layouts.json is saved");
    wait_for(WAIT, || layouts().then_some(())).expect("the new layouts are published");

    std::fs::write(dir.join("default.md"), "This brain keeps the garden.").expect("the scope");
    std::fs::write(dir.join("constitution.md"), "# You are moa.").expect("the constitution");
    let lines = || {
        rig.store()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM activity WHERE text = ?1",
                [INSTRUCTIONS_CHANGED],
                |r| r.get::<_, i64>(0),
            )
            .expect("the activity rows")
    };
    wait_for(WAIT, || (lines() > 0).then_some(())).expect("the line is saved");
    quiet().await;
    assert_eq!(lines(), 1);
    up.stop().await;
}
