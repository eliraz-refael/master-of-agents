use std::time::Duration;

use moa_platform::ZoneWatch;
use tokio::time::timeout;

#[cfg(target_os = "macos")]
#[tokio::test]
async fn zone_watch_sees_a_post() {
    // Never the system's own name: posting that would disturb the real machine.
    let name = format!("moa.test.{}.zone", std::process::id());
    let mut w = ZoneWatch::notify_name(&name).unwrap();
    assert!(
        timeout(Duration::from_millis(300), w.changed())
            .await
            .is_err(),
        "nothing posted yet"
    );
    moa_platform::zone::post_for_test(&name).unwrap();
    moa_platform::zone::post_for_test(&name).unwrap();
    timeout(Duration::from_secs(5), w.changed())
        .await
        .unwrap()
        .unwrap();
    // Several posts collapse into the one `changed` that already returned.
    assert!(
        timeout(Duration::from_millis(300), w.changed())
            .await
            .is_err()
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn zone_watch_ignores_other_names() {
    let pid = std::process::id();
    let mut w = ZoneWatch::notify_name(&format!("moa.test.{pid}.mine")).unwrap();
    moa_platform::zone::post_for_test(&format!("moa.test.{pid}.theirs")).unwrap();
    assert!(
        timeout(Duration::from_millis(300), w.changed())
            .await
            .is_err()
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn zone_watch_sees_the_file_replaced() {
    let tmp = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::write(tmp.path().join("localtime"), "old").unwrap();
    let mut w = ZoneWatch::file(tmp.path(), "localtime").unwrap();

    std::fs::write(tmp.path().join("other"), "x").unwrap();
    assert!(
        timeout(Duration::from_millis(300), w.changed())
            .await
            .is_err(),
        "an unrelated file must not count"
    );

    std::fs::write(tmp.path().join("localtime.new"), "new").unwrap();
    std::fs::rename(
        tmp.path().join("localtime.new"),
        tmp.path().join("localtime"),
    )
    .unwrap();
    timeout(Duration::from_secs(5), w.changed())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn the_system_watch_opens() {
    ZoneWatch::system().unwrap();
}
