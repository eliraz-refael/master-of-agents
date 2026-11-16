//! Its own test binary: the log subscriber is global, so this one process has exactly one
//! `run` that sets it, and it is this test's.

mod common;

use std::time::Duration;

use moa_daemon::logging::init_at;
use serde_json::json;
use tokio::time::timeout;
use tracing_subscriber::filter::LevelFilter;

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_log_names_no_record_contents() {
    // Everything, so a leak at any level would show.
    let log = std::cell::OnceCell::new();
    let d = boot_with(
        |paths| {
            moa_platform::ensure_private_dir(&paths.log_dir()).unwrap();
            init_at(&paths.log("default"), LevelFilter::TRACE).unwrap();
            log.set(paths.log("default")).unwrap();
        },
        |_| {},
    )
    .await;
    let mut c = Client::connect(&d.sock);
    c.hello();
    let added = c.call(
        "task.add",
        json!({"project": "Lantern", "title": "SECRET-TITLE-7"}),
    );
    assert!(added["result"].is_object(), "{added}");
    assert_eq!(c.call("shutdown", json!({}))["result"], json!({}));
    timeout(Duration::from_secs(10), d.done)
        .await
        .expect("run ends")
        .expect("no panic")
        .expect("a clean stop");

    let text = std::fs::read_to_string(log.get().unwrap()).expect("the log exists");
    assert!(!text.contains("SECRET-TITLE-7"), "{text}");
    // The log isn't empty because it logs nothing: it names the method.
    assert!(text.contains("task.add"), "{text}");
}
