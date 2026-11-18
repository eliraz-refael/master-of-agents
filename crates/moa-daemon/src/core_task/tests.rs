//! The start drain's signal to the core: what the core steps, in order, as a start runs.

use std::time::Duration;

use moa_core::config::BrainName;
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use serde_json::json;
use tokio::time::timeout;

use super::*;
use crate::testing::FakeTime;

const PATIENCE: Duration = Duration::from_secs(10);

fn options(brain: &TypicalBrain, time: &Arc<FakeTime>, spool: Option<PathBuf>) -> CoreOptions {
    CoreOptions {
        brain: BrainName::default_brain(),
        config: brain.config(),
        deps: Deps::default(),
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        db: brain.db.clone(),
        backups: brain.dir.path().join("backups"),
        events_capacity: 16,
        tmux: None,
        spool,
    }
}

/// A hook line for a session moa doesn't know: applied as history, or not at all, it is
/// still one `Hook` input.
fn hook_line(seq: u32) -> Vec<u8> {
    let conv = "c0ffee00-0000-4000-8000-000000000001";
    json!({
        "v": 1,
        "agent": "claude",
        "at_ns": 1_700_000_000_000_000_000i64 + i64::from(seq),
        "pid": 100,
        "seq": seq,
        "env": {"claude_session_id": conv},
        "payload": {"session_id": conv, "hook_event_name": "Stop"},
    })
    .to_string()
    .into_bytes()
}

/// The kinds of inputs the core stepped by the time a start has settled. The link is a
/// channel that is kept open, so `Startup`'s connect adds no input of its own.
async fn stepped_at_start(brain: &TypicalBrain, spool: Option<PathBuf>) -> Vec<&'static str> {
    let time = FakeTime::new(brain.now().wall);
    let (link, _commands) = mpsc::unbounded_channel();
    let core = timeout(
        PATIENCE,
        CoreHandle::start_inner(
            options(brain, &time, spool),
            brain.store(),
            vec![],
            Some(link),
            0,
        ),
    )
    .await
    .expect("the core starts in time")
    .expect("the core starts");
    timeout(PATIENCE, core.barrier())
        .await
        .expect("the barrier answers");
    let kinds = lock(&core.stats.stepped).clone();
    timeout(PATIENCE, core.stop())
        .await
        .expect("the core stops");
    kinds
}

/// R2: the core hears `StartDrained` once, after every hook the start drain fed it, or
/// right after `Startup` when there is no spool.
#[tokio::test]
async fn start_drained_is_sent_after_the_drain() {
    let brain = TypicalBrain::new();
    let spool = brain.dir.path().join("spool").join("default.jsonl");
    for seq in 1..=3 {
        spool::append(&spool, &hook_line(seq)).unwrap();
    }
    assert_eq!(
        stepped_at_start(&brain, Some(spool.clone())).await,
        ["Startup", "Hook", "Hook", "Hook", "StartDrained"]
    );
    assert!(!spool::draining(&spool).exists(), "the drain was committed");

    let brain = TypicalBrain::new();
    assert_eq!(
        stepped_at_start(&brain, None).await,
        ["Startup", "StartDrained"]
    );
}
