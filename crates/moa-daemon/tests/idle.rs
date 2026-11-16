use std::sync::atomic::Ordering;
use std::time::Duration;

use jiff::tz::TimeZone;
use moa_core::config::BrainName;
use moa_core::state::Deps;
use moa_daemon::testing::FakeTime;
use moa_daemon::{CoreHandle, CoreOptions};
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use tokio::time::timeout;

const PATIENCE: Duration = Duration::from_secs(5);

#[tokio::test]
async fn an_idle_brain_wakes_only_for_the_backup() {
    let brain = TypicalBrain::new();
    let time = FakeTime::new(brain.now().wall);
    let backups = brain.dir.path().join("backups");
    let core = timeout(
        PATIENCE,
        CoreHandle::start(CoreOptions {
            brain: BrainName::default_brain(),
            config: brain.config(),
            deps: Deps::default(),
            tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
            clock: time.clone(),
            alarm: Box::new(time.alarm()),
            db: brain.db.clone(),
            backups: backups.clone(),
            events_capacity: 256,
        }),
    )
    .await
    .expect("start is in time")
    .expect("the core starts");
    let launches = moa_platform::spawn::launches();
    let stats = core.stats();
    for hour in 1..=24 {
        time.advance(Duration::from_secs(3600));
        if hour == 12 {
            // Local midnight: the wake must come from the alarm, not from a barrier.
            timeout(PATIENCE, async {
                while stats.timer_fires.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the alarm fired");
        }
        timeout(PATIENCE, core.barrier())
            .await
            .expect("barrier is in time");
    }
    // Startup, the link's answer to Startup's connect (there is no terminal link yet) and
    // the midnight Timer(Backup), not counting the barriers.
    assert_eq!(stats.inputs.load(Ordering::SeqCst), 3);
    assert_eq!(stats.timer_fires.load(Ordering::SeqCst), 1);
    assert!(backups.join("daily-2026-11-15.db").exists());
    assert!(backups.join("daily-2026-11-16.db").exists());
    assert_eq!(moa_platform::spawn::launches(), launches);
}
