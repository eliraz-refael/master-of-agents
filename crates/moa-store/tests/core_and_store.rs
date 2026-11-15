//! The core's in-memory model and the store agree: after any sequence of verbs, each
//! `Persist` committed through the real store, a fresh load equals the live state.

use moa_core::config::BrainName;
use moa_core::effect::{Effect, Input, handle_checked, reply_of};
use moa_core::op::{ConnId, NoParams, Op, ReqId, TaskAdd, TaskSet};
use moa_core::state::{CallerInfo, State};
use moa_store::actor::StoreHandle;
use moa_testkit::brain::TypicalBrain;
use moa_testkit::ids::*;

/// A fresh state loaded from whatever is on disk.
fn reloaded(b: &TypicalBrain, snapshot: moa_store::Snapshot) -> State {
    let mut state = State::new(BrainName::default_brain(), b.config());
    let _ = handle_checked(&mut state, Input::Startup(Box::new(snapshot)), b.now());
    state
}

fn s(text: &str) -> Option<String> {
    Some(text.to_string())
}

#[tokio::test]
async fn verbs_through_the_store_match_a_reload() {
    let b = TypicalBrain::new();
    let mut live = b.state();
    let now = b.now();
    let store = StoreHandle::spawn(b.store(), b.dir.path().join("backups"));
    let lantern = b.home.join("private").join("lantern");

    let task_add = |project: &str, title: &str| TaskAdd {
        project: project.into(),
        title: title.into(),
        ..TaskAdd::default()
    };
    let task_set = |id: i64| TaskSet {
        id,
        ..TaskSet::default()
    };

    // (the verb, whether it must succeed)
    let verbs: Vec<(Op, bool)> = vec![
        (
            Op::GroupAdd {
                path: "Private/Boats".into(),
            },
            true,
        ),
        (
            Op::GroupRename {
                old: "Private/Boats".into(),
                new: "Private/Harbour".into(),
            },
            true,
        ),
        (
            Op::SessionSet {
                q: S1_WORKING[..8].into(),
                title: s("Dimmer, take two"),
                group: s("Private/Harbour"),
            },
            true,
        ),
        (
            Op::SessionSet {
                q: S2_IDLE[..8].into(),
                title: None,
                group: s("Scratch/New"),
            },
            true,
        ),
        (
            Op::SessionArchive {
                qs: vec![S4_ENDED[..8].into(), S3_WAITING[..8].into()],
            },
            true,
        ),
        (
            Op::GroupRm {
                path: "Scratch/New".into(),
            },
            false,
        ),
        (
            Op::TaskAdd(TaskAdd {
                today: true,
                due: s("fri"),
                priority: s("p1"),
                who: s("agent"),
                session: s(&S1_WORKING[..8]),
                note: s("first pass"),
                ..task_add("Lantern", "Solder the switch")
            }),
            true,
        ),
        (Op::TaskAdd(task_add("Tidepool", "Chart the tides")), true),
        (
            Op::TaskSet(TaskSet {
                status: s("active"),
                due: s("2026-12-01"),
                ..task_set(1)
            }),
            true,
        ),
        (
            Op::TaskSet(TaskSet {
                status: s("blocked"),
                blocked_on: s("a part"),
                not_today: true,
                ..task_set(2)
            }),
            true,
        ),
        (
            Op::TaskSet(TaskSet {
                status: s("done"),
                due: s(""),
                ..task_set(1)
            }),
            true,
        ),
        (
            Op::NoteAdd {
                text: "Likes the long way round".into(),
                project: None,
                kind: s("preference"),
            },
            true,
        ),
        (
            Op::NoteAdd {
                text: "Brass fittings only".into(),
                project: s("Lantern"),
                kind: None,
            },
            true,
        ),
        // An unknown project: whatever the core answers, memory and disk must still agree.
        (
            Op::NoteAdd {
                text: "Lost in the fog".into(),
                project: s("No such project"),
                kind: None,
            },
            false,
        ),
        (
            Op::ProjectAdd {
                name: "Harbour".into(),
                path: lantern.join("harbour"),
                summary: s("Boats"),
            },
            true,
        ),
        (
            Op::ProjectAdd {
                name: "Shore".into(),
                path: b.home.join("private"),
                summary: None,
            },
            true,
        ),
        (Op::TaskAdd(task_add("Harbour", "Paint the hull")), true),
        // Read-only verbs change nothing, on either side.
        (Op::Brief(NoParams), true),
        (Op::Attention(NoParams), true),
        (
            Op::SessionFind {
                q: "lantern".into(),
            },
            true,
        ),
    ];

    let mut writes = 0;
    for (n, (op, must_succeed)) in verbs.into_iter().enumerate() {
        let method = op.method();
        let effects = handle_checked(
            &mut live,
            Input::Client {
                conn: ConnId(1),
                req: ReqId(n as u64),
                caller: CallerInfo::default(),
                op,
            },
            now.clone(),
        );
        let reply = reply_of(&effects).expect("an op always gets a reply");
        if must_succeed {
            assert!(reply.is_ok(), "verb {n} ({method}) was refused");
        }
        for e in &effects {
            if let Effect::Persist(changes) = e {
                store
                    .commit(changes.clone())
                    .await
                    .unwrap_or_else(|_| panic!("verb {n} ({method}) did not commit"));
                writes += 1;
            }
        }
        // After every verb, not just at the end, so a drift names its verb.
        let disk = reloaded(&b, store.snapshot().await.expect("the snapshot loads"));
        assert_eq!(
            disk.model().topics,
            live.model().topics,
            "memory and disk differ after verb {n} ({method})"
        );
    }
    assert!(writes >= 15, "only {writes} verbs wrote anything");

    // The rows themselves, not only what the console is shown.
    let disk = reloaded(&b, store.snapshot().await.unwrap());
    assert_eq!(
        live.sessions().collect::<Vec<_>>(),
        disk.sessions().collect::<Vec<_>>()
    );
    assert_eq!(
        live.tasks().collect::<Vec<_>>(),
        disk.tasks().collect::<Vec<_>>()
    );
    assert_eq!(
        live.projects().collect::<Vec<_>>(),
        disk.projects().collect::<Vec<_>>()
    );
    assert_eq!(
        live.groups().collect::<Vec<_>>(),
        disk.groups().collect::<Vec<_>>()
    );
    assert_eq!(
        live.notes().collect::<Vec<_>>(),
        disk.notes().collect::<Vec<_>>()
    );
}
