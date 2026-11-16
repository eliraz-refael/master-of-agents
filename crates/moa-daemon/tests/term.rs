//! Its own test binary: it sends SIGTERM to its own process, which `run` has taken over.

mod common;

use std::process::Command;
use std::time::Duration;

use serde_json::json;
use tokio::time::timeout;

use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_stops_cleanly_even_with_a_subscriber_connected() {
    let d = boot().await;
    let mut sub = Client::connect(&d.sock);
    sub.hello();
    sub.call("subscribe", json!({"topics": ["tasks"]}));

    let status = Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success());

    let ended = timeout(Duration::from_secs(10), d.done)
        .await
        .expect("run ends within 10 s")
        .expect("no panic");
    assert!(ended.is_ok(), "{ended:?}");
    assert!(!d.sock.exists(), "the socket file is removed");
    while sub.read().is_some() {}
}
