use std::time::{Duration, Instant, SystemTime};

use moa_platform::{WallAlarm, WallTimer};
use tokio::time::timeout;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[tokio::test]
async fn fires_at_an_absolute_deadline() {
    let mut a = WallAlarm::new().unwrap();
    let t0 = Instant::now();
    a.arm(SystemTime::now() + ms(300)).unwrap();
    timeout(ms(10_000), a.fired()).await.unwrap().unwrap();
    // A wrong NOTE_* unit would fire at once or never.
    assert!(t0.elapsed() >= ms(250), "fired early: {:?}", t0.elapsed());
}

#[tokio::test]
async fn re_arming_earlier_wins() {
    let mut a = WallAlarm::new().unwrap();
    let t0 = Instant::now();
    a.arm(SystemTime::now() + ms(5_000)).unwrap();
    a.arm(SystemTime::now() + ms(200)).unwrap();
    timeout(ms(4_000), a.fired()).await.unwrap().unwrap();
    assert!(t0.elapsed() >= ms(150), "fired early: {:?}", t0.elapsed());
}

#[tokio::test]
async fn re_arming_later_wins() {
    let mut a = WallAlarm::new().unwrap();
    let deadline = SystemTime::now() + ms(1_500);
    a.arm(SystemTime::now() + ms(200)).unwrap();
    a.arm(deadline).unwrap();
    // The first deadline passes while we wait; the replaced one must not fire.
    assert!(timeout(ms(900), a.fired()).await.is_err());
    timeout(ms(10_000), a.fired()).await.unwrap().unwrap();
    assert!(SystemTime::now() >= deadline, "fired before the deadline");
}

#[tokio::test]
async fn a_past_deadline_fires_at_once() {
    let mut a = WallAlarm::new().unwrap();
    a.arm(SystemTime::now() - Duration::from_secs(60)).unwrap();
    timeout(ms(5_000), a.fired()).await.unwrap().unwrap();
}

#[tokio::test]
async fn disarm_stops_it() {
    let mut a = WallAlarm::new().unwrap();
    a.arm(SystemTime::now() + ms(200)).unwrap();
    a.disarm().unwrap();
    assert!(timeout(ms(800), a.fired()).await.is_err());
}

#[tokio::test]
async fn disarm_without_arming_is_fine() {
    let mut a = WallAlarm::new().unwrap();
    a.disarm().unwrap();
    assert!(timeout(ms(100), a.fired()).await.is_err());
}

#[tokio::test]
async fn fires_once_per_arm() {
    let mut a = WallAlarm::new().unwrap();
    a.arm(SystemTime::now() + ms(100)).unwrap();
    timeout(ms(10_000), a.fired()).await.unwrap().unwrap();
    assert!(timeout(ms(400), a.fired()).await.is_err());
}

#[tokio::test]
async fn a_dropped_wait_loses_nothing() {
    let mut a = WallAlarm::new().unwrap();
    a.arm(SystemTime::now() + ms(300)).unwrap();
    // Poll and drop the future before the expiry...
    assert!(timeout(ms(50), a.fired()).await.is_err());
    // ...and the expiry is still delivered to the next wait.
    timeout(ms(10_000), a.fired()).await.unwrap().unwrap();
}

#[tokio::test]
async fn it_works_through_the_trait_object() {
    let mut a: Box<dyn WallTimer> = Box::new(WallAlarm::new().unwrap());
    a.arm(SystemTime::now() + ms(100)).unwrap();
    timeout(ms(10_000), a.fired()).await.unwrap().unwrap();
}
